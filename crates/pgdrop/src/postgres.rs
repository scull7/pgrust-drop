//! The `postgres` applet: pgrust's server, reached through `main_main`.
//!
//! Only the version fast path is wired so far (NAT-376): `main.c:168` answers
//! `--version` / `-V` in `argv[1]` with `PG_BACKEND_VERSIONSTR` before anything
//! else, and pgrust's `main_main` carries that string. Everything else — the
//! seam installation `main_main`'s own `bin/postgres.rs` does, the allocator,
//! the stack, then `pg_main` — is embedding the server, which is NAT-407.

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// pgrust's `postgres --version` line, `"postgres (PostgreSQL) 18.6\n"`.
pub const VERSION_LINE: &str = main_main::PG_BACKEND_VERSIONSTR;

/// What a `postgres` command line asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// `argv[1]` is `--version` or `-V` (`main.c:168`).
    Version,
    /// Anything else: the server proper, not embedded yet.
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

/// Run the applet. Write failures on the version line are ignored and the exit
/// status is still 0, as `main.c:170`'s unchecked `fputs` is.
pub fn run(args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> ExitCode {
    match request(args) {
        Request::Version => {
            let _ = stdout.write_all(VERSION_LINE.as_bytes());
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Request::Server => {
            let _ = writeln!(
                stderr,
                "pgdrop: error: the pgrust server is not embedded yet (Linear NAT-407)"
            );
            ExitCode::FAILURE
        }
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
}
