//! Everything initdb prints on the happy path, byte for byte against C
//! initdb (Linear NAT-387): the `-d`/`-n` notices, the preamble, the `-s`/`-d`
//! settings block, the progress lines of `initialize_data_directory`, the
//! sync line or the `--no-sync` note, the `trust` warning and the closing
//! instructions (`src/bin/initdb/initdb.c`, `main` from `:3481` to `:3562`).
//!
//! `001_initdb.pl` asserts none of this — its `command_ok` cases only need
//! exit 0 — so these gates are this port's own, over command lines taken from
//! it and from `PostgreSQL::Test::Cluster::init` (`--no-sync`, `--auth trust`).
//!
//! Each side runs in a directory of its own with the data directory given as
//! the relative `data`, so both print the same path and no normalizer is
//! needed for it. The normalizers are `pg-ctl-directory` — the closing
//! instructions name the `pg_ctl` beside initdb's own `argv[0]`, and the two
//! binaries live in two directories — and, for `-s` and `-d` only,
//! `install-directories` for the same reason, `extra-version` for a
//! distribution's suffix on the block's `VERSION=` line, and `backend-log`
//! for the bootstrap backend's DEBUG log, which this port has no backend to
//! write.
//! Both run under `LC_ALL=C`, `TZ=UTC` and
//! a `USER` that is the real user, because this port reads the effective user
//! from `USER` and does not consult the environment's locale
//! (`docs/divergences.md`), and with `-E UTF8 -U postgres`, the encoding and
//! superuser the template fixes.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]
#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use testkit::normalize::{
    BACKEND_LOG, EXTRA_VERSION, INSTALL_DIRECTORIES, Normalizer, PATCHED_SUCCESS,
    PATCHED_SUCCESS_LINE, PG_CTL_DIRECTORY,
};
use testkit::{CommandOutcome, Environment, Scope, reference};

const RINITDB: &str = env!("CARGO_BIN_EXE_rinitdb");

/// The template's encoding and superuser (`rinitdb::image::MINT_ARGS`).
const TEMPLATE_ARGS: [&str; 4] = ["-E", "UTF8", "-U", "postgres"];

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pgdrop-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the test's temporary directory");
        Self(path)
    }

    /// A fresh working directory for one side of one case.
    fn side(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).expect("create a working directory");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

/// The name `getpwuid(geteuid())` gives C initdb, asked of `id`.
fn real_user() -> String {
    let output = Command::new("id").arg("-un").output().expect("run id -un");
    assert!(output.status.success(), "id -un failed");
    String::from_utf8(output.stdout)
        .expect("a user name is UTF-8")
        .trim_end()
        .to_owned()
}

/// Both sides' environment: C locale, UTC, and `USER` the real user.
fn gate_env() -> Environment {
    Environment::inherited()
        .with("LC_ALL", "C")
        .with("TZ", "UTC")
        .with("USER", real_user())
        .without("PGDATA")
}

/// Action: run `bin` with `argv` in `cwd`, in `env`, stdin closed.
fn run_in_dir(bin: &Path, argv: &[OsString], cwd: &Path, env: &Environment) -> CommandOutcome {
    let mut command = Command::new(bin);
    command.args(argv).current_dir(cwd).stdin(Stdio::null());
    env.apply(&mut command);
    CommandOutcome::from(
        command
            .output()
            .unwrap_or_else(|err| panic!("run {}: {err}", bin.display())),
    )
}

/// One gate case: `argv` then `data`, through C initdb and rinitdb, each in
/// its own working directory (made ready by `prepare`), all three streams
/// compared.
fn gate(tag: &str, argv: &[&str], prepare: impl Fn(&Path)) {
    gate_with(tag, &[argv, &["data"]].concat(), prepare, &[], |_| ());
}

