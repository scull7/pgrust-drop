//! Port of `src/bin/psql/t/001_basic.pl` (PostgreSQL 18.6), in upstream order.
//!
//! Only the server-free assertions of the first block exist so far; the
//! `\copyright`, `\help` and `\echo :ENCODING` cases need a running cluster,
//! which this environment has no PostgreSQL 18 to start, and the rest of the
//! file lands with Linear NAT-399 … NAT-405.
//!
//! The byte-diff gate this issue's Acceptance names —
//! `psql -X -c 'select 1'` through C psql and through rpsql — needs both the
//! reference binary and a server, so it is declared here and prints
//! `SKIP (flagged, not silent)` when either is missing. It is never narrowed
//! to something that can pass without them.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::Path;

use testkit::{Gate, reference};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// `program_help_ok('psql');`
///
/// `usage()` must be byte-identical to upstream, which is NAT-399's issue; the
/// stolen assertion is declared here, ignored with its reason, so the file
/// keeps upstream's order and the gap is visible rather than absent.
#[test]
#[ignore = "psql --help is byte-identical work tracked in Linear NAT-399"]
fn program_help_ok() {
    testkit::program_help_ok(Path::new(RPSQL));
}

/// `program_version_ok('psql');`
#[test]
fn program_version_ok() {
    testkit::program_version_ok(Path::new(RPSQL));
}

/// `program_options_handling_ok('psql');`
///
/// Only a nonzero exit and a non-empty stderr are required, which is what lets
/// usage-rs's clap-shaped parse errors stand in for glibc getopt's (ADR-0004).
#[test]
fn program_options_handling_ok() {
    testkit::program_options_handling_ok(Path::new(RPSQL));
}

/// The Acceptance gate: `psql -X -c 'select 1'` byte for byte against C psql,
/// on stdout, stderr and the exit status.
///
/// Both sides need a server to connect to. Without one, C psql fails to
/// connect and so does rpsql, and a gate over two connection failures would
/// prove nothing about `select 1` — so the gate runs only when a reference
/// psql *and* a cluster to point it at are both present, and flags the skip
/// otherwise.
#[test]
fn select_one_matches_c_psql() {
    let Some(gate) = Gate::for_tool("psql", RPSQL) else {
        reference::skip("psql");
        return;
    };
    if std::env::var_os("PGDROP_TEST_CLUSTER").is_none() {
        reference::announce_skip(&format!(
            "{}: no PostgreSQL 18 cluster to run `psql -X -c 'select 1'` against; \
             set PGDROP_TEST_CLUSTER to a connectable cluster's PGHOST",
            reference::SKIP_FLAG
        ));
        return;
    }
    let argv: Vec<OsString> = ["-X", "-c", "select 1"]
        .iter()
        .map(OsString::from)
        .collect();
    gate.with_args(argv).assert_clean();
}

/// The same gate over a small corpus of SQL shapes: DDL, DML, a SELECT and an
/// error with its caret.
///
/// The corpus is written here (AGENTS.md forbids copying pgrust's), one
/// statement per `-c`, and is gated whole so a difference in any one shape
/// fails the test.
#[test]
fn a_small_sql_corpus_matches_c_psql() {
    let Some(gate) = Gate::for_tool("psql", RPSQL) else {
        reference::skip("psql");
        return;
    };
    if std::env::var_os("PGDROP_TEST_CLUSTER").is_none() {
        reference::announce_skip(&format!(
            "{}: no PostgreSQL 18 cluster to run the SQL corpus against; \
             set PGDROP_TEST_CLUSTER to a connectable cluster's PGHOST",
            reference::SKIP_FLAG
        ));
        return;
    }
    let corpus = [
        "create table t (n int, s text)",
        "insert into t values (1, 'a'), (2, 'b')",
        "select n, s from t order by n",
        "update t set s = 'c' where n = 1",
        "delete from t where n = 2",
        "begin",
        "select count(*) from t",
        "commit",
        "selec 1",
        "drop table t",
    ];
    let mut argv: Vec<OsString> = vec![OsString::from("-X")];
    for statement in corpus {
        argv.push(OsString::from("-c"));
        argv.push(OsString::from(statement));
    }
    gate.with_args(argv).assert_clean();
}

/// `psql --version` through both binaries, which needs no server at all.
#[test]
fn version_matches_c_psql() {
    let Some(gate) = Gate::for_tool("psql", RPSQL) else {
        reference::skip("psql");
        return;
    };
    gate.arg("--version").assert_clean();
}

/// Without a server, `-c` still reports a connection failure and exits 2
/// (`EXIT_BADCONN`, `settings.h:199`) rather than succeeding or hanging.
#[test]
fn a_connection_failure_exits_badconn() {
    let outcome = testkit::run(
        Path::new(RPSQL),
        [
            OsString::from("-X"),
            OsString::from("-h"),
            // A path that cannot hold a socket, so the failure is immediate
            // and does not depend on the network.
            OsString::from("/nonexistent-pgdrop-socket-dir"),
            OsString::from("-c"),
            OsString::from("select 1"),
        ],
    )
    .expect("spawn rpsql");
    assert_eq!(
        outcome.status,
        Some(i32::from(rpsql::settings::EXIT_BADCONN))
    );
    assert!(!outcome.stderr.is_empty(), "a failure must say why");
    assert!(outcome.stdout.is_empty(), "nothing is printed on stdout");
}
