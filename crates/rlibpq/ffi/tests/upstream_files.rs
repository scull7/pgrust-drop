//! The vendored upstream files are PostgreSQL 18.6's, byte for byte.
//!
//! Each digest was recorded with `sha256sum` from the files at tag
//! `REL_18_6` (commit `724edf9bde9d356724ad384a2e196edc3c9f80f7`), not
//! computed from the copies here, so a mismatch says a copy is not upstream's
//! (ADR-0008). Re-vendor from the tag or from `postgresql-18.6.tar.bz2`
//! (sha256 `555610c24d53e4316da5b7d3fc25c279d96856d5e0e23ee308c328c5fa881d9f`),
//! never from pgrust's `crates/postgres-18.6-reference/`.

#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;

use rlibpq::sha256::sha256;

/// `(vendored path, bytes, upstream path, upstream sha256)`.
const UPSTREAM_FILES: [(&str, &[u8], &str, &str); 10] = [
    (
        "include/libpq-fe.h",
        include_bytes!("../include/libpq-fe.h"),
        "src/interfaces/libpq/libpq-fe.h",
        "499d984421f5490be016f7d49a8599e7f8be120f0cf80c200e872265064be7c0",
    ),
    (
        "include/libpq-events.h",
        include_bytes!("../include/libpq-events.h"),
        "src/interfaces/libpq/libpq-events.h",
        "1f68ab7cf5e957bac8e9a4994e6dc6253736b231ddfa1ae4ea61517a583f202a",
    ),
    (
        "include/postgres_ext.h",
        include_bytes!("../include/postgres_ext.h"),
        "src/include/postgres_ext.h",
        "7f2d7945243abf649e0c574457db63abd3b704603db245e15f2268e823154322",
    ),
    (
        "upstream/exports.txt",
        include_bytes!("../upstream/exports.txt"),
        "src/interfaces/libpq/exports.txt",
        "1a755715e6089ddcb3e3e6e2a2988223d8f7b877fb52dcbeeecd6e629a3a4085",
    ),
    (
        "tests/c/libpq_testclient.c",
        include_bytes!("c/libpq_testclient.c"),
        "src/interfaces/libpq/test/libpq_testclient.c",
        "e3f1c7320a0bafe6d6355e090427c3e7b9e4a2044e3a25a85bb0a14edb8d79fa",
    ),
    (
        "tests/c/libpq_uri_regress.c",
        include_bytes!("c/libpq_uri_regress.c"),
        "src/interfaces/libpq/test/libpq_uri_regress.c",
        "dec61536820b560c9d2ff2b761fb0fc99b01d921a662f9de6c4e0062d9f20bce",
    ),
    (
        "tests/c/testlibpq.c",
        include_bytes!("c/testlibpq.c"),
        "src/test/examples/testlibpq.c",
        "453993625e892e5d0740f7cddc1aa2632330c0bd80ddd5c40a18b48d3b830530",
    ),
    (
        "tests/c/testlibpq3.c",
        include_bytes!("c/testlibpq3.c"),
        "src/test/examples/testlibpq3.c",
        "07d9f72e2c0dd1854d14d9b4c780884cbd339a161b715ecf50d72d6604e4c292",
    ),
    (
        "tests/c/testlibpq4.c",
        include_bytes!("c/testlibpq4.c"),
        "src/test/examples/testlibpq4.c",
        "78ef0137141d3875d77387d8a8cc06764a12579e36dba1f67faf8166ce50067b",
    ),
    (
        "tests/c/testlibpq3.sql",
        include_bytes!("c/testlibpq3.sql"),
        "src/test/examples/testlibpq3.sql",
        "cc4f96a8571daa5bb1ee1d100aff583a456dff4c78ee96a524736b1db185f68c",
    ),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

#[test]
fn each_vendored_file_is_the_one_postgresql_18_6_ships() {
    for (vendored, bytes, upstream, digest) in UPSTREAM_FILES {
        assert_eq!(
            hex(&sha256(bytes)),
            digest,
            "{vendored} is not {upstream} at REL_18_6"
        );
    }
}
