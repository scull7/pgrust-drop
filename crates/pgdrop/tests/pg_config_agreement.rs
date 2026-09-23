//! `rinitdb` and `rlibpq` agree on the two `pg_config` values they both
//! define (Linear NAT-428).
//!
//! In C these are one constant each — `DEF_PGPORT_STR`
//! (`src/include/pg_config.h.in:43`) and `DEFAULT_PGSOCKET_DIR`
//! (`src/include/pg_config_manual.h:193`, `:195` on Windows) — read by both
//! `initdb.c` and `fe-connect.c`, so a C build cannot disagree with itself.
//! Here they are two definitions in two crates that deliberately depend on
//! nothing of each other (NAT-424 decided against a shared crate), and
//! `pgdrop` is the one crate that links both and so the one that breaks when
//! they drift: `pgdrop initdb` writes the socket directory and port into
//! `postgresql.conf` (`crates/rinitdb/src/conf.rs`), and `pgdrop psql` dials
//! them (`crates/rlibpq/src/connection.rs`).
//!
//! No upstream test to steal: upstream has one definition, so nothing to
//! compare. Each crate also pins its own half to the stock literal; these
//! tests compare the two halves directly, arm for arm by construction, and
//! keep holding if the stock values ever legitimately change.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

#[test]
fn a_cluster_pgdrop_initdb_creates_puts_its_socket_where_pgdrop_psql_looks() {
    assert_eq!(
        rinitdb::pg_config::DEFAULT_PGSOCKET_DIR,
        rlibpq::pg_config::DEFAULT_PGSOCKET_DIR,
        "rinitdb writes unix_socket_directories from its DEFAULT_PGSOCKET_DIR \
         and rlibpq dials its own; if they differ, pgdrop psql cannot reach a \
         cluster pgdrop initdb created"
    );
}

#[test]
fn pgdrop_initdb_and_pgdrop_psql_agree_on_the_compiled_in_port() {
    assert_eq!(
        rinitdb::pg_config::DEF_PGPORT_STR,
        rlibpq::pg_config::DEF_PGPORT_STR,
        "rinitdb writes #port from its DEF_PGPORT_STR and rlibpq defaults to \
         its own; if they differ, pgdrop psql dials the wrong port"
    );
}
