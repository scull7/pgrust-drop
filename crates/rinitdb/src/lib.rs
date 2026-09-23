//! rinitdb: `initdb` in Rust for pgrust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/initdb/initdb.c`. The CLI surface (41 long
//! options, the `"A:c:dD:E:gkL:nNsST:U:WX:"` short options, the `argv[1]`-only
//! `--help`/`--version` fast path) is complete and byte-identical in its help
//! and version output; cluster creation itself lands issue by issue
//! (Linear NAT-378 … NAT-387) following ADR-0002.
//!
//! Layout: [`cli`] is data + pure parsing, [`validate`] is the pure pre-flight
//! calculation over a [`validate::FsProbe`], [`encoding`] and [`error`] are the
//! tables and messages they need, [`file_perm`] holds the mode constants the
//! plan carries, [`layout`] turns that plan into the data directory tree as
//! pure [`layout::FsOp`]s, [`cleanup`] says what a failed run has to take back
//! again, [`conf`] renders the configuration files from the vendored templates
//! over [`pg_config`]'s build-time constants, [`sync`] plans and performs
//! `sync_pgdata`, [`control`] parses and rewrites `pg_control` over
//! [`crc32c`], [`image`] packs and expands the template cluster ADR-0002
//! builds on, [`tz`] reads the timezone database and [`findtimezone`] picks
//! the default zone over it, [`help`] is the upstream text, and [`run`] is the
//! only function that writes to a stream.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod cleanup;
pub mod cli;
pub mod conf;
pub mod control;
pub mod crc32c;
pub mod encoding;
pub mod error;
pub mod file_perm;
pub mod findtimezone;
pub mod help;
pub mod image;
pub mod layout;
pub mod pg_config;
pub mod strerror;
pub mod sync;
pub mod tz;
pub mod validate;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

pub use cleanup::Progress;
pub use cli::{Invocation, Options};
pub use conf::{AuthMethods, DateOrder, Settings};
pub use control::{
    CheckPoint, ChecksumSwitch, ControlFile, ControlFileError, DataChecksums, DbState,
    SystemIdentifier,
};
pub use error::{InitdbError, LocaleProvider};
pub use file_perm::DataDirPerm;
pub use findtimezone::{RealTzSource, TzSource, select_default_timezone};
pub use layout::{FsOp, layout};
pub use sync::{SyncMethod, SyncOp};
pub use validate::{
    CreatePlan, DirAction, Environment, FsProbe, Plan, RealFs, SyncPlan, classify_waldir, validate,
};

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
                Ok(Plan::Create(plan)) => create_cluster(&plan, options.no_clean, stderr),
            }
        }
    }
}

/// `initialize_data_directory` (`initdb.c:3044`) as far as this port has it,
/// with `cleanup_directories_atexit` (`:762`) behind it.
///
/// Action, and the reason the two are one function: C's exit handler reports
/// the directories the creation sequence had already made, so every failure
/// from the first `mkdir` onwards leaves by the same door — render the
/// diagnostic, then hand [`cleanup::plan`]'s verdict to [`cleanup::apply`].
fn create_cluster(plan: &CreatePlan, no_clean: bool, stderr: &mut impl Write) -> ExitCode {
    let mut progress = Progress::default();
    let diagnostic = match create_directories(plan, &mut progress) {
        Err(err) => err.render(),
        // The rest of initialize_data_directory (the subdirs loop at
        // initdb.c:3068 onwards) lands with Linear NAT-381 … NAT-387. Until it
        // does this is where a run stops, and it stops the way C stops:
        // `success` stays false, so the handler takes the directory back.
        Ok(()) => format!(
            "{}: error: cluster initialization is not implemented yet (Linear NAT-381 … NAT-387)",
            help::PROGNAME
        ),
    };
    let _ = writeln!(stderr, "{diagnostic}");
    cleanup::apply(&cleanup::plan(&progress, no_clean), stderr);
    ExitCode::from(EXIT_FAILURE)
}

/// Action: `create_data_directory` (`initdb.c:2890`) and then the head of
/// `create_xlog_or_symlink` (`:2948`), in C's order.
///
/// The order is the whole point. C judges `--waldir` only once PGDATA exists,
/// which is why both of its `--waldir` refusals are followed by `removing data
/// directory`; `progress` records the data directory the moment the `mkdir`
/// succeeds, exactly where `initdb.c:2907` sets `made_new_pgdata`.
fn create_directories(plan: &CreatePlan, progress: &mut Progress) -> Result<(), InitdbError> {
    layout::apply(std::slice::from_ref(&layout::data_directory_op(plan)))?;
    progress.pgdata = Some((plan.pgdata.clone(), plan.pgdata_action));

    // initdb.c:2955-:3012. Its mkdir half, and the subdirs loop after it, are
    // NAT-381 … NAT-387; nothing beyond this point touches the filesystem yet,
    // so `progress.waldir` stays None.
    classify_waldir(plan.waldir.as_deref(), &RealFs)?;
    Ok(())
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
        // check_ok(), initdb.c:2109.
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
