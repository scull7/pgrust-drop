//! Port of `src/test/regress/sql/psql_pipeline.sql` (PostgreSQL 18.6),
//! NAT-404's third slice: `\startpipeline`, `\sendpipeline`,
//! `\syncpipeline`, `\flush`, `\flushrequest`, `\getresults` and
//! `\endpipeline`.
//!
//! The script is cut into the sections its own comment headers mark
//! (`regress::split`), and every section is run in order, each through
//! `rpsql -X -a -q` against one PostgreSQL 18 cluster, and compared byte for
//! byte with its slice of `expected/psql_pipeline.out`; then all of them
//! again through C psql, compared with rpsql's. The sections share only the database — the table the first one
//! creates and the rows two of them commit — so running each in its own
//! session, in order, is the whole script's run with a precise diff. The
//! cluster needs the reference `initdb` and `pg_ctl`; without them the gate
//! prints `SKIP (flagged, not silent)`.
//!
//! [`NOT_YET`] names the sections that need a command another issue owns,
//! and why. They are skipped by name, never by pattern, and
//! [`every_section_is_gated_or_named_as_owned_elsewhere`] holds the list to
//! the file, so the gate cannot quietly narrow.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::path::Path;

use regress::{Cluster, PSQL_PIPELINE_OUT, PSQL_PIPELINE_SQL, first_difference, sha256_hex, split};
use testkit::reference;

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// The port the live gate's cluster listens on.
const PSQL_PIPELINE_PORT: u16 = 55_496;

/// The sections of `psql_pipeline.sql` not gated yet, by header, each with
/// the command it needs and the issue that owns it.
const NOT_YET: [(&str, &str); 9] = [
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

#[test]
fn the_vendored_files_are_the_ones_postgresql_18_6_ships() {
    // ADR-0008: vendored bytes come from the tag or the tarball.
    assert_eq!(
        sha256_hex(PSQL_PIPELINE_SQL.as_bytes()),
        regress::PSQL_PIPELINE_SQL_SHA256,
        "crates/rpsql/tests/regress/psql_pipeline.sql is not REL_18_6's"
    );
    assert_eq!(
        sha256_hex(PSQL_PIPELINE_OUT.as_bytes()),
        regress::PSQL_PIPELINE_OUT_SHA256,
        "crates/rpsql/tests/regress/expected/psql_pipeline.out is not REL_18_6's"
    );
}

#[test]
fn every_section_is_gated_or_named_as_owned_elsewhere() {
    let sections = split(PSQL_PIPELINE_SQL, PSQL_PIPELINE_OUT).expect("the vendored files split");
    let sql: String = sections.iter().map(|s| s.sql).collect();
    let out: String = sections.iter().map(|s| s.expected).collect();
    assert_eq!(sql, PSQL_PIPELINE_SQL, "the sections must tile the script");
    assert_eq!(out, PSQL_PIPELINE_OUT, "the slices must tile the output");
    assert_eq!(sections.len(), 58);
    // Each name in NOT_YET heads exactly one section: a typo cannot skip
    // nothing, and a repeated header cannot skip two.
    for (header, _) in NOT_YET {
        assert_eq!(
            sections.iter().filter(|s| s.header == header).count(),
            1,
            "{header:?}"
        );
    }
    let gated = sections
        .iter()
        .filter(|s| !NOT_YET.iter().any(|(h, _)| *h == s.header))
        .count();
    assert_eq!(gated, 49);
}

/// `psql_pipeline.sql`, section by section in order, through rpsql and C
/// psql against a server — every section but [`NOT_YET`]'s.
///
/// Two passes, not one: a section run through rpsql and then again through
/// C psql would create its table twice. The script ends by dropping what it
/// created, so C psql's pass starts from the database rpsql's started from.
#[test]
fn psql_pipeline() {
    let Some(cluster) = Cluster::start(PSQL_PIPELINE_PORT) else {
        return;
    };
    let sections = split(PSQL_PIPELINE_SQL, PSQL_PIPELINE_OUT).expect("the vendored files split");
    let gated: Vec<_> = sections
        .iter()
        .filter(|s| !NOT_YET.iter().any(|(h, _)| *h == s.header))
        .collect();

    let ours: Vec<Vec<u8>> = gated
        .iter()
        .map(|section| {
            let output = cluster.run_script(Path::new(RPSQL), section.sql);
            if let Some(diff) = first_difference(section.expected.as_bytes(), &output) {
                panic!(
                    "rpsql, sql:{} vs out:{} ({}): {diff}",
                    section.sql_line, section.out_line, section.header
                );
            }
            output
        })
        .collect();

    let Some(psql) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    for (section, ours) in gated.iter().zip(&ours) {
        let theirs = cluster.run_script(&psql, section.sql);
        if let Some(diff) = first_difference(&theirs, ours) {
            panic!("rpsql vs C psql ({}): {diff}", section.header);
        }
    }
}
