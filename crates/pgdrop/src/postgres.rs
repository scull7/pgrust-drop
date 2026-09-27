//! The `postgres` applet: pgrust's server, reached through `main_main`.
//!
//! `main.c:168` answers `--version` / `-V` in `argv[1]` with
//! `PG_BACKEND_VERSIONSTR` before anything else (NAT-376). Every other
//! command line goes to pgrust's `pg_main` with the arguments untouched,
//! wrapped the way `main_main`'s own `bin/postgres.rs` wraps it: the seams
//! installed first, the `ProcExitThread` unwind a clean exit turns into
//! carried back out as the exit status, an unhandled error reported.
//!
//! Around that, NAT-407 does what `bin/postgres.rs` and pgrust's README do
//! outside it: mimalloc is the global allocator (`main.rs`) with its hooks
//! installed ([`crate::allocator`]), and the server runs on a stack sized
//! for it ([`crate::stack`]).

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use rinitdb::single_user::Server;

use crate::dispatch::applet_from_argv0;

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
/// finds its own executable, and `share_path` beside it, from it
/// (`find_my_exec`), as C does.
///
/// Write failures on the version line are ignored and the exit status is
/// still 0, as `main.c:170`'s unchecked `fputs` is.
///
/// Must be called while the process is still single-threaded: a server
/// command line goes through [`crate::stack::run_on_server_stack`], which
/// writes the environment (`RUST_MIN_STACK`). `main` calls it before
/// starting any thread; a caller that has threads of its own (NAT-409's
/// `start`, say) must run it in a fresh process instead.
pub fn run(argv0: &OsStr, args: &[OsString], stdout: &mut dyn Write) -> ExitCode {
    match request(args) {
        Request::Version => {
            let _ = stdout.write_all(VERSION_LINE.as_bytes());
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Request::Server => crate::stack::run_on_server_stack(|| serve(argv0, args)),
    }
}

/// Action: this binary, as the `postgres` `initdb` runs its single-user
/// session with when there is none beside it (NAT-383,
/// `rinitdb::single_user::resolve_server`).
#[must_use]
pub fn embedded_server() -> Option<Server> {
    // Resolved, so that a symlink named `initdb` is seen as the pgdrop it
    // points to (std resolves it on Linux already, not on macOS).
    embedded_server_at(
        std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .ok()?,
    )
}

/// Pure: `exe` as a multicall server, `pgdrop postgres …` — unless its own
/// name selects an applet (a hard link named `initdb`), which would win over
/// the first word and make it that applet instead.
#[must_use]
pub fn embedded_server_at(exe: PathBuf) -> Option<Server> {
    applet_from_argv0(exe.as_os_str())
        .is_none()
        .then(|| Server::multicall(exe))
}

/// Action: `bin/postgres.rs`'s `main` and `run`, after the embedded share
/// directory is in place ([`crate::share::prepare`], NAT-408), minus its
/// debug-only instruments ([`crate::allocator`]).
///
/// `pg_main` ends a clean shutdown by unwinding an `ipc::ProcExitThread`
/// rather than calling `exit(2)`; it is caught here and its code becomes the
/// process's, with `std::process::exit` exactly as `bin/postgres.rs` does.
/// Any other panic is re-raised untouched.
fn serve(argv0: &OsStr, args: &[OsString]) -> ExitCode {
    crate::share::prepare(&mut std::io::stderr());
    seams_init::init_all_with_transport(transport(args));
    crate::allocator::install_hooks();
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
    fn the_embedded_server_is_this_binary_unless_its_name_is_an_applet() {
        assert_eq!(
            embedded_server_at(PathBuf::from("/opt/bin/pgdrop")),
            Some(Server::multicall(PathBuf::from("/opt/bin/pgdrop")))
        );
        for name in ["initdb", "psql", "postgres"] {
            assert_eq!(
                embedded_server_at(PathBuf::from("/opt/bin").join(name)),
                None
            );
        }
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
