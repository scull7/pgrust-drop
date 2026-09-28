//! `src/test/examples/testlibpq4.c` (PostgreSQL REL_18_6), upstream's
//! two-connection libpq example, over the C ABI; and `tests/c/connect.c`,
//! which opens connections every other way the C ABI exports and reads the
//! `PGconn` accessors on each.
//!
//! `tests/c/testlibpq4.c` is upstream's file, unmodified (its sha256 is
//! pinned in `upstream_files.rs`). It connects twice through `PQsetdb`, the
//! `libpq-fe.h:339` macro over `PQsetdbLogin`, with every argument but the
//! database NULL, so the host, port and user come from `PGHOST`, `PGPORT`
//! and `PGUSER`. Upstream builds it (`src/test/examples/Makefile:17`) but
//! checks no expected output, so the port supplies one, as for
//! `testlibpq.c`: the first connection prints `pg_database` through a
//! cursor, in the layout `testlibpq4.c:134`-`:145` gives it, and the
//! reference `psql` — C libpq — reads the same catalog.
//!
//! The live gates need the reference tools; without them they print `SKIP
//! (flagged, not silent)` and pass, and with `PGDROP_REQUIRE_REF=1` a missing
//! reference fails instead. The failed-connection case needs no server and
//! always runs.

#![allow(clippy::doc_markdown)]

mod common;
#[path = "../../tests/common/mod.rs"]
mod live;

use std::path::Path;
use std::process::Command;

use common::{build, crate_dir};
use live::Cluster;
use testkit::{CommandOutcome, Environment, run_in};

/// Action: run `program` in the scrubbed TAP environment plus `env`, so no
/// `PG*` variable of the caller's reaches the defaults.
fn run_scrubbed(program: &Path, args: &[&str], env: &[(&str, &str)]) -> CommandOutcome {
    let mut environment = Environment::postgres_test("t_testlibpq4");
    for (key, value) in env {
        environment = environment.with(*key, *value);
    }
    run_in(program, args, &[], &environment).expect("the program runs")
}

/// `printf("%-15s", text)`: the text, padded with spaces to 15 bytes.
fn left15(out: &mut Vec<u8>, text: &[u8]) {
    out.extend_from_slice(text);
    out.resize(out.len() + 15usize.saturating_sub(text.len()), b' ');
}

/// Calculation: what `testlibpq4.c:134`-`:145` prints for a result whose
/// header and rows `psql -A -F '\x1f'` printed as `unaligned` — the layout
/// of `testlibpq.c` too.
fn testlibpq4_layout(unaligned: &[u8]) -> Vec<u8> {
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
fn testlibpq4_layout_pads_every_field_to_fifteen_bytes() {
    assert_eq!(
        testlibpq4_layout(b"a\x1fbb\n1\x1f\n"),
        format!("{:<15}{:<15}\n\n{:<15}{:<15}\n", "a", "bb", "1", "").into_bytes()
    );
}

/// The whole of `testlibpq4.c`'s `main`, run as `testlibpq4 t postgres
/// template1`: two `PQsetdb` connections, each `check_prepare_conn`'d, then
/// `BEGIN`, `DECLARE myportal`, `FETCH ALL`, print, `CLOSE`, `END` on the
/// first, and `PQfinish` on both.
#[test]
fn testlibpq4_prints_pg_database_as_c_libpq_reads_it() {
    let Some(cluster) = Cluster::start_configured("trust", 55_494, "", "autovacuum = off\n") else {
        return;
    };
    let program = build(
        "testlibpq4",
        &[crate_dir().join("tests/c/testlibpq4.c")],
        &[],
    );

    // The query `testlibpq4.c:116` declares its cursor for, read by C libpq.
    let reference = Command::new(cluster.bin.join("psql"))
        .args(["-X", "-A", "-q", "-F", "\x1f", "-P", "footer=off"])
        .args(["-c", "select * from pg_database", "-d", &cluster.conninfo()])
        .env("LC_ALL", "C")
        .output()
        .expect("reference psql runs");
    assert!(reference.status.success(), "reference psql failed");

    let host = cluster.dir.display().to_string();
    let port = cluster.port.to_string();
    let outcome = run_scrubbed(
        &program,
        &["t", "postgres", "template1"],
        &[("PGHOST", &host), ("PGPORT", &port), ("PGUSER", "gateuser")],
    );

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        String::from_utf8_lossy(&testlibpq4_layout(&reference.stdout))
    );
    assert_eq!(outcome.status, Some(0));
}

