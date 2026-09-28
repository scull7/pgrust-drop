//! `src/test/modules/libpq_pipeline/t/001_libpq_pipeline.pl`, ported: each
//! `libpq_pipeline.c` test that needs only pipeline mode, run through this
//! crate against a PostgreSQL 18 server, with its `PQtrace` output compared
//! whole and byte for byte with the trace PostgreSQL 18.6 ships
//! (`tests/traces/`, "<test> trace match", `001_libpq_pipeline.pl:72`).
//!
//! The Rust test names are the C function names, and each body follows its C
//! function statement by statement, citing it. Every C `pg_fatal` check is an
//! assertion here carrying the same text. The connection is set up as
//! `main` sets it up before tracing (`libpq_pipeline.c:2332`-`:2354`), and
//! ends as `PQfinish` ends it, whose Terminate is each trace's last line.
//!
//! All nine traces upstream ships are compared here, and
//! `test_pipelined_insert`, which has none, runs as upstream runs it. Not here
//! yet: `test_uniqviol`, which has no trace to compare either,
//! `test_protocol_version`, which needs protocol 3.2 (NAT-391), and the second
//! half of `test_cancel`, which drives `PQcancelStart` and `PQcancelPoll`
//! with `select()`; those two are not ported yet. Its blocking half is
//! `test_cancel_blocking`.
//!
//! Without the reference tools every test prints `SKIP (flagged, not silent)`
//! and passes; with `PGDROP_REQUIRE_REF=1` a missing reference fails instead.

// Each test follows its C function statement by statement, so it is as long
// as that function; splitting it would lose the one-to-one reading.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use rlibpq::{
    Connection, ExecStatus, Flush, Format, Params, PipelineStatus, QueryResult, ResultError,
    result::diag,
};

mod common;

use common::{
    Cluster, confirm_query_canceled, finish_and_compare_trace, like_libpq_pipeline,
    send_cancellable_query, traced_like_libpq_pipeline,
};

/// `INT4OID`, `INT8OID`, …, `pg_type_d.h`.
const INT4OID: u32 = 23;
const INT8OID: u32 = 20;
const TEXTOID: u32 = 25;
const NUMERICOID: u32 = 1700;
const INTERVALOID: u32 = 1186;

/// `drop_table_sql`, `create_table_sql`, `insert_sql`, `insert_sql2`
/// (`libpq_pipeline.c:44`-`:52`).
const DROP_TABLE_SQL: &[u8] = b"DROP TABLE IF EXISTS pq_pipeline_demo";
const CREATE_TABLE_SQL: &[u8] = b"CREATE UNLOGGED TABLE pq_pipeline_demo(id serial primary key, itemno integer,int8filler int8);";
const INSERT_SQL: &[u8] = b"INSERT INTO pq_pipeline_demo(itemno) VALUES ($1)";
const INSERT_SQL2: &[u8] = b"INSERT INTO pq_pipeline_demo(itemno,int8filler) VALUES ($1, $2)";

/// `PQgetResult`, with a broken connection a test failure.
fn get(conn: &mut Connection) -> Option<QueryResult> {
    conn.get_result().expect("PQgetResult")
}

/// `PQresultStatus`, where a NULL result is `PGRES_FATAL_ERROR`
/// (`fe-exec.c:3445`) — the C tests lean on that for `PQexec`.
fn status(result: Option<&QueryResult>) -> ExecStatus {
    result.map_or(ExecStatus::FatalError, QueryResult::status)
}

/// `PQexec`'s one result: the last of them (`PQexecFinish`,
/// `fe-exec.c:2427`).
fn exec(conn: &mut Connection, query: &[u8]) -> Option<QueryResult> {
    conn.exec(query).expect("PQexec").pop()
}

/// `PQresultErrorField(res, PG_DIAG_SQLSTATE)`.
fn sqlstate(result: &QueryResult) -> &[u8] {
    result
        .error()
        .and_then(ResultError::sqlstate)
        .unwrap_or_default()
}

/// `PQerrorMessage` after a refused call: libpq's message and its newline.
fn error_message(error: &rlibpq::ConnectionError) -> String {
    format!("{error}\n")
}

/// `test_disallowed_in_pipeline`, `libpq_pipeline.c:408`.
#[test]
fn test_disallowed_in_pipeline() {
    let Some(cluster) = Cluster::start("trust", 55_460) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);

    // :414
    assert!(!conn.is_nonblocking(), "Expected blocking connection mode");
    conn.enter_pipeline_mode()
        .expect("Unable to enter pipeline mode");
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Pipeline mode not activated properly"
    );

    // :424 — PQexec should fail in pipeline mode.
    let error = conn
        .exec(b"SELECT 1")
        .expect_err("PQexec should fail in pipeline mode but succeeded");
    assert_eq!(
        error_message(&error),
        "synchronous command execution functions are not allowed in pipeline mode\n",
        "did not get expected error message"
    );

    // :433 — PQsendQuery should fail in pipeline mode.
    let error = conn
        .send_query(b"SELECT 1")
        .expect_err("PQsendQuery should fail in pipeline mode but succeeded");
    assert_eq!(
        error_message(&error),
        "PQsendQuery not allowed in pipeline mode\n",
        "did not get expected error message"
    );

    // :441 — entering pipeline mode when already in it is OK.
    conn.enter_pipeline_mode()
        .expect("re-entering pipeline mode should be a no-op but failed");
    assert!(
        !conn.is_busy().expect("PQisBusy"),
        "PQisBusy should return 0 when idle in pipeline mode, returned 1"
    );

    // :448 — back to normal command mode.
    conn.exit_pipeline_mode()
        .expect("couldn't exit idle empty pipeline mode");
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Pipeline mode not terminated properly"
    );
    conn.exit_pipeline_mode()
        .expect("pipeline mode exit when not in pipeline mode should succeed but failed");

    // :459 — can now PQexec again.
    let res = exec(&mut conn, b"SELECT 1");
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::TuplesOk,
        "PQexec should succeed after exiting pipeline mode but failed"
    );

    finish_and_compare_trace(conn, &sink, "disallowed_in_pipeline.trace");
}

