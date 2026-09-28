//! A pseudo-terminal for the interactive gates: what IPC::Run's `<pty<` and
//! `>pty>` give `BackgroundPsql` (`BackgroundPsql.pm`, `new`).
//!
//! The standard library cannot open one, so the four calls it takes come
//! from `rustix::pty`'s safe API, a dev-dependency of the test crate only and
//! never of `rpsql` itself (owner, 2026-09-28, NAT-405; ADR-0010 amendment):
//! `openpt`, `grantpt`, `unlockpt` and `ptsname`. `ptsname` is rustix's
//! re-entrant one, so parallel tests need no lock around it. The child does
//! not make the terminal its controlling one, so the terminal sends it no
//! signals: a Control-C typed here reaches it as a byte.

#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::ffi::OsStrExt as _;

use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

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
    let master = openpt(OpenptFlags::RDWR)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let path = std::ffi::OsStr::from_bytes(name.as_bytes());
    let slave = OpenOptions::new().read(true).write(true).open(path)?;
    Ok(Pty {
        master: File::from(master),
        slave,
    })
}