/// [`gate`] with the whole command line given, `extra` normalizers on top of
/// `pg-ctl-directory`, and `after` handed each side's working directory once
/// both have run.
fn gate_with(
    tag: &str,
    argv: &[&str],
    prepare: impl Fn(&Path),
    extra: &[Normalizer],
    after: impl Fn(&Path),
) {
    let Some(initdb) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    let tempdir = TempDir::new(tag);
    let argv = args(argv);
    let env = gate_env();
    let [theirs, ours] =
        [(initdb.as_path(), "c"), (Path::new(RINITDB), "rinitdb")].map(|(bin, side)| {
            let cwd = tempdir.side(side);
            prepare(&cwd);
            let outcome = run_in_dir(bin, &argv, &cwd, &env);
            after(&cwd);
            outcome
        });
    assert_eq!(
        theirs.status,
        Some(0),
        "C initdb {argv:?} ({tag}): {}",
        theirs.stderr_text()
    );
    let mut normalizers = [&[PG_CTL_DIRECTORY], extra].concat();
    if prints_patched_success(&theirs.stdout_text()) {
        reference::announce_skip(&format!(
            "{}: the reference initdb at {} prints a bare `{PATCHED_SUCCESS_LINE}` where upstream \
             prints the closing instructions (Alpine's package patch), so ours are folded to it \
             for this gate ({tag}); their bytes are pinned by \
             rinitdb_prints_upstreams_success_text",
            reference::SKIP_FLAG,
            initdb.display()
        ));
        normalizers.push(PATCHED_SUCCESS);
    }
    let report = testkit::gate::compare(&theirs, &ours, &normalizers, Scope::Everything);
    assert!(
        report.is_clean(),
        "gate {} vs {RINITDB} {argv:?} ({tag})\n{report}",
        initdb.display()
    );
}

/// Whether C's stdout ends with a distribution's bare `Success.` instead of
/// upstream's instructions (`initdb.c:3554`) — see
/// [`testkit::normalize::PATCHED_SUCCESS`].
fn prints_patched_success(stdout: &str) -> bool {
    stdout.ends_with(&format!("\n\n{PATCHED_SUCCESS_LINE}\n\n"))
}

fn nothing(_: &Path) {}

/// The default run: sync, the `trust` warning, the closing instructions.
#[test]
fn a_default_run_matches_reference_initdb() {
    gate("success-default", &TEMPLATE_ARGS, nothing);
}

/// `--no-sync`: the note at `initdb.c:3516` instead of the sync line —
/// the NAT-384 acceptance case that needed a finished
/// `initialize_data_directory` first.
#[test]
fn no_sync_matches_reference_initdb() {
    gate(
        "success-no-sync",
        &[&TEMPLATE_ARGS[..], &["--no-sync"]].concat(),
        nothing,
    );
}

/// `--no-instructions` (`initdb.c:3526`): no closing instructions.
#[test]
fn no_instructions_matches_reference_initdb() {
    gate(
        "success-no-instructions",
        &[&TEMPLATE_ARGS[..], &["--no-sync", "--no-instructions"]].concat(),
        nothing,
    );
}

/// `PostgreSQL::Test::Cluster::init`'s own command line (`Cluster.pm:644`):
/// `--auth trust` fills both sides, so there is no `trust` warning.
#[test]
fn auth_trust_matches_reference_initdb() {
    gate(
        "success-auth-trust",
        &[&TEMPLATE_ARGS[..], &["--no-sync", "--auth", "trust"]].concat(),
        nothing,
    );
}

/// `--auth-local` alone still leaves the host side to
/// `check_authmethod_unspecified` (`initdb.c:2572`), so the warning stays.
#[test]
fn auth_local_alone_still_warns_like_reference_initdb() {
    gate(
        "success-auth-local",
        &[&TEMPLATE_ARGS[..], &["--no-sync", "--auth-local", "peer"]].concat(),
        nothing,
    );
}

/// `--no-data-checksums` (`initdb.c:3499`), and `-T` with its warning on
/// stderr (`:2859`) and its name in the preamble (`:2864`), as in the
/// 'successful creation' case (`001_initdb.pl:51`).
#[test]
fn no_data_checksums_and_text_search_config_match_reference_initdb() {
    gate(
        "success-checksums-tsearch",
        &[
            &TEMPLATE_ARGS[..],
            &[
                "--no-sync",
                "--no-data-checksums",
                "--text-search-config",
                "german",
            ],
        ]
        .concat(),
        nothing,
    );
}

