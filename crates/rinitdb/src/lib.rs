//! rinitdb: `initdb` in Rust for pgrust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/initdb/initdb.c`. The CLI surface (41 long
//! options, the `"A:c:dD:E:gkL:nNsST:U:WX:"` short options, the `argv[1]`-only
//! `--help`/`--version` fast path) is complete and byte-identical in its help
//! and version output; cluster creation itself lands issue by issue
//! (Linear NAT-378 … NAT-387) following ADR-0002.
//!
//! Layout: [`cli`] is data + pure parsing, [`validate`] is the pure pre-flight
//! calculation over an [`validate::FsProbe`], [`encoding`] and [`error`] are
//! the tables and messages they need, [`conf`] renders the configuration files
//! from the vendored templates over [`pg_config`]'s build-time constants,
//! [`sync`] plans and performs `sync_pgdata`, [`help`] is the upstream text,
//! and [`run`] is the only function that writes to a stream.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod cli;
pub mod conf;
pub mod encoding;
pub mod error;
pub mod file_perm;
pub mod help;
pub mod layout;
pub mod pg_config;
pub mod sync;
pub mod validate;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

pub use cli::{Invocation, Options};
pub use conf::{AuthMethods, DateOrder, Settings};
pub use error::{InitdbError, LocaleProvider};
pub use file_perm::DataDirPerm;
pub use layout::{FsOp, layout};
pub use sync::{SyncMethod, SyncOp};
pub use validate::{CreatePlan, Environment, FsProbe, Plan, RealFs, SyncPlan, validate};

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
        Invocation::Unparsable(rendered) => {
            let _ = stderr.write_all(rendered.as_bytes());
            ExitCode::from(EXIT_USAGE)
        }
        // Everything C decides between the getopt loop and the first `mkdir`
        // is one pure calculation; only its inputs are read from the process.
        Invocation::Init(options) => {
            match validate(&options, &Environment::from_process(), &RealFs) {
                Err(err) => {
                    let _ = writeln!(stderr, "{}", err.render());
                    ExitCode::from(EXIT_FAILURE)
                }
                // initdb.c:3439 — `--sync-only` does its one job and returns 0
                // before any of the cluster-creation steps.
                Ok(Plan::Sync(plan)) => sync_only(&plan, stdout, stderr),
                Ok(Plan::Create(_)) => {
                    let _ = writeln!(
                        stderr,
                        "{}: error: cluster initialization is not implemented yet (Linear NAT-381 … NAT-387)",
                        help::PROGNAME
                    );
                    ExitCode::from(EXIT_FAILURE)
                }
            }
        }
    }
}

/// `initdb.c:3439`: the whole of the `--sync-only` path.
fn sync_only(plan: &SyncPlan, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    // initdb.c:3447 — fputs then fflush, because check_ok finishes the line
    // only once the syncing is done.
    let _ = stdout.write_all(sync::SYNCING_PROGRESS.as_bytes());
    let _ = stdout.flush();

    let ops = sync::plan(
        &plan.pgdata,
        plan.sync_method,
        plan.sync_data_files,
        &sync::RealFs,
    );
    match sync::apply(&ops, stderr) {
        // check_ok(), initdb.c:2127.
        Ok(()) => {
            let _ = stdout.write_all(sync::CHECK_OK.as_bytes());
            ExitCode::SUCCESS
        }
        Err(err) => {
            let _ = writeln!(stderr, "{}", err.render());
            ExitCode::from(EXIT_FAILURE)
        }
    }
}