/// `test_multi_pipelines`, `libpq_pipeline.c:468`.
#[test]
fn test_multi_pipelines() {
    let Some(cluster) = Cluster::start("trust", 55_461) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let dummy_params: [Option<&[u8]>; 1] = [Some(b"1")];
    let params = Params::text(&dummy_params);

    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");

    // :483 — first pipeline.
    conn.send_query_params(b"SELECT $1", &[INT4OID], &params)
        .expect("dispatching first SELECT failed");
    conn.pipeline_sync().expect("Pipeline sync failed");

    // :491 — second pipeline, skipping the flush once.
    conn.send_query_params(b"SELECT $1", &[INT4OID], &params)
        .expect("dispatching second SELECT failed");
    conn.send_pipeline_sync().expect("Pipeline sync failed");

    // :500 — third pipeline.
    conn.send_query_params(b"SELECT $1", &[INT4OID], &params)
        .expect("dispatching third SELECT failed");
    conn.pipeline_sync().expect("pipeline sync failed");

    // :510 — first and second pipelines' results.
    for ordinal in ["first", "second"] {
        let res = get(&mut conn).expect("PQgetResult returned null when there's a pipeline item");
        assert_eq!(
            res.status(),
            ExecStatus::TuplesOk,
            "Unexpected result code from {ordinal} pipeline item"
        );
        assert!(
            get(&mut conn).is_none(),
            "PQgetResult returned something extra after first result"
        );
        assert!(
            conn.exit_pipeline_mode().is_err(),
            "exiting pipeline mode after query but before sync succeeded incorrectly"
        );
        let res = get(&mut conn).expect("PQgetResult returned null when sync result expected");
        assert_eq!(
            res.status(),
            ExecStatus::PipelineSync,
            "Unexpected result code instead of sync result"
        );
    }

    // :568 — third pipeline.
    let res = get(&mut conn).expect("PQgetResult returned null when there's a pipeline item");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "Unexpected result code from third pipeline item"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");
    let res = get(&mut conn).expect("PQgetResult returned null when there's a pipeline item");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code from second pipeline sync"
    );

    // :593 — still in pipeline mode, until we end it.
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Fell out of pipeline mode somehow"
    );
    conn.exit_pipeline_mode()
        .expect("attempt to exit pipeline mode failed when it should've succeeded");
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "exiting pipeline mode didn't seem to work"
    );

    finish_and_compare_trace(conn, &sink, "multi_pipelines.trace");
}

/// `test_nosync`, `libpq_pipeline.c:613`.
#[test]
fn test_nosync() {
    let Some(cluster) = Cluster::start("trust", 55_462) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let numqueries = 10;
    let mut results = 0;

    conn.enter_pipeline_mode()
        .expect("could not enter pipeline mode");
    for _ in 0..numqueries {
        conn.send_query_params(b"SELECT repeat('xyzxz', 12)", &[], &Params::text(&[]))
            .expect("error sending select");
        conn.flush().expect("PQflush");
        // :637 — "If the server has written anything to us, read (some of)
        // it now": `select()` with a zero timeout, then `PQconsumeInput`,
        // which is `consume_input`'s non-blocking read in one call.
        conn.consume_input().expect("failed to read from server");
    }

    // :652 — tell the server to flush its output buffer.
    conn.send_flush_request()
        .expect("failed to send flush request");
    conn.flush().expect("PQflush");

    // :657 — now read all results.
    loop {
        let res = get(&mut conn);
        let Some(res) = res else {
            panic!("got unexpected NULL result after {results} results");
        };
        // We expect exactly one TUPLES_OK result for each query we sent.
        assert_eq!(
            res.status(),
            ExecStatus::TuplesOk,
            "got unexpected {}",
            res.status().as_str()
        );
        // And one NULL result should follow each.
        let res2 = get(&mut conn);
        assert!(res2.is_none(), "expected NULL, got {res2:?}");
        results += 1;
        if results == numqueries {
            break;
        }
    }

    finish_and_compare_trace(conn, &sink, "nosync.trace");
}

