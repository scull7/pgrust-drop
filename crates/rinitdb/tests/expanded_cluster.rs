//! `rinitdb` makes a cluster from the embedded template (NAT-381, ADR-0002),
//! and the reference C server starts on it.
//!
//! There is no upstream test to steal for the template itself. The oracle is
//! the reference PostgreSQL 18.6 installation: its `pg_controldata` reads the
//! new `pg_control`, and its `postgres --single` starts on the cluster and
//! answers `select 1` — the same check pgdrop makes against pgrust
//! (`crates/pgdrop/tests/template_boot.rs`). Without the reference binaries
//! those halves print `SKIP (flagged, not silent)`; `PGDROP_REQUIRE_REF=1`
//! makes them fail instead. What does not need a server — exit status,
//! stderr, modes, the refusals — is asserted either way.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use rinitdb::control::ControlFile;
use rinitdb::single_user::SERVER_ENV;
use testkit::env::Environment;
use testkit::reference;

const RINITDB: &str = env!("CARGO_BIN_EXE_rinitdb");

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

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
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

/// `rinitdb <args> <pgdata>`: exit 0, the closing instructions at the end of
/// stdout, and on stderr only the `trust` warning (`initdb.c:3521`), which
/// `-A` silences by filling in both sides. The whole of stdout is gated
/// against C initdb in `tests/success_output.rs` (NAT-387).
fn rinitdb_ok(before: &[&str], pgdata: &Path, env: &Environment) {
    let mut argv = args(before);
    argv.push(pgdata.into());
    let outcome = testkit::run_in(Path::new(RINITDB), &argv, &[], env).expect("run rinitdb");
    assert_eq!(
        outcome.status,
        Some(0),
        "{argv:?}\nstderr: {}",
        outcome.stderr_text()
    );
    let warned = !before.iter().any(|arg| matches!(*arg, "-A" | "--auth"));
    let expected_stderr = if warned {
        format!("{}\n", rinitdb::report::trust_warning())
    } else {
        String::new()
    };
    assert_eq!(outcome.stderr_text(), expected_stderr, "{argv:?}");
    assert!(
        outcome
            .stdout_text()
            .ends_with(&format!(" -D {} -l logfile start\n\n", pgdata.display())),
        "{argv:?}: {}",
        outcome.stdout_text()
    );
}

/// The reference `postgres --single -D <pgdata> postgres` fed `sql`, or
/// `None` (with the flagged skip printed) without one.
fn reference_single_user(pgdata: &Path, sql: &str) -> Option<testkit::CommandOutcome> {
    let postgres = reference::find_or_skip("postgres")?;
    let argv = [
        OsString::from("--single"),
        OsString::from("-D"),
        pgdata.into(),
        OsString::from("postgres"),
    ];
    Some(testkit::run_with_stdin(&postgres, argv, sql.as_bytes()).expect("run postgres"))
}

/// The value single-user mode printed for `column` (`printatt`,
/// `src/backend/access/common/printtup.c:423`).
fn single_user_values<'a>(stdout: &'a str, column: &str) -> Vec<&'a str> {
    let prefix = format!("{column} = \"");
    stdout
        .lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once(&prefix)?;
            Some(rest.split_once("\"\t")?.0)
        })
        .collect()
}

#[test]
fn rinitdb_makes_a_cluster_the_reference_server_starts() {
    let tempdir = TempDir::new("expanded");
    let pgdata = tempdir.join("data");
    rinitdb_ok(
        &["-U", "postgres", "--no-sync"],
        &pgdata,
        &Environment::inherited(),
    );

    testkit::check_mode_recursive_ok(
        &pgdata,
        testkit::files::PGDATA_DIR_MODE,
        testkit::files::PGDATA_FILE_MODE,
        &[],
    );
    let control = ControlFile::parse(
        &std::fs::read(testkit::control_file_path(&pgdata)).expect("read pg_control"),
    )
    .expect("parse pg_control");
    assert!(control.crc_is_valid());
    assert_eq!(control.data_checksum_version, 1, "initdb.c:167's default");

    if let Some(pg_controldata) = reference::find_or_skip("pg_controldata") {
        let pattern = testkit::Pattern::new(
            "(?m)^Database cluster state: +shut down$[\\s\\S]*^Latest checkpoint location: +0/2000028$",
        )
        .expect("compile the pattern");
        testkit::command_like(&pg_controldata, [&pgdata], &pattern);
    }

    let Some(outcome) = reference_single_user(&pgdata, "select 1 as one;\n") else {
        return;
    };
    assert_eq!(outcome.status, Some(0), "stderr: {}", outcome.stderr_text());
    assert_eq!(single_user_values(&outcome.stdout_text(), "one"), ["1"]);
}

