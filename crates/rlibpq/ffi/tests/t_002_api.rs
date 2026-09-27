//! Port of `src/interfaces/libpq/t/002_api.pl` (PostgreSQL REL_18_6), which
//! drives `src/interfaces/libpq/test/libpq_testclient.c`.
//!
//! `tests/c/libpq_testclient.c` is upstream's file, unmodified; it is
//! compiled against the vendored `libpq-fe.h` and linked with this crate's
//! `libpq.a`, which is the acceptance NAT-395 sets for the C ABI. Its
//! `#include "postgres_fe.h"` finds `tests/c/postgres_fe.h`, a stand-in for
//! the internal header.
//!
//! Upstream picks its assertion by `$ENV{with_ssl}` (`:11`): `openssl` expects
//! `OpenSSL` on stdout, anything else expects `SSL is not enabled` on stderr.
//! `rlibpq` has no TLS backend, so this is the second arm — the one a C libpq
//! configured without SSL takes. No reference comparison runs: no package
//! ships `libpq_testclient`, and a distribution libpq is built with OpenSSL
//! and would take the other arm.

#![allow(clippy::doc_markdown)]

mod common;

use common::{build, chomp, crate_dir, run};

/// `002_api.pl:17`-`:19`: `PQsslAttribute(NULL, "library") returns NULL`.
#[test]
fn pq_ssl_attribute_null_library_returns_null() {
    let testclient = build(
        "libpq_testclient",
        &[crate_dir().join("tests/c/libpq_testclient.c")],
        &[],
    );

    // `002_api.pl:9`: run_command([ 'libpq_testclient', '--ssl' ]).
    let outcome = run(&testclient, &["--ssl"]);

    assert_eq!(
        String::from_utf8_lossy(chomp(&outcome.stderr)),
        "SSL is not enabled",
        "PQsslAttribute(NULL, \"library\") returns NULL"
    );
    // Not asserted upstream, which reads only the streams, but what
    // `libpq_testclient.c:32`-`:33` does after printing: nothing, exit 0.
    assert_eq!(outcome.stdout, b"");
    assert_eq!(outcome.status, Some(0));
}
