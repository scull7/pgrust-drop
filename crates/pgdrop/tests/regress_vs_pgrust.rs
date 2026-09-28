//! NAT-404's acceptance, against pgrust: `src/test/regress/sql/psql_crosstab.sql`
//! and `psql_pipeline.sql` (PostgreSQL 18.6) run through rpsql and through
//! C psql against a server `pgdrop start` brought up, and compared byte for
//! byte with `expected/psql_crosstab.out` and `expected/psql_pipeline.out`.
//! The runner is `pgrust_regress` ([`pgrust_regress::gate`]); see it for the
//! two lists each gate names.
//!
//! The server is pgrust, in this binary, so the rpsql half always runs. The
//! C psql half needs the reference `psql`; without it it prints `SKIP
//! (flagged, not silent)`, and `PGDROP_REQUIRE_REF=1` (every CI lane) makes
//! it fail instead.
//!
//! Neither script prints anything else on pgrust at the pinned revision, so
//! neither gate names a pgrust-side divergence; `docs/divergences.md`
//! ("pgrust-side divergences") is where one would be recorded.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod pgrust_regress;

use pgrust_regress::regress::{
    PSQL_CROSSTAB_OUT, PSQL_CROSSTAB_SQL, PSQL_PIPELINE_OUT, PSQL_PIPELINE_SQL, Section, split,
};
use pgrust_regress::{Named, Server, gate};

/// The sections of `psql_pipeline.sql` rpsql cannot run yet, by header,
/// each with the command it needs and the issue that owns it: the
/// `NOT_YET` of `crates/rpsql/tests/t_regress_psql_pipeline.rs`, which gates
/// the rest against stock PostgreSQL. C psql still runs them against pgrust.
const PIPELINE_NOT_YET: [Named<'static>; 9] = [
    (
        "-- Use pipeline with chunked results for both \\getresults and \\endpipeline.",
        "FETCH_COUNT's chunked fetch (NAT-403)",
    ),
    (
        "-- \\watch is not allowed in a pipeline.",
        "\\watch (NAT-403)",
    ),
    (
        "-- \\gdesc should fail as synchronous commands are not allowed in a pipeline,",
        "\\gdesc (NAT-402)",
    ),
    (
        "-- \\gset is not allowed in a pipeline, pipeline should still be usable.",
        "\\gset (NAT-402)",
    ),
    (
        "-- \\g and \\gx are not allowed, pipeline should still be usable.",
        "\\reset (NAT-403)",
    ),
    (
        "-- \\sendpipeline is not allowed outside of a pipeline",
        "\\reset (NAT-403)",
    ),
    (
        "-- \\gexec is not allowed, pipeline should still be usable.",
        "\\gexec (NAT-402)",
    ),
    (
        "-- Test chunked results with an aborted pipeline.",
        "FETCH_COUNT's chunked fetch (NAT-403)",
    ),
    (
        "-- Error messages accumulate and are repeated.",
        "\\gdesc (NAT-402)",
    ),
];

/// Where pgrust prints something else than PostgreSQL 18.6 in
/// `psql_pipeline.sql`: nowhere, at the pinned revision.
const PIPELINE_PGRUST: [Named<'static>; 0] = [];

/// `psql_crosstab.sql`, whole, as `crates/rpsql/tests/t_regress_psql_crosstab.rs`
/// gates it against stock PostgreSQL: every block needs the table the first
/// one creates.
#[test]
fn psql_crosstab() {
    let server = Server::start("crosstab");
    let whole = Section {
        header: "--",
        sql: PSQL_CROSSTAB_SQL,
        expected: PSQL_CROSSTAB_OUT,
        sql_line: 1,
        out_line: 1,
    };
    gate(&server, &[whole], &[], &[]);
}

/// `psql_pipeline.sql`, section by section in order (`regress::split`).
#[test]
fn psql_pipeline() {
    let server = Server::start("pipeline");
    let sections = split(PSQL_PIPELINE_SQL, PSQL_PIPELINE_OUT).expect("the vendored files split");
    assert_eq!(sections.len(), 58);
    gate(&server, &sections, &PIPELINE_NOT_YET, &PIPELINE_PGRUST);
}
