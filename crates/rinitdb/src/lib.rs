//! rinitdb: `initdb` in Rust for pgrust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/initdb/initdb.c`. The CLI surface (41 long
//! options, the `"A:c:dD:E:gkL:nNsST:U:WX:"` short options, the `argv[1]`-only
//! `--help`/`--version` fast path) is complete and byte-identical in its help
//! and version output; cluster creation itself lands issue by issue
//! (Linear NAT-378 … NAT-387) following ADR-0002.
//!
//! Layout: [`cli`] is data + pure planning, [`help`] is the upstream text,
//! [`run`] is the only function that writes to a stream.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod cli;
pub mod help;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

pub use cli::{Invocation, Options};

/// Exit status C initdb uses for its own errors (`pg_fatal`, `exit(1)`).
const EXIT_FAILURE: u8 = 1;
/// Exit status usage-rs uses for a command line it cannot parse (clap's).
const EXIT_USAGE: u8 = 2;

/// Perform the invocation `args` describes, writing to the given streams.
pub fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    // Writes to a closed stream are not worth a second error message.
    match cli::plan(args) {
        Invocation::PrintHelp => {
            let _ = stdout.write_all(help::usage(help::PROGNAME).as_bytes());
            ExitCode::SUCCESS
        }
        Invocation::PrintVersion => {
            let _ = writeln!(stdout, "{}", help::version_line(help::PROGNAME));
            ExitCode::SUCCESS
        }
        Invocation::Hint => {
            let _ = writeln!(stderr, "{}", help::try_help_hint(help::PROGNAME));
            ExitCode::from(EXIT_FAILURE)
        }
        Invocation::Fatal(message) => {
            let _ = writeln!(stderr, "{}: error: {message}", help::PROGNAME);
            let _ = writeln!(stderr, "{}", help::try_help_hint(help::PROGNAME));
            ExitCode::from(EXIT_FAILURE)
        }
        Invocation::Unparsable(rendered) => {
            let _ = stderr.write_all(rendered.as_bytes());
            ExitCode::from(EXIT_USAGE)
        }
        Invocation::Init(_) => {
            let _ = writeln!(
                stderr,
                "{}: error: cluster initialization is not implemented yet (Linear NAT-378 … NAT-387)",
                help::PROGNAME
            );
            ExitCode::from(EXIT_FAILURE)
        }
    }
}