/// `test_pipeline_abort`, `libpq_pipeline.c:705`.
#[test]
fn test_pipeline_abort() {
    let Some(cluster) = Cluster::start("trust", 55_463) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);

    let res = exec(&mut conn, DROP_TABLE_SQL);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "dispatching DROP TABLE failed"
    );
    let res = exec(&mut conn, CREATE_TABLE_SQL);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "dispatching CREATE TABLE failed"
    );

    // :725 — two small pipelines; the second operation of the first errors.
    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_query_params(INSERT_SQL, &[INT4OID], &Params::text(&[Some(b"1")]))
        .expect("dispatching first insert failed");
    conn.send_query_params(
        b"SELECT no_such_function($1)",
        &[INT4OID],
        &Params::text(&[Some(b"1")]),
    )
    .expect("dispatching error select failed");
    conn.send_query_params(INSERT_SQL, &[INT4OID], &Params::text(&[Some(b"2")]))
        .expect("dispatching second insert failed");
    conn.pipeline_sync().expect("pipeline sync failed");
    conn.send_query_params(INSERT_SQL, &[INT4OID], &Params::text(&[Some(b"3")]))
        .expect("dispatching second-pipeline insert failed");
    conn.pipeline_sync().expect("pipeline sync failed");

    // :760 — command-ok for the first query, then its NULL.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::CommandOk,
        "Unexpected result status"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");

    // :780 — the second query caused an error.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::FatalError,
        "Unexpected result code -- expected PGRES_FATAL_ERROR"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");

    // :795 — the pipeline is now aborted.
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Aborted,
        "pipeline should be flagged as aborted but isn't"
    );

    // :804 — the third query, the second insert.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineAborted,
        "Unexpected result code -- expected PGRES_PIPELINE_ABORTED"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Aborted,
        "pipeline should be flagged as aborted but isn't"
    );
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Fell out of pipeline mode somehow"
    );

    // :825 — the end of a failed pipeline is a PGRES_PIPELINE_SYNC.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code from first pipeline sync"
    );
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Aborted,
        "sync should've cleared the aborted flag but didn't"
    );
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Fell out of pipeline mode somehow"
    );

    // :846 — the insert from the second pipeline, its NULL, its sync.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::CommandOk,
        "Unexpected result code from first item in second pipeline"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code from second pipeline sync"
    );
    let res = get(&mut conn);
    assert!(res.is_none(), "Expected null result, got {res:?}");

    // :872 — try to send two queries in one command.
    conn.send_query_params(b"SELECT 1; SELECT 2", &[], &Params::text(&[]))
        .expect("failed to send query");
    conn.pipeline_sync().expect("pipeline sync failed");
    let mut goterror = false;
    while let Some(res) = get(&mut conn) {
        match res.status() {
            ExecStatus::FatalError => {
                assert_eq!(
                    sqlstate(&res),
                    b"42601",
                    "expected error about multiple commands"
                );
                goterror = true;
            }
            other => panic!("got unexpected status {}", other.as_str()),
        }
    }
    assert!(
        goterror,
        "did not get cannot-insert-multiple-commands error"
    );
    let res = get(&mut conn).expect("got NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code from pipeline sync"
    );

    // :904 — single-row mode with an error partway.
    conn.send_query_params(
        b"SELECT 1.0/g FROM generate_series(3, -1, -1) g",
        &[],
        &Params::text(&[]),
    )
    .expect("failed to send query");
    conn.pipeline_sync().expect("pipeline sync failed");
    conn.set_single_row_mode();
    let mut goterror = false;
    let mut gotrows = 0;
    while let Some(res) = get(&mut conn) {
        match res.status() {
            ExecStatus::SingleTuple => gotrows += 1,
            ExecStatus::FatalError => {
                assert_eq!(sqlstate(&res), b"22012", "expected division-by-zero");
                goterror = true;
            }
            other => panic!("got unexpected result {}", other.as_str()),
        }
    }
    assert!(goterror, "did not get division-by-zero error");
    assert_eq!(gotrows, 3, "did not get three rows");
    // :938 — the third pipeline sync.
    let res = get(&mut conn).expect("Unexpected NULL result");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code from third pipeline sync"
    );

    // :946 — still in pipeline mode, until we end it.
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Fell out of pipeline mode somehow"
    );
    conn.exit_pipeline_mode()
        .expect("attempt to exit pipeline mode failed when it should've succeeded");
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "exiting pipeline mode didn't seem to work"
    );

    // :973 — only the value 3 was inserted.
    let res = exec(&mut conn, b"SELECT itemno FROM pq_pipeline_demo");
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::TuplesOk,
        "Expected tuples"
    );
    let res = res.expect("a result");
    assert_eq!(res.ntuples(), 1, "expected 1 result");
    for i in 0..res.ntuples() {
        assert_eq!(
            res.value(i, 0),
            Some(&b"3"[..]),
            "expected only insert with value 3"
        );
    }

    finish_and_compare_trace(conn, &sink, "pipeline_abort.trace");
}

/// `enum PipelineInsertStep`, `libpq_pipeline.c:994`, in its order: the
/// derived `Ord` is C's enum arithmetic (`recv_step < BI_DONE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PipelineInsertStep {
    BeginTx,
    DropTable,
    CreateTable,
    Prepare,
    InsertRows,
    CommitTx,
    Sync,
    Done,
}

impl PipelineInsertStep {
    /// `step++`.
    fn next(self) -> Self {
        match self {
            Self::BeginTx => Self::DropTable,
            Self::DropTable => Self::CreateTable,
            Self::CreateTable => Self::Prepare,
            Self::Prepare => Self::InsertRows,
            Self::InsertRows => Self::CommitTx,
            Self::CommitTx => Self::Sync,
            Self::Sync | Self::Done => Self::Done,
        }
    }
}

