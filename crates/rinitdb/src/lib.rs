//! rinitdb: `initdb` in Rust for pgrust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/initdb/initdb.c`. The CLI surface (41 long
//! options, the `"A:c:dD:E:gkL:nNsST:U:WX:"` short options, the `argv[1]`-only
//! `--help`/`--version` fast path) is complete and byte-identical in its help
//! and version output; cluster creation expands the embedded template
//! (ADR-0002, NAT-381), and the rest lands issue by issue (Linear NAT-383 …
//! NAT-387).
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
//! builds on, [`wal`] writes a new cluster's first WAL segment, [`cluster`]
//! says what the template can make and what is written on top of it, [`tz`]
//! reads the timezone database and [`findtimezone`] picks
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
pub mod cluster;
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
/// SHA-256 for the provenance tests only; see the module header for why this
/// crate carries its own and why it is not compiled into the binary.
#[cfg(test)]
mod sha256;
pub mod strerror;
pub mod sync;
pub mod tz;
pub mod validate;
pub mod wal;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

pub use cleanup::Progress;
pub use cli::{Invocation, Options};
pub use conf::{AuthMethods, DateOrder, Settings};
pub use control::{
    CheckPoint, ChecksumSwitch, ControlFile, ControlFileError, DataChecksums, DbState, NewCluster,
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
                Ok(Plan::Create(plan)) => create_cluster(&plan, &options, stderr),
            }
        }
    }
}

