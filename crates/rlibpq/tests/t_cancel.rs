//! Query cancel through this crate against a real PostgreSQL 18 server.
//!
//! This is NAT-391's acceptance gate: "Cancel of `select pg_sleep(60)`
//! returns `57014` promptly (< 1 s)". There is no upstream test with a
//! deadline to steal — `libpq_pipeline.c`'s `test_cancel` (ported as
//! `t_001_libpq_pipeline.rs::test_cancel_blocking`) waits as long as it
//! takes — so these cases are named for what they pin. Each one blocks a
//! connection in `PQexec("select pg_sleep(60)")` on one thread, waits until
//! `pg_stat_activity` shows the sleep running (as `send_cancellable_query`
//! does, `libpq_pipeline.c:196`), fires one cancel API from another thread,
//! and times the query's return from that moment. `PQcancel` and
//! `PQcancelBlocking` are each driven over the Unix socket and over TCP,
//! since the peer address (`conn->raddr`) is read differently for each;
//! `PQrequestCancel`, which is `PQgetCancel` plus `PQcancel`, over the Unix
//! socket.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use std::thread;
use std::time::{Duration, Instant};

use rlibpq::{CancelStatus, Connection, ExecStatus, Peer};

mod common;

use common::{Cluster, WaitFor, assert_query_canceled, only, wait_for_connection_state};

/// "Promptly", as the acceptance puts it.
const PROMPTLY: Duration = Duration::from_secs(1);

/// The query the acceptance names.
const SLEEP: &[u8] = b"select pg_sleep(60)";

/// Block `conn` in `PQexec(SLEEP)` on a thread of its own, wait for the
/// sleep to be running, run `fire` here, and return the connection with how
/// long the query took to come back after `fire` began. The query's one
/// result must be the cancellation.
fn cancel_a_running_sleep(
    conn: Connection,
    monitor: &mut Connection,
    fire: impl FnOnce(),
) -> (Connection, Duration) {
    let pid = conn.backend_pid();
    wait_for_connection_state(monitor, pid, &WaitFor::State("idle"));
    let sleeper = thread::spawn(move || {
        let mut conn = conn;
        let results = conn.exec(SLEEP).expect("PQexec");
        (conn, results, Instant::now())
    });
    wait_for_connection_state(monitor, pid, &WaitFor::Event("PgSleep"));

    let fired = Instant::now();
    fire();
    let (conn, results, returned) = sleeper.join().expect("the sleeping thread");
    assert_query_canceled(&only(results));
    (conn, returned.duration_since(fired))
}

fn assert_prompt(api: &str, took: Duration) {
    assert!(
        took < PROMPTLY,
        "{api}: the cancelled query took {took:?} to return"
    );
}

/// The connection still runs queries after the cancel: the request hit the
/// sleep and nothing after it.
fn assert_still_usable(conn: &mut Connection) {
    let result = only(conn.exec(b"SELECT 1").expect("PQexec"));
    assert_eq!(result.status(), ExecStatus::TuplesOk, "{result:?}");
    assert_eq!(result.value(0, 0), Some(&b"1"[..]));
}

/// `PQgetCancel` + `PQcancel` and `PQcancelCreate` + `PQcancelBlocking`,
/// each fired from another thread at a connection blocked in `PQexec`.
fn both_apis_cancel_promptly(mut conn: Connection, monitor: &mut Connection) {
    let cancel = conn.get_cancel().expect("PQgetCancel");
    let (back, took) = cancel_a_running_sleep(conn, monitor, || {
        let cancel = cancel.clone();
        thread::spawn(move || cancel.cancel())
            .join()
            .expect("the cancelling thread")
            .expect("PQcancel");
    });
    assert_prompt("PQcancel", took);
    conn = back;
    assert_still_usable(&mut conn);

    let mut cancel_conn = conn.cancel_create();
    assert_eq!(cancel_conn.status(), CancelStatus::Allocated);
    let mut outcome = None;
    let (back, took) = cancel_a_running_sleep(conn, monitor, || {
        let sent = thread::spawn(move || (cancel_conn.blocking(), cancel_conn));
        outcome = Some(sent.join().expect("the cancelling thread"));
    });
    let (delivered, cancel_conn) = outcome.expect("the cancel was fired");
    assert!(
        delivered,
        "PQcancelBlocking: {}",
        String::from_utf8_lossy(cancel_conn.error_message())
    );
    assert_prompt("PQcancelBlocking", took);
    assert_eq!(cancel_conn.status(), CancelStatus::Ok);
    assert!(cancel_conn.error_message().is_empty());
    conn = back;
    assert_still_usable(&mut conn);
}

