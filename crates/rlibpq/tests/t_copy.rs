//! COPY through this crate against a real PostgreSQL 18 server, compared
//! byte for byte with what C psql's `\copy` moves through C libpq.
//!
//! This is NAT-391's acceptance gate: "COPY of a 10 MB table round-trips
//! identical bytes vs `psql \copy` output". There is no upstream test to
//! steal for it — `src/interfaces/libpq/t/` has none that drives
//! `PQputCopyData` or `PQgetCopyData`, and `libpq_pipeline.c` has no COPY
//! test — so the cases are named for what they pin. The reference side is
//! psql's `\copy … to stdout` (`do_copy`, `copy.c:268`, which runs
//! `COPY … TO STDOUT` and hands the data to `handleCopyOut`, `copy.c:434`, a
//! `PQgetCopyData` loop), so both sides are one libpq each, reading the same
//! server.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use rlibpq::{Connection, CopyRead, ExecStatus, Params, QueryResult};

mod common;

use common::{Cluster, only};

/// Ten mebibytes: the size the acceptance names, as a floor.
const TEN_MB: usize = 10 * 1024 * 1024;

/// A table whose text COPY form is over 10 MB and exercises every escape
/// `CopyAttributeOutText` (`copyto.c:1147`) writes: tab, newline and backslash
/// inside a value, NULL as `\N`, and bytes outside ASCII.
const CREATE_COPY_GATE: &[u8] = b"CREATE TABLE copy_gate AS \
    SELECT g AS id, \
           CASE WHEN g % 97 = 0 THEN NULL \
                ELSE md5(g::text) || E'\\t' || E'\\\\' || E'\\n' || E'\\xc3\\xa9' \
                     || repeat(chr(65 + g % 26), g % 50) END AS payload, \
           g * 1.5 AS amount \
    FROM generate_series(1, 140000) AS g";

/// The same rows, in a fixed order, for either side to copy out.
const ORDERED: &str = "(SELECT * FROM copy_gate ORDER BY id)";

/// `PQexec` of a COPY: exactly the one COPY result `PQexecFinish` stops at.
fn start_copy(conn: &mut Connection, command: &[u8], status: ExecStatus) -> QueryResult {
    let result = only(conn.exec(command).expect("the COPY is sent"));
    assert_eq!(result.status(), status, "{result:?}");
    result
}

/// `handleCopyOut`'s loop (`copy.c:434`): every CopyData, concatenated,
/// then the command's result, then the NULL that ends it.
fn copy_out(conn: &mut Connection) -> (Vec<u8>, QueryResult) {
    let mut data = Vec::new();
    loop {
        match conn.get_copy_data(false).expect("PQgetCopyData") {
            CopyRead::Row(row) => data.extend_from_slice(&row),
            CopyRead::End => break,
            CopyRead::WouldBlock => unreachable!("a blocking read never says 0"),
        }
    }
    let result = conn.get_result().expect("PQgetResult").expect("a result");
    assert!(conn.get_result().expect("PQgetResult").is_none());
    (data, result)
}

/// `handleCopyIn` with data that is not cut at row boundaries — the server
/// reassembles the stream (`CopyGetData`, `copyfromparse.c:245`), so the
/// chunking is the client's business only.
fn copy_in(conn: &mut Connection, data: &[u8], chunk: usize) -> QueryResult {
    for piece in data.chunks(chunk) {
        conn.put_copy_data(piece).expect("PQputCopyData");
    }
    conn.put_copy_end(None).expect("PQputCopyEnd");
    let result = conn.get_result().expect("PQgetResult").expect("a result");
    assert!(conn.get_result().expect("PQgetResult").is_none());
    result
}

/// C psql's `\copy`, whose stdout is the data, byte for byte.
fn psql_copy_out(cluster: &Cluster, source: &str, options: &str) -> Vec<u8> {
    let (stdout, stderr, code) =
        cluster.psql(&format!("\\copy {source} to stdout {options}"), "default");
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert!(stderr.is_empty(), "{}", String::from_utf8_lossy(&stderr));
    stdout
}