/// `initialize_data_directory` (`initdb.c:3044`) and the sync after it, over
/// the embedded template (ADR-0002), with `cleanup_directories_atexit`
/// (`:762`) behind them.
///
/// Action, and the reason it is one function: C's exit handler reports the
/// directories the creation sequence had already made, so every failure from
/// the first `mkdir` onwards leaves by the same door — render the
/// diagnostic, then hand [`cleanup::plan`]'s verdict to [`cleanup::apply`].
///
/// What the template cannot make is refused before that door
/// ([`cluster::check_template_can_make`]), so a refusal leaves nothing to
/// take back — unless `--waldir` is going to fail. C reports that failure
/// after making PGDATA (and then removes it), and an upstream error keeps
/// precedence over this port's refusal, so then the refusal waits and the
/// sequence below fails where C's does. The refusal is evaluated again once
/// the directories exist, so a `--waldir` that fails here and passes there
/// cannot skip it. `setup_text_search`'s warning (`initdb.c:3492`) comes
/// next, before the first `mkdir` as in C — also ahead of a waiting refusal,
/// unless `lc_ctype` is not C ([`cluster::text_search_warning`]).
/// Progress output and the closing
/// instructions are C's stdout and are not printed yet (NAT-387).
fn create_cluster(plan: &CreatePlan, options: &Options, stderr: &mut impl Write) -> ExitCode {
    let waldir_will_fail = classify_waldir(plan.waldir.as_deref(), &RealFs).is_err();
    let can_make = cluster::check_template_can_make(options, plan);
    match can_make {
        Err(err) if !waldir_will_fail => {
            let _ = writeln!(stderr, "{}", err.render());
            return ExitCode::from(EXIT_FAILURE);
        }
        // Behind a failing `--waldir`, C writes the warning first whatever
        // is refused; text_search_warning is None when `lc_ctype` is not C.
        _ => {
            if let Some(warning) = cluster::text_search_warning(options) {
                let _ = writeln!(stderr, "{warning}");
            }
        }
    }
    let mut progress = Progress::default();
    let created = initialize_data_directory(plan, options, &mut progress)
        .and_then(|()| sync_new_cluster(plan, stderr));
    match created {
        // `success = true` (initdb.c:3562): the exit handler has nothing to do.
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(stderr, "{}", err.render());
            cleanup::apply(&cleanup::plan(&progress, options.no_clean), stderr);
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Action: `initialize_data_directory` (`initdb.c:3044`) with the template
/// standing in for `bootstrap_template1` and the single-user steps after it.
///
/// C's order where the two overlap: the data directory (`:2890`), the WAL
/// directory or its symlink (`:2948`), the `subdirs[]` loop (`:3068`), the
/// top-level `PG_VERSION` (`:3087`), the configuration files (`setup_config`,
/// `:3094`). Then the image, and the files only a template needs: a
/// `pg_control` and a first WAL segment of this cluster's own.
fn initialize_data_directory(
    plan: &CreatePlan,
    options: &Options,
    progress: &mut Progress,
) -> Result<(), InitdbError> {
    create_directories(plan, progress)?;
    // Again, on the path that writes the image: create_cluster's check is
    // skipped when its `--waldir` probe fails, and that probe is not this one.
    cluster::check_template_can_make(options, plan)?;

    let template = image::parse(image::TEMPLATE).map_err(|err| InitdbError::TemplateDamaged {
        reason: err.to_string(),
    })?;
    let template_control = ControlFile::parse(image::TEMPLATE_CONTROL).map_err(|err| {
        InitdbError::TemplateDamaged {
            reason: format!("template.control: {err}"),
        }
    })?;
    let new = NewCluster {
        system_identifier: SystemIdentifier::generate(),
        checksums: cluster::checksums(options),
        mock_authentication_nonce: control::strong_random_nonce()
            .map_err(|_| InitdbError::CouldNotGenerateSecretToken)?,
        now: unix_now(),
    };
    let default_timezone = RealTzSource::from_env().and_then(|src| select_default_timezone(&src));
    let settings = cluster::settings(options, plan, default_timezone);
    let generated = cluster::generated_files(&settings, &template_control, &new);

    // The configuration files first, as setup_config writes them before the
    // catalogs exist; then the catalogs; then pg_control and the WAL.
    let (config, rest) = generated.split_at(conf::CONF_FILES.len());
    image::expand(&entries(config)?, &plan.pgdata, plan.perm)?;
    image::expand(&template, &plan.pgdata, plan.perm)?;
    image::expand(&entries(rest)?, &plan.pgdata, plan.perm)?;
    Ok(())
}

/// The generated files as image entries, so [`image::expand`] writes them
/// with the same modes and the same refusal to overwrite.
fn entries(files: &[cluster::GeneratedFile]) -> Result<Vec<image::Entry<&[u8]>>, InitdbError> {
    files
        .iter()
        .map(|(path, contents)| {
            image::ImagePath::new(path)
                .map(|path| image::Entry::file(path, contents.as_slice()))
                .map_err(|err| InitdbError::TemplateDamaged {
                    reason: err.to_string(),
                })
        })
        .collect()
}

/// `time(NULL)`.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Action: `create_data_directory` (`initdb.c:2890`), `create_xlog_or_symlink`
/// (`:2948`), the `subdirs[]` loop (`:3068`) and `write_version_file(NULL)`
/// (`:3087`), in C's order.
///
/// The order is the whole point. C judges `--waldir` only once PGDATA exists,
/// which is why both of its `--waldir` refusals are followed by `removing data
/// directory`; `progress` records the data directory the moment the `mkdir`
/// succeeds, exactly where `initdb.c:2907` sets `made_new_pgdata`, and the WAL
/// directory where `:2979` sets `made_new_xlogdir`.
fn create_directories(plan: &CreatePlan, progress: &mut Progress) -> Result<(), InitdbError> {
    layout::apply(std::slice::from_ref(&layout::data_directory_op(plan)))?;
    progress.pgdata = Some((plan.pgdata.clone(), plan.pgdata_action));

    let waldir = classify_waldir(plan.waldir.as_deref(), &RealFs)?;
    let ops = layout::wal_directory_and_below(plan, waldir.as_ref());
    // With --waldir the first op makes (or adopts) that directory.
    let (wal_directory, below) = match &waldir {
        Some(_) => ops.split_at(1),
        None => ops.split_at(0),
    };
    layout::apply(wal_directory)?;
    progress.waldir.clone_from(&waldir);
    layout::apply(below)?;
    Ok(())
}

/// The `do_sync` block at the end of `main` (`initdb.c:3508`): `sync_pgdata`
/// over the new cluster, or nothing under `--no-sync`.
fn sync_new_cluster(plan: &CreatePlan, stderr: &mut impl Write) -> Result<(), InitdbError> {
    if !plan.do_sync {
        return Ok(());
    }
    let ops = sync::plan(
        &plan.pgdata,
        plan.sync_method,
        plan.sync_data_files,
        &sync::RealFs,
    );
    sync::apply(&ops, stderr)
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
