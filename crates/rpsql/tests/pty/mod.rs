//! A pseudo-terminal for the interactive gates: what IPC::Run's `<pty<` and
//! `>pty>` give `BackgroundPsql` (`BackgroundPsql.pm`, `new`).
//!
//! The standard library cannot open one, and no crate for it is approved, so
//! the four C calls it takes are declared here, in the test crate only and
//! never in `rpsql` itself (ADR-0005, 2026-09-27 amendment): `posix_openpt`,
//! `grantpt`, `unlockpt` and `ptsname`. Their signatures are POSIX and the
//! same on glibc, musl and Darwin, and so is `O_RDWR`. The child does not
//! make the terminal its controlling one, so the terminal sends it no
//! signals: a Control-C typed here reaches it as a byte.

#![allow(unsafe_code, dead_code)]

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::FromRawFd as _;
use std::os::raw::{c_char, c_int};
use std::sync::Mutex;

unsafe extern "C" {
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    fn ptsname(fd: c_int) -> *mut c_char;
}

/// `O_RDWR`, 2 in glibc's, musl's and Darwin's `fcntl.h`.
const O_RDWR: c_int = 2;

/// Held from `ptsname` until its static buffer has been copied out: the
/// tests of one binary run in parallel, and each may open a terminal.
static PTSNAME: Mutex<()> = Mutex::new(());

/// A terminal pair: the side the test types into and reads from, and the
/// side the program under test gets as stdin and stdout.
pub struct Pty {
    pub master: File,
    pub slave: File,
}

/// Open a new pseudo-terminal.
///
/// # Errors
/// Any of the calls failed.
pub fn open() -> io::Result<Pty> {
    // SAFETY: `posix_openpt` takes flags and returns a descriptor or -1.
    let fd = unsafe { posix_openpt(O_RDWR) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a descriptor this function just opened and owns.
    let master = unsafe { File::from_raw_fd(fd) };
    // SAFETY: both take a descriptor, which is open for as long as `master`.
    if unsafe { grantpt(fd) } != 0 || unsafe { unlockpt(fd) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let guard = PTSNAME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // SAFETY: `ptsname` returns NULL or a NUL-terminated string in static
    // storage; `guard` keeps every other caller in this binary out until it
    // has been copied.
    let name = unsafe { ptsname(fd) };
    if name.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: not NULL, so a C string, per the above.
    let path = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    drop(guard);
    let slave = OpenOptions::new().read(true).write(true).open(path)?;
    Ok(Pty { master, slave })
}