/// The acceptance: a table whose text COPY is over 10 MB comes out of this
/// crate exactly as it comes out of C psql's `\copy`, goes back in through
/// `PQputCopyData`, and comes out of C psql again unchanged.
#[test]
fn a_ten_megabyte_table_round_trips_through_copy() {
    let Some(cluster) = Cluster::start("trust", 55_470) else {
        return;
    };
    let mut conn = cluster.connect();
    let created = only(conn.exec(CREATE_COPY_GATE).expect("runs"));
    assert_eq!(created.status(), ExecStatus::CommandOk, "{created:?}");

    let reference = psql_copy_out(&cluster, ORDERED, "");
    assert!(
        reference.len() >= TEN_MB,
        "the table is only {} bytes of COPY",
        reference.len()
    );

    let command = format!("COPY {ORDERED} TO STDOUT");
    let started = start_copy(&mut conn, command.as_bytes(), ExecStatus::CopyOut);
    assert_eq!(started.nfields(), 3);
    assert!(!started.binary_tuples());
    let (ours, done) = copy_out(&mut conn);
    assert_eq!(done.status(), ExecStatus::CommandOk);
    assert_eq!(done.command_status(), b"COPY 140000");
    assert!(
        ours == reference,
        "COPY TO STDOUT differs from psql's \\copy"
    );

    // Back in, cut into pieces that straddle rows.
    let created = only(
        conn.exec(b"CREATE TABLE copy_gate_in (LIKE copy_gate)")
            .expect("runs"),
    );
    assert_eq!(created.status(), ExecStatus::CommandOk);
    start_copy(
        &mut conn,
        b"COPY copy_gate_in FROM STDIN",
        ExecStatus::CopyIn,
    );
    let done = copy_in(&mut conn, &ours, 65_521);
    assert_eq!(done.status(), ExecStatus::CommandOk, "{done:?}");
    assert_eq!(done.command_status(), b"COPY 140000");

    let again = psql_copy_out(&cluster, "(SELECT * FROM copy_gate_in ORDER BY id)", "");
    assert!(again == reference, "the round trip changed the data");
}

/// Binary COPY both ways: the `PGCOPY` stream, header and trailer included,
/// is psql's byte for byte, and `PQbinaryTuples` says binary.
#[test]
fn a_binary_copy_round_trips_through_copy() {
    let Some(cluster) = Cluster::start("trust", 55_471) else {
        return;
    };
    let mut conn = cluster.connect();
    let created = only(conn.exec(CREATE_COPY_GATE).expect("runs"));
    assert_eq!(created.status(), ExecStatus::CommandOk);

    let reference = psql_copy_out(&cluster, ORDERED, "with (format binary)");
    let command = format!("COPY {ORDERED} TO STDOUT (FORMAT binary)");
    let started = start_copy(&mut conn, command.as_bytes(), ExecStatus::CopyOut);
    assert!(started.binary_tuples());
    assert_eq!(started.fformat(0), Some(1));
    let (ours, _) = copy_out(&mut conn);
    assert!(ours.starts_with(b"PGCOPY\n\xff\r\n\0"));
    assert!(ours == reference, "binary COPY differs from psql's \\copy");

    let created = only(
        conn.exec(b"CREATE TABLE copy_gate_in (LIKE copy_gate)")
            .expect("runs"),
    );
    assert_eq!(created.status(), ExecStatus::CommandOk);
    start_copy(
        &mut conn,
        b"COPY copy_gate_in FROM STDIN (FORMAT binary)",
        ExecStatus::CopyIn,
    );
    let done = copy_in(&mut conn, &ours, 8191);
    assert_eq!(done.command_status(), b"COPY 140000", "{done:?}");
    let again = psql_copy_out(
        &cluster,
        "(SELECT * FROM copy_gate_in ORDER BY id)",
        "with (format binary)",
    );
    assert!(again == reference, "the binary round trip changed the data");
}