/// `test_pipelined_insert`, `libpq_pipeline.c:1007`, with the `n_rows`
/// `001_libpq_pipeline.pl` passes (`-r 700`, `:28`).
///
/// No trace is compared: upstream ships none for it (`001_libpq_pipeline.pl:39`-`:42`). What it
/// proves is that a pipeline can send while it receives: non-blocking mode,
/// `PQsocket`, and a `select()` on both directions, here
/// [`rlibpq::poll::socket_poll`], which is `poll(2)` as `PQsocketPoll` calls
/// it. A `select()` that fails ends the C test (`:1096`); here it panics.
#[test]
fn test_pipelined_insert() {
    const N_ROWS: usize = 700;
    let Some(cluster) = Cluster::start("trust", 55_475) else {
        return;
    };
    let mut conn = like_libpq_pipeline(&cluster);

    let insert_param_oids = [INT4OID, INT8OID];
    let mut send_step = PipelineInsertStep::BeginTx;
    let mut recv_step = PipelineInsertStep::BeginTx;
    let mut rows_to_send = N_ROWS;
    let mut rows_to_receive = N_ROWS;

    // :1024 — "Do a pipelined insert into a table created at the start of
    // the pipeline".
    if let Err(err) = conn.enter_pipeline_mode() {
        panic!("failed to enter pipeline mode: {err}");
    }

    // :1029
    while send_step != PipelineInsertStep::Prepare {
        let sql = match send_step {
            PipelineInsertStep::BeginTx => &b"BEGIN TRANSACTION"[..],
            PipelineInsertStep::DropTable => DROP_TABLE_SQL,
            PipelineInsertStep::CreateTable => CREATE_TABLE_SQL,
            _ => panic!("invalid state"),
        };
        send_step = send_step.next();
        if let Err(err) = conn.send_query_params(sql, &[], &Params::text(&[])) {
            panic!("dispatching {} failed: {err}", String::from_utf8_lossy(sql));
        }
    }

    // :1061
    assert_eq!(send_step, PipelineInsertStep::Prepare);
    if let Err(err) = conn.send_prepare(b"my_insert", INSERT_SQL2, &insert_param_oids) {
        panic!("dispatching PREPARE failed: {err}");
    }
    send_step = PipelineInsertStep::InsertRows;

    // :1068 — "We'll be sending enough data that we could fill our output
    // buffer, so to avoid deadlocking we need to enter nonblocking mode and
    // consume input while we send more output."
    if let Err(err) = conn.set_nonblocking(true) {
        panic!("failed to set nonblocking mode: {err}");
    }

    // :1078
    while recv_step != PipelineInsertStep::Done {
        // :1084 — PQsocket, then select() on input and output.
        let ready = rlibpq::poll::socket_poll(conn.socket(), true, true, None)
            .unwrap_or_else(|err| panic!("select() failed: {err}"));

        // :1101 — "Process any results, so we keep the server's output
        // buffer free flowing and it can continue to process input".
        if ready.read {
            // C ignores what PQconsumeInput returns.
            let _ = conn.consume_input();

            // :1108 — "Read until we'd block if we tried to read".
            while !conn.is_busy().expect("PQisBusy") && recv_step < PipelineInsertStep::Done {
                // :1117 — "If no more results from this query, advance to
                // the next query".
                let Some(res) = get(&mut conn) else {
                    continue;
                };

                let mut expected = ExecStatus::CommandOk;
                let (cmdtag, description) = match recv_step {
                    PipelineInsertStep::BeginTx => ("BEGIN", ""),
                    PipelineInsertStep::DropTable => ("DROP TABLE", ""),
                    PipelineInsertStep::CreateTable => ("CREATE TABLE", ""),
                    PipelineInsertStep::Prepare => ("", "PREPARE"),
                    PipelineInsertStep::InsertRows => ("INSERT", ""),
                    PipelineInsertStep::CommitTx => ("COMMIT", ""),
                    PipelineInsertStep::Sync => {
                        expected = ExecStatus::PipelineSync;
                        ("", "SYNC")
                    }
                    PipelineInsertStep::Done => panic!("unreachable state"),
                };
                if recv_step == PipelineInsertStep::InsertRows {
                    rows_to_receive -= 1;
                    if rows_to_receive == 0 {
                        recv_step = recv_step.next();
                    }
                } else {
                    recv_step = recv_step.next();
                }

                // :1165
                assert_eq!(
                    res.status(),
                    expected,
                    "{description} reported status {}, expected {}\nError message: \"{}\"",
                    res.status().as_str(),
                    expected.as_str(),
                    String::from_utf8_lossy(&res.error_message())
                );

                // :1171 — strncmp over the expected tag's length.
                assert!(
                    res.command_status().starts_with(cmdtag.as_bytes()),
                    "{description} expected command tag '{cmdtag}', got '{}'",
                    String::from_utf8_lossy(res.command_status())
                );
            }
        }

        // :1181 — "Write more rows and/or the end pipeline message, if
        // needed".
        if ready.write {
            // C ignores what PQflush returns; a flush that fails here has
            // broken the connection, which the next call would report.
            let _ = conn.flush();

            if send_step == PipelineInsertStep::InsertRows {
                let insert_param_0 = rows_to_send.to_string();
                // "use up some buffer space with a wide value"
                let insert_param_1 = (1_i64 << 62).to_string();
                let values = [
                    Some(insert_param_0.as_bytes()),
                    Some(insert_param_1.as_bytes()),
                ];
                match conn.send_query_prepared(b"my_insert", &Params::text(&values)) {
                    Ok(()) => {
                        rows_to_send -= 1;
                        if rows_to_send == 0 {
                            send_step = send_step.next();
                        }
                    }
                    // :1204 — "in nonblocking mode, so it's OK for an
                    // insert to fail to send".
                    Err(err) => eprintln!("WARNING: failed to send insert #{rows_to_send}: {err}"),
                }
            } else if send_step == PipelineInsertStep::CommitTx {
                match conn.send_query_params(b"COMMIT", &[], &Params::text(&[])) {
                    Ok(()) => send_step = send_step.next(),
                    Err(err) => eprintln!("WARNING: failed to send commit: {err}"),
                }
            } else if send_step == PipelineInsertStep::Sync {
                match conn.pipeline_sync() {
                    // :1229 — fprintf(stdout, "pipeline sync sent\n").
                    Ok(()) => send_step = send_step.next(),
                    Err(err) => eprintln!("WARNING: pipeline sync failed: {err}"),
                }
            }
        }
    }

    // :1241 — "We've got the sync message and the pipeline should be done".
    if let Err(err) = conn.exit_pipeline_mode() {
        panic!("attempt to exit pipeline mode failed when it should've succeeded: {err}");
    }

    if let Err(err) = conn.set_nonblocking(false) {
        panic!("failed to clear nonblocking mode: {err}");
    }
}

