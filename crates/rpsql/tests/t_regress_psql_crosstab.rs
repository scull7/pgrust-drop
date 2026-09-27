//! Port of `src/test/regress/sql/psql_crosstab.sql` (PostgreSQL 18.6),
//! NAT-404's first slice: `\crosstabview`.
//!
//! The script is 124 lines and every block depends on the `ctv_data` table
//! the first one creates, so it is gated whole, the way `pg_regress` runs it:
//! `rpsql -X -a -q` against a PostgreSQL 18 cluster, compared byte for byte
//! with `expected/psql_crosstab.out` and with C psql's output. The cluster
//! needs the reference `initdb` and `pg_ctl`; without them the gate prints
//! `SKIP (flagged, not silent)`. What the pivot does is also pinned
//! server-free by `rpsql::crosstab`'s unit tests.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::path::Path;

use regress::{Cluster, PSQL_CROSSTAB_OUT, PSQL_CROSSTAB_SQL, Section, sha256_hex};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// The port the live gate's cluster listens on.
const PSQL_CROSSTAB_PORT: u16 = 55_492;

#[test]
fn the_vendored_files_are_the_ones_postgresql_18_6_ships() {
    // ADR-0008: vendored bytes come from the tag or the tarball.
    assert_eq!(
        sha256_hex(PSQL_CROSSTAB_SQL.as_bytes()),
        regress::PSQL_CROSSTAB_SQL_SHA256,
        "crates/rpsql/tests/regress/psql_crosstab.sql is not REL_18_6's"
    );
    assert_eq!(
        sha256_hex(PSQL_CROSSTAB_OUT.as_bytes()),
        regress::PSQL_CROSSTAB_OUT_SHA256,
        "crates/rpsql/tests/regress/expected/psql_crosstab.out is not REL_18_6's"
    );
}

/// `psql_crosstab.sql`, whole, through rpsql and C psql against a server.
#[test]
fn psql_crosstab() {
    let Some(cluster) = Cluster::start(PSQL_CROSSTAB_PORT) else {
        return;
    };
    regress::gate_section(
        &cluster,
        Path::new(RPSQL),
        &Section {
            header: "--",
            sql: PSQL_CROSSTAB_SQL,
            expected: PSQL_CROSSTAB_OUT,
            sql_line: 1,
            out_line: 1,
        },
    );
}
