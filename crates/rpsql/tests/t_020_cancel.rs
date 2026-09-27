//! Port of `src/bin/psql/t/020_cancel.pl` (PostgreSQL 18.6): "Test query
//! canceling by sending SIGINT to a running psql".
//!
//! Upstream starts psql on a pipe it never closes, sends
//! `select pg_sleep($timeout_default);`, waits until the server shows the
//! sleep running, sends SIGINT, and requires a failed exit and
//! `canceling statement due to user request` on stderr. The same test runs
//! here against rpsql and, where this lane has one, against C psql; then
//! the two runs are diffed byte for byte — stdout, stderr and the exit
//! status, which is `EXIT_USER` under `ON_ERROR_STOP=1` (`mainloop.c:590`).
//!
//! The cluster comes from the reference `initdb` and `pg_ctl`; without them
//! the test prints `SKIP (flagged, not silent)` and passes, and under
//! `PGDROP_REQUIRE_REF=1` (CI) it fails instead.

#![allow(clippy::doc_markdown)]

use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Child, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

mod regress;

use regress::Cluster;

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// Unique among this crate's live gates; each starts its own cluster.
const CANCEL_PORT: u16 = 55_420;

/// `$PostgreSQL::Test::Utils::timeout_default` (`Utils.pm:172`-`:174`).
const TIMEOUT_DEFAULT: u64 = 180;

/// `$node->poll_query_until('postgres', $query)` (`Cluster.pm:2683`): run
/// `query` every 100 ms until it answers `t`, for at most ten times
/// `timeout_default` attempts.
fn poll_query_until(cluster: &Cluster, query: &[u8]) -> bool {
    let mut monitor = cluster.connect();
    for _ in 0..10 * TIMEOUT_DEFAULT {
        let results = monitor.exec(query).expect("the poll query runs");
        if results.first().and_then(|r| r.value(0, 0)) == Some(&b"t"[..]) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// 020_cancel.pl:22-:43 against one psql: start it, feed it the sleep, wait
/// for the server to register it, SIGINT it, and collect what it printed.
fn cancel_a_sleep(cluster: &Cluster, psql: &Path) -> Output {
    // 020_cancel.pl:24-:31.
    let mut child = cluster
        .command(psql)
        .args(["--no-psqlrc", "--set", "ON_ERROR_STOP=1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("psql starts");

    // Send sleep command and wait until the server has registered it
    // (020_cancel.pl:33-:38). The pipe stays open, as IPC::Run's does.
    let mut stdin = child.stdin.take().expect("a stdin pipe");
    stdin
        .write_all(format!("select pg_sleep({TIMEOUT_DEFAULT});\n").as_bytes())
        .expect("the query is sent");
    assert!(
        poll_query_until(
            cluster,
            b"SELECT (SELECT count(*) FROM pg_stat_activity WHERE query ~ '^select pg_sleep') > 0;",
        ),
        "timed out"
    );

    // Send cancel request (020_cancel.pl:40-:41).
    let sent = Instant::now();
    let kill = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill(1) runs");
    assert!(kill.success(), "SIGINT is delivered");

    // 020_cancel.pl:43. IPC::Run's `finish` waits for the exit; this waits
    // at most `timeout_default`, so a cancel that regresses — psql still
    // blocked on the open stdin after the sleep — fails instead of hanging.
    let output =
        wait_until(child, sent + Duration::from_secs(TIMEOUT_DEFAULT)).unwrap_or_else(|| {
            panic!(
                "{} did not exit within {TIMEOUT_DEFAULT}s of the cancel",
                psql.display()
            )
        });
    drop(stdin);
    output
}

/// Collect `child`'s exit status and both streams, or kill it and return
/// `None` if it has not exited by `deadline`. The streams are drained on
/// their own threads so a full pipe cannot stall the child.
fn wait_until(mut child: Child, deadline: Instant) -> Option<Output> {
    fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).expect("the pipe reads");
            bytes
        })
    }
    let stdout = drain(child.stdout.take().expect("a stdout pipe"));
    let stderr = drain(child.stderr.take().expect("a stderr pipe"));
    let status = loop {
        if let Some(status) = child.try_wait().expect("psql's status") {
            break status;
        }
        if Instant::now() >= deadline {
            // Best effort: the test fails either way.
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    };
    Some(Output {
        status,
        stdout: stdout.join().expect("the stdout reader"),
        stderr: stderr.join().expect("the stderr reader"),
    })
}

/// 020_cancel.pl:45-:49, both assertions, in upstream order.
fn assert_canceled(psql: &Path, output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "query failed as expected: {} exited {:?}\nstderr: {stderr}",
        psql.display(),
        output.status
    );
    assert!(
        stderr.contains("canceling statement due to user request"),
        "query was canceled: {} printed\n{stderr}",
        psql.display()
    );
}

/// `ok(!$result, 'query failed as expected')` and
/// `like($stderr, qr/canceling statement due to user request/, 'query was canceled')`
/// — 020_cancel.pl:45-:49 — for rpsql, then for C psql when it is present.
#[test]
fn query_was_canceled() {
    let Some(cluster) = Cluster::start(CANCEL_PORT) else {
        return;
    };

    let rpsql = Path::new(RPSQL);
    let ours = cancel_a_sleep(&cluster, rpsql);
    assert_canceled(rpsql, &ours);

    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cancel_a_sleep(&cluster, &psql);
            assert_canceled(&psql, &theirs);
            // Past upstream's two assertions: the whole of each stream, byte
            // for byte, with no normalizer — `Cancel request sent` from the
            // handler, then the server's error.
            assert_eq!(
                ours.status.code(),
                theirs.status.code(),
                "rpsql and C psql exit alike after the cancel"
            );
            assert_eq!(
                String::from_utf8_lossy(&ours.stderr),
                String::from_utf8_lossy(&theirs.stderr),
                "stderr, rpsql (left) against C psql (right)"
            );
            assert_eq!(ours.stderr, theirs.stderr, "stderr, as bytes");
            assert_eq!(ours.stdout, theirs.stdout, "stdout, as bytes");
        }
        // 020_cancel.pl ran against rpsql only; under PGDROP_REQUIRE_REF=1
        // a lane without C psql fails here rather than dropping the diff.
        None => testkit::reference::skip("psql"),
    }
}
