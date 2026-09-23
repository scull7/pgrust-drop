//! The `postgres` applet: pgrust's server, reached through `main_main`.
//!
//! `main.c:168` answers `--version` / `-V` in `argv[1]` with
//! `PG_BACKEND_VERSIONSTR` before anything else (NAT-376). Every other
//! command line goes to pgrust's `pg_main` with the arguments untouched,
//! wrapped the way `main_main`'s own `bin/postgres.rs` wraps it: the seams
//! installed first, the `ProcExitThread` unwind a clean exit turns into
//! carried back out as the exit status, an unhandled error reported.
//!
//! That is the minimum NAT-381's boot test needs (`postgres --single`).
//! What `bin/postgres.rs` does beyond it is NAT-407's: mimalloc as the global
//! allocator and its release and statistics hooks, the debug allocation
//! tracker, and a main-thread stack sized for the server rather than the
//! process default.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::process::ExitCode;

/// pgrust's `postgres --version` line, `"postgres (PostgreSQL) 18.6\n"`.
pub const VERSION_LINE: &str = main_main::PG_BACKEND_VERSIONSTR;

/// What a `postgres` command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `argv[1]` is `--version` or `-V` (`main.c:168`).
    Version,
    /// Anything else: the server proper.
    Server,
}

/// Classify the arguments after the program name. Only `argv[1]` counts, as in
/// `main.c:168`: `postgres -D x --version` is a server command line.
#[must_use]
pub fn request(args: &[OsString]) -> Request {
    match args.first().and_then(|a| a.to_str()) {
        Some("--version" | "-V") => Request::Version,
        _ => Request::Server,
    }
}

/// Pure: the transport `bin/postgres.rs` picks from `argv[1]` before any
/// seam is installed — pgwire over stdin/stdout for `--stdio-wire`, sockets
/// for everything else.
#[must_use]
pub fn transport(args: &[OsString]) -> seams_init::Transport {
    match args.first().and_then(|a| a.to_str()) {
        Some("--stdio-wire") => seams_init::Transport::StdioWire,
        _ => seams_init::Transport::Socket,
    }
}

/// Run the applet. `argv0` is the name the process was started as: pgrust
/// finds its share directory from it (`find_my_exec`), as C does.
///
/// Write failures on the version line are ignored and the exit status is
/// still 0, as `main.c:170`'s unchecked `fputs` is.
pub fn run(argv0: &OsStr, args: &[OsString], stdout: &mut dyn Write) -> ExitCode {
    match request(args) {
        Request::Version => {
            let _ = stdout.write_all(VERSION_LINE.as_bytes());
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Request::Server => serve(argv0, args),
    }
}

/// Action: `bin/postgres.rs`'s `main` and `run`, minus what NAT-407 owns
/// (see the module header).
///
/// `pg_main` ends a clean shutdown by unwinding an `ipc::ProcExitThread`
/// rather than calling `exit(2)`; it is caught here and its code becomes the
/// process's, with `std::process::exit` exactly as `bin/postgres.rs` does.
/// Any other panic is re-raised untouched.
fn serve(argv0: &OsStr, args: &[OsString]) -> ExitCode {
    seams_init::init_all_with_transport(transport(args));
    let pg_argv = main_main::argv_from_os(std::iter::once(argv0.to_owned()).chain(args.to_vec()));
    match std::panic::catch_unwind(|| main_main::pg_main(&pg_argv)) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(error)) => {
            elog::emit_unhandled_error_report(&error);
            ExitCode::FAILURE
        }
        Err(payload) => match payload.downcast_ref::<ipc::ProcExitThread>() {
            Some(exit) => std::process::exit(exit.code),
            None => std::panic::resume_unwind(payload),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_version_line_is_pgrusts_and_names_postgresql_18_6() {
        assert_eq!(VERSION_LINE, "postgres (PostgreSQL) 18.6\n");
    }

    #[test]
    fn only_the_first_argument_asks_for_the_version() {
        assert_eq!(request(&args(&["--version"])), Request::Version);
        assert_eq!(request(&args(&["-V"])), Request::Version);
        assert_eq!(request(&args(&["-D", "x", "--version"])), Request::Server);
        assert_eq!(request(&args(&["--single"])), Request::Server);
        assert_eq!(request(&[]), Request::Server);
    }

    #[test]
    fn only_a_leading_stdio_wire_picks_the_stdio_transport() {
        assert!(matches!(
            transport(&args(&["--stdio-wire"])),
            seams_init::Transport::StdioWire
        ));
        assert!(matches!(
            transport(&args(&["--single", "--stdio-wire"])),
            seams_init::Transport::Socket
        ));
        assert!(matches!(transport(&[]), seams_init::Transport::Socket));
    }
}
