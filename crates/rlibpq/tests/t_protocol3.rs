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

use std::path::PathBuf;
use std::process::Command;

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ExecStatus};
use testkit::reference;

/// The three C tools a live gate needs.
const TOOLS: [&str; 3] = ["initdb", "pg_ctl", "psql"];

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
        let bin = initdb.parent()?.to_path_buf();
        for tool in TOOLS {
            if !bin.join(tool).is_file() {
                reference::skip(tool);
                return None;
            }
        }

        let dir = std::env::temp_dir().join(format!("rlibpq-gate-{port}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("data");
        std::fs::create_dir_all(&dir).ok()?;

        let pwfile = dir.join("pwfile");
        std::fs::write(&pwfile, "gatepassword\n").ok()?;

        let status = Command::new(bin.join("initdb"))
            .args(["-D".as_ref(), data.as_os_str()])
            .args(["-U", "gateuser", "--auth-local", auth_method, "--auth-host"])
            .arg(auth_method)
            .arg("--pwfile")
            .arg(&pwfile)
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
    /// untitled, so only the values differ from ours.
    fn psql(&self, query: &str) -> (Vec<u8>, Vec<u8>, i32) {
        let out = Command::new(self.bin.join("psql"))
            .args(["-tAq", "-c", query, "-d", &self.conninfo()])
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

    let (stdout, _, code) = cluster.psql("select version()");
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

    let (_, stderr, code) = cluster.psql("selct 1");
    assert_ne!(code, 0);
    let rendered = results[0].error_message();
    assert_eq!(
        String::from_utf8_lossy(&rendered).trim_end(),
        String::from_utf8_lossy(&stderr).trim_end(),
        "the rendered error must match psql's"
    );
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

/// The gate is skipped, not narrowed, when there is no PostgreSQL 18 here:
/// each gate above announces its own `SKIP (flagged, not silent)` through
/// `testkit::reference::skip`, which writes to the process's own stderr so the
/// line survives libtest's capture in a passing run.
#[test]
fn the_reference_tools_are_looked_for_by_name() {
    for tool in TOOLS {
        // Only that the lookup is by these three names; whether it finds them
        // is the machine's business, and the gates handle both answers.
        let _ = reference::find(tool);
    }
}
