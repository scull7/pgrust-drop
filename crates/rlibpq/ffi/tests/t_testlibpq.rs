//! `src/test/examples/testlibpq.c` (PostgreSQL REL_18_6), upstream's first
//! libpq example, over the C ABI; and `tests/c/exec.c`, which drives the
//! result accessors past what the example reaches.
//!
//! `tests/c/testlibpq.c` is upstream's file, unmodified (its sha256 is pinned
//! in `upstream_files.rs`); it is compiled against the vendored `libpq-fe.h`
//! and linked with this crate's `libpq.a`. Upstream builds it
//! (`src/test/examples/Makefile:17`) but checks no expected output, so the
//! port supplies one: the example prints `pg_database` through a cursor, and
//! the reference `psql` — C libpq — reads the same catalog, whose rows the
//! example's `%-15s` layout must reproduce byte for byte. The cluster runs
//! with `autovacuum = off`, so nothing rewrites `pg_database` between the two
//! reads.
//!
//! The live gates need the reference tools; without them they print `SKIP
//! (flagged, not silent)` and pass, and with `PGDROP_REQUIRE_REF=1` a missing
//! reference fails instead. The failed-connection case needs no server and
//! always runs.

#![allow(clippy::doc_markdown)]

mod common;
#[path = "../../tests/common/mod.rs"]
mod live;

use std::process::Command;

use common::{build, crate_dir, run};
use live::Cluster;

/// `printf("%-15s", text)`: the text, padded with spaces to 15 bytes.
fn left15(out: &mut Vec<u8>, text: &[u8]) {
    out.extend_from_slice(text);
    out.resize(out.len() + 15usize.saturating_sub(text.len()), b' ');
}

/// Calculation: what `testlibpq.c:104`-`:115` prints for a result whose
/// header and rows `psql -A -F '\x1f'` printed as `unaligned`.
fn testlibpq_layout(unaligned: &[u8]) -> Vec<u8> {
    let mut lines = unaligned
        .strip_suffix(b"\n")
        .unwrap_or(unaligned)
        .split(|&byte| byte == b'\n');
    let mut out = Vec::new();
    for name in lines.next().unwrap_or_default().split(|&byte| byte == 0x1f) {
        left15(&mut out, name);
    }
    out.extend_from_slice(b"\n\n");
    for row in lines {
        for value in row.split(|&byte| byte == 0x1f) {
            left15(&mut out, value);
        }
        out.push(b'\n');
    }
    out
}

#[test]
fn testlibpq_layout_pads_every_field_to_fifteen_bytes() {
    assert_eq!(
        testlibpq_layout(b"a\x1fbb\n1\x1f\n"),
        format!("{:<15}{:<15}\n\n{:<15}{:<15}\n", "a", "bb", "1", "").into_bytes()
    );
}

/// The whole of `testlibpq.c`'s `main`: connect, `set_config`, `BEGIN`,
/// `DECLARE myportal`, `FETCH ALL`, print, `CLOSE`, `END`, `PQfinish`.
#[test]
fn testlibpq_prints_pg_database_as_c_libpq_reads_it() {
    let Some(cluster) = Cluster::start_configured("trust", 55_490, "", "autovacuum = off\n") else {
        return;
    };
    let program = build("testlibpq", &[crate_dir().join("tests/c/testlibpq.c")], &[]);

    // The query `testlibpq.c:86` declares its cursor for, read by C libpq.
    let reference = Command::new(cluster.bin.join("psql"))
        .args(["-X", "-A", "-q", "-F", "\x1f", "-P", "footer=off"])
        .args(["-c", "select * from pg_database", "-d", &cluster.conninfo()])
        .env("LC_ALL", "C")
        .output()
        .expect("reference psql runs");
    assert!(reference.status.success(), "reference psql failed");

    let outcome = run(&program, &[&cluster.conninfo()]);

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&testlibpq_layout(&reference.stdout))
    );
    assert_eq!(outcome.status, Some(0));
}

/// `testlibpq.c:43`-`:47`: a failed connection prints `PQerrorMessage` and
/// exits 1 through `exit_nicely`, which still calls `PQfinish`. A conninfo
/// that does not parse fails before any socket is opened, with
/// `conninfo_parse`'s message (`fe-connect.c:6347`) — C's text exactly.
#[test]
fn testlibpq_reports_a_failed_connection() {
    let program = build(
        "testlibpq_failed",
        &[crate_dir().join("tests/c/testlibpq.c")],
        &[],
    );
    let outcome = run(&program, &["bogus"]);

    assert_eq!(
        String::from_utf8_lossy(&outcome.stderr),
        "missing \"=\" after \"bogus\" in connection info string\n"
    );
    assert_eq!(outcome.stdout, b"");
    assert_eq!(outcome.status, Some(1));
}