/// `test_prepared`, `libpq_pipeline.c:1253`.
#[test]
fn test_prepared() {
    let Some(cluster) = Cluster::start("trust", 55_464) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let expected_oids = [INT4OID, TEXTOID, NUMERICOID, INTERVALOID];

    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_prepare(
        b"select_one",
        b"SELECT $1, '42', $1::numeric, interval '1 sec'",
        &[INT4OID],
    )
    .expect("preparing query failed");
    conn.send_describe_prepared(b"select_one")
        .expect("failed to send describePrepared");
    conn.pipeline_sync().expect("pipeline sync failed");

    // :1277 — the Parse's result, its NULL, the Describe's result.
    let res = get(&mut conn).expect("PQgetResult returned null");
    assert_eq!(res.status(), ExecStatus::CommandOk, "expected COMMAND_OK");
    assert!(get(&mut conn).is_none(), "expected NULL result");
    let res = get(&mut conn).expect("PQgetResult returned NULL");
    assert_eq!(res.status(), ExecStatus::CommandOk, "expected COMMAND_OK");
    assert_eq!(
        res.nfields(),
        expected_oids.len(),
        "expected {} columns",
        expected_oids.len()
    );
    for (i, expected) in expected_oids.iter().enumerate() {
        assert_eq!(res.ftype(i), Some(*expected), "field {i}: expected type");
    }
    assert!(get(&mut conn).is_none(), "expected NULL result");
    let res = get(&mut conn);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::PipelineSync,
        "expected PGRES_PIPELINE_SYNC"
    );

    // :1311 — closing the statement.
    conn.send_close_prepared(b"select_one")
        .expect("PQsendClosePrepared failed");
    conn.pipeline_sync().expect("pipeline sync failed");
    let res = get(&mut conn).expect("expected non-NULL result");
    assert_eq!(res.status(), ExecStatus::CommandOk, "expected COMMAND_OK");
    assert!(get(&mut conn).is_none(), "expected NULL result");
    let res = get(&mut conn);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::PipelineSync,
        "expected PGRES_PIPELINE_SYNC"
    );
    conn.exit_pipeline_mode()
        .expect("could not exit pipeline mode");

    // :1333 — now that it's closed, describing it is an error; closing it
    // again is a no-op.
    let res = conn.describe_prepared(b"select_one").expect("runs").pop();
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::FatalError,
        "expected FATAL_ERROR"
    );
    let res = conn.close_prepared(b"select_one").expect("runs").pop();
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "expected COMMAND_OK"
    );

    // :1346 — a portal.
    exec(&mut conn, b"BEGIN");
    exec(&mut conn, b"DECLARE cursor_one CURSOR FOR SELECT 1");
    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_describe_portal(b"cursor_one")
        .expect("PQsendDescribePortal failed");
    conn.pipeline_sync().expect("pipeline sync failed");
    let res = get(&mut conn).expect("PQgetResult returned null");
    assert_eq!(res.status(), ExecStatus::CommandOk, "expected COMMAND_OK");
    assert_eq!(res.ftype(0), Some(INT4OID), "portal: expected type");
    assert!(get(&mut conn).is_none(), "expected NULL result");
    let res = get(&mut conn);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::PipelineSync,
        "expected PGRES_PIPELINE_SYNC"
    );

    // :1372 — closing the portal.
    conn.send_close_portal(b"cursor_one")
        .expect("PQsendClosePortal failed");
    conn.pipeline_sync().expect("pipeline sync failed");
    let res = get(&mut conn).expect("expected non-NULL result");
    assert_eq!(res.status(), ExecStatus::CommandOk, "expected COMMAND_OK");
    assert!(get(&mut conn).is_none(), "expected NULL result");
    let res = get(&mut conn);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::PipelineSync,
        "expected PGRES_PIPELINE_SYNC"
    );
    conn.exit_pipeline_mode()
        .expect("could not exit pipeline mode");

    // :1394 — describing the closed portal is an error; closing it is not.
    let res = conn.describe_portal(b"cursor_one").expect("runs").pop();
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::FatalError,
        "expected FATAL_ERROR"
    );
    let res = conn.close_portal(b"cursor_one").expect("runs").pop();
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "expected COMMAND_OK"
    );

    finish_and_compare_trace(conn, &sink, "prepared.trace");
}

/// `test_pipeline_idle`, `libpq_pipeline.c:1526`. The notice processor
/// that counts notices (`:1516`) is the connection's notice list.
#[test]
fn test_pipeline_idle() {
    let Some(cluster) = Cluster::start("trust", 55_465) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let notices_before = conn.notices().len();

    // :1535 — try to exit pipeline mode in pipeline-idle state.
    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_query_params(b"SELECT 1", &[], &Params::text(&[]))
        .expect("failed to send query");
    conn.send_flush_request().expect("PQsendFlushRequest");
    let res = get(&mut conn).expect("PQgetResult returned null when there's a pipeline item");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "unexpected result code from first pipeline item"
    );
    assert!(get(&mut conn).is_none(), "did not receive terminating NULL");
    conn.send_query_params(b"SELECT 2", &[], &Params::text(&[]))
        .expect("failed to send query");
    let error = conn
        .exit_pipeline_mode()
        .expect_err("exiting pipeline succeeded when it shouldn't");
    assert!(
        error_message(&error).starts_with("cannot exit pipeline mode"),
        "did not get expected error; got: {error}"
    );
    conn.send_flush_request().expect("PQsendFlushRequest");
    let res = get(&mut conn);
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::TuplesOk,
        "unexpected result code from second pipeline item"
    );
    assert!(get(&mut conn).is_none(), "did not receive terminating NULL");
    conn.exit_pipeline_mode().expect("exiting pipeline failed");

    let n_notices = conn.notices().len() - notices_before;
    assert_eq!(n_notices, 0, "got {n_notices} notice(s)");

    // :1576 — a WARNING in the middle of a result set.
    conn.enter_pipeline_mode()
        .expect("entering pipeline mode failed");
    conn.send_query_params(
        b"SELECT pg_catalog.pg_advisory_unlock(1,1)",
        &[],
        &Params::text(&[]),
    )
    .expect("failed to send query");
    conn.send_flush_request().expect("PQsendFlushRequest");
    let res = get(&mut conn).expect("unexpected NULL result received");
    assert_eq!(res.status(), ExecStatus::TuplesOk, "unexpected result code");
    conn.exit_pipeline_mode()
        .expect("failed to exit pipeline mode");
    let warning = conn.notices().last().expect("the WARNING was delivered");
    assert_eq!(warning.field(diag::SEVERITY), Some(&b"WARNING"[..]));

    finish_and_compare_trace(conn, &sink, "pipeline_idle.trace");
}

