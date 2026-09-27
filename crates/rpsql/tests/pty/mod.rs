//! A pseudo-terminal for the interactive gates: what IPC::Run's `<pty<` and
//! `>pty>` give `BackgroundPsql` (`BackgroundPsql.pm`, `new`).
//!
//! The standard library cannot open one, and no crate for it is approved, so
//! the four C calls it takes are declared here, in the test crate only, as
//! `rpsql`'s SIGINT handler declares its three (ADR-0009): `posix_openpt`,
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

unsafe extern "C" {
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    fn ptsname(fd: c_int) -> *mut c_char;
}

/// `O_RDWR`, 2 on every target (ADR-0007).
const O_RDWR: c_int = 2;

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
    // SAFETY: `ptsname` returns NULL or a NUL-terminated string in static
    // storage, copied out at once; the tests that call it do not race it.
    let name = unsafe { ptsname(fd) };
    if name.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: not NULL, so a C string, per the above.
    let path = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    let slave = OpenOptions::new().read(true).write(true).open(path)?;
    Ok(Pty { master, slave })
}
