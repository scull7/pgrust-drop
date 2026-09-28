//! `src/test/examples/testlibpq2.c` (PostgreSQL REL_18_6), upstream's
//! example of the asynchronous notification interface, over the C ABI; and
//! `tests/c/async.c`, which drives every call that does not wait and the
//! notice hooks.
//!
//! `tests/c/testlibpq2.c` and `tests/c/testlibpq2.sql` are upstream's files,
//! unmodified (their sha256s are pinned in `upstream_files.rs`). The
//! example's header comment (`testlibpq2.c:12`-`:25`) is the procedure: load
//! `testlibpq2.sql`, start the program, then insert into `TESTLIBPQ2.TBL1`
//! four times from another session; the rule `r1` turns each insert into a
//! `NOTIFY TBL2`, and the program prints each notification as it arrives
//! and exits after the fourth. The other session here is `rlibpq`, and the
//! expected text is what `testlibpq2.c:135`-`:137` and `:144` print.
//!
//! `async.c`'s expected text is what C libpq 18.6 (PGDG's `libpq.so.5`)
//! printed for the same program against the same server, byte for byte and
//! run after run; that side-by-side run is not a gate here, as for
//! `params.c` and `connect.c` (see the PR for NAT-395 slice 6).
//!
//! The live gates need the reference tools; without them they print `SKIP
//! (flagged, not silent)` and pass, and with `PGDROP_REQUIRE_REF=1` a missing
//! reference fails instead.

#![allow(clippy::doc_markdown)]

mod common;
#[path = "../../tests/common/mod.rs"]
mod live;

use std::io::Read as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{build, crate_dir, run};
use live::{Cluster, only};
use rlibpq::Connection;

/// How long a program gets before the gate gives up on it.
const PATIENCE: Duration = Duration::from_mins(1);

/// Action: the one value `sql` returns on `conn`.
fn value(conn: &mut Connection, sql: &str) -> String {
    let result = only(conn.exec(sql.as_bytes()).expect("the query runs"));
    String::from_utf8_lossy(result.value(0, 0).expect("a value")).into_owned()
}

