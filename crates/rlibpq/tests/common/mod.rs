//! The live-gate harness the rlibpq integration tests share: a PostgreSQL 18
//! cluster started from the reference tools for one gate and stopped with it,
//! and the trace plumbing the `libpq_pipeline` ports compare with.
//!
//! When the reference tools are missing, [`Cluster::start`] prints
//! `SKIP (flagged, not silent)` and returns `None`; every gate then returns
//! without narrowing into a weaker assertion.

// Each integration test is its own crate and uses only part of this module.
#![allow(dead_code, clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ExecStatus, QueryResult, TraceFlags};
use testkit::reference;

/// The three C tools a live gate needs.
pub const TOOLS: [&str; 3] = ["initdb", "pg_ctl", "psql"];

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
pub fn bin_dir_with_every_tool(
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
pub struct Cluster {
    pub bin: PathBuf,
    pub dir: PathBuf,
    pub port: u16,
}

impl Cluster {
    /// Start a cluster whose `pg_hba.conf` uses `auth_method` for local
    /// connections, or `None` when the reference tools are absent.
    pub fn start(auth_method: &str, port: u16) -> Option<Self> {
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
    pub fn conninfo(&self) -> String {
        format!(
            "host={} port={} user=gateuser dbname=postgres password=gatepassword",
            self.dir.display(),
            self.port
        )
    }

    pub fn connect(&self) -> Connection {
        let mut info = parse_conninfo(self.conninfo().as_bytes()).expect("conninfo parses");
        info.add_defaults(&Env::empty());
        Connection::connect(&info).expect("rlibpq connects")
    }

    /// `psql -tAq -c query` — the reference rendering, unaligned and
    /// untitled, so only the values differ from ours. `verbosity` is passed
    /// through as psql's `VERBOSITY` variable.
    pub fn psql(&self, query: &str, verbosity: &str) -> (Vec<u8>, Vec<u8>, i32) {
        let out = Command::new(self.bin.join("psql"))
            .args(["-tAq", "-v", &format!("VERBOSITY={verbosity}")])
            .args(["-c", query, "-d", &self.conninfo()])
            .env("LC_ALL", "C")
            .env("PGPASSWORD", "gatepassword")
            .output()
            .expect("reference psql runs");
        (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
    }

    /// `psql -tAqX` reading `script` from a pipe, at `VERBOSITY terse`. A
    /// piped stdin is not an input *file*, so psql prefixes no
    /// `psql:<file>:<line>:` to its errors, and the extended-query
    /// meta-commands (`\bind`, `\parse`, `\bind_named`, `\close_prepared`),
    /// which `-c` cannot mix with SQL, are available.
    pub fn psql_script(&self, script: &str) -> (Vec<u8>, Vec<u8>, i32) {
        use std::io::Write as _;
        let mut child = Command::new(self.bin.join("psql"))
            .args(["-tAqX", "-v", "VERBOSITY=terse", "-d", &self.conninfo()])
            .env("LC_ALL", "C")
            .env("PGPASSWORD", "gatepassword")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("reference psql runs");
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(script.as_bytes())
            .expect("the script is written");
        let out = child.wait_with_output().expect("reference psql exits");
        (out.stdout, out.stderr, out.status.code().unwrap_or(-1))
    }
}

/// Calculation: a result as `psql -tA` prints it — each row's values joined
/// by `|`, one row per line, NULL as the empty string.
pub fn unaligned(result: &QueryResult) -> Vec<u8> {
    let mut out = Vec::new();
    for row in 0..result.ntuples() {
        for column in 0..result.nfields() {
            if column > 0 {
                out.push(b'|');
            }
            out.extend_from_slice(result.value(row, column).unwrap_or_default());
        }
        out.push(b'\n');
    }
    out
}

/// The one result an extended-query command must produce.
pub fn only(results: Vec<QueryResult>) -> QueryResult {
    assert_eq!(results.len(), 1, "one result: {results:?}");
    results.into_iter().next().expect("one result")
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

/// A trace sink the gate reads back after `PQuntrace` hands it over.
#[derive(Clone, Default)]
pub struct SharedSink(pub std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("sink lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// An upstream trace vendored in `tests/traces/`, whole.
pub fn upstream_trace(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/traces")
        .join(name);
    std::fs::read(&path).expect("the vendored trace is readable")
}

/// `001_libpq_pipeline.pl:72`, "<test> trace match": end the connection as
/// `PQfinish` does (its Terminate is the trace's last line), then compare
/// what was traced with the upstream file, whole and byte for byte.
pub fn finish_and_compare_trace(mut conn: Connection, sink: &SharedSink, name: &str) {
    conn.terminate().expect("Terminate is sent");
    assert!(conn.untrace().is_some(), "the trace was on");
    let ours = sink.0.lock().expect("sink lock").clone();
    let expected = upstream_trace(name);
    assert_eq!(
        String::from_utf8_lossy(&ours),
        String::from_utf8_lossy(&expected),
        "{name} must match byte for byte"
    );
    assert_eq!(ours, expected);
}

/// A connection set up the way `libpq_pipeline.c`'s `main` sets one up
/// before it turns tracing on (`:2332`-`:2337`, `:2352`-`:2354`).
pub fn traced_like_libpq_pipeline(cluster: &Cluster) -> (Connection, SharedSink) {
    let mut conn = cluster.connect();
    for setup in [
        &b"SET lc_messages TO \"C\""[..],
        b"SET debug_parallel_query = off",
    ] {
        let result = only(conn.exec(setup).expect("runs"));
        assert_eq!(result.status(), ExecStatus::CommandOk);
    }
    let sink = SharedSink::default();
    conn.trace(Box::new(sink.clone()));
    conn.set_trace_flags(TraceFlags::SUPPRESS_TIMESTAMPS | TraceFlags::REGRESS_MODE);
    (conn, sink)
}