#[test]
fn a_sleep_is_cancelled_promptly_over_the_unix_socket() {
    let Some(cluster) = Cluster::start("trust", 55_480) else {
        return;
    };
    let conn = cluster.connect();
    // The socket path dialled, as C's `conn->raddr` (`fe-connect.c:3249`):
    // not `getpeername`'s answer, which on Darwin is NUL-padded.
    assert_eq!(
        conn.peer(),
        Some(Peer::Unix(
            cluster.dir.join(format!(".s.PGSQL.{}", cluster.port))
        ))
    );
    let mut monitor = cluster.connect();
    both_apis_cancel_promptly(conn, &mut monitor);
}

#[test]
fn a_sleep_is_cancelled_promptly_over_tcp() {
    let Some(cluster) = Cluster::start_listening("trust", 55_481, "127.0.0.1") else {
        return;
    };
    let conn = cluster.connect_tcp("127.0.0.1");
    assert!(
        matches!(conn.peer(), Some(Peer::Tcp(addr)) if addr.port() == 55_481),
        "{:?}",
        conn.peer()
    );
    let mut monitor = cluster.connect();
    both_apis_cancel_promptly(conn, &mut monitor);
}

/// `PQrequestCancel` reads the connection it cancels, so it cannot run while
/// another thread holds that connection in `PQexec`; it runs between
/// `PQsendQuery` and `PQgetResult` instead, as a single-threaded client
/// (or a signal handler) uses it.
#[test]
fn pqrequestcancel_cancels_a_sent_sleep_promptly() {
    let Some(cluster) = Cluster::start("trust", 55_482) else {
        return;
    };
    let mut conn = cluster.connect();
    let mut monitor = cluster.connect();
    let pid = conn.backend_pid();

    conn.send_query(SLEEP).expect("PQsendQuery");
    wait_for_connection_state(&mut monitor, pid, &WaitFor::Event("PgSleep"));
    let fired = Instant::now();
    conn.request_cancel().expect("PQrequestCancel");
    let result = conn.get_result().expect("PQgetResult").expect("a result");
    assert_prompt("PQrequestCancel", fired.elapsed());
    assert_query_canceled(&result);
    assert!(conn.get_result().expect("PQgetResult").is_none());
    assert_still_usable(&mut conn);
}

/// A cancel that arrives when nothing is running cancels nothing, and a
/// `PGcancelConn` needs `PQcancelReset` before it sends a second request
/// (`fe-cancel.c:209`, `:337`).
#[test]
fn a_cancel_with_nothing_running_is_harmless_and_a_reset_reuses_the_object() {
    let Some(cluster) = Cluster::start("trust", 55_483) else {
        return;
    };
    let mut conn = cluster.connect();
    let mut monitor = cluster.connect();

    let mut cancel_conn = conn.cancel_create();
    assert!(cancel_conn.blocking());
    assert_still_usable(&mut conn);

    assert!(!cancel_conn.blocking());
    assert_eq!(cancel_conn.status(), CancelStatus::Bad);
    assert_eq!(
        cancel_conn.error_message(),
        b"cancel request is already being sent on this connection\n"
    );

    cancel_conn.reset();
    let (mut conn, took) = cancel_a_running_sleep(conn, &mut monitor, || {
        assert!(
            cancel_conn.blocking(),
            "PQcancelBlocking after PQcancelReset: {}",
            String::from_utf8_lossy(cancel_conn.error_message())
        );
    });
    assert_prompt("PQcancelBlocking after PQcancelReset", took);
    assert_still_usable(&mut conn);
}