/// An existing empty data directory: `fixing permissions on existing
/// directory` (`initdb.c:2912`) instead of `creating directory` (`:2898`).
#[test]
fn an_existing_empty_directory_matches_reference_initdb() {
    gate(
        "success-existing",
        &[&TEMPLATE_ARGS[..], &["--no-sync"]].concat(),
        |cwd| std::fs::create_dir(cwd.join("data")).expect("make the empty data directory"),
    );
}

/// `--lc-messages` feeds the locale report (`initdb.c:2689`): a category
/// that differs from `lc_ctype` turns the one line into the table (`:2699`).
/// `POSIX` is the one other spelling of the C locale every lane knows:
/// glibc's and musl's `setlocale` name it `C`, so the report is the one
/// line, and Darwin's keeps `POSIX`, so it is the table.
#[test]
fn a_posix_lc_messages_is_reported_like_reference_initdb() {
    gate(
        "success-lc-messages",
        &[&TEMPLATE_ARGS[..], &["--no-sync", "--lc-messages", "POSIX"]].concat(),
        nothing,
    );
}

/// `-s` (`initdb.c:2805`-`:2819`): the ownership lines on stdout, the
/// settings block on stderr, exit 0, and nothing made. The data directory is
/// given as `./data//`, which `setup_pgdata` canonicalizes (`:2634`) before
/// the block names it; the `-E`/`-T` values are ones C only judges after `-s`
/// has exited.
#[test]
fn show_matches_reference_initdb() {
    gate_with(
        "show",
        &[
            "-s", "-U", "postgres", "-E", "LATIN1", "-T", "nope", "./data//",
        ],
        nothing,
        &[EXTRA_VERSION, INSTALL_DIRECTORIES],
        |cwd| assert!(!cwd.join("data").exists(), "-s made {}", cwd.display()),
    );
}

/// `--show` with `--debug` and `--no-clean`: both notices, in command-line
/// order, ahead of the ownership lines (`initdb.c:3298`, `:3302`).
#[test]
fn show_with_the_notices_matches_reference_initdb() {
    gate_with(
        "show-notices",
        &["--no-clean", "-U", "postgres", "--show", "--debug", "data"],
        nothing,
        &[EXTRA_VERSION, INSTALL_DIRECTORIES],
        |cwd| assert!(!cwd.join("data").exists(), "-s made {}", cwd.display()),
    );
}

/// What `-s` does not get past: the auth methods, the superuser password
/// they need and the WAL segment size are judged at `initdb.c:3454`-`:3466`,
/// after the getopt loop and before `setup_pgdata`, so C refuses these with
/// exit 1 and prints no settings block. All three streams are compared as
/// they are.
#[test]
fn show_refuses_what_reference_initdb_refuses() {
    let Some(initdb) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    let tempdir = TempDir::new("show-refusals");
    let env = gate_env();
    for (tag, argv) in [
        ("auth", &["-s", "-A", "bogus", "data"][..]),
        ("auth-host", &["-s", "--auth-host", "peer", "data"]),
        ("password", &["-s", "-A", "md5", "data"]),
        ("segsize", &["-s", "--wal-segsize", "3", "data"]),
        ("segsize-value", &["-s", "--wal-segsize", "16MB", "data"]),
        ("segsize-range", &["-s", "--wal-segsize", "2048", "data"]),
    ] {
        let argv = args(argv);
        let [theirs, ours] =
            [(initdb.as_path(), "c"), (Path::new(RINITDB), "rinitdb")].map(|(bin, side)| {
                let cwd = tempdir.side(&format!("{tag}-{side}"));
                run_in_dir(bin, &argv, &cwd, &env)
            });
        assert_eq!(
            theirs.status,
            Some(1),
            "C initdb {argv:?} ({tag}): {}",
            theirs.stderr_text()
        );
        let report = testkit::gate::compare(&theirs, &ours, &[], Scope::Everything);
        assert!(
            report.is_clean(),
            "gate {} vs {RINITDB} {argv:?} ({tag})\n{report}",
            initdb.display()
        );
    }
}

