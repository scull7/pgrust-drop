//! Port of `src/interfaces/libpq/t/001_uri.pl` (PostgreSQL 18.6).
//!
//! Upstream runs the C helper `libpq_uri_regress` once per row of `@tests`
//! (`001_uri.pl:14`) and compares its stdout, its stderr and whether it
//! succeeded against the row. This runs *our* `libpq_uri_regress` the same way,
//! from the same table, in the environment `PostgreSQL::Test::Utils` scrubs
//! (`testkit::Environment::postgres_test`) — the expectations were written
//! against that environment and mean nothing outside it.
//!
//! `libpq_uri_regress_matches_the_c_helper` is the byte-diff gate over the same
//! table; it is skipped, flagged, when there is no C helper to gate against.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::path::Path;

use testkit::Gate;

mod uri_table;

use uri_table::{TESTS, UriTest, environment, test_uri_failures};

const LIBPQ_URI_REGRESS: &str = env!("CARGO_BIN_EXE_libpq_uri_regress");

/// `test_uri` (`001_uri.pl:255`), run for each row by the `foreach` at `:283`.
///
/// Upstream's `is_deeply(\%result, \%expect, $uri)` is one test result per row
/// that keeps stdout, stderr and the exit status distinguishable, and every row
/// is reported whatever the earlier ones did. One `#[test]` that collects all
/// the mismatches is the closest Rust equivalent: the URI is the upstream test
/// name and it names every failure below.
#[test]
fn test_uri() {
    let failures = test_uri_failures(Path::new(LIBPQ_URI_REGRESS));
    assert!(
        failures.is_empty(),
        "{} of {} cases from 001_uri.pl failed:\n{}",
        failures.len(),
        TESTS.len(),
        failures.join("\n")
    );
}

/// Calculation: every URI the table holds more than once, with how many times,
/// in a stable order.
fn repeated_uris(tests: &[UriTest]) -> Vec<(&'static str, usize)> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for test in tests {
        *counts.entry(test.uri).or_default() += 1;
    }
    counts.into_iter().filter(|(_, times)| *times > 1).collect()
}

/// The table above was copied from `001_uri.pl:14`; this is the pin that says
/// nothing was dropped on the way.
///
/// The row *count* needs no test: `TESTS` is declared `[UriTest; 63]`, so
/// `TESTS.len() == 63` is a compile-time property and an assertion on it
/// cannot fail. What the array type cannot give is which rows those 63 are,
/// and the transcription slip that survives a correct count is pasting one row
/// twice instead of advancing to the next — the count stays 63 while a row of
/// upstream's table is gone and its coverage with it.
///
/// Upstream repeats exactly one URI, `postgresql://host/db`, at `001_uri.pl:32`
/// and again at `:46`, so exactly one repeat is expected here and any other is
/// a row of upstream's table overwritten by its neighbour.
#[test]
fn the_stolen_table_repeats_only_the_row_upstream_repeats() {
    assert_eq!(
        repeated_uris(&TESTS),
        [("postgresql://host/db", 2)],
        "a URI repeated here that upstream does not repeat means a row was pasted over"
    );
}

/// Gate: every row of the stolen table through the C `libpq_uri_regress` and
/// through ours, byte for byte on stdout, stderr and the exit status.
///
/// The helper is a test program, not an installed one: it is built into
/// `src/interfaces/libpq/test/` of a PostgreSQL 18.6 source tree and no
/// package ships it, so `PGDROP_REF_BIN` has to point at that directory for
/// this gate to be live. Without it the gate prints `SKIP (flagged, not
/// silent)` and passes — it is never narrowed to something weaker.
#[test]
fn libpq_uri_regress_matches_the_c_helper() {
    let Some(gate) = Gate::for_tool_or_skip("libpq_uri_regress", LIBPQ_URI_REGRESS) else {
        return; // The flagged skip is already on stderr.
    };
    for test in &TESTS {
        gate.clone()
            .arg(test.uri)
            .with_env(environment(test))
            .assert_clean();
    }
}
