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
//! pgrust reads its share files — `timezonesets`, `tsearch_data` and the
//! compiled `timezone` database — from the copy pgdrop embeds and extracts on
//! first run (NAT-408). So the server runs as a hard link named `postgres` in
//! a scratch `bin/` with no `share/` beside it, with `PGRUST_PGSHAREDIR` and
//! `PGRUST_TZDIR` removed from its environment and `XDG_CACHE_HOME` pointed
//! into the scratch directory. The pgrust half needs nothing else and always
//! runs. Without a reference installation the C half prints
//! `SKIP (flagged, not silent)` and passes; `PGDROP_REQUIRE_REF=1` makes it
//! fail instead.
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
/// applet and `find_my_exec` lands in `<scratch>/bin`). There is no
/// `<scratch>/share`: every share file must come from the embedded copy.
fn install(scratch: &Path) -> PathBuf {
    install_as(scratch, "postgres")
}

/// [`install`], with the link in `bin/` named `name`.
fn install_as(scratch: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(scratch.join("bin")).expect("create bin/");
    link_pgdrop(scratch, name)
}

/// `<scratch>/bin/<name>`, a hard link to pgdrop (a copy where the
/// filesystem will not link).
fn link_pgdrop(scratch: &Path, name: &str) -> PathBuf {
    let link = scratch.join("bin").join(name);
    if std::fs::hard_link(PGDROP, &link).is_err() {
        std::fs::copy(PGDROP, &link).expect("copy pgdrop");
    }
    link
}