#[test]
fn with_waldir_and_group_access_the_cluster_still_starts() {
    let tempdir = TempDir::new("expanded-waldir");
    let pgdata = tempdir.join("data");
    let waldir = tempdir.join("wal");
    rinitdb_ok(
        &[
            "-U",
            "postgres",
            "--no-sync",
            "-g",
            "--no-data-checksums",
            "--waldir",
            &waldir.to_string_lossy(),
        ],
        &pgdata,
        &Environment::inherited(),
    );
    assert!(
        std::fs::symlink_metadata(pgdata.join("pg_wal"))
            .expect("pg_wal")
            .file_type()
            .is_symlink()
    );
    assert!(waldir.join("000000010000000000000002").is_file());
    testkit::check_mode_recursive_ok(
        &pgdata,
        testkit::files::GROUP_DIR_MODE,
        testkit::files::GROUP_FILE_MODE,
        &[],
    );
    assert_eq!(testkit::read_control_file(&pgdata).data_checksum_version, 0);

    let Some(outcome) = reference_single_user(&pgdata, "select 1 as one;\n") else {
        return;
    };
    assert_eq!(outcome.status, Some(0), "stderr: {}", outcome.stderr_text());
    assert_eq!(single_user_values(&outcome.stdout_text(), "one"), ["1"]);
}

#[test]
fn two_clusters_share_neither_identifier_nor_nonce() {
    let tempdir = TempDir::new("expanded-twice");
    let (first, second) = (tempdir.join("one"), tempdir.join("two"));
    for pgdata in [&first, &second] {
        rinitdb_ok(
            &["-U", "postgres", "--no-sync"],
            pgdata,
            &Environment::inherited(),
        );
    }
    let read = |pgdata: &Path| {
        ControlFile::parse(&std::fs::read(testkit::control_file_path(pgdata)).expect("read"))
            .expect("parse")
    };
    let (a, b) = (read(&first), read(&second));
    assert_ne!(a.system_identifier, b.system_identifier);
    assert_ne!(a.mock_authentication_nonce, b.mock_authentication_nonce);
}

/// The divergence `docs/divergences.md` records for the template: it fixes
/// encoding UTF8 and locale C. Anything else on the command line is refused
/// before a directory is made; the environment's locale is not consulted —
/// under `LC_ALL=en_US.UTF-8` and no locale switch, where C `initdb` makes an
/// `en_US.UTF-8` cluster, this one is C throughout — and `--no-locale` gets
/// UTF8 where C derives SQL_ASCII from the C locale
/// (`setup_locale_encoding`, `initdb.c:2685`).
#[test]
fn the_template_fixes_encoding_utf8_and_locale_c() {
    let tempdir = TempDir::new("expanded-locale");
    for (extra, first_line) in [
        (
            &["-E", "LATIN1"][..],
            "initdb: error: encoding \"LATIN1\" is not supported yet: the embedded template \
             cluster has encoding \"UTF8\" and locale \"C\"",
        ),
        (
            &["--lc-collate", "en_US.UTF-8"],
            "initdb: error: locale \"en_US.UTF-8\" (--lc-collate) is not supported yet: the \
             embedded template cluster has encoding \"UTF8\" and locale \"C\"",
        ),
    ] {
        let pgdata = tempdir.join("refused");
        let mut argv = args(&["-U", "postgres"]);
        argv.extend(args(extra));
        argv.push(pgdata.clone().into());
        let outcome = testkit::run(Path::new(RINITDB), &argv).expect("run rinitdb");
        assert_eq!(outcome.status, Some(1), "{argv:?}");
        assert_eq!(outcome.stdout_text(), "", "{argv:?}");
        assert_eq!(
            outcome.stderr_text().lines().next(),
            Some(first_line),
            "{argv:?}"
        );
        assert!(!pgdata.exists(), "{argv:?}: nothing is made");
    }

    let env = Environment::inherited()
        .with("LANG", "en_US.UTF-8")
        .with("LC_ALL", "en_US.UTF-8");
    let no_locale = tempdir.join("data");
    rinitdb_ok(
        &["-U", "postgres", "--no-sync", "--no-locale"],
        &no_locale,
        &env,
    );
    // No locale switch at all: C's setlocales would take all six categories
    // from LC_ALL (check_locale_name with NULL, initdb.c:2452-:2468).
    let from_env = tempdir.join("data-env");
    rinitdb_ok(&["-U", "postgres", "--no-sync"], &from_env, &env);
    let conf = std::fs::read_to_string(from_env.join("postgresql.conf")).expect("postgresql.conf");
    for guc in ["lc_messages", "lc_monetary", "lc_numeric", "lc_time"] {
        assert!(
            conf.lines()
                .any(|line| line.starts_with(&format!("{guc} = C\t"))),
            "{guc} is C, not the environment's"
        );
    }

    for pgdata in [&no_locale, &from_env] {
        let Some(outcome) = reference_single_user(
            pgdata,
            "select pg_encoding_to_char(encoding) as enc, datcollate as coll, datctype as ctype \
             from pg_database where datname = 'postgres';\n",
        ) else {
            return;
        };
        let stdout = outcome.stdout_text();
        assert_eq!(single_user_values(&stdout, "enc"), ["UTF8"], "{stdout}");
        assert_eq!(single_user_values(&stdout, "coll"), ["C"], "{stdout}");
        assert_eq!(single_user_values(&stdout, "ctype"), ["C"], "{stdout}");
    }
}

