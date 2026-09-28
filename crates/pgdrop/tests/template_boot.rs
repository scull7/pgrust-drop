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
    single_in(postgres, pgdata, env, "postgres", input)
}

/// [`single`] on `database`.
fn single_in(
    postgres: &Path,
    pgdata: &Path,
    env: &Environment,
    database: &str,
    input: &str,
) -> String {
    let argv = [
        OsString::from("--single"),
        OsString::from("-D"),
        pgdata.into(),
        OsString::from(database),
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

/// pgrust `--single` on `pgdata`, in `template1`, `template0` and
/// `postgres`: that database's statistics on `pg_authid.rolname` (the
/// histogram, slot 1), and whether `VACUUM FREEZE` has advanced
/// `pg_authid`'s `relfrozenxid` past the superuser's row.
fn analyzed_and_frozen(postgres: &Path, pgdata: &Path, env: &Environment) -> Vec<String> {
    ["template1", "template0", "postgres"]
        .into_iter()
        .flat_map(|database| {
            let stdout = single_in(
                postgres,
                pgdata,
                env,
                database,
                "select stavalues1 as rolnames from pg_statistic \
                 where starelid = 1260 and staattnum = 2;\n\
                 select (select relfrozenxid::text::int8 from pg_class where oid = 1260) \
                 > xmin::text::int8 as frozen from pg_authid where oid = 10;\n",
            );
            let rolnames = values(&stdout, "rolnames");
            let frozen = values(&stdout, "frozen");
            vec![
                format!(
                    "{database}: {}",
                    rolnames
                        .first()
                        .map_or("", |names| names.split(',').next().unwrap_or(""))
                ),
                format!("{database}: frozen {frozen:?}"),
            ]
        })
        .collect()
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
    // vacuum_db's ANALYZE and VACUUM FREEZE (initdb.c:2004) ran after the
    // rename in every database, under pgrust too.
    assert_eq!(
        analyzed_and_frozen(&pgrust, &beside, &server_env(&scratch.0)),
        [
            "template1: {alice",
            "template1: frozen [\"t\"]",
            "template0: {alice",
            "template0: frozen [\"t\"]",
            "postgres: {alice",
            "postgres: frozen [\"t\"]",
        ]
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

/// pgrust serving `pgdata` on a Unix socket in its own directory, stopped
/// with SIGINT (fast shutdown) when dropped.
struct Server {
    child: std::process::Child,
    socket_dir: PathBuf,
    port: u16,
}

impl Server {
    /// `<postgres> -D <pgdata> -k <dir> -p <port> -c listen_addresses=`.
    /// The socket directory is under the system's temporary directory, not
    /// the scratch one, so its path stays short of `sun_path`'s limit.
    fn start(postgres: &Path, pgdata: &Path, env: &Environment, port: u16) -> Self {
        let socket_dir =
            std::env::temp_dir().join(format!("pgdrop-sock-{}-{port}", std::process::id()));
        let _ = std::fs::remove_dir_all(&socket_dir);
        std::fs::create_dir_all(&socket_dir).expect("create the socket directory");
        let mut command = std::process::Command::new(postgres);
        command
            .arg("-D")
            .arg(pgdata)
            .arg("-k")
            .arg(&socket_dir)
            .args(["-p", &port.to_string(), "-c", "listen_addresses="])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        env.apply(&mut command);
        let child = command.spawn().expect("start pgrust");
        Self {
            child,
            socket_dir,
            port,
        }
    }

    /// `PQconnectdb` as `user` with `password`, retried while the server
    /// is still starting (up to a minute): the connection, or the last
    /// error's message.
    fn connect(&mut self, user: &str, password: &str) -> Result<rlibpq::Connection, String> {
        // conninfo_parse (fe-connect.c:6290): `\` escapes the next byte.
        let quote = |value: &str| value.replace('\\', "\\\\").replace('\'', "\\'");
        let conninfo = format!(
            "host='{}' port={} dbname=postgres user='{}' password='{}'",
            quote(&self.socket_dir.display().to_string()),
            self.port,
            quote(user),
            quote(password)
        );
        let mut info = rlibpq::parse_conninfo(conninfo.as_bytes()).expect("conninfo parses");
        info.add_defaults(&rlibpq::Env::empty(), &rlibpq::Filesystem)
            .expect("no service to look up");
        let deadline = std::time::Instant::now() + std::time::Duration::from_mins(1);
        loop {
            match rlibpq::Connection::connect(&info) {
                Ok(conn) => return Ok(conn),
                Err(err) => {
                    let message = err.to_string();
                    let starting = message.contains("No such file or directory")
                        || message.contains("Connection refused")
                        || message.contains("the database system is starting up");
                    let exited = self.child.try_wait().ok().flatten().is_some();
                    if !starting || exited || std::time::Instant::now() > deadline {
                        return Err(message);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SIGINT to this child's PID only: a fast shutdown.
        let _ = std::process::Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.socket_dir);
    }
}

/// NAT-383 acceptance: after `initdb -U alice --pwfile f`, connecting as
/// alice with the password works against pgrust, and with another password
/// does not. `-A scram-sha-256` puts password authentication on both sides
/// (`check_need_password`, `initdb.c:2597`), so the login is the password's
/// doing; rlibpq is the client, over SCRAM-SHA-256.
#[test]
fn a_password_file_sets_the_password_alice_logs_in_with_under_pgrust() {
    let scratch = Scratch::new("password");
    let pgrust = install(&scratch.0);
    let pwfile = scratch.0.join("pwfile");
    std::fs::write(&pwfile, "s3cret'pw\n").expect("write the password file");
    let pgdata = scratch.0.join("data");
    let argv = [
        OsString::from("-U"),
        OsString::from("alice"),
        OsString::from("-A"),
        OsString::from("scram-sha-256"),
        OsString::from("--pwfile"),
        pwfile.into(),
        OsString::from("--no-sync"),
        pgdata.clone().into(),
    ];
    let env = server_env(&scratch.0).without(rinitdb::single_user::SERVER_ENV);
    let initdb = link_pgdrop(&scratch.0, "initdb");
    let outcome = testkit::run_in(&initdb, &argv, &[], &env).expect("run initdb");
    let stderr = outcome.stderr_text();
    assert_eq!(outcome.status, Some(0), "stderr: {stderr}");
    for line in stderr.lines() {
        assert!(line.contains(" LOG:  "), "stderr: {stderr}");
    }

    let mut server = Server::start(&pgrust, &pgdata, &server_env(&scratch.0), 55_683);
    let mut conn = server
        .connect("alice", "s3cret'pw")
        .expect("alice logs in with the password");
    let results = conn.exec(b"select current_user").expect("select");
    assert_eq!(results[0].value(0, 0), Some(&b"alice"[..]));
    drop(conn);
    let Err(refused) = server.connect("alice", "wrong") else {
        panic!("another password was accepted");
    };
    assert!(
        refused.contains("password authentication failed for user \"alice\""),
        "{refused}"
    );
}
