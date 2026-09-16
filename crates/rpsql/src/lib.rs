//! rpsql: psql in Rust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/psql/`, ported fresh from the C sources
//! (ADR-0003: nothing is copied from pgrust's Rust psql) on top of `rlibpq`.
//! Gates: regress `psql.sql` / `psql_crosstab.sql` / `psql_pipeline.sql`,
//! `t/001_basic.pl`, `t/020_cancel.pl`, and a byte-diff of the same input
//! through PGDG psql 18 (the method pgrust uses for its own psql).
//!
//! Today only the `--version` fast path exists so the multicall binary has a
//! real applet to dispatch to.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// The psql version this port tracks (`PG_VERSION` in `pg_config.h`).
pub const PG_VERSION: &str = "18.6";

/// What one invocation should do. Pure result of looking at argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// `psql --version` / `-V` as the first argument
    /// (`src/fe_utils/option_utils.c: handle_help_version_opts`).
    PrintVersion,
    /// Everything else, not ported yet.
    NotImplemented,
}

/// Decide what to do from the arguments after the program name.
#[must_use]
pub fn plan(args: &[OsString]) -> Invocation {
    match args.first().and_then(|a| a.to_str()) {
        Some("--version" | "-V") => Invocation::PrintVersion,
        _ => Invocation::NotImplemented,
    }
}

/// `psql (PostgreSQL) 18.6`
#[must_use]
pub fn version_line() -> String {
    format!("psql (PostgreSQL) {PG_VERSION}")
}

/// Perform the invocation, writing to the given streams.
pub fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    match plan(args) {
        Invocation::PrintVersion => {
            // A closed stdout is not an error worth reporting for a version line.
            let _ = writeln!(stdout, "{}", version_line());
            ExitCode::SUCCESS
        }
        Invocation::NotImplemented => {
            let _ = writeln!(
                stderr,
                "rpsql: error: not implemented yet (Linear NAT-398, NAT-399)"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn version_only_as_first_argument() {
        assert_eq!(plan(&args(&["--version"])), Invocation::PrintVersion);
        assert_eq!(plan(&args(&["-V"])), Invocation::PrintVersion);
        assert_eq!(
            plan(&args(&["-X", "--version"])),
            Invocation::NotImplemented
        );
        assert_eq!(plan(&args(&[])), Invocation::NotImplemented);
    }

    #[test]
    fn version_line_matches_upstream_shape() {
        assert_eq!(version_line(), "psql (PostgreSQL) 18.6");
    }
}