/// `setup_text_search`'s warning (`initdb.c:2859`): a `-T` other than the
/// `english` that locale C suggests is taken, with the line C writes for
/// `--no-locale -E UTF8 -T simple`.
#[test]
fn a_text_search_config_that_does_not_match_locale_c_warns() {
    let tempdir = TempDir::new("expanded-tsearch");
    let pgdata = tempdir.join("data");
    let argv = args(&[
        "-U",
        "postgres",
        "--no-sync",
        "--no-locale",
        "-E",
        "UTF8",
        "-T",
        "simple",
        &pgdata.to_string_lossy(),
    ]);
    let outcome = testkit::run(Path::new(RINITDB), &argv).expect("run rinitdb");
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "initdb: warning: specified text search configuration \"simple\" might not match \
             locale \"C\"\n{}\n",
            rinitdb::report::trust_warning()
        )
    );
    let conf = std::fs::read_to_string(pgdata.join("postgresql.conf")).expect("postgresql.conf");
    assert!(
        conf.lines()
            .any(|line| line == "default_text_search_config = 'pg_catalog.simple'")
    );
}

/// A superuser other than the template's is a single-user session to run,
/// and a `--waldir` that fails still fails where C's does, after the
/// warning: the reference initdb writes these four stderr lines, in this
/// order, for this command line. The `postgres` found ([`fake_postgres`])
/// is never started.
#[test]
fn a_failing_waldir_behind_a_superuser_rename_still_warns_first() {
    let tempdir = TempDir::new("expanded-tsearch-waldir");
    let postgres = fake_postgres(&tempdir);
    let pgdata = tempdir.join("data");
    let waldir = tempdir.join("wal");
    std::fs::create_dir(&waldir).expect("create the WAL directory");
    std::fs::write(waldir.join("occupied"), b"").expect("make the WAL directory non-empty");
    let argv = args(&[
        "--no-sync",
        "--no-locale",
        "-E",
        "UTF8",
        "-T",
        "simple",
        "-U",
        "alice",
        "--waldir",
        &waldir.to_string_lossy(),
        &pgdata.to_string_lossy(),
    ]);
    let env = Environment::inherited().with(SERVER_ENV, &postgres);
    let outcome = testkit::run_in(Path::new(RINITDB), &argv, &[], &env).expect("run rinitdb");
    assert!(!tempdir.join("session.log").exists(), "the server was run");
    assert_eq!(outcome.status, Some(1), "{}", outcome.stderr_text());
    let (pgdata, waldir) = (pgdata.display(), waldir.display());
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "initdb: warning: specified text search configuration \"simple\" might not match \
             locale \"C\"\n\
             initdb: error: directory \"{waldir}\" exists but is not empty\n\
             initdb: hint: If you want to store the WAL there, either remove or empty the \
             directory \"{waldir}\".\n\
             initdb: removing data directory \"{pgdata}\"\n"
        )
    );
}

/// A locale every lane's libc knows by this name, for the command lines the
/// reference initdb must get past `setlocales` (`initdb.c:2424`) with: musl
/// keeps any name, glibc has `C.UTF-8` built in, Darwin has no `C.UTF-8`.
const NON_C_LOCALE: &str = if cfg!(target_os = "macos") {
    "en_US.UTF-8"
} else {
    "C.UTF-8"
};

/// `<tool> --no-sync -T simple -U postgres <extra> --waldir=<non-empty> <pgdata>`
/// in `tempdir` and `env`, and the stderr it is expected to end with: C's
/// `--waldir` error, its hint and the removal of the data directory it made.
fn behind_a_failing_waldir(
    tool: &Path,
    tempdir: &TempDir,
    extra: &[&str],
    env: &Environment,
) -> (testkit::CommandOutcome, String) {
    let pgdata = tempdir.join("data");
    let waldir = tempdir.join("wal");
    let _ = std::fs::create_dir(&waldir);
    std::fs::write(waldir.join("occupied"), b"").expect("make the WAL directory non-empty");
    let mut argv = args(&["--no-sync", "-T", "simple", "-U", "postgres"]);
    argv.extend(extra.iter().map(OsString::from));
    argv.extend([
        OsString::from("--waldir"),
        waldir.clone().into(),
        pgdata.clone().into(),
    ]);
    let outcome = testkit::run_in(tool, &argv, &[], env).expect("run initdb");
    let (pgdata, waldir) = (pgdata.display(), waldir.display());
    let tail = format!(
        "initdb: error: directory \"{waldir}\" exists but is not empty\n\
         initdb: hint: If you want to store the WAL there, either remove or empty the \
         directory \"{waldir}\".\n\
         initdb: removing data directory \"{pgdata}\"\n"
    );
    (outcome, tail)
}

/// `setup_text_search`'s line (`initdb.c:2859`) for `-T simple` and `lc_ctype`.
fn simple_might_not_match(lc_ctype: &str) -> String {
    format!(
        "initdb: warning: specified text search configuration \"simple\" might not match \
         locale \"{lc_ctype}\"\n"
    )
}