/// The server's environment: no share directory named by the caller, and an
/// XDG cache of the test's own, `<scratch>/cache`.
fn server_env(scratch: &Path) -> Environment {
    Environment::inherited()
        .without_all([pgdrop::share::SHAREDIR_VAR, pgdrop::share::TZDIR_VAR])
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

/// `pgdrop initdb -U postgres --no-sync <pgdata>`: exit 0, and on stderr only
/// the `trust` warning C initdb prints for a command line without `-A`
/// (`initdb.c:3521`).
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
    assert_eq!(
        outcome.stderr_text(),
        format!("{}\n", rinitdb::report::trust_warning())
    );
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

/// `timezone`, NAT-408's acceptance: `select now() at time zone
/// 'Europe/Paris'` answers with neither `PGRUST_PGSHAREDIR` nor
/// `PGRUST_TZDIR` set and no `share/` beside the binary. `now()` has no fixed
/// expected value, so upstream's own Paris queries pin the zone's rules,
/// under pg_regress's `PGDATESTYLE=Postgres, MDY`
/// (`src/test/regress/pg_regress.c:786`):
/// `src/test/regress/sql/timestamptz.sql:471`, expected
/// `src/test/regress/expected/timestamptz.out:2484`-`:2488` (Paris's local
/// mean time before 1891, 0:09:21 ahead of UTC); and `timestamptz.sql:655`-`:658`,
/// expected `timestamptz.out:3253`-`:3265` (`AT LOCAL` under
/// `SET LOCAL TIME ZONE 'Europe/Paris'`, CEST in July).
#[test]
fn timestamptz_europe_paris() {
    let scratch = Scratch::new("share-timezone");
    let pgrust = install(&scratch.0);
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let stdout = single(
        &pgrust,
        &pgdata,
        &server_env(&scratch.0),
        "select now() at time zone 'Europe/Paris' as paris;\n\
         set datestyle = 'Postgres, MDY';\n\
         SELECT make_timestamptz(1881, 12, 10, 0, 0, 0, 'Europe/Paris') AT TIME ZONE 'UTC';\n\
         BEGIN;\n\
         SET LOCAL TIME ZONE 'Europe/Paris';\n\
         VALUES (CAST('1978-07-07 19:38 America/New_York' AS TIMESTAMP WITH TIME ZONE) AT LOCAL);\n\
         VALUES (TIMESTAMP '1978-07-07 19:38' AT LOCAL);\n\
         COMMIT;\n",
    );
    let paris = values(&stdout, "paris");
    assert_eq!(paris.len(), 1, "stdout: {stdout}");
    assert!(
        paris[0].len() >= "yyyy-mm-dd hh:mm:ss".len() && paris[0].as_bytes()[4] == b'-',
        "an ISO timestamp: {paris:?}"
    );
    assert_eq!(
        values(&stdout, "timezone"),
        ["Fri Dec 09 23:50:39 1881"],
        "stdout: {stdout}"
    );
    assert_eq!(
        values(&stdout, "column1"),
        ["Sat Jul 08 01:38:00 1978", "Fri Jul 07 19:38:00 1978 CEST"],
        "stdout: {stdout}"
    );
}

/// `timezone`: `src/test/regress/sql/sysviews.sql:94`, expected
/// `src/test/regress/expected/sysviews.out:200`-`:204` — `pg_timezone_names`
/// enumerates the embedded database and finds at least 24 distinct offsets.
#[test]
fn sysviews_timezone_names() {
    let scratch = Scratch::new("share-tznames-view");
    let pgrust = install(&scratch.0);
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let stdout = single(
        &pgrust,
        &pgdata,
        &server_env(&scratch.0),
        "select count(distinct utc_offset) >= 24 as ok from pg_timezone_names;\n",
    );
    assert_eq!(values(&stdout, "ok"), ["t"], "stdout: {stdout}");
}

/// `<initdb> [initdb] -U alice --no-sync <pgdata>` in `scratch`'s
/// [`server_env`] with no `PGDROP_POSTGRES`: exit 0, and nothing on stderr
/// but pgrust's own LOG lines and, last, the `trust` warning. pgrust's `--single` ends a session with status
/// 0 even after an ERROR (C's `exit_on_error` would make it FATAL), so an
/// ERROR on stderr is the only sign one happened.
fn initdb_alice(scratch: &Path, initdb: &Path, first_word: Option<&str>, pgdata: &Path) {
    let argv: Vec<OsString> = first_word
        .into_iter()
        .chain(["-U", "alice", "--no-sync"])
        .map(OsString::from)
        .chain([pgdata.into()])
        .collect();
    let env = server_env(scratch).without(rinitdb::single_user::SERVER_ENV);
    let outcome = testkit::run_in(initdb, &argv, &[], &env).expect("run initdb");
    let stderr = outcome.stderr_text();
    assert_eq!(outcome.status, Some(0), "{argv:?}\nstderr: {stderr}");
    let session = stderr
        .strip_suffix(&format!("{}\n", rinitdb::report::trust_warning()))
        .unwrap_or_else(|| panic!("{argv:?}: no trust warning last\nstderr: {stderr}"));
    for line in session.lines() {
        assert!(line.contains(" LOG:  "), "{argv:?}\nstderr: {stderr}");
    }
}

/// pgrust `--single` on `pgdata`: who the superuser is now.
fn superuser(postgres: &Path, pgdata: &Path, env: &Environment) -> (Vec<String>, Vec<String>) {
    let stdout = single(
        postgres,
        pgdata,
        env,
        "select rolname as su from pg_authid where oid = 10;\n\
         select current_user as me;\n\
         select rolname as leftover from pg_authid where rolname = 'postgres';\n",
    );
    let owned = |column: &str| -> Vec<String> {
        values(&stdout, column)
            .into_iter()
            .map(str::to_owned)
            .collect()
    };
    ([owned("su"), owned("me")].concat(), owned("leftover"))
}

/// NAT-383: `-U alice` renames the template's superuser in a pgrust
/// single-user session, found the two ways pgdrop finds one — `postgres`
/// beside `initdb` (`setup_bin_paths`, `initdb.c:2648`), and pgdrop itself
/// when nothing is beside it.
#[test]
fn another_superuser_is_the_templates_renamed_under_pgrust() {
    let scratch = Scratch::new("superuser");
    let pgrust = install(&scratch.0);

    let beside = scratch.0.join("data-beside");
    initdb_alice(
        &scratch.0,
        &link_pgdrop(&scratch.0, "initdb"),
        None,
        &beside,
    );
    assert_eq!(
        superuser(&pgrust, &beside, &server_env(&scratch.0)),
        (vec!["alice".to_owned(), "alice".to_owned()], Vec::new())
    );

    // A bin/ with pgdrop alone: the embedded server, `pgdrop postgres`.
    let alone = Scratch::new("superuser-embedded");
    let pgdrop = install_as(&alone.0, "pgdrop");
    let embedded = alone.0.join("data");
    initdb_alice(&alone.0, &pgdrop, Some("initdb"), &embedded);
    assert_eq!(
        superuser(&pgrust, &embedded, &server_env(&scratch.0)),
        (vec!["alice".to_owned(), "alice".to_owned()], Vec::new())
    );
}