/// `testlibpq4.c:30`-`:34`: a failed connection prints `PQerrorMessage` and
/// exits 1. A `dbName` that looks like a connection string is parsed as one
/// (`PQsetdbLogin`, `fe-connect.c:2250`), so an unknown keyword in it fails
/// before any socket is opened, with `conninfo_parse`'s message — C's text
/// exactly.
#[test]
fn testlibpq4_reports_a_failed_connection() {
    let program = build(
        "testlibpq4_failed",
        &[crate_dir().join("tests/c/testlibpq4.c")],
        &[],
    );
    let outcome = run_scrubbed(&program, &["t", "bogus=1", "template1"], &[]);

    assert_eq!(
        String::from_utf8_lossy(&outcome.stderr),
        "invalid connection option \"bogus\"\n"
    );
    assert_eq!(outcome.stdout, b"");
    assert_eq!(outcome.status, Some(1));
}

/// Calculation: what `tests/c/connect.c` prints for one connection that is
/// up, as `print_conn` lays it out.
fn up(label: &str, db: &str, port: u16) -> String {
    format!(
        "-- {label}\n\
         PQstatus 0 PQerrorMessage \"\"\n\
         PQdb {db} PQuser gateuser PQpass \"\" PQoptions \"\" PQtty \"\"\n\
         PQhost <host> PQport {port}\n\
         PQsocket open PQtransactionStatus 0\n\
         PQbackendPID pg_backend_pid()\n\
         PQserverVersion server_version_num\n\
         PQparameterStatus server_encoding SQL_ASCII DateStyle ISO, MDY nosuch (null)\n"
    )
}

/// `tests/c/connect.c` against a live server. Each line is what C libpq 18.6
/// prints for the same program against the same server (checked against
/// PGDG's `libpq.so.5`): the option fields `fillPGconn` and
/// `pqConnectOptions2` fill, `PQdb` defaulting to nothing but what was
/// asked, the password file defaulting to `$HOME/.pgpass`
/// (`fe-connect.c:1426`-`:1441`); `PQparameterStatus` following a `SET`;
/// `PQtransactionStatus` through a transaction, an error in it, and a COPY
/// in progress (`PQTRANS_ACTIVE`, `:7583`); `dbname` expanded in place
/// (`conninfo_array_parse`, `:6541`); a connection the server terminated,
/// `CONNECTION_BAD` but still holding what the server reported until
/// `PQreset` (`pqClosePGconn`, `:5254`); `PQresetStart` and `PQconnectStart`
/// through the `PQconnectPoll` loop `libpq.sgml` gives; and a server that is
/// not there, `CONNECTION_BAD` from the start with `PQhost` and `PQport`
/// still naming it.
#[test]
fn pq_connect_calls_and_accessors_answer_as_c_libpq() {
    let Some(cluster) = Cluster::start("trust", 55_495) else {
        return;
    };
    let program = build(
        "connect",
        &[crate_dir().join("tests/c/connect.c")],
        &["-Wall", "-Werror"],
    );

    let host = cluster.dir.display().to_string();
    let port = cluster.port;
    let outcome = run_scrubbed(
        &program,
        &[&host, &port.to_string()],
        &[("HOME", "/nonexistent-home")],
    );

    let expected = [
        up("PQconnectdbParams", "postgres", port),
        format!(
            "PQconninfo user=gateuser\n\
             PQconninfo password=(null)\n\
             PQconninfo passfile=/nonexistent-home/.pgpass\n\
             PQconninfo dbname=postgres\n\
             PQconninfo host=<host>\n\
             PQconninfo hostaddr=(null)\n\
             PQconninfo port={port}\n\
             PQconninfo application_name=connect_c\n\
             PQparameterStatus application_name connect_c\n\
             set application_name = renamed: PQtransactionStatus 0\n\
             PQparameterStatus application_name renamed\n\
             begin: PQtransactionStatus 2\n\
             select 1/0: PQtransactionStatus 3\n\
             rollback: PQtransactionStatus 0\n\
             copy (select 1) to stdout: PQtransactionStatus 1\n"
        ),
        up("PQconnectdbParams expand_dbname", "template1", port),
        up("PQsetdbLogin", "template1", port),
        "-- terminated\n\
         PQstatus 1 PQsocket -1 PQtransactionStatus 4 PQbackendPID 0 PQserverVersion 0\n\
         PQparameterStatus server_encoding SQL_ASCII PQdb template1\n"
            .to_owned(),
        up("PQreset", "template1", port),
        "PQresetStart 1\n\
         PQresetPoll PGRES_POLLING_OK\n\
         new backend yes\n"
            .to_owned(),
        up("PQresetStart", "template1", port),
        "PQconnectPoll PGRES_POLLING_OK\n".to_owned(),
        up("PQconnectStart", "postgres", port),
        "-- PQconnectStartParams, no server\n\
         PQstatus 1 PQconnectPoll PGRES_POLLING_FAILED PQhost <host> PQport 1 PQsocket -1\n"
            .to_owned(),
    ]
    .concat();

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(String::from_utf8_lossy(&outcome.stdout), expected);
    assert_eq!(outcome.status, Some(0));
}