/// `setup_text_search` (`initdb.c:3492`) runs before `create_data_directory`
/// (`:3060`) and names `lc_ctype` (`:2859`), which only `--lc-ctype` and
/// `--locale` set (`:2432`). So behind a failing `--waldir`, a refusal that
/// leaves `lc_ctype` at C — `-E`, `--locale-provider`, `--lc-collate`, or a
/// `--locale` that `--lc-ctype=C` overrides — still gets C's warning first.
/// Byte for byte against the reference initdb's stderr and exit status.
#[test]
fn a_failing_waldir_behind_a_refusal_that_keeps_lc_ctype_c_still_warns_first() {
    let reference = reference::find_or_skip("initdb");
    for extra in [
        &["--no-locale", "-E", "UTF8", "--lc-collate", NON_C_LOCALE][..],
        &["--no-locale", "-E", "LATIN1"],
        &[
            "--no-locale",
            "-E",
            "UTF8",
            "--locale-provider",
            "builtin",
            "--builtin-locale",
            "C",
        ],
        &["-E", "UTF8", "--locale", NON_C_LOCALE, "--lc-ctype", "C"],
    ] {
        let tempdir = TempDir::new("expanded-tsearch-refusal");
        let (ours, tail) = behind_a_failing_waldir(
            Path::new(RINITDB),
            &tempdir,
            extra,
            &Environment::inherited(),
        );
        assert_eq!(ours.status, Some(1), "{extra:?}: {}", ours.stderr_text());
        let expected = simple_might_not_match("C") + &tail;
        assert_eq!(ours.stderr_text(), expected, "{extra:?}");
        if let Some(initdb) = &reference {
            let (theirs, _) =
                behind_a_failing_waldir(initdb, &tempdir, extra, &Environment::inherited());
            assert_eq!(theirs.status, ours.status, "{extra:?}");
            assert_eq!(theirs.stderr_text(), ours.stderr_text(), "{extra:?}");
        }
    }
}

/// When `--lc-ctype` or `--locale` is what is refused, `lc_ctype` is not C
/// and C's warning names it as `setlocale` canonicalizes it, which this port
/// does not reach: the line is left out rather than misstated, and the rest
/// of stderr is C's (`docs/divergences.md`). Against the reference initdb:
/// its stderr is exactly that one line followed by ours.
#[test]
fn a_failing_waldir_behind_an_lc_ctype_refusal_leaves_the_warning_out() {
    let reference = reference::find_or_skip("initdb");
    for extra in [
        &["--no-locale", "-E", "UTF8", "--lc-ctype", NON_C_LOCALE][..],
        &["-E", "UTF8", "--locale", NON_C_LOCALE],
    ] {
        let tempdir = TempDir::new("expanded-tsearch-lc-ctype");
        let (ours, tail) = behind_a_failing_waldir(
            Path::new(RINITDB),
            &tempdir,
            extra,
            &Environment::inherited(),
        );
        assert_eq!(ours.status, Some(1), "{extra:?}: {}", ours.stderr_text());
        assert_eq!(ours.stderr_text(), tail, "{extra:?}");
        if let Some(initdb) = &reference {
            let (theirs, _) =
                behind_a_failing_waldir(initdb, &tempdir, extra, &Environment::inherited());
            assert_eq!(theirs.status, ours.status, "{extra:?}");
            assert_eq!(
                theirs.stderr_text(),
                simple_might_not_match(NON_C_LOCALE) + &tail,
                "{extra:?}"
            );
        }
    }
}

/// With no `--lc-ctype` or `--locale`, or an empty one, C's `lc_ctype` is the
/// environment's (`check_locale_name`, `initdb.c:2202`) and its warning names
/// it — on musl `C.UTF-8` even with no locale variable set. This port takes
/// the environment to be C and names `C` (`docs/divergences.md`). Under
/// `LC_ALL` naming a non-C locale, against the reference initdb: its stderr
/// is ours with that locale's name in place of `C`.
#[test]
fn a_failing_waldir_with_no_lc_ctype_given_warns_for_c_not_the_environment() {
    let reference = reference::find_or_skip("initdb");
    let env = Environment::inherited().with("LC_ALL", NON_C_LOCALE);
    for extra in [
        &["-E", "UTF8"][..],
        &["-E", "UTF8", "--locale", ""],
        &["-E", "UTF8", "--lc-ctype", ""],
    ] {
        let tempdir = TempDir::new("expanded-tsearch-environment");
        let (ours, tail) = behind_a_failing_waldir(Path::new(RINITDB), &tempdir, extra, &env);
        assert_eq!(ours.status, Some(1), "{extra:?}: {}", ours.stderr_text());
        assert_eq!(
            ours.stderr_text(),
            simple_might_not_match("C") + &tail,
            "{extra:?}"
        );
        if let Some(initdb) = &reference {
            let (theirs, _) = behind_a_failing_waldir(initdb, &tempdir, extra, &env);
            assert_eq!(theirs.status, ours.status, "{extra:?}");
            assert_eq!(
                theirs.stderr_text(),
                simple_might_not_match(NON_C_LOCALE) + &tail,
                "{extra:?}"
            );
        }
    }
}

/// The variables `setlocale(category, "")` reads.
const LOCALE_VARIABLES: [&str; 8] = [
    "LANG",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
];

