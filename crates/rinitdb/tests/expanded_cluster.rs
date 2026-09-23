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

/// `rinitdb <args> <pgdata>`: exit 0, nothing on stderr, and nothing on
/// stdout yet — C's progress lines, sync note and instructions are NAT-387's
/// (`docs/divergences.md`).
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
    assert_eq!(outcome.stderr_text(), "", "{argv:?}");
    assert_eq!(outcome.stdout_text(), "", "{argv:?}");
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
        (
            &["-U", "alice"],
            "initdb: error: superuser name \"alice\" is not supported yet: the embedded template \
             cluster's superuser is \"postgres\" and renaming it is not implemented",
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
        "initdb: warning: specified text search configuration \"simple\" might not match \
         locale \"C\"\n"
    );
    let conf = std::fs::read_to_string(pgdata.join("postgresql.conf")).expect("postgresql.conf");
    assert!(
        conf.lines()
            .any(|line| line == "default_text_search_config = 'pg_catalog.simple'")
    );
}

/// A `--waldir` that fails keeps precedence over this port's refusal
/// (`docs/divergences.md`), and a refusal that leaves `lc_ctype` at C does
/// not take the warning with it: the reference initdb writes these four
/// stderr lines, in this order, for this command line with `-U alice`.
#[test]
fn a_failing_waldir_behind_a_superuser_refusal_still_warns_first() {
    let tempdir = TempDir::new("expanded-tsearch-waldir");
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
    let outcome = testkit::run(Path::new(RINITDB), &argv).expect("run rinitdb");
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

/// An empty `--locale`, `--lc-collate` or `--lc-ctype` is kept by
/// `setlocales` (`initdb.c:2432`-`:2443`) and read by `check_locale_name`
/// (`:2202`) as the environment's, like no switch at all: C accepts it, so
/// it is not refused. With no locale variable set, against the reference
/// initdb: both exit 0 and write the same four `postgresql.conf` locale
/// lines, which are those of no switch.
#[test]
fn an_empty_locale_is_the_environments_like_no_switch() {
    let reference = reference::find_or_skip("initdb");
    let env = Environment::inherited().without_all(LOCALE_VARIABLES);
    let tempdir = TempDir::new("expanded-empty-locale");
    let no_switch = tempdir.join("no-switch");
    rinitdb_ok(&["-U", "postgres", "--no-sync"], &no_switch, &env);
    let expected = conf_locale_lines(&no_switch);
    assert_eq!(expected.len(), 4, "{expected:?}");
    for extra in [
        &["--locale", ""][..],
        &["--lc-collate", ""],
        &["--lc-ctype", ""],
        &["--locale", "C", "--lc-collate", ""],
    ] {
        let mut before = vec!["-U", "postgres", "--no-sync"];
        before.extend(extra);
        let ours = tempdir.join("ours");
        rinitdb_ok(&before, &ours, &env);
        assert_eq!(conf_locale_lines(&ours), expected, "{extra:?}");
        std::fs::remove_dir_all(&ours).expect("remove the cluster");
        if let Some(initdb) = &reference {
            let theirs = tempdir.join("theirs");
            let mut argv = args(&before);
            argv.push(theirs.clone().into());
            let outcome = testkit::run_in(initdb, &argv, &[], &env).expect("run initdb");
            assert_eq!(
                outcome.status,
                Some(0),
                "{extra:?}: {}",
                outcome.stderr_text()
            );
            assert_eq!(conf_locale_lines(&theirs), expected, "{extra:?}");
            std::fs::remove_dir_all(&theirs).expect("remove the cluster");
        }
    }
}