/// `test_simple_pipeline`, `libpq_pipeline.c:1593`.
#[test]
fn test_simple_pipeline() {
    let Some(cluster) = Cluster::start("trust", 55_466) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let dummy_params: [Option<&[u8]>; 1] = [Some(b"1")];

    // :1609
    assert!(!conn.is_nonblocking(), "Expected blocking connection mode");
    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_query_params(b"SELECT $1", &[INT4OID], &Params::text(&dummy_params))
        .expect("dispatching SELECT failed");
    assert!(
        conn.exit_pipeline_mode().is_err(),
        "exiting pipeline mode with work in progress should fail, but succeeded"
    );
    conn.pipeline_sync().expect("pipeline sync failed");

    let res = get(&mut conn).expect("PQgetResult returned null when there's a pipeline item");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "Unexpected result code from first pipeline item"
    );
    assert!(
        get(&mut conn).is_none(),
        "PQgetResult returned something extra after first query result."
    );

    // :1642 — a sync is still to come, so pipeline mode cannot end yet.
    assert!(
        conn.exit_pipeline_mode().is_err(),
        "exiting pipeline mode after query but before sync succeeded incorrectly"
    );
    let res = get(&mut conn)
        .expect("PQgetResult returned null when sync result PGRES_PIPELINE_SYNC expected");
    assert_eq!(
        res.status(),
        ExecStatus::PipelineSync,
        "Unexpected result code instead of PGRES_PIPELINE_SYNC"
    );
    assert!(
        get(&mut conn).is_none(),
        "PQgetResult returned something extra after pipeline end"
    );

    // :1664 — still in pipeline mode, until we end it.
    assert_ne!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Fell out of pipeline mode somehow"
    );
    conn.exit_pipeline_mode()
        .expect("attempt to exit pipeline mode failed when it should've succeeded");
    assert_eq!(
        conn.pipeline_status(),
        PipelineStatus::Off,
        "Exiting pipeline mode didn't seem to work"
    );

    finish_and_compare_trace(conn, &sink, "simple_pipeline.trace");
}

/// `test_singlerowmode`, `libpq_pipeline.c:1680`.
#[test]
fn test_singlerowmode() {
    let Some(cluster) = Cluster::start("trust", 55_468) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let no_params = Params::text(&[]);
    let mut pipeline_ended = false;

    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");

    // :1690 — one series of three commands, using single-row mode for the
    // first two.
    for i in 0..3 {
        let param = format!("{}", 44 + i);
        let values: [Option<&[u8]>; 1] = [Some(param.as_bytes())];
        conn.send_query_params(
            b"SELECT generate_series(42, $1)",
            &[],
            &Params::text(&values),
        )
        .expect("failed to send query");
    }
    conn.pipeline_sync().expect("pipeline sync failed");

    // :1712
    let mut i = 0;
    while !pipeline_ended {
        let mut first = true;
        let mut is_single_tuple = false;

        // Set single row mode for only first 2 SELECT queries.
        if i < 2 {
            assert!(
                conn.set_single_row_mode(),
                "PQsetSingleRowMode() failed for i={i}"
            );
        }

        // :1725 — consume rows for this query.
        let mut saw_ending_tuplesok = false;
        while let Some(res) = get(&mut conn) {
            let est = res.status();
            if est == ExecStatus::PipelineSync {
                eprintln!("end of pipeline reached");
                pipeline_ended = true;
                assert_eq!(i, 3, "Expected three results, got {i}");
                break;
            }

            // :1741 — expect SINGLE_TUPLE for queries 0 and 1, TUPLES_OK
            // for 2.
            if first {
                if i <= 1 {
                    assert_eq!(
                        est,
                        ExecStatus::SingleTuple,
                        "Expected PGRES_SINGLE_TUPLE for query {i}, got {}",
                        est.as_str()
                    );
                }
                if i >= 2 {
                    assert_eq!(
                        est,
                        ExecStatus::TuplesOk,
                        "Expected PGRES_TUPLES_OK for query {i}, got {}",
                        est.as_str()
                    );
                }
                first = false;
            }

            // :1753
            eprint!("Result status {} for query {i}", est.as_str());
            match est {
                ExecStatus::TuplesOk => {
                    eprintln!(", tuples: {}", res.ntuples());
                    saw_ending_tuplesok = true;
                    if is_single_tuple {
                        assert_eq!(
                            res.ntuples(),
                            0,
                            "Expected to follow PGRES_SINGLE_TUPLE, but received PGRES_TUPLES_OK directly instead"
                        );
                        eprintln!("all tuples received in query {i}");
                    }
                }
                ExecStatus::SingleTuple => {
                    is_single_tuple = true;
                    eprintln!(
                        ", {} tuple: {}",
                        res.ntuples(),
                        String::from_utf8_lossy(res.value(0, 0).unwrap_or_default())
                    );
                }
                _ => panic!("unexpected"),
            }
        }
        assert!(
            pipeline_ended || saw_ending_tuplesok,
            "didn't get expected terminating TUPLES_OK"
        );
        i += 1;
    }

    // :1782 — now issue one command, get its results in with single-row
    // mode, then issue another command, and get its results in normal mode;
    // make sure the single-row mode flag is reset as expected.
    conn.send_query_params(b"SELECT generate_series(0, 0)", &[], &no_params)
        .expect("failed to send query");
    conn.send_flush_request()
        .expect("failed to send flush request");
    assert!(conn.set_single_row_mode(), "PQsetSingleRowMode() failed");
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::SingleTuple,
        "Expected PGRES_SINGLE_TUPLE, got {}",
        res.status().as_str()
    );
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "Expected PGRES_TUPLES_OK, got {}",
        res.status().as_str()
    );
    assert!(get(&mut conn).is_none(), "expected NULL result");

    // :1810
    conn.send_query_params(b"SELECT 1", &[], &no_params)
        .expect("failed to send query");
    conn.send_flush_request()
        .expect("failed to send flush request");
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "Expected PGRES_TUPLES_OK, got {}",
        res.status().as_str()
    );
    assert!(get(&mut conn).is_none(), "expected NULL result");

    // :1825 — try chunked mode as well; make sure that it correctly
    // delivers a partial final chunk.
    conn.send_query_params(b"SELECT generate_series(1, 5)", &[], &no_params)
        .expect("failed to send query");
    conn.send_flush_request()
        .expect("failed to send flush request");
    assert!(
        conn.set_chunked_rows_mode(3),
        "PQsetChunkedRowsMode() failed"
    );
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesChunk,
        "Expected PGRES_TUPLES_CHUNK, got {}",
        res.status().as_str()
    );
    assert_eq!(res.ntuples(), 3, "Expected 3 rows, got {}", res.ntuples());
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesChunk,
        "Expected PGRES_TUPLES_CHUNK, got {}",
        res.status().as_str()
    );
    assert_eq!(res.ntuples(), 2, "Expected 2 rows, got {}", res.ntuples());
    let res = get(&mut conn).expect("unexpected NULL");
    assert_eq!(
        res.status(),
        ExecStatus::TuplesOk,
        "Expected PGRES_TUPLES_OK, got {}",
        res.status().as_str()
    );
    assert_eq!(res.ntuples(), 0, "Expected 0 rows, got {}", res.ntuples());
    assert!(get(&mut conn).is_none(), "expected NULL result");

    conn.exit_pipeline_mode()
        .expect("failed to end pipeline mode");

    eprintln!("ok");

    finish_and_compare_trace(conn, &sink, "singlerow.trace");
}

