//! NAT-381 acceptance: a cluster `pgdrop initdb` expands from the embedded
//! template, with its rewritten `pg_control` and regenerated first WAL
//! segment, boots under pgrust `postgres --single -D <dir>`, and `select 1`
//! works. The same cluster shape boots under the reference C `postgres` too,
//! as a second oracle (`crates/rinitdb/tests/expanded_cluster.rs` does that
//! on its own; here both servers meet the one binary users run).
//!
//! This lives in pgdrop because pgdrop is where pgrust is linked (AGPL-3.0,
//! ADR-0003); rinitdb stays MIT and never reaches pgrust
//! (`scripts/check-license-wall.sh`).
//!
//! pgrust reads `timezonesets` and `tsearch_data` from the share directory
//! pgdrop embeds and extracts on first run (NAT-408), and still reads the
//! compiled `timezone` database from `<bindir>/../share` of the executable it
//! runs as (`find_my_exec`): that one is not embedded yet. So the server runs
//! as a hard link named `postgres` in a scratch `bin/`, beside a `share/`
//! holding only a link to this machine's timezone database, with
//! `PGRUST_PGSHAREDIR` and `PGRUST_TZDIR` removed from its environment and
//! `XDG_CACHE_HOME` pointed into the scratch directory. The pgrust half needs
//! nothing else and always runs. Without a reference installation the C
//! half prints `SKIP (flagged, not silent)` and passes;
//! `PGDROP_REQUIRE_REF=1` makes it fail instead.
//!
//! The NAT-408 tests steal upstream regression queries that read the
//! embedded files and hold pgrust to upstream's expected output.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use testkit::Environment;
use testkit::reference;

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A scratch directory under Cargo's target tmpdir — the same filesystem as
/// the pgdrop binary, so it can be hard-linked — removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgdrop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `<scratch>/bin/postgres`, a hard link to pgdrop (so `argv[0]` selects the
/// applet and `find_my_exec` lands in `<scratch>/bin`), and
/// `<scratch>/share/timezone`, linked to the timezone database rinitdb reads
/// too — the one share file pgdrop does not embed yet. Nothing else is in
/// `share/`: `timezonesets` and `tsearch_data` must come from the embedded
/// copy.
fn install(scratch: &Path) -> PathBuf {
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin/");
    std::fs::create_dir_all(scratch.join("share")).expect("create share/");
    let postgres = bin.join("postgres");
    if std::fs::hard_link(PGDROP, &postgres).is_err() {
        std::fs::copy(PGDROP, &postgres).expect("copy pgdrop");
    }
    let tzdir = rinitdb::RealTzSource::from_env()
        .expect("a timezone database on this machine")
        .tzdir()
        .to_path_buf();
    std::os::unix::fs::symlink(tzdir, scratch.join("share/timezone")).expect("link timezone");
    postgres
}

/// The server's environment: no share directory named by the caller, and an
/// XDG cache of the test's own, `<scratch>/cache`.
fn server_env(scratch: &Path) -> Environment {
    Environment::inherited()
        .without_all([pgdrop::share::SHAREDIR_VAR, "PGRUST_TZDIR"])
        .with("XDG_CACHE_HOME", scratch.join("cache"))
}

/// `<postgres> --single -D <pgdata> postgres` with `input`: exit 0; stdout.
fn single(postgres: &Path, pgdata: &Path, env: &Environment, input: &str) -> String {
    let argv = [
        OsString::from("--single"),
        OsString::from("-D"),
        pgdata.into(),
        OsString::from("postgres"),
    ];
    let outcome =
        testkit::run_in(postgres, argv, input.as_bytes(), env).expect("run postgres --single");
    let stdout = outcome.stdout_text();
    assert_eq!(
        outcome.status,
        Some(0),
        "{}\nstdout: {stdout}\nstderr: {}",
        postgres.display(),
        outcome.stderr_text()
    );
    stdout
}

/// Every value single-user mode printed for a column named `column`, in
/// order: the `printatt` lines (`src/backend/access/common/printtup.c:423`).
fn values<'a>(stdout: &'a str, column: &str) -> Vec<&'a str> {
    let marker = format!(": {column} = \"");
    stdout
        .lines()
        .filter_map(|line| line.split_once(&marker))
        .filter_map(|(_, rest)| rest.split_once("\"\t"))
        .map(|(value, _)| value)
        .collect()
}

/// `pgdrop initdb -U postgres --no-sync <pgdata>`: exit 0, nothing on stderr.
fn pgdrop_initdb(pgdata: &Path) {
    let argv = [
        OsString::from("initdb"),
        OsString::from("-U"),
        OsString::from("postgres"),
        OsString::from("--no-sync"),
        pgdata.into(),
    ];
    let outcome = testkit::run(Path::new(PGDROP), &argv).expect("run pgdrop initdb");
    assert_eq!(outcome.status, Some(0), "stderr: {}", outcome.stderr_text());
    assert_eq!(outcome.stderr_text(), "");
}

