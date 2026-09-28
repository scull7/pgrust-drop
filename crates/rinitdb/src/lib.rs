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
//! says what the template can make and what is written on top of it, [`guc`]
//! is the server's check on the names `postgresql.conf` assigns, [`tz`]
//! reads the timezone database and [`findtimezone`] picks
//! the default zone over it, [`help`] is the upstream text, [`report`] is
//! what a successful run prints over [`path`]'s `src/port/path.c`, and [`run`]
//! is the only function that writes to a stream.

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
pub mod guc;
pub mod help;
pub mod image;
pub mod layout;
pub mod path;
pub mod pg_config;
pub mod report;
/// SHA-256 for the provenance tests only; see the module header for why this
/// crate carries its own and why it is not compiled into the binary.
#[cfg(test)]
mod sha256;
pub mod strerror;
pub mod sync;
pub mod tz;
pub mod validate;
pub mod wal;

use std::ffi::{OsStr, OsString};
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
///
/// `argv0` is the name this process was started as; the closing instructions
/// name the `pg_ctl` beside it (`initdb.c:3533`).
pub fn run(
    argv0: &OsStr,
    args: &[OsString],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> ExitCode {
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
            let env = Environment::from_process();
            match validate(&options, &env, &RealFs) {
                Err(err) => {
                    let _ = writeln!(stderr, "{}", err.render());
                    ExitCode::from(EXIT_FAILURE)
                }
                // initdb.c:3439 — `--sync-only` does its one job and returns 0
                // before any of the cluster-creation steps.
                Ok(Plan::Sync(plan)) => sync_only(&plan, stdout, stderr),
                Ok(Plan::Create(plan)) => {
                    let run = Run {
                        options: &options,
                        env: &env,
                        argv0,
                    };
                    create_cluster(&plan, &run, stdout, stderr)
                }
            }
        }
    }
}

/// What [`create_cluster`] needs besides the plan: the command line, the
/// environment it was validated in, and the name the process was started as.
struct Run<'a> {
    options: &'a Options,
    env: &'a Environment,
    argv0: &'a OsStr,
}

/// `main` from the ownership line (`initdb.c:3481`) to `success = true`
/// (`:3562`): the preamble, `initialize_data_directory` (`:3044`) over the
/// embedded template (ADR-0002), the sync, the `trust` warning and the
/// closing instructions, with `cleanup_directories_atexit` (`:762`) behind
/// them.
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
/// within the preamble, before the first `mkdir` as in C — also ahead of a
/// waiting refusal, unless `lc_ctype` is not C
/// ([`cluster::text_search_warning`]).
fn create_cluster(
    plan: &CreatePlan,
    run: &Run<'_>,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> ExitCode {
    let options = run.options;
    let waldir_will_fail = classify_waldir(plan.waldir.as_deref(), &RealFs).is_err();
    if let Err(err) = cluster::check_template_can_make(options, plan)
        && !waldir_will_fail
    {
        let _ = writeln!(stderr, "{}", err.render());
        return ExitCode::from(EXIT_FAILURE);
    }
    // `select_default_timezone` only reads; C asks it once the directories
    // exist (initdb.c:3091), and announces the answer there too.
    let default_timezone = RealTzSource::from_env().and_then(|src| select_default_timezone(&src));
    let settings = cluster::settings(options, plan, default_timezone);
    preamble(run, &settings, stdout, stderr);

    let mut progress = Progress::default();
    let created = initialize_data_directory(plan, options, &settings, &mut progress, stdout)
        .and_then(|()| sync_new_cluster(plan, stdout, stderr));
    if let Err(err) = created {
        let _ = writeln!(stderr, "{}", err.render());
        return clean_up_and_fail(&progress, options, stderr);
    }

    if report::needs_trust_warning(
        options.auth.as_deref(),
        options.auth_local.as_deref(),
        options.auth_host.as_deref(),
    ) {
        let _ = stdout.write_all(report::TRUST_WARNING_STDOUT.as_bytes());
        let _ = stdout.flush();
        let _ = writeln!(stderr, "{}", report::trust_warning());
    }

    if !options.no_instructions {
        let start =
            report::start_command(&run.argv0.to_string_lossy(), &plan.pgdata.to_string_lossy());
        match start {
            Ok(command) => {
                let _ = stdout.write_all(report::success(&command).as_bytes());
            }
            // appendShellString's exit(EXIT_FAILURE) (string_utils.c:589)
            // comes before `success = true`, so the exit handler still runs.
            Err(err) => {
                let _ = stdout.flush();
                let _ = writeln!(stderr, "{}", err.render());
                return clean_up_and_fail(&progress, options, stderr);
            }
        }
    }
    // `success = true` (initdb.c:3562): the exit handler has nothing to do.
    ExitCode::SUCCESS
}