/// `-d -n`: the notices, the settings block on stderr under the ownership
/// lines, then an ordinary run. C's stderr also carries the bootstrap
/// backend's `-d 5` log (`initdb.c:1616`), which `backend-log` drops.
#[test]
fn debug_and_no_clean_match_reference_initdb() {
    gate_with(
        "debug",
        &[&TEMPLATE_ARGS[..], &["-d", "-n", "--no-sync", "data"]].concat(),
        nothing,
        &[EXTRA_VERSION, INSTALL_DIRECTORIES, BACKEND_LOG],
        |_| (),
    );
}

/// `-s` as this port prints it, pinned whole, so a machine without the
/// reference still checks every byte: `PGPATH` is this executable's own
/// directory and `share_path` what `get_share_path` makes of it
/// (`docs/divergences.md`), or `-L` canonicalized; no `USER` leaves the
/// ownership lines out and the superuser name empty.
#[test]
fn rinitdb_prints_upstreams_settings_block() {
    let tempdir = TempDir::new("show-own");
    let cwd = tempdir.side("rinitdb");
    let exe = std::fs::canonicalize(RINITDB).expect("canonicalize the binary");
    let exe = exe.to_string_lossy();
    let bindir = rinitdb::path::get_parent_directory(&exe);
    let block = |pgdata: &str, share: &str, user: &str| {
        format!(
            "VERSION=18.6\nPGDATA={pgdata}\nshare_path={share}\nPGPATH={bindir}\n\
             POSTGRES_SUPERUSERNAME={user}\nPOSTGRES_BKI={share}/postgres.bki\n\
             POSTGRESQL_CONF_SAMPLE={share}/postgresql.conf.sample\n\
             PG_HBA_SAMPLE={share}/pg_hba.conf.sample\n\
             PG_IDENT_SAMPLE={share}/pg_ident.conf.sample\n"
        )
    };

    let env = Environment::inherited()
        .with("USER", "alice")
        .with("PGDATA", "/tmp/./x/../pgdata/");
    let outcome = run_in_dir(Path::new(RINITDB), &args(&["-n", "-s"]), &cwd, &env);
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    assert_eq!(
        outcome.stdout_text(),
        "Running in no-clean mode.  Mistakes will not be cleaned up.\n\
         The files belonging to this database system will be owned by user \"alice\".\n\
         This user must also own the server process.\n\n"
    );
    assert_eq!(
        outcome.stderr_text(),
        block("/tmp/pgdata", &rinitdb::path::get_share_path(&exe), "alice")
    );

    let env = Environment::inherited()
        .without("USER")
        .without("LOGNAME")
        .without("PGDATA");
    let argv = args(&["--show", "-L", "/opt//pg/share/", "data"]);
    let outcome = run_in_dir(Path::new(RINITDB), &argv, &cwd, &env);
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    assert_eq!(outcome.stdout_text(), "");
    assert_eq!(outcome.stderr_text(), block("data", "/opt/pg/share", ""));
    assert!(!cwd.join("data").exists());
}