/// `test_transaction`, `libpq_pipeline.c:1876`.
#[test]
fn test_transaction() {
    let Some(cluster) = Cluster::start("trust", 55_467) else {
        return;
    };
    let (mut conn, sink) = traced_like_libpq_pipeline(&cluster);
    let no_params = Params::text(&[]);
    // :1911 — `PQsendQueryPrepared(conn, "rollback", 0, NULL, NULL, NULL, 1)`:
    // a binary result format.
    let rollback = Params {
        values: &[],
        formats: &[],
        result_format: Format::Binary,
    };

    let res = exec(
        &mut conn,
        b"DROP TABLE IF EXISTS pq_pipeline_tst;CREATE TABLE pq_pipeline_tst (id int)",
    );
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "failed to create test table"
    );

    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");
    conn.send_prepare(b"rollback", b"ROLLBACK", &[])
        .expect("could not send prepare on pipeline");
    conn.send_query_params(b"BEGIN", &[], &no_params)
        .expect("failed to send query");
    conn.send_query_params(b"SELECT 0/0", &[], &no_params)
        .expect("failed to send query");
    // :1911 — a ROLLBACK that cannot work: the pipeline is aborted first.
    conn.send_query_prepared(b"rollback", &rollback)
        .expect("failed to execute prepared");
    // :1915 — this insert fails because the pipeline is aborted.
    conn.send_query_params(b"INSERT INTO pq_pipeline_tst VALUES (1)", &[], &no_params)
        .expect("failed to send query");
    conn.pipeline_sync().expect("pipeline sync failed");
    let mut num_syncs = 1;

    // :1926 — fails even after the sync: the transaction is aborted.
    conn.send_query_params(b"INSERT INTO pq_pipeline_tst VALUES (2)", &[], &no_params)
        .expect("failed to send query");
    conn.pipeline_sync().expect("pipeline sync failed");
    num_syncs += 1;

    // :1939 — this ROLLBACK works, and so does the insert after it.
    conn.send_query_prepared(b"rollback", &rollback)
        .expect("failed to execute prepared");
    conn.send_query_params(b"INSERT INTO pq_pipeline_tst VALUES (3)", &[], &no_params)
        .expect("failed to send query");
    // :1955 — two syncs, to match the ReadyForQuery messages below.
    conn.pipeline_sync().expect("pipeline sync failed");
    num_syncs += 1;
    conn.pipeline_sync().expect("pipeline sync failed");
    num_syncs += 1;

    // :1964 — walk the results: every result but a sync is followed by a
    // NULL, and the walk ends at the last sync.
    let mut expect_null = false;
    loop {
        let Some(res) = get(&mut conn) else {
            assert!(expect_null, "did not expect NULL here");
            expect_null = false;
            continue;
        };
        assert!(!expect_null, "expected NULL");
        if res.status() == ExecStatus::PipelineSync {
            num_syncs -= 1;
        } else {
            expect_null = true;
        }
        if num_syncs <= 0 {
            break;
        }
    }
    let res = get(&mut conn);
    assert!(
        res.is_none(),
        "returned something extra after all the syncs: {res:?}"
    );
    conn.exit_pipeline_mode()
        .expect("failed to end pipeline mode");

    // :2006 — one tuple containing "3".
    let res = exec(&mut conn, b"SELECT * FROM pq_pipeline_tst");
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::TuplesOk,
        "failed to obtain result"
    );
    let res = res.expect("a result");
    assert_eq!(res.ntuples(), 1, "did not get 1 tuple");
    assert_eq!(
        res.value(0, 0),
        Some(&b"3"[..]),
        "did not get expected tuple"
    );

    finish_and_compare_trace(conn, &sink, "transaction.trace");
}