/// `<postgres> --single -D <pgdata> postgres` with `select 1`: exit 0, and
/// single-user mode's `printatt` line for the value
/// (`src/backend/access/common/printtup.c:423`). `version()` rides along so
/// the test can tell which server answered.
fn select_one(postgres: &Path, pgdata: &Path, env: &Environment) -> String {
    let stdout = single(postgres, pgdata, env, "select 1 as one, version() as v;\n");
    assert_eq!(
        values(&stdout, "one"),
        ["1"],
        "{}\nstdout: {stdout}",
        postgres.display()
    );
    values(&stdout, "v")
        .first()
        .map(|v| (*v).to_owned())
        .unwrap_or_default()
}

#[test]
fn the_expanded_template_boots_under_pgrust_single_user_mode() {
    let scratch = Scratch::new("template-boot");
    let pgrust = install(&scratch.0);

    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let version = select_one(&pgrust, &pgdata, &server_env(&scratch.0));
    assert!(version.contains("(pgrust "), "{version}");

    // The second oracle: the reference C server, on a cluster of its own
    // from the same binary.
    let Some(reference_postgres) = reference::find_or_skip("postgres") else {
        return;
    };
    let c_pgdata = scratch.0.join("data-c");
    pgdrop_initdb(&c_pgdata);
    let version = select_one(&reference_postgres, &c_pgdata, &Environment::inherited());
    assert!(
        version.starts_with("PostgreSQL 18.6") && !version.contains("pgrust"),
        "{version}"
    );
}

/// NAT-408: with no share directory beside the binary or in the environment,
/// the embedded one is extracted to `$XDG_CACHE_HOME/pgdrop/<key>/share`.
#[test]
fn the_share_files_are_extracted_to_the_xdg_cache() {
    let scratch = Scratch::new("share-extract");
    let pgrust = install(&scratch.0);
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    select_one(&pgrust, &pgdata, &server_env(&scratch.0));

    let share =
        pgdrop::share::extraction_dir(&scratch.0.join("cache"), pgdrop::share::KEY).join("share");
    for (path, bytes) in pgdrop::share::FILES {
        assert_eq!(
            std::fs::read(share.join(path)).expect("an extracted file"),
            *bytes,
            "{path}"
        );
    }
}

/// `timezonesets`: `src/test/regress/sql/sysviews.sql:95`-`:100`, expected
/// `src/test/regress/expected/sysviews.out:206`-`:225` — `t` three times,
/// for the `Default`, `Australia` and `India` abbreviation sets.
#[test]
fn sysviews_timezone_abbreviation_sets() {
    let scratch = Scratch::new("share-tznames");
    let pgrust = install(&scratch.0);
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let stdout = single(
        &pgrust,
        &pgdata,
        &server_env(&scratch.0),
        "select count(distinct utc_offset) >= 24 as ok from pg_timezone_abbrevs;\n\
         set timezone_abbreviations = 'Australia';\n\
         select count(distinct utc_offset) >= 24 as ok from pg_timezone_abbrevs;\n\
         set timezone_abbreviations = 'India';\n\
         select count(distinct utc_offset) >= 24 as ok from pg_timezone_abbrevs;\n",
    );
    assert_eq!(values(&stdout, "ok"), ["t", "t", "t"], "stdout: {stdout}");
}

/// `tsearch_data`: the ispell sample dictionary,
/// `src/test/regress/sql/tsdicts.sql:4`-`:10` (the `CREATE` on one line:
/// single-user mode ends a statement at a newline), expected
/// `src/test/regress/expected/tsdicts.out:8`-`:12`; and `english_stem`,
/// whose `StopWords=english` reads `english.stop`,
/// `src/test/regress/sql/tsearch.sql:275`, expected
/// `src/test/regress/expected/tsearch.out:1090`-`:1094`. `{sky}` both times.
#[test]
fn tsdicts_ispell_and_tsearch_english_stem() {
    let scratch = Scratch::new("share-tsearch");
    let pgrust = install(&scratch.0);
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let stdout = single(
        &pgrust,
        &pgdata,
        &server_env(&scratch.0),
        "CREATE TEXT SEARCH DICTIONARY ispell ( Template=ispell, DictFile=ispell_sample, AffFile=ispell_sample );\n\
         SELECT ts_lexize('ispell', 'skies');\n\
         SELECT ts_lexize('english_stem', 'skies');\n",
    );
    assert_eq!(
        values(&stdout, "ts_lexize"),
        ["{sky}", "{sky}"],
        "stdout: {stdout}"
    );
}
