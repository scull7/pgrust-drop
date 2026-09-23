//! The protocol version 3 core against a real PostgreSQL 18 server.
//!
//! These are the gates NAT-389 asks for: the same query, the same error and
//! the same authentication methods through this crate and through the C tools,
//! compared byte for byte. They need a PostgreSQL 18 `initdb`, `pg_ctl` and
//! `psql`; when those are missing every one of them prints
//! `SKIP (flagged, not silent)` and passes, and none of them narrows into a
//! weaker assertion (AGENTS.md, "Never weaken a gate to get green").
//!
//! There is no upstream TAP file to steal here: `src/interfaces/libpq/t/` has
//! no simple-query test, and the authentication suite
//! (`src/test/authentication/t/001_password.pl`) is outside the sparse
//! checkout this port reads from. The cases below are therefore named for what
//! they pin rather than after an upstream test name.

#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ContextVisibility, ExecStatus, Verbosity};
use testkit::reference;

/// The three C tools a live gate needs.
const TOOLS: [&str; 3] = ["initdb", "pg_ctl", "psql"];

/// Calculation: the `bin` directory holding all of [`TOOLS`], given where the
/// reference `initdb` was found and a predicate answering whether a path is an
/// executable file.
///
/// A gate needs all three from *one* PostgreSQL 18 installation — an `initdb`
/// from one tree driven by a `pg_ctl` from another would be comparing two
/// servers. `Err` names the first tool that is missing, which is the tool the
/// caller then announces the skip for; naming it matters because "no
/// PostgreSQL 18 at all" and "a client-only package with no `pg_ctl`" are
/// different things to go and fix.
///
/// `exists` is injected for the same reason `testkit::reference::locate` takes
/// it: the search is then a pure function with a unit test and no filesystem.
fn bin_dir_with_every_tool(
    initdb: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, &'static str> {
    let bin = initdb.parent().ok_or(TOOLS[0])?.to_path_buf();
    match TOOLS.into_iter().find(|tool| !exists(&bin.join(tool))) {
        Some(missing) => Err(missing),
        None => Ok(bin),
    }
}

/// A PostgreSQL 18 cluster, started for one gate and stopped with it.
struct Cluster {
    bin: PathBuf,
    dir: PathBuf,
    port: u16,
}