/// `cleanup_directories_atexit` (`initdb.c:762`) after a failure, and the
/// exit status of the `pg_fatal` or `exit(1)` that got it there.
fn clean_up_and_fail(progress: &Progress, options: &Options, stderr: &mut impl Write) -> ExitCode {
    cleanup::apply(&cleanup::plan(progress, options.no_clean), stderr);
    ExitCode::from(EXIT_FAILURE)
}

/// Action: what `main` prints between the `pg_` check and
/// `initialize_data_directory` — the ownership lines (`initdb.c:3481`),
/// `setup_locale_encoding`'s report (`:2689`, `:2765`), `setup_text_search`
/// (`:2850`-`:2865`) and the checksum line (`:3494`-`:3504`).
///
/// The ownership lines need the effective user, which this port reads from
/// `USER`/`LOGNAME`; when neither is set they are left out rather than name
/// someone (`docs/divergences.md`). The encoding line is printed when `-E`
/// was not given, as in C, and names the encoding the cluster gets.
fn preamble(run: &Run<'_>, settings: &Settings, stdout: &mut impl Write, stderr: &mut impl Write) {
    let mut text = String::new();
    if let Some(user) = run.env.effective_user.as_deref() {
        text.push_str(&report::owned_by(user));
    }
    text.push_str(&report::locale_configuration(
        &report::Locales::of_template(settings, cluster::catalog_locales(run.options)),
    ));
    if run.options.encoding.is_none() {
        text.push_str(&report::default_encoding(cluster::TEMPLATE_ENCODING));
    }
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
    // Behind a failing `--waldir`, C writes the warning first whatever is
    // refused; text_search_warning is None when `lc_ctype` is not C.
    if let Some(warning) = cluster::text_search_warning(run.options) {
        let _ = writeln!(stderr, "{warning}");
    }
    let mut text = report::text_search_configuration(&settings.default_text_search_config);
    text.push_str(report::data_checksums(cluster::checksums(run.options)));
    let _ = stdout.write_all(text.as_bytes());
}

/// Action: one progress step — C's `printf` and `fflush(stdout)`, the step,
/// then `check_ok()` (`initdb.c:2109`) only if it succeeded, so a failure
/// leaves the line unfinished as C's `pg_fatal` does.
fn step<T>(
    stdout: &mut impl Write,
    announce: &str,
    work: impl FnOnce() -> Result<T, InitdbError>,
) -> Result<T, InitdbError> {
    let _ = stdout.write_all(announce.as_bytes());
    let _ = stdout.flush();
    let done = work()?;
    let _ = stdout.write_all(sync::CHECK_OK.as_bytes());
    let _ = stdout.flush();
    Ok(done)
}

/// Action: `initialize_data_directory` (`initdb.c:3044`) with the template
/// standing in for `bootstrap_template1` and the single-user steps after it.
///
/// C's order where the two overlap: the data directory (`:2890`), the WAL
/// directory or its symlink (`:2948`), the `subdirs[]` loop (`:3068`), the
/// top-level `PG_VERSION` (`:3087`), the probe lines of
/// `test_config_settings` (`:3091`), the configuration files (`setup_config`,
/// `:3094`). Then the image, and the files only a template needs: a
/// `pg_control` and a first WAL segment of this cluster's own. Those are
/// what `bootstrap_template1` (`:3097`) makes in C, so they are its
/// progress line; the template already carries everything the post-bootstrap
/// session (`:3108`) adds, so its line has no work of its own.
fn initialize_data_directory(
    plan: &CreatePlan,
    options: &Options,
    settings: &Settings,
    progress: &mut Progress,
    stdout: &mut impl Write,
) -> Result<(), InitdbError> {
    create_directories(plan, progress, stdout)?;
    // test_config_settings (initdb.c:1118) is not run; these are the values
    // it would try first (docs/divergences.md).
    let _ = stdout.write_all(report::config_settings(settings).as_bytes());
    let _ = stdout.flush();
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
    let generated = cluster::generated_files(settings, &template_control, &new);

    // The configuration files first, as setup_config writes them before the
    // catalogs exist; then the catalogs; then pg_control and the WAL.
    let (config, rest) = generated.split_at(conf::CONF_FILES.len());
    step(stdout, report::CREATING_CONFIGURATION_FILES, || {
        image::expand(&entries(config)?, &plan.pgdata, plan.perm)
    })?;
    step(stdout, report::RUNNING_BOOTSTRAP_SCRIPT, || {
        // The first thing the bootstrap backend does is read the file just
        // written, and refuse it over an unknown name.
        check_postgresql_conf(config, &plan.pgdata)?;
        image::expand(&template, &plan.pgdata, plan.perm)?;
        image::expand(&entries(rest)?, &plan.pgdata, plan.perm)
    })?;
    step(stdout, report::PERFORMING_POST_BOOTSTRAP, || Ok(()))
}