/// The `lc_*` lines of `<pgdata>/postgresql.conf`.
fn conf_locale_lines(pgdata: &Path) -> Vec<String> {
    std::fs::read_to_string(pgdata.join("postgresql.conf"))
        .expect("postgresql.conf")
        .lines()
        .filter(|line| line.starts_with("lc_"))
        .map(str::to_owned)
        .collect()
}

/// The `lc_*` lines the reference initdb writes for `before`, under `env`.
fn reference_conf_locale_lines(
    initdb: &Path,
    before: &[&str],
    pgdata: &Path,
    env: &Environment,
) -> Vec<String> {
    let mut argv = args(before);
    argv.push(pgdata.into());
    let outcome = testkit::run_in(initdb, &argv, &[], env).expect("run initdb");
    assert_eq!(
        outcome.status,
        Some(0),
        "{before:?}: {}",
        outcome.stderr_text()
    );
    let lines = conf_locale_lines(pgdata);
    std::fs::remove_dir_all(pgdata).expect("remove the cluster");
    lines
}

/// An empty `--locale`, `--lc-collate` or `--lc-ctype` is kept by
/// `setlocales` (`initdb.c:2432`-`:2443`) and read by `check_locale_name`
/// (`:2202`) as the environment's, like no switch at all: C accepts it, so
/// it is not refused. In two environments — no locale variable set, and
/// `LC_ALL=C` alone — each initdb writes, for a command line with an empty
/// switch, the four `postgresql.conf` locale lines it writes for the same
/// command line without it, and ours are the reference's.
///
/// Except on macOS with no locale variable set: there the reference takes
/// the system's preferred locale (`en_US` on the CI runner, run
/// 35837072908), where glibc and musl take C — the Homebrew build's
/// `setlocale` being, it appears, gettext's `libintl_setlocale`, whose `""`
/// falls back to the user's preferences. This port writes `C` for the
/// environment everywhere (`docs/divergences.md`), so there only the
/// reference's own empty-equals-no-switch is compared; under `LC_ALL=C` the
/// cross comparison holds on every lane.
#[test]
fn an_empty_locale_is_the_environments_like_no_switch() {
    let reference = reference::find_or_skip("initdb");
    let scrubbed = Environment::inherited().without_all(LOCALE_VARIABLES);
    let lc_all_c = Environment::inherited()
        .without_all(LOCALE_VARIABLES)
        .with("LC_ALL", "C");
    let base = ["-U", "postgres", "--no-sync"];
    // Each empty switch, and the command line it is the same as: the one
    // without it.
    let cases: [(&[&str], &[&str]); 4] = [
        (&["--locale", ""], &[]),
        (&["--lc-collate", ""], &[]),
        (&["--lc-ctype", ""], &[]),
        (&["--locale", "C", "--lc-collate", ""], &["--locale", "C"]),
    ];
    for (env, the_reference_is_c) in [(&scrubbed, !cfg!(target_os = "macos")), (&lc_all_c, true)] {
        let tempdir = TempDir::new("expanded-empty-locale");
        let ours = tempdir.join("ours");
        let theirs = tempdir.join("theirs");
        for (extra, without) in cases {
            let mut before = base.to_vec();
            before.extend(extra);
            let mut same_as = base.to_vec();
            same_as.extend(without);

            rinitdb_ok(&same_as, &ours, env);
            let expected = conf_locale_lines(&ours);
            assert_eq!(expected.len(), 4, "{expected:?}");
            std::fs::remove_dir_all(&ours).expect("remove the cluster");
            rinitdb_ok(&before, &ours, env);
            assert_eq!(conf_locale_lines(&ours), expected, "{extra:?}, {env:?}");
            std::fs::remove_dir_all(&ours).expect("remove the cluster");

            if let Some(initdb) = &reference {
                let reference_expected =
                    reference_conf_locale_lines(initdb, &same_as, &theirs, env);
                assert_eq!(
                    reference_conf_locale_lines(initdb, &before, &theirs, env),
                    reference_expected,
                    "{extra:?}, {env:?}"
                );
                if the_reference_is_c {
                    assert_eq!(reference_expected, expected, "{extra:?}, {env:?}");
                }
            }
        }
    }
}

/// A stand-in `postgres` in `tempdir`, for the single-user session
/// (`rinitdb::single_user`): it answers `-V` with 18.6's
/// `PG_BACKEND_VERSIONSTR`, and otherwise writes `PGDATA`, its arguments and
/// its stdin to `session.log` beside itself and exits with `FAKE_EXIT`
/// (0 by default). Named by [`SERVER_ENV`], as nothing is beside `rinitdb`.
fn fake_postgres(tempdir: &TempDir) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let path = tempdir.join("postgres");
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         if [ \"$1\" = -V ]; then echo 'postgres (PostgreSQL) 18.6'; exit 0; fi\n\
         log=\"$(dirname \"$0\")/session.log\"\n\
         { echo \"PGDATA=$PGDATA\"; for arg in \"$@\"; do echo \"arg=$arg\"; done; cat; } > \"$log\"\n\
         exit \"${FAKE_EXIT:-0}\"\n",
    )
    .expect("write the stand-in postgres");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in postgres executable");
    path
}

