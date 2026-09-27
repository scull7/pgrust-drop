//! Asynchronous notification against a real PostgreSQL 18 server:
//! `src/test/examples/testlibpq2.c`, ported, and `PQnotifies` checked
//! against C libpq through the reference `psql`.
//!
//! Upstream builds `testlibpq2` (`src/test/examples/Makefile:17`) but checks
//! no expected output; its header comment is the test procedure
//! (`testlibpq2.c:8`-`:25`): load `testlibpq2.sql`, start the program, then
//! "from psql do this four times: `INSERT INTO TESTLIBPQ2.TBL1 VALUES (10);`".
//! [`testlibpq2`] does exactly that. The schema and the four inserts go
//! through the reference `psql`, so the NOTIFYs come from a C libpq session,
//! and the program's stderr must be exactly what the C program prints: one
//! `ASYNC NOTIFY of 'tbl2' received from backend PID <pid>` per insert, the
//! PID the one each inserting session reports for itself, then `Done.`.
//!
//! Two substitutions, both in how the program waits:
//!
//! - `select(2)` on `PQsocket` followed by `PQconsumeInput`
//!   (`testlibpq2.c:117`-`:132`) is [`Connection::wait_for_input`], which
//!   reads what ends the wait; rlibpq has no libc to `select` with (see
//!   `docs/divergences.md`).
//! - The wait is bounded at [`PATIENCE`] instead of `select`'s `NULL`
//!   timeout, so a lost notification fails the gate instead of hanging CI.
//!
//! [`notifies_matches_c_libpq`] checks the rest of `PGnotify`: the payload,
//! the order and a channel that needs quoting, rendered the way psql's
//! `PrintNotifications` renders them (`src/bin/psql/common.c:746`), against
//! what the reference psql prints for the same script.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use std::io::Write as _;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rlibpq::{Connection, ExecStatus, Notify};

mod common;

use common::{Cluster, only};

/// How long [`testlibpq2_main`] waits for the next notification before the
/// gate fails.
const PATIENCE: Duration = Duration::from_mins(1);

/// `src/test/examples/testlibpq2.sql`, as REL_18_6 ships it.
const TESTLIBPQ2_SQL: &str = "CREATE SCHEMA TESTLIBPQ2;
SET search_path = TESTLIBPQ2;
CREATE TABLE TBL1 (i int4);
CREATE TABLE TBL2 (i int4);
CREATE RULE r1 AS ON INSERT TO TBL1 DO
  (INSERT INTO TBL2 VALUES (new.i); NOTIFY TBL2);
";

/// `main`, `testlibpq2.c:49`, from the connection on (`:68`); what it
/// writes to stderr goes to `stderr`. `listening` runs once the LISTEN has
/// succeeded, where upstream's reader switches to psql.
fn testlibpq2_main(mut conn: Connection, stderr: &mut Vec<u8>, listening: impl FnOnce()) {
    // testlibpq2.c:78 — always-secure search path.
    let res = only(
        conn.exec(b"SELECT pg_catalog.set_config('search_path', '', false)")
            .expect("the connection holds"),
    );
    if res.status() != ExecStatus::TuplesOk {
        stderr.extend_from_slice(b"SET failed: ");
        stderr.extend_from_slice(&res.error_message());
        return;
    }

    // testlibpq2.c:96
    let res = only(conn.exec(b"LISTEN TBL2").expect("the connection holds"));
    if res.status() != ExecStatus::CommandOk {
        stderr.extend_from_slice(b"LISTEN command failed: ");
        stderr.extend_from_slice(&res.error_message());
        return;
    }
    listening();

    // testlibpq2.c:106 — quit after four notifies are received.
    let mut nnotifies = 0;
    while nnotifies < 4 {
        // testlibpq2.c:117-:132: sleep until something happens on the
        // connection, then check for input.
        let arrived = conn
            .wait_for_input(Some(Instant::now() + PATIENCE))
            .expect("the connection holds");
        assert!(arrived, "no notification within {PATIENCE:?}");
        // testlibpq2.c:133
        while let Some(notify) = conn.notifies().expect("the input parses") {
            stderr.extend_from_slice(b"ASYNC NOTIFY of '");
            stderr.extend_from_slice(&notify.relname);
            let _ = writeln!(stderr, "' received from backend PID {}", notify.be_pid);
            nnotifies += 1;
            conn.consume_input().expect("the connection holds");
        }
    }

    // testlibpq2.c:144
    stderr.extend_from_slice(b"Done.\n");
    // testlibpq2.c:147 — PQfinish.
    let _ = conn.terminate();
}