/// Action at the edge of [`guc::unrecognized_parameters`]: the names in the
/// `postgresql.conf` among `config`, judged as the server would, and reported
/// with the path `SelectConfigFiles` gives the file (`guc.c:1794`, `:1823`),
/// which `make_absolute_path` has made absolute against the working directory.
fn check_postgresql_conf(
    config: &[cluster::GeneratedFile],
    pgdata: &std::path::Path,
) -> Result<(), InitdbError> {
    let Some((name, contents)) = config
        .iter()
        .find(|(name, _)| name.as_str() == conf::CONF_FILES[0])
    else {
        return Ok(());
    };
    let unrecognized = guc::unrecognized_parameters(&String::from_utf8_lossy(contents));
    if unrecognized.is_empty() {
        return Ok(());
    }
    let configdir = std::path::absolute(pgdata).unwrap_or_else(|_| pgdata.to_path_buf());
    Err(InitdbError::ConfigurationFileContainsErrors {
        path: configdir.join(name).to_string_lossy().into_owned(),
        unrecognized,
    })
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
/// (`:3087`), in C's order and with C's progress lines.
///
/// The order is the whole point. C judges `--waldir` only once PGDATA exists,
/// which is why both of its `--waldir` refusals are followed by `removing data
/// directory`; `progress` records the data directory the moment the `mkdir`
/// succeeds, exactly where `initdb.c:2907` sets `made_new_pgdata`, and the WAL
/// directory where `:2979` sets `made_new_xlogdir`.
fn create_directories(
    plan: &CreatePlan,
    progress: &mut Progress,
    stdout: &mut impl Write,
) -> Result<(), InitdbError> {
    let announce = directory_progress(&plan.pgdata, plan.pgdata_action);
    step(stdout, &announce, || {
        layout::apply(std::slice::from_ref(&layout::data_directory_op(plan)))
    })?;
    progress.pgdata = Some((plan.pgdata.clone(), plan.pgdata_action));

    let waldir = classify_waldir(plan.waldir.as_deref(), &RealFs)?;
    let ops = layout::wal_directory_and_below(plan, waldir.as_ref());
    // With --waldir the first op makes (or adopts) that directory, and the
    // next one is the symlink to it; without, the first op is `pg_wal`. The
    // `subdirs[]` loop follows, and the top-level PG_VERSION is last.
    let (wal_directory, below) = ops.split_at(usize::from(waldir.is_some()));
    if let Some((path, action)) = &waldir {
        step(stdout, &directory_progress(path, *action), || {
            layout::apply(wal_directory)
        })?;
    }
    progress.waldir.clone_from(&waldir);
    let (pg_wal, below) = below.split_at(1);
    layout::apply(pg_wal)?;
    let (subdirs, version_file) = below.split_at(below.len() - 1);
    step(stdout, report::CREATING_SUBDIRECTORIES, || {
        layout::apply(subdirs)
    })?;
    layout::apply(version_file)?;
    Ok(())
}

/// `initdb.c:2898` / `:2912` (and `:2969` / `:2984` for the WAL directory):
/// which line announces what is about to be done to `path`.
fn directory_progress(path: &std::path::Path, action: DirAction) -> String {
    let path = path.to_string_lossy();
    match action {
        DirAction::Create => report::creating_directory(&path),
        DirAction::ReuseEmpty => report::fixing_permissions(&path),
    }
}

/// The `do_sync` block at the end of `main` (`initdb.c:3508`): `sync_pgdata`
/// over the new cluster with its progress line, or the note at `:3516` under
/// `--no-sync`.
fn sync_new_cluster(
    plan: &CreatePlan,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), InitdbError> {
    if !plan.do_sync {
        let _ = stdout.write_all(sync::SYNC_SKIPPED_NOTE.as_bytes());
        return Ok(());
    }
    step(stdout, sync::SYNCING_PROGRESS, || {
        let ops = sync::plan(
            &plan.pgdata,
            plan.sync_method,
            plan.sync_data_files,
            &sync::RealFs,
        );
        sync::apply(&ops, stderr)
    })
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