/// The environment `rinitdb` runs in for these tests: no `PGDROP_POSTGRES`
/// but the one given.
fn with_server(postgres: Option<&Path>) -> Environment {
    let env = Environment::inherited().without(SERVER_ENV);
    match postgres {
        Some(path) => env.with(SERVER_ENV, path),
        None => env,
    }
}

/// `setup_bin_paths` (`initdb.c:2661`): a superuser other than the
/// template's needs a `postgres`, and with none beside `rinitdb` and none
/// named, C's error — before anything is made, as in C.
#[test]
fn another_superuser_needs_a_postgres_beside_initdb() {
    let tempdir = TempDir::new("single-user-none");
    let pgdata = tempdir.join("data");
    let argv = [
        OsString::from("-U"),
        OsString::from("alice"),
        pgdata.clone().into(),
    ];
    let outcome =
        testkit::run_in(Path::new(RINITDB), &argv, &[], &with_server(None)).expect("run rinitdb");
    assert_eq!(outcome.status, Some(1), "{}", outcome.stderr_text());
    assert_eq!(outcome.stdout_text(), "");
    let my_exec = std::fs::canonicalize(RINITDB).expect("canonicalize rinitdb");
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "initdb: error: program \"postgres\" is needed by initdb but was not found in the \
             same directory as \"{}\"\n",
            my_exec.display()
        )
    );
    assert!(!pgdata.exists());

    // The template's own superuser needs no session and no server.
    rinitdb_ok(
        &["-U", "postgres", "--no-sync"],
        &pgdata,
        &with_server(None),
    );
}

/// A `PGDROP_POSTGRES` that is not a `postgres` of this version is refused
/// rather than skipped.
#[test]
fn an_unusable_server_override_is_an_error() {
    let tempdir = TempDir::new("single-user-override");
    let pgdata = tempdir.join("data");
    let bogus = tempdir.join("no-such-postgres");
    let argv = [
        OsString::from("-U"),
        OsString::from("alice"),
        pgdata.clone().into(),
    ];
    let outcome = testkit::run_in(Path::new(RINITDB), &argv, &[], &with_server(Some(&bogus)))
        .expect("run rinitdb");
    assert_eq!(outcome.status, Some(1));
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "initdb: error: PGDROP_POSTGRES names \"{}\", which is not a postgres executable of \
             the same version as initdb\n",
            bogus.display()
        )
    );
    assert!(!pgdata.exists());
}

/// The session is started as `initialize_data_directory` starts it
/// (`initdb.c:226`, `:3112`; PGDATA exported, `:2642`), after the cluster is
/// written, and is fed the rename.
#[test]
fn another_superuser_is_renamed_in_a_single_user_session() {
    let tempdir = TempDir::new("single-user-args");
    let postgres = fake_postgres(&tempdir);
    let pgdata = tempdir.join("data");
    rinitdb_ok(
        &["-U", "alice", "--no-sync"],
        &pgdata,
        &with_server(Some(&postgres)),
    );
    let log = std::fs::read_to_string(tempdir.join("session.log")).expect("session.log");
    assert_eq!(
        log,
        format!(
            "PGDATA={}\n\
             arg=--single\narg=-F\narg=-O\narg=-j\n\
             arg=-c\narg=search_path=pg_catalog\narg=-c\narg=exit_on_error=true\n\
             arg=-c\narg=log_checkpoints=false\narg=template1\n\
             UPDATE pg_authid SET rolname = E'alice' WHERE oid = 10;\n\n",
            pgdata.display()
        )
    );
    assert!(pgdata.join("global/pg_control").is_file());
}

/// `pclose_check` (`src/common/exec.c:410`), then the exit handler removes
/// what was made (`cleanup_directories_atexit`, `initdb.c:762`).
#[test]
fn a_failed_single_user_session_removes_the_data_directory() {
    let tempdir = TempDir::new("single-user-fails");
    let postgres = fake_postgres(&tempdir);
    let pgdata = tempdir.join("data");
    let argv = [
        OsString::from("-U"),
        OsString::from("alice"),
        OsString::from("--no-sync"),
        pgdata.clone().into(),
    ];
    let env = with_server(Some(&postgres)).with("FAKE_EXIT", "3");
    let outcome = testkit::run_in(Path::new(RINITDB), &argv, &[], &env).expect("run rinitdb");
    assert_eq!(outcome.status, Some(1));
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "initdb: error: child process exited with exit code 3\n\
             initdb: removing data directory \"{}\"\n",
            pgdata.display()
        )
    );
    assert!(!pgdata.exists());
}

/// The superuser, the databases it owns and an ACL naming it, as a
/// single-user session prints them.
const SUPERUSER_QUERIES: &str = "\
select oid, rolname, rolsuper, rolinherit, rolcreaterole, rolcreatedb, rolcanlogin, \
rolreplication, rolbypassrls, rolconnlimit, rolpassword, rolvaliduntil from pg_authid order by oid;
select datname, datdba::regrole as owner from pg_database order by oid;
select relacl from pg_class where oid = 'pg_class'::regclass;
select current_user as session_superuser;
";