/// A COPY IN the server refuses: the error is the COPY command's result,
/// rendered exactly as C psql renders the same failure of `\copy … from`.
#[test]
fn a_rejected_copy_in_is_the_error_psql_reports() {
    let Some(cluster) = Cluster::start("trust", 55_472) else {
        return;
    };
    let mut conn = cluster.connect();
    only(
        conn.exec(b"CREATE TABLE copy_bad (id int, t text)")
            .expect("runs"),
    );
    let data = b"1\tone\nx\ttwo\n3\tthree\n";

    let file = cluster.dir.join("copy_bad.data");
    std::fs::write(&file, data).expect("the data file is written");
    let (_, stderr, code) = cluster.psql(
        &format!("\\copy copy_bad from '{}'", file.display()),
        "default",
    );
    assert_eq!(code, 1);

    start_copy(&mut conn, b"COPY copy_bad FROM STDIN", ExecStatus::CopyIn);
    let failed = copy_in(&mut conn, data, 4);
    assert_eq!(failed.status(), ExecStatus::FatalError);
    assert_eq!(
        String::from_utf8_lossy(&failed.error_message()),
        String::from_utf8_lossy(&stderr)
    );

    // The connection is idle and usable again.
    let count = only(conn.exec(b"SELECT count(*) FROM copy_bad").expect("runs"));
    assert_eq!(count.value(0, 0), Some(&b"0"[..]));
}

/// `PQputCopyEnd` with an error message sends CopyFail, which the server
/// answers with `57014` ("COPY from stdin failed: %s", `copyfromparse.c:317`);
/// through the extended protocol it needs the Sync `PQputCopyEnd` adds
/// (`fe-exec.c:2801`), without which the server would never answer.
#[test]
fn a_failed_copy_in_is_query_canceled() {
    let Some(cluster) = Cluster::start("trust", 55_473) else {
        return;
    };
    let mut conn = cluster.connect();
    only(conn.exec(b"CREATE TABLE copy_fail (id int)").expect("runs"));

    start_copy(&mut conn, b"COPY copy_fail FROM STDIN", ExecStatus::CopyIn);
    conn.put_copy_data(b"1\n").expect("PQputCopyData");
    conn.put_copy_end(Some(b"stopped by the client"))
        .expect("PQputCopyEnd");
    let failed = conn.get_result().expect("PQgetResult").expect("a result");
    assert_eq!(failed.status(), ExecStatus::FatalError);
    assert_eq!(
        failed.error().and_then(rlibpq::ResultError::sqlstate),
        Some(&b"57014"[..])
    );
    assert_eq!(
        String::from_utf8_lossy(&failed.error_message()),
        "ERROR:  COPY from stdin failed: stopped by the client\n\
         CONTEXT:  COPY copy_fail, line 2\n"
    );
    assert!(conn.get_result().expect("PQgetResult").is_none());

    let results = conn
        .exec_params(b"COPY copy_fail FROM STDIN", &[], &Params::default())
        .expect("the COPY is sent");
    assert_eq!(only(results).status(), ExecStatus::CopyIn);
    let done = copy_in(&mut conn, b"7\n8\n", 1);
    assert_eq!(done.command_status(), b"COPY 2", "{done:?}");
    let count = only(conn.exec(b"SELECT sum(id) FROM copy_fail").expect("runs"));
    assert_eq!(count.value(0, 0), Some(&b"15"[..]));
}

/// `PQexecStart` (`fe-exec.c:2391`-`:2412`) ends a COPY the caller walked
/// away from: COPY OUT by dropping the rest of its data, COPY IN with a
/// CopyFail. Either way the next `PQexec` just runs.
#[test]
fn a_new_exec_ends_an_unfinished_copy() {
    let Some(cluster) = Cluster::start("trust", 55_474) else {
        return;
    };
    let mut conn = cluster.connect();
    start_copy(
        &mut conn,
        b"COPY (SELECT g FROM generate_series(1, 100000) g) TO STDOUT",
        ExecStatus::CopyOut,
    );
    assert_eq!(
        conn.get_copy_data(false).expect("PQgetCopyData"),
        CopyRead::Row(b"1\n".to_vec())
    );
    let one = only(conn.exec(b"SELECT 1").expect("runs"));
    assert_eq!(one.value(0, 0), Some(&b"1"[..]));

    only(conn.exec(b"CREATE TABLE copy_open (id int)").expect("runs"));
    start_copy(&mut conn, b"COPY copy_open FROM STDIN", ExecStatus::CopyIn);
    conn.put_copy_data(b"1\n").expect("PQputCopyData");
    let count = only(conn.exec(b"SELECT count(*) FROM copy_open").expect("runs"));
    assert_eq!(count.value(0, 0), Some(&b"0"[..]), "the COPY was failed");
}