/// The divergence `docs/divergences.md` records: a socket that cannot be
/// reached leaves `rlibpq`'s one line of `std::io::Error` text in
/// `PQerrorMessage`, where C writes `connection to server on socket "…"
/// failed: No such file or directory` and a hint line (`emitHostIdentityInfo`,
/// `fe-connect.c:2405`; `connectFailureMessage`, `:2461`).
/// The path through `testlibpq.c:43`-`:47` is the same.
#[test]
fn testlibpq_reports_an_unreachable_socket_in_io_error_words() {
    let program = build(
        "testlibpq_unreachable",
        &[crate_dir().join("tests/c/testlibpq.c")],
        &[],
    );
    let outcome = run(&program, &["host=/nonexistent-rlibpq-ffi port=1"]);

    assert_eq!(
        String::from_utf8_lossy(&outcome.stderr),
        format!("{}\n", std::io::Error::from_raw_os_error(2)) // ENOENT
    );
    assert_eq!(outcome.stdout, b"");
    assert_eq!(outcome.status, Some(1));
}

/// `tests/c/exec.c` against a live server. Each expected line is what C's
/// accessor answers: `PQgetvalue` of a NULL field is `""` with `PQgetisnull`
/// 1 (`fe-exec.c:3907`-`:3942`); an out-of-range row or column is NULL, 0 or
/// "pretend it is null" with a notice naming the row before the column
/// (`check_tuple_field_number`, `:3556`), printed by the default notice
/// processor as the bare message and a newline (`pqInternalNotice`, `:979`);
/// `PQexec` returns the last result and `PQerrorMessage` holds every error on
/// the way (`PQexecFinish`, `:2427`); a server notice reaches stderr as
/// `PQresultErrorMessage` renders it; a COPY result's columns have no name
/// (`getCopyStart`, `fe-protocol3.c:1732`).
#[test]
fn pq_exec_results_answer_as_c_libpq_reads_them() {
    let Some(cluster) = Cluster::start("trust", 55_491) else {
        return;
    };
    let program = build(
        "exec",
        &[crate_dir().join("tests/c/exec.c")],
        &["-Wall", "-Werror"],
    );

    let outcome = run(&program, &[&cluster.conninfo()]);

    assert_eq!(
        String::from_utf8_lossy(&outcome.stderr),
        "row number 1 is out of range 0..0\n\
         column number 3 is out of range 0..2\n\
         column number -1 is out of range 0..2\n\
         row number -1 is out of range 0..0\n\
         column number 5 is out of range 0..2\n\
         NOTICE:  from the server\n"
    );
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        "status 0 errorMessage \"\"\n\
         -- select 1 as a, null::text as b, 'xyz'::text as c\n\
         status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" ntuples 1 nfields 3\n\
         fname 0 a\n\
         fname 1 b\n\
         fname 2 c\n\
         value 0 0 \"1\" length 1 isnull 0\n\
         value 0 1 \"\" length 0 isnull 1\n\
         value 0 2 \"xyz\" length 3 isnull 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         out of range: NULL NULL NULL 0 1\n\
         -- \n\
         status PGRES_EMPTY_QUERY cmdStatus \"\" ntuples 0 nfields 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         -- create temp table t (x int)\n\
         status PGRES_COMMAND_OK cmdStatus \"CREATE TABLE\" ntuples 0 nfields 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         -- insert into t values (1), (2)\n\
         status PGRES_COMMAND_OK cmdStatus \"INSERT 0 2\" ntuples 0 nfields 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         -- select 1/0\n\
         status PGRES_FATAL_ERROR cmdStatus \"\" ntuples 0 nfields 0\n\
         resultErrorMessage \"ERROR:  division by zero\n\"\n\
         errorMessage \"ERROR:  division by zero\n\"\n\
         -- select x from t order by x; select 'last'\n\
         status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" ntuples 1 nfields 1\n\
         fname 0 ?column?\n\
         value 0 0 \"last\" length 4 isnull 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         -- select 1; select 1/0; select 2\n\
         status PGRES_FATAL_ERROR cmdStatus \"\" ntuples 0 nfields 0\n\
         resultErrorMessage \"ERROR:  division by zero\n\"\n\
         errorMessage \"ERROR:  division by zero\n\"\n\
         -- do $$ begin raise notice 'from the server'; end $$\n\
         status PGRES_COMMAND_OK cmdStatus \"DO\" ntuples 0 nfields 0\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         -- copy (select 1, 2) to stdout\n\
         status PGRES_COPY_OUT cmdStatus \"\" ntuples 0 nfields 2\n\
         fname 0 NULL\n\
         fname 1 NULL\n\
         resultErrorMessage \"\"\n\
         errorMessage \"\"\n\
         status 0\n"
    );
    assert_eq!(outcome.status, Some(0));
}
