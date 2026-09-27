//! Port of `src/interfaces/libpq/t/001_uri.pl` (PostgreSQL REL_18_6), which
//! drives `src/interfaces/libpq/test/libpq_uri_regress.c`, over the C ABI.
//!
//! `tests/c/libpq_uri_regress.c` is upstream's file, unmodified; it is
//! compiled against the vendored `libpq-fe.h` and linked with this crate's
//! `libpq.a`, so every row of the table goes through `PQconninfoParse`,
//! `PQconndefaults` and the `PQconninfoOption` layout as a C caller sees them.
//! The table and the per-row check are rlibpq's (`tests/uri_table/`), shared
//! with its port over the Rust helper so the two cannot drift. The byte-diff
//! against the C helper is rlibpq's `libpq_uri_regress_matches_the_c_helper`;
//! this proves the C ABI reproduces the same table.

#![allow(clippy::doc_markdown)]

mod common;
#[path = "../../tests/uri_table/mod.rs"]
mod uri_table;

use common::{build, crate_dir};
use uri_table::{TESTS, test_uri_failures};

/// `test_uri` (`001_uri.pl:255`), run for each row by the `foreach` at `:283`,
/// against upstream's helper built on `libpq.a`.
#[test]
fn test_uri() {
    let helper = build(
        "libpq_uri_regress",
        &[crate_dir().join("tests/c/libpq_uri_regress.c")],
        &[],
    );
    let failures = test_uri_failures(&helper);
    assert!(
        failures.is_empty(),
        "{} of {} cases from 001_uri.pl failed through libpq.a:\n{}",
        failures.len(),
        TESTS.len(),
        failures.join("\n")
    );
}