/// Action: wait until `child` has run `LISTEN TBL2` (`testlibpq2.c:96`) and
/// gone idle, so no notification is sent before it listens.
fn await_listen(monitor: &mut Connection, child: &mut Child) {
    let started = Instant::now();
    while value(
        monitor,
        "select count(*) from pg_stat_activity \
         where query = 'LISTEN TBL2' and state = 'idle'",
    ) != "1"
    {
        assert!(
            child.try_wait().expect("the program's status").is_none(),
            "testlibpq2 exited before it listened"
        );
        assert!(started.elapsed() < PATIENCE, "testlibpq2 never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Action: wait for `child` to exit, killing it (by its own PID) if it
/// outlasts [`PATIENCE`]; its exit code and stderr.
fn finish(mut child: Child) -> (Option<i32>, String) {
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("the program's status") {
            break status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            break child.wait().expect("the program is reaped");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr is piped")
        .read_to_string(&mut stderr)
        .expect("stderr reads");
    (status.code(), stderr)
}

/// The whole of `testlibpq2.c`'s `main` over `testlibpq2.sql`'s schema:
/// `LISTEN TBL2`, then the `select(2)` / `PQconsumeInput` / `PQnotifies`
/// loop until four notifications have come, each from the backend that
/// inserted, then `Done.`.
#[test]
fn testlibpq2_prints_four_notifications_then_done() {
    let Some(cluster) = Cluster::start("trust", 55_497) else {
        return;
    };
    let (_, stderr, code) = cluster.psql_script(include_str!("c/testlibpq2.sql"));
    assert_eq!(String::from_utf8_lossy(&stderr), "");
    assert_eq!(code, 0, "testlibpq2.sql loads");
    let program = build(
        "testlibpq2",
        &[crate_dir().join("tests/c/testlibpq2.c")],
        &[],
    );

    let mut child = Command::new(&program)
        .arg(cluster.conninfo())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("testlibpq2 starts");
    let mut session = cluster.connect();
    await_listen(&mut session, &mut child);
    let pid = value(&mut session, "select pg_backend_pid()");
    for _ in 0..4 {
        // Each insert its own transaction, so each NOTIFY is delivered.
        only(
            session
                .exec(b"INSERT INTO TESTLIBPQ2.TBL1 VALUES (10)")
                .expect("the insert runs"),
        );
    }
    let (code, stderr) = finish(child);

    let expected = format!(
        "{}Done.\n",
        format!("ASYNC NOTIFY of 'tbl2' received from backend PID {pid}\n").repeat(4)
    );
    assert_eq!(stderr, expected);
    assert_eq!(code, Some(0));
}

/// `tests/c/async.c` against a live server. What it covers, and where C
/// says so:
///
/// - the notice hooks: the defaults installed (`fe-connect.c:4976`), a NULL
///   `proc` changing nothing (`:7810`, `:7827`), a server notice reaching
///   the receiver as a `PGRES_NONFATAL_ERROR` result and through the
///   default receiver the processor (`:7842`); a result keeping the hooks
///   it was made with (`fe-exec.c:192`), so a range notice from an older
///   result goes to the older processor (`pqInternalNotice`, `:944`);
/// - `PQsendQuery` of three statements: the error ends it, and
///   `conn->errorMessage` holds the error after the NULL;
/// - a second command while one runs, refused without clearing the error
///   (`PQsendQueryStart`, `fe-exec.c:1700`, `:1711`), and each argument
///   check on an idle connection;
/// - the extended-query calls, the unnamed portal gone after the Sync;
/// - non-blocking mode: a 1 MiB query flushed with `PQflush` in turns;
/// - notifications from this session, with and without a payload;
/// - the server ending the session while a command runs: the FATAL result,
///   then `PQconsumeInput` failing on EOF (`pqReadData`, `fe-misc.c:833`),
///   then `PQgetResult` waiting on the closed socket ("invalid socket",
///   `fe-misc.c:1244`) and reporting the unreported error text
///   (`pqPrepareAsyncResult`, `fe-exec.c:901`); every call after that
///   refused.
///
/// Up to there the text is C libpq's. The last section, after a `PQreset`,
/// reads with `PQconsumeInput` alone until the server has closed the
/// socket, so the FATAL it sent is still unparsed then. Here it is parsed
/// and handed out before the "invalid socket" error; C keeps the bytes and
/// parses them later on a connection already `CONNECTION_BAD`, and does
/// not report them as the server sent them (`docs/divergences.md`).
#[test]
fn async_calls_and_notice_hooks_answer_as_c_libpq() {
    let Some(cluster) = Cluster::start("trust", 55_498) else {
        return;
    };
    let program = build(
        "async",
        &[crate_dir().join("tests/c/async.c")],
        &["-Wall", "-Werror"],
    );

    let outcome = run(&program, &[&cluster.conninfo()]);

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        [ASYNC_OUT, UNPARSED_OUT].concat()
    );
    assert_eq!(outcome.status, Some(0));
}

const ASYNC_OUT: &str = "PQsetNoticeProcessor default\n\
     processor one: NOTICE:  hello\n\
     PQsetNoticeReceiver default\n\
     PQsetNoticeReceiver NULL proc receiver\n\
     PQsetNoticeProcessor NULL proc processor\n\
     NULL conn NULL NULL\n\
     receiver r: PGRES_NONFATAL_ERROR severity WARNING primary careful\n\
     processor one: WARNING:  careful\n\
     receiver r: PGRES_NONFATAL_ERROR severity NOTICE primary row number 5 is out of range 0..0\n\
     processor one: row number 5 is out of range 0..0\n\
     PQgetvalue (null)\n\
     receiver r: PGRES_NONFATAL_ERROR severity NOTICE primary column number 7 is out of range 0..0\n\
     processor two: column number 7 is out of range 0..0\n\
     PQgetvalue (null)\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     three: PGRES_TUPLES_OK 1 row(s), first \"1\"\n\
     three: PGRES_FATAL_ERROR \"ERROR:  division by zero\n\
     \"\n\
     three: NULL PQerrorMessage \"ERROR:  division by zero\n\
     \"\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     receiver r: PGRES_NONFATAL_ERROR severity NOTICE primary async\n\
     processor two: NOTICE:  async\n\
     notice: PGRES_COMMAND_OK \"DO\" nparams 0 nfields 0\n\
     notice: NULL PQerrorMessage \"\"\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     PQsendQuery busy 0 PQerrorMessage \"another command is already in progress\n\
     \"\n\
     PQsendPrepare busy 0 PQerrorMessage \"another command is already in progress\n\
     another command is already in progress\n\
     \"\n\
     busy: PGRES_TUPLES_OK 1 row(s), first \"a\"\n\
     busy: NULL PQerrorMessage \"another command is already in progress\n\
     another command is already in progress\n\
     \"\n\
     PQsendQuery NULL 0 PQerrorMessage \"command string is a null pointer\n\
     \"\n\
     PQsendQueryParams -1 0 PQerrorMessage \"number of parameters must be between 0 and 65535\n\
     \"\n\
     PQsendPrepare NULL name 0 PQerrorMessage \"statement name is a null pointer\n\
     \"\n\
     PQsendPrepare NULL query 0 PQerrorMessage \"command string is a null pointer\n\
     \"\n\
     PQsendQueryPrepared NULL 0 PQerrorMessage \"statement name is a null pointer\n\
     \"\n\
     PQsendQueryParams 1 PQerrorMessage \"\"\n\
     params: PGRES_TUPLES_OK 1 row(s), first \"42\"\n\
     params: NULL PQerrorMessage \"\"\n\
     PQsendPrepare 1 PQerrorMessage \"\"\n\
     prepare: PGRES_COMMAND_OK \"\" nparams 0 nfields 0\n\
     prepare: NULL PQerrorMessage \"\"\n\
     PQsendQueryPrepared 1 PQerrorMessage \"\"\n\
     prepared: PGRES_TUPLES_OK 1 row(s), first \"hi!\"\n\
     prepared: NULL PQerrorMessage \"\"\n\
     PQsendDescribePrepared 1 PQerrorMessage \"\"\n\
     describe s1: PGRES_COMMAND_OK \"\" nparams 1 nfields 1\n\
     describe s1: NULL PQerrorMessage \"\"\n\
     PQsendDescribePortal 1 PQerrorMessage \"\"\n\
     describe portal: PGRES_FATAL_ERROR \"ERROR:  portal \"\" does not exist\n\
     \"\n\
     describe portal: NULL PQerrorMessage \"ERROR:  portal \"\" does not exist\n\
     \"\n\
     PQisnonblocking 0\n\
     PQsetnonblocking 1: 0\n\
     PQisnonblocking 1\n\
     PQsetnonblocking 1 again: 0\n\
     PQsendQuery 1 MiB 1 PQerrorMessage \"\"\n\
     PQflush 0\n\
     big: PGRES_TUPLES_OK 1 row(s), first \"1048576\"\n\
     big: NULL PQerrorMessage \"\"\n\
     PQsetnonblocking 0: 0\n\
     PQisnonblocking 0 PQflush 0\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     notify: PGRES_COMMAND_OK \"NOTIFY\" nparams 0 nfields 0\n\
     notify: NULL PQerrorMessage \"\"\n\
     PQnotifies ch \"payload\" from PQbackendPID\n\
     PQnotifies ch \"\" from PQbackendPID\n\
     PQnotifies NULL\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     terminated: PGRES_FATAL_ERROR \"FATAL:  terminating connection due to administrator command\n\
     \"\n\
     PQconsumeInput 0 PQstatus 1 PQsocket -1\n\
     terminated: PGRES_FATAL_ERROR \"server closed the connection unexpectedly\n\
     \tThis probably means the server terminated abnormally\n\
     \tbefore or while processing the request.\n\
     invalid socket\n\
     \"\n\
     terminated: NULL PQerrorMessage \"FATAL:  terminating connection due to administrator command\n\
     server closed the connection unexpectedly\n\
     \tThis probably means the server terminated abnormally\n\
     \tbefore or while processing the request.\n\
     invalid socket\n\
     \"\n\
     PQstatus 1 PQsocket -1 PQisBusy 0 PQflush -1 PQsetnonblocking -1 PQisnonblocking 0\n\
     PQsendQuery 0 PQerrorMessage \"no connection to the server\n\
     \"\n\
     PQconsumeInput 0 PQerrorMessage \"no connection to the server\n\
     connection not open\n\
     \"\n\
     PQgetResult NULL\n";

/// The last section of `async.c`, which is not C's: see its test.
const UNPARSED_OUT: &str = "PQreset PQstatus 0\n\
     PQsendQuery 1 PQerrorMessage \"\"\n\
     PQconsumeInput 0 PQstatus 1\n\
     unparsed: PGRES_FATAL_ERROR \"FATAL:  terminating connection due to administrator command\n\
     \"\n\
     unparsed: PGRES_FATAL_ERROR \"invalid socket\n\
     \"\n\
     unparsed: NULL PQerrorMessage \"FATAL:  terminating connection due to administrator command\n\
     server closed the connection unexpectedly\n\
     \tThis probably means the server terminated abnormally\n\
     \tbefore or while processing the request.\n\
     invalid socket\n\
     \"\n";