/// Gate: `-U alice` makes the roles, owners and ACLs the reference initdb
/// makes (the image's own recipe, `--no-locale -E UTF8`), as the reference
/// `postgres --single` reads them from each cluster. The reference server
/// also runs our session (`PGDROP_POSTGRES`). NAT-383's `\du` gate is this
/// check without a psql and a running server.
#[test]
fn the_renamed_superuser_matches_reference_initdb() {
    let tempdir = TempDir::new("single-user-gate");
    let Some(initdb) = reference::find_or_skip("initdb") else {
        return;
    };
    let Some(postgres) = reference::find_or_skip("postgres") else {
        return;
    };
    let ours = tempdir.join("ours");
    rinitdb_ok(
        &["-U", "alice", "--no-sync"],
        &ours,
        &with_server(Some(&postgres)),
    );
    let theirs = tempdir.join("theirs");
    let argv = [
        OsString::from("-U"),
        OsString::from("alice"),
        OsString::from("--no-sync"),
        OsString::from("--no-locale"),
        OsString::from("-E"),
        OsString::from("UTF8"),
        theirs.clone().into(),
    ];
    let outcome = testkit::run(&initdb, &argv).expect("run the reference initdb");
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());

    let read = |pgdata: &Path| {
        let outcome = reference_single_user(pgdata, SUPERUSER_QUERIES).expect("found above");
        assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
        outcome.stdout_text()
    };
    let (ours, theirs) = (read(&ours), read(&theirs));
    assert_eq!(
        single_user_values(&ours, "session_superuser"),
        ["alice"],
        "{ours}"
    );
    assert_eq!(single_user_values(&ours, "rolname")[0], "alice", "{ours}");
    assert_eq!(ours, theirs);
}

/// `rinitdb <before> <pgdata>` with `--pwfile` holding `contents`, in
/// `tempdir`: the password file's path and the outcome.
fn rinitdb_with_pwfile(
    tempdir: &TempDir,
    contents: Option<&[u8]>,
    before: &[&str],
    env: &Environment,
) -> (PathBuf, testkit::CommandOutcome) {
    let pwfile = tempdir.join("pwfile");
    if let Some(contents) = contents {
        std::fs::write(&pwfile, contents).expect("write the password file");
    }
    let mut argv = args(before);
    argv.extend([
        OsString::from("--pwfile"),
        pwfile.clone().into(),
        tempdir.join("data").into(),
    ]);
    let outcome = testkit::run_in(Path::new(RINITDB), &argv, &[], env).expect("run rinitdb");
    (pwfile, outcome)
}

/// `--pwfile`: `setup_auth`'s `ALTER USER` (`initdb.c:1649`) is fed to the
/// session after the rename, with the file's first line, its line end
/// stripped and its quotes doubled. A password alone, for the template's
/// own superuser, is a session too.
#[test]
fn a_password_file_is_set_in_the_single_user_session() {
    let tempdir = TempDir::new("single-user-password");
    let postgres = fake_postgres(&tempdir);
    let env = with_server(Some(&postgres));
    let (_, outcome) = rinitdb_with_pwfile(
        &tempdir,
        Some(b"it's\\secret\r\nsecond line\n"),
        &["-U", "alice", "--no-sync"],
        &env,
    );
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    // No -A: C's trust warning (initdb.c:3521) is all of stderr.
    assert_eq!(
        outcome.stderr_text(),
        format!("{}\n", rinitdb::report::trust_warning())
    );
    let log = std::fs::read_to_string(tempdir.join("session.log")).expect("session.log");
    assert!(
        log.ends_with(
            "arg=template1\n\
             UPDATE pg_authid SET rolname = E'alice' WHERE oid = 10;\n\n\
             ALTER USER \"alice\" WITH PASSWORD E'it''s\\\\secret';\n\n"
        ),
        "{log}"
    );

    let tempdir = TempDir::new("single-user-password-only");
    let postgres = fake_postgres(&tempdir);
    let (_, outcome) = rinitdb_with_pwfile(
        &tempdir,
        Some(b"pw\n"),
        &["-U", "postgres", "--no-sync"],
        &with_server(Some(&postgres)),
    );
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    let log = std::fs::read_to_string(tempdir.join("session.log")).expect("session.log");
    assert!(
        log.ends_with("arg=template1\nALTER USER \"postgres\" WITH PASSWORD E'pw';\n\n"),
        "{log}"
    );
}