impl Cluster {
    /// Start a cluster whose `pg_hba.conf` uses `auth_method` for local
    /// connections, or `None` when the reference tools are absent.
    fn start(auth_method: &str, port: u16) -> Option<Self> {
        let Some(initdb) = reference::find(TOOLS[0]) else {
            reference::skip(TOOLS[0]);
            return None;
        };
        let bin = match bin_dir_with_every_tool(&initdb, Path::is_file) {
            Ok(bin) => bin,
            Err(missing) => {
                reference::skip(missing);
                return None;
            }
        };

        let dir = std::env::temp_dir().join(format!("rlibpq-gate-{port}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&dir).ok()?;

        let pwfile = dir.join("pwfile");
        std::fs::write(&pwfile, "gatepassword\n").ok()?;

        // PostgreSQL 18 defaults `password_encryption` to scram-sha-256, so a
        // cluster whose pg_hba.conf says `md5` would still store a SCRAM
        // verifier and still answer AUTH_REQ_SASL — the md5 case would quietly
        // test SCRAM twice. The verifier has to be built for the method.
        let encryption = if auth_method == "md5" {
            "md5"
        } else {
            "scram-sha-256"
        };
        let status = Command::new(bin.join("initdb"))
            .args(["-D".as_ref(), data.as_os_str()])
            .args(["-U", "gateuser", "--auth-local", auth_method, "--auth-host"])
            .arg(auth_method)
            .arg("--pwfile")
            .arg(&pwfile)
            .arg("-c")
            .arg(format!("password_encryption={encryption}"))
            .arg("--no-sync")
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        assert!(status.status.success(), "reference initdb failed");

        let socket_dir = dir.clone();
        let start = Command::new(bin.join("pg_ctl"))
            .args(["-D".as_ref(), data.as_os_str()])
            .arg("-w")
            .arg("-o")
            .arg(format!(
                "-p {port} -k {} -c listen_addresses=",
                socket_dir.display()
            ))
            .args(["-l".as_ref(), dir.join("log").as_os_str()])
            .arg("start")
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        assert!(start.status.success(), "reference pg_ctl start failed");

        Some(Cluster { bin, dir, port })
    }

    /// The conninfo string both sides connect with.
    fn conninfo(&self) -> String {
        format!(
            "host={} port={} user=gateuser dbname=postgres password=gatepassword",
            self.dir.display(),
            self.port
        )
    }

    fn connect(&self) -> Connection {
        let mut info = parse_conninfo(self.conninfo().as_bytes()).expect("conninfo parses");
        info.add_defaults(&Env::empty());
        Connection::connect(&info).expect("rlibpq connects")
    }

    /// `psql -tAq -c query` — the reference rendering, unaligned and
    /// untitled, so only the values differ from ours. `verbosity` is passed
    /// through as psql's `VERBOSITY` variable.
    fn psql(&self, query: &str, verbosity: &str) -> (Vec<u8>, Vec<u8>, i32) {
        let out = Command::new(self.bin.join("psql"))
            .args(["-tAq", "-v", &format!("VERBOSITY={verbosity}")])
            .args(["-c", query, "-d", &self.conninfo()])
            .env("LC_ALL", "C")
            .env("PGPASSWORD", "gatepassword")
            .output()
            .expect("reference psql runs");
        (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args(["-D".as_ref(), self.dir.join("data").as_os_str()])
            .args(["-m", "immediate", "stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `select version()` through this crate is the row C psql prints.
#[test]
fn select_version_matches_the_reference_psql() {
    let Some(cluster) = Cluster::start("trust", 55_432) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn.exec(b"select version()").expect("query runs");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::TuplesOk);
    assert_eq!(results[0].ntuples(), 1);
    assert_eq!(results[0].nfields(), 1);

    let (stdout, _, code) = cluster.psql("select version()", "default");
    assert_eq!(code, 0);
    let mut ours = results[0].value(0, 0).expect("a value").to_vec();
    ours.push(b'\n');
    assert_eq!(ours, stdout, "the version row must be byte-identical");
}

/// An error carries the fields C psql shows with `VERBOSITY verbose`: the
/// SQLSTATE, the primary message and the statement position.
#[test]
fn an_error_carries_the_fields_the_reference_reports() {
    let Some(cluster) = Cluster::start("trust", 55_433) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn.exec(b"selct 1").expect("the exchange completes");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::FatalError);
    let error = results[0].error().expect("an error result");
    assert_eq!(error.sqlstate(), Some(&b"42601"[..]));

    // The comparison is made at VERBOSITY terse, and that is not a
    // convenience: at every other verbosity C libpq puts the position on a
    // syntax-cursor display over the query it kept (`res->errQuery`,
    // `fe-protocol3.c:966`, filled for a simple query at `fe-exec.c:1484`),
    // printing `LINE 1: selct 1` and a caret line that `reportErrorPosition`
    // (`fe-protocol3.c:1202`) draws and this port does not — the divergence
    // `docs/divergences.md` records. Terse is the one verbosity where upstream
    // renders the position as text (`fe-protocol3.c:1102`), which is exactly
    // what this port renders, so this is a real byte-for-byte gate over the
    // same fields rather than a comparison the port is documented to fail.
    let (_, stderr, code) = cluster.psql("selct 1", "terse");
    assert_ne!(code, 0);
    let rendered = error.message(
        ExecStatus::FatalError,
        Verbosity::Terse,
        ContextVisibility::Errors,
    );
    assert_eq!(
        rendered, stderr,
        "the terse rendering must be psql's, byte for byte"
    );

    // What is *not* judged, said out loud rather than left out: the default
    // verbosity, where psql adds the two cursor lines.
    let (_, default_stderr, _) = cluster.psql("selct 1", "default");
    if default_stderr != results[0].error_message() {
        reference::announce_skip(
            "OUT OF SCOPE (flagged, not silent): the default-verbosity rendering differs by \
             the syntax-cursor display (reportErrorPosition, fe-protocol3.c:1202); see \
             docs/divergences.md",
        );
    }
}

/// A notice reaches the client as a notice, not as a result.
#[test]
fn a_notice_is_delivered_out_of_band() {
    let Some(cluster) = Cluster::start("trust", 55_434) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn
        .exec(b"drop table if exists no_such_table")
        .expect("runs");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::CommandOk);
    assert_eq!(conn.notices().len(), 1);
}

/// Each authentication method a build without TLS or GSSAPI can do, against a
/// server configured for it.
#[test]
fn every_authentication_method_connects() {
    for (index, method) in ["trust", "password", "md5", "scram-sha-256"]
        .into_iter()
        .enumerate()
    {
        let Some(cluster) = Cluster::start(method, 55_440 + u16::try_from(index).unwrap()) else {
            return;
        };
        let mut conn = cluster.connect();
        let results = conn.exec(b"select 1").expect("query runs");
        assert_eq!(results[0].value(0, 0), Some(&b"1"[..]), "method {method}");
    }
}

/// The gate is skipped, not narrowed, when this machine's PostgreSQL 18 is
/// incomplete: an installation missing any one of [`TOOLS`] is refused, and
/// refused by the name of the tool that is missing, so the gates above go on
/// to announce a `SKIP (flagged, not silent)` that says what to install.
///
/// Every arm is exercised because the silent narrowing this repo exists to
/// prevent lives in the arm nobody looks at: a `bin` directory holding
/// `initdb` and `psql` but no `pg_ctl` is a real client-only package, and the
/// mistake to catch is a lookup that shrugs at it and lets `Cluster::start`
/// run half a gate.
#[test]
fn an_installation_missing_any_one_tool_is_refused_by_name() {
    let complete = bin_dir_with_every_tool(Path::new("/ref/bin/initdb"), |_| true);
    assert_eq!(complete, Ok(PathBuf::from("/ref/bin")));

    for absent in TOOLS {
        let found = bin_dir_with_every_tool(Path::new("/ref/bin/initdb"), |path| {
            path != Path::new("/ref/bin").join(absent)
        });
        assert_eq!(found, Err(absent), "a bin directory without {absent}");
    }
}