/// This port's own stdout, pinned whole, so a machine without the reference
/// still checks every byte: the same text C prints, with the encoding line
/// `-E` leaves out when it is not given naming the template's `UTF8`
/// (`docs/divergences.md`), and a `--waldir` line where C prints one.
#[test]
fn rinitdb_prints_upstreams_success_text() {
    let tempdir = TempDir::new("success-own");
    let cwd = tempdir.side("rinitdb");
    let waldir = tempdir.side("wal-parent").join("wal");
    let argv = args(&[
        "-U",
        "postgres",
        "--no-sync",
        "--waldir",
        &waldir.to_string_lossy(),
        "data",
    ]);
    let env = Environment::inherited()
        .with("USER", "alice")
        .with("TZ", "UTC")
        .without("PGDATA");
    let outcome = run_in_dir(Path::new(RINITDB), &argv, &cwd, &env);
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    let bindir = Path::new(RINITDB).parent().expect("the binary's directory");
    let timezone = outcome
        .stdout_text()
        .lines()
        .find_map(|line| line.strip_prefix("selecting default time zone ... "))
        .expect("the time zone line")
        .to_owned();
    assert_eq!(
        outcome.stdout_text(),
        format!(
            "The files belonging to this database system will be owned by user \"alice\".\n\
             This user must also own the server process.\n\
             \n\
             The database cluster will be initialized with locale \"C\".\n\
             The default database encoding has accordingly been set to \"UTF8\".\n\
             The default text search configuration will be set to \"english\".\n\
             \n\
             Data page checksums are enabled.\n\
             \n\
             creating directory data ... ok\n\
             creating directory {waldir} ... ok\n\
             creating subdirectories ... ok\n\
             selecting dynamic shared memory implementation ... posix\n\
             selecting default \"max_connections\" ... 100\n\
             selecting default \"shared_buffers\" ... 128MB\n\
             selecting default time zone ... {timezone}\n\
             creating configuration files ... ok\n\
             running bootstrap script ... ok\n\
             performing post-bootstrap initialization ... ok\n\
             \n\
             Sync to disk skipped.\n\
             The data directory might become corrupt if the operating system crashes.\n\
             \n\
             \n\
             Success. You can now start the database server using:\n\
             \n\
             \x20   {bindir}/pg_ctl -D data -l logfile start\n\
             \n",
            waldir = waldir.display(),
            bindir = bindir.display(),
        )
    );
    assert_eq!(
        outcome.stderr_text(),
        "initdb: warning: enabling \"trust\" authentication for local connections\n\
         initdb: hint: You can change this by editing pg_hba.conf or using the option -A, or \
         --auth-local and --auth-host, the next time you run initdb.\n"
    );
}

/// With neither `USER` nor `LOGNAME` this port cannot name the effective
/// user, so the two ownership lines are left out rather than name someone
/// (`docs/divergences.md`); the rest is unchanged.
#[test]
fn without_a_user_name_the_ownership_lines_are_left_out() {
    let tempdir = TempDir::new("success-no-user");
    let cwd = tempdir.side("rinitdb");
    let argv = args(&[
        "-U",
        "postgres",
        "-A",
        "trust",
        "--no-sync",
        "--no-instructions",
        "data",
    ]);
    let env = Environment::inherited()
        .without("USER")
        .without("LOGNAME")
        .without("PGDATA");
    let outcome = run_in_dir(Path::new(RINITDB), &argv, &cwd, &env);
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    assert!(
        outcome
            .stdout_text()
            .starts_with("The database cluster will be initialized with locale \"C\".\n"),
        "{}",
        outcome.stdout_text()
    );
    assert_eq!(outcome.stderr_text(), "");
}

/// `appendShellString` refuses a data directory with a newline in it
/// (`src/fe_utils/string_utils.c:586`), after the cluster exists and before
/// `success = true`, so the exit handler removes what was made.
#[test]
fn a_data_directory_the_shell_cannot_quote_fails_and_is_removed() {
    let tempdir = TempDir::new("success-newline");
    let cwd = tempdir.side("rinitdb");
    let argv = args(&["-U", "postgres", "-A", "trust", "--no-sync", "da\nta"]);
    let env = Environment::inherited()
        .with("USER", "alice")
        .without("PGDATA");
    let outcome = run_in_dir(Path::new(RINITDB), &argv, &cwd, &env);
    assert_eq!(outcome.status, Some(1));
    assert_eq!(
        outcome.stderr_text(),
        "shell command argument contains a newline or carriage return: \"da\nta\"\n\
         initdb: removing data directory \"da\nta\"\n"
    );
    assert!(!cwd.join("da\nta").exists());
}