/// `get_su_pwd`'s three `pg_fatal`s (`initdb.c:1692`, `:1701`; `:1698`
/// in the unit tests) come before the first `mkdir`, so nothing is made or
/// removed, and no session runs. Gate: the reference initdb writes the same
/// stderr and exits 1 too.
#[test]
fn an_unreadable_or_empty_password_file_is_cs_error() {
    for (tag, contents, message) in [
        (
            "pwfile-missing",
            None,
            "could not open file \"{}\" for reading: No such file or directory",
        ),
        (
            "pwfile-empty",
            Some(&b""[..]),
            "password file \"{}\" is empty",
        ),
    ] {
        let tempdir = TempDir::new(tag);
        let postgres = fake_postgres(&tempdir);
        let before = ["-U", "alice", "--no-sync", "--no-locale", "-E", "UTF8"];
        let (pwfile, outcome) =
            rinitdb_with_pwfile(&tempdir, contents, &before, &with_server(Some(&postgres)));
        let expected = format!(
            "initdb: error: {}\n",
            message.replace("{}", &pwfile.display().to_string())
        );
        assert_eq!(outcome.status, Some(1), "{tag}");
        assert_eq!(outcome.stderr_text(), expected, "{tag}");
        assert!(!tempdir.join("data").exists(), "{tag}");
        assert!(!tempdir.join("session.log").exists(), "{tag}");

        let Some(initdb) = reference::find_or_skip("initdb") else {
            continue;
        };
        let mut argv = args(&before);
        argv.extend([
            OsString::from("--pwfile"),
            pwfile.clone().into(),
            tempdir.join("data").into(),
        ]);
        let theirs = testkit::run(&initdb, &argv).expect("run the reference initdb");
        assert_eq!(theirs.status, Some(1), "{tag}");
        assert_eq!(theirs.stderr_text(), expected, "{tag}");
        assert!(!tempdir.join("data").exists(), "{tag}");
    }
}

/// `SCRAM-SHA-256$<iterations>:<salt>$<StoredKey>:<ServerKey>` with the
/// three random-per-cluster parts replaced, after checking their lengths:
/// a 16-byte salt and two 32-byte keys, base64 (`scram_build_secret`,
/// `src/common/scram-common.c:209`). Anything else is left alone.
fn normalize_scram(stdout: &str) -> String {
    let mut out = String::with_capacity(stdout.len());
    let mut rest = stdout;
    while let Some(at) = rest.find("SCRAM-SHA-256$") {
        out.push_str(&rest[..at]);
        let secret = &rest[at..];
        let end = secret.find('"').unwrap_or(secret.len());
        let (secret, after) = secret.split_at(end);
        let parsed = secret
            .strip_prefix("SCRAM-SHA-256$")
            .and_then(|s| s.split_once(':'))
            .and_then(|(iterations, s)| {
                let (salt, keys) = s.split_once('$')?;
                let (stored, server) = keys.split_once(':')?;
                Some((iterations, salt.len(), stored.len(), server.len()))
            });
        match parsed {
            Some((iterations, 24, 44, 44)) => {
                out.push_str("SCRAM-SHA-256$");
                out.push_str(iterations);
                out.push_str(":<salt>$<StoredKey>:<ServerKey>");
            }
            _ => panic!("not a SCRAM secret: {secret}"),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Gate: `-U alice --pwfile` makes the `pg_authid` row the reference
/// initdb makes for the same command line — a SCRAM secret by
/// `password_encryption`'s default, with 4096 iterations — up to the salt
/// and the keys, which are random per cluster. The reference `postgres`
/// runs our session. That the secret opens a login is
/// `crates/pgdrop/tests/template_boot.rs`'s, against pgrust.
#[test]
fn the_superuser_password_matches_reference_initdb() {
    let tempdir = TempDir::new("single-user-password-gate");
    let Some(initdb) = reference::find_or_skip("initdb") else {
        return;
    };
    let Some(postgres) = reference::find_or_skip("postgres") else {
        return;
    };
    let pwfile = tempdir.join("pwfile");
    std::fs::write(&pwfile, "gate'pw\n").expect("write the password file");
    let common = [
        OsString::from("-U"),
        OsString::from("alice"),
        OsString::from("--pwfile"),
        pwfile.into(),
        OsString::from("--no-sync"),
    ];
    let ours = tempdir.join("ours");
    let mut argv = common.to_vec();
    argv.push(ours.clone().into());
    let outcome = testkit::run_in(
        Path::new(RINITDB),
        &argv,
        &[],
        &with_server(Some(&postgres)),
    )
    .expect("run rinitdb");
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    // No -A: C's trust warning (initdb.c:3521) is all of stderr.
    let ours_stderr = outcome.stderr_text();
    assert_eq!(
        ours_stderr,
        format!("{}\n", rinitdb::report::trust_warning())
    );

    let theirs = tempdir.join("theirs");
    let mut argv = common.to_vec();
    argv.extend(args(&["--no-locale", "-E", "UTF8"]));
    argv.push(theirs.clone().into());
    let outcome = testkit::run(&initdb, &argv).expect("run the reference initdb");
    assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
    assert_eq!(outcome.stderr_text(), ours_stderr);

    let read = |pgdata: &Path| {
        let outcome = reference_single_user(pgdata, SUPERUSER_QUERIES).expect("found above");
        assert_eq!(outcome.status, Some(0), "{}", outcome.stderr_text());
        outcome.stdout_text()
    };
    let (ours, theirs) = (read(&ours), read(&theirs));
    assert_eq!(
        normalize_scram(single_user_values(&ours, "rolpassword")[0]),
        "SCRAM-SHA-256$4096:<salt>$<StoredKey>:<ServerKey>",
        "{ours}"
    );
    assert_eq!(normalize_scram(&ours), normalize_scram(&theirs));
}