#[test]
fn testlibpq2() {
    let Some(cluster) = Cluster::start("trust", 55_540) else {
        return;
    };
    let (out, err, code) = cluster.psql(TESTLIBPQ2_SQL, "default");
    assert_eq!(
        (code, err.as_slice()),
        (0, &b""[..]),
        "testlibpq2.sql loads"
    );
    assert!(out.is_empty(), "{}", String::from_utf8_lossy(&out));

    // "from psql do this four times": each insert is its own session, and
    // reports the backend PID the notification must carry.
    let (pids_tx, pids_rx) = mpsc::channel();
    let mut stderr = Vec::new();
    std::thread::scope(|scope| {
        testlibpq2_main(cluster.connect(), &mut stderr, || {
            let cluster = &cluster;
            scope.spawn(move || {
                for _ in 0..4 {
                    let (out, err, code) = cluster.psql(
                        "INSERT INTO TESTLIBPQ2.TBL1 VALUES (10); SELECT pg_backend_pid()",
                        "default",
                    );
                    assert_eq!((code, err.as_slice()), (0, &b""[..]), "the insert runs");
                    let pid: i32 = String::from_utf8(out)
                        .expect("a PID is ASCII")
                        .trim_end()
                        .parse()
                        .expect("psql prints the PID");
                    pids_tx.send(pid).expect("the gate is listening");
                }
            });
        });
    });

    let expected: String = pids_rx
        .iter()
        .map(|pid| format!("ASYNC NOTIFY of 'tbl2' received from backend PID {pid}\n"))
        .chain(std::iter::once("Done.\n".to_owned()))
        .collect();
    assert_eq!(String::from_utf8_lossy(&stderr), expected);

    // The rule did its other half too.
    let (out, _, _) = cluster.psql("SELECT count(*) FROM TESTLIBPQ2.TBL2", "default");
    assert_eq!(out, b"4\n");
}

/// Notifications, rendered as `PrintNotifications` renders them
/// (`src/bin/psql/common.c:754`-`:759`), with the PID written as `<pid>`
/// once it is checked to be `pid`.
fn print_notifications(notifies: &[Notify], pid: i32) -> String {
    notifies
        .iter()
        .map(|notify| {
            assert_eq!(notify.be_pid, pid, "a self-notification carries our PID");
            let relname = String::from_utf8_lossy(&notify.relname);
            if notify.extra.is_empty() {
                format!(
                    "Asynchronous notification \"{relname}\" received from server process with PID <pid>.\n"
                )
            } else {
                format!(
                    "Asynchronous notification \"{relname}\" with payload \"{}\" received from server process with PID <pid>.\n",
                    String::from_utf8_lossy(&notify.extra)
                )
            }
        })
        .collect()
}

#[test]
fn notifies_matches_c_libpq() {
    // One statement per query, as psql runs a script. Delivered at COMMIT,
    // in the order sent; the repeated `NOTIFY a, 'one'` in the same
    // transaction is folded into the first.
    const SCRIPT: [&str; 8] = [
        "LISTEN a;",
        "LISTEN \"B c\";",
        "BEGIN;",
        "NOTIFY a, 'one';",
        "SELECT pg_notify('B c', 'two');",
        "NOTIFY a;",
        "NOTIFY a, 'one';",
        "COMMIT;",
    ];
    let Some(cluster) = Cluster::start("trust", 55_541) else {
        return;
    };

    let mut conn = cluster.connect();
    for statement in SCRIPT {
        let result = only(
            conn.exec(statement.as_bytes())
                .expect("the connection holds"),
        );
        assert!(
            matches!(
                result.status(),
                ExecStatus::CommandOk | ExecStatus::TuplesOk
            ),
            "{statement}: {}",
            String::from_utf8_lossy(&result.error_message())
        );
    }
    let mut notifies = Vec::new();
    while let Some(notify) = conn.notifies().expect("the input parses") {
        notifies.push(notify);
    }
    assert!(conn.notifications().is_empty());
    let ours = print_notifications(&notifies, conn.backend_pid());
    assert_eq!(
        ours,
        "Asynchronous notification \"a\" with payload \"one\" received from server process with PID <pid>.\n\
         Asynchronous notification \"B c\" with payload \"two\" received from server process with PID <pid>.\n\
         Asynchronous notification \"a\" received from server process with PID <pid>.\n"
    );

    // C libpq, through the reference psql: the same statements, then the
    // session's PID. PrintNotifications runs after each (`common.c:750`).
    let (out, err, code) = cluster.psql_script(&format!(
        "{}\nSELECT 'pid ' || pg_backend_pid();\n",
        SCRIPT.join("\n")
    ));
    assert_eq!((code, err.as_slice()), (0, &b""[..]), "the script runs");
    let out = String::from_utf8(out).expect("psql's output is ASCII");
    // `pg_notify` returns void, which -tA prints as an empty row.
    let theirs = out.strip_prefix('\n').expect("pg_notify's empty row");
    let (theirs, pid) = theirs.rsplit_once("pid ").expect("the PID row");
    let pid = pid.strip_suffix('\n').expect("the PID row ends");
    assert_eq!(ours, theirs.replace(&format!("PID {pid}."), "PID <pid>."));
}