/// `test_uniqviol`, `libpq_pipeline.c:2024`: "we send a stream of queries,
/// with one in the middle causing an error. Verify that we can still send
/// some more after the error and have libpq work properly" (`:2019`).
///
/// It writes until a non-blocking `PQflush` answers `1` — the socket is
/// full — and only then reads, so it runs only if a flush can come back with
/// output still pending. No trace is compared: upstream ships none for it.
/// The C test's progress marks on stderr (`.`, `E`, `result …`) are left
/// out; they are not checked upstream either.
#[test]
fn test_uniqviol() {
    let Some(cluster) = Cluster::start("trust", 55_476) else {
        return;
    };
    let mut conn = like_libpq_pipeline(&cluster);

    let param_types = [INT8OID, INT8OID];
    let param_value_1 = b"42";
    let mut ctr: i64 = 0;
    let mut numsent: i64 = 0;
    let mut results: i64 = 0;
    let mut read_done = false;
    let mut write_done = false;
    let mut error_sent = false;
    let mut got_error = false;
    let mut switched = 0;
    let mut socketful: i64 = 0;

    // :2046 — C ignores what PQsetnonblocking returns.
    let _ = conn.set_nonblocking(true);

    // :2052
    let res = exec(
        &mut conn,
        b"drop table if exists ppln_uniqviol;create table ppln_uniqviol(id bigint primary key, idata bigint)",
    );
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "failed to create table: {res:?}"
    );

    let res = exec(&mut conn, b"begin");
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "failed to begin transaction: {res:?}"
    );

    let res = conn
        .prepare(
            b"insertion",
            b"insert into ppln_uniqviol values ($1, $2) returning id",
            &param_types,
        )
        .expect("PQprepare")
        .pop();
    assert_eq!(
        status(res.as_ref()),
        ExecStatus::CommandOk,
        "failed to prepare query: {res:?}"
    );

    conn.enter_pipeline_mode()
        .expect("failed to enter pipeline mode");

    // :2070
    while !read_done {
        // :2073 — "Avoid deadlocks by reading everything the server has sent
        // before sending anything. (Special precaution is needed here to
        // process PQisBusy before testing the socket for read-readiness,
        // because the socket does not turn read-ready after "sending"
        // queries in aborted pipeline mode.)"
        while !conn.is_busy().expect("PQisBusy") {
            if results >= numsent {
                if write_done {
                    read_done = true;
                }
                break;
            }

            let res = get(&mut conn);
            let new_error = process_result(&mut conn, res);
            assert!(!(new_error && got_error), "got two errors");
            got_error |= new_error;
            let last = results >= numsent - 1;
            results += 1;
            if last {
                if write_done {
                    read_done = true;
                }
                break;
            }
        }

        if read_done {
            break;
        }

        // :2112 — select() on input, and on output until done writing.
        let ready = match rlibpq::poll::socket_poll(conn.socket(), true, !write_done, None) {
            Ok(ready) => ready,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => panic!("select() failed: {err}"),
        };

        if ready.read
            && let Err(err) = conn.consume_input()
        {
            panic!("PQconsumeInput failed: {err}");
        }

        // :2123 — "If the socket is writable and we haven't finished
        // sending queries, send some."
        if !write_done && ready.write {
            loop {
                // :2133 — "provoke uniqueness violation exactly once after
                // having switched to read mode."
                let param_value_0 =
                    if switched >= 1 && !error_sent && ctr % socketful >= socketful / 2 {
                        error_sent = true;
                        (numsent / 2).to_string()
                    } else {
                        let value = ctr.to_string();
                        ctr += 1;
                        value
                    };

                let values = [Some(param_value_0.as_bytes()), Some(&param_value_1[..])];
                if let Err(err) = conn.send_query_prepared(b"insertion", &Params::text(&values)) {
                    panic!("failed to execute prepared query: {err}");
                }
                numsent += 1;

                // :2152 — "Are we done writing?"
                if socketful != 0 && numsent % socketful == 42 && error_sent {
                    conn.send_flush_request()
                        .expect("failed to send flush request");
                    write_done = true;
                    // C ignores what this PQflush returns.
                    let _ = conn.flush();
                    break;
                }

                // :2163 — "is the outgoing socket full?"
                let flush = conn
                    .flush()
                    .unwrap_or_else(|err| panic!("failed to flush: {err}"));
                if flush == Flush::Pending {
                    if socketful == 0 {
                        socketful = numsent;
                    }
                    switched += 1;
                    break;
                }
            }
        }
    }

    // :2179
    assert!(got_error, "did not get expected error");
}

/// `process_result`, `libpq_pipeline.c:2192`: check one result of
/// `test_uniqviol` and consume the NULL that must follow it; `true` for the
/// expected unique violation.
fn process_result(conn: &mut Connection, res: Option<QueryResult>) -> bool {
    let Some(res) = res else {
        panic!("got unexpected NULL");
    };

    let got_error = match res.status() {
        ExecStatus::FatalError => true,
        ExecStatus::TuplesOk | ExecStatus::PipelineAborted => false,
        other => panic!("got unexpected {}", other.as_str()),
    };
    if let Some(res2) = get(conn) {
        panic!("expected NULL, got {}", res2.status().as_str());
    }
    got_error
}

/// `test_cancel`, `libpq_pipeline.c:244`, up to `:290`: the blocking calls —
/// `PQcancel` twice with one `PGcancel`, `PQrequestCancel`, and
/// `PQcancelBlocking` — each against a running `pg_sleep`, each confirmed by
/// the query failing with 57014. From `:292` the C test polls with
/// `PQcancelStart`/`PQcancelPoll` and `select()`; those two are not ported
/// yet.
///
/// It runs under protocol 3.0, the one this crate speaks, which is
/// `001_libpq_pipeline.pl:78`'s "libpq_pipeline cancel with protocol 3.0".
#[test]
fn test_cancel_blocking() {
    let Some(cluster) = Cluster::start("trust", 55_469) else {
        return;
    };
    let mut conn = cluster.connect();

    // :253
    if let Err(err) = conn.set_nonblocking(true) {
        panic!("failed to set nonblocking mode: {err}");
    }

    // :260 — "a separate connection to the database to monitor the query".
    let mut monitor = cluster.connect();

    // :263 — test PQcancel.
    send_cancellable_query(&mut conn, &mut monitor);
    let cancel = conn.get_cancel().expect("PQgetCancel");
    if let Err(err) = cancel.cancel() {
        panic!("failed to run PQcancel: {err}");
    }
    confirm_query_canceled(&mut conn);

    // :270 — "PGcancel object can be reused for the next query".
    send_cancellable_query(&mut conn, &mut monitor);
    if let Err(err) = cancel.cancel() {
        panic!("failed to run PQcancel: {err}");
    }
    confirm_query_canceled(&mut conn);

    drop(cancel); // :276, PQfreeCancel

    // :278 — test PQrequestCancel.
    send_cancellable_query(&mut conn, &mut monitor);
    if let Err(err) = conn.request_cancel() {
        panic!("failed to run PQrequestCancel: {err}");
    }
    confirm_query_canceled(&mut conn);

    // :284 — test PQcancelBlocking.
    send_cancellable_query(&mut conn, &mut monitor);
    let mut cancel_conn = conn.cancel_create();
    assert!(
        cancel_conn.blocking(),
        "failed to run PQcancelBlocking: {}",
        String::from_utf8_lossy(cancel_conn.error_message())
    );
    confirm_query_canceled(&mut conn);
    drop(cancel_conn); // :290, PQcancelFinish
}
