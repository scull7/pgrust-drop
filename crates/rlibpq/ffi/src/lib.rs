//! rlibpq-ffi: the C ABI of libpq, over the pure-Rust `rlibpq` crate.
//!
//! The library is named `pq`, so the archive is `libpq.a` and a C program
//! written against PostgreSQL 18.6's `libpq-fe.h` (vendored, unmodified, in
//! `include/`) links it the way it links C libpq. Every symbol C libpq
//! exports is listed in `upstream/exports.txt`, also unmodified; [`abi`]
//! reads it and records which of them this crate exports, and
//! `docs/libpq-abi.md` is generated from that record.
//!
//! What is exported so far is what a C libpq built without SSL and without
//! GSSAPI answers with no connection at all: the library version, the thread
//! safety probe, `PQfreemem`, and the `#ifndef USE_SSL`, `#ifndef
//! USE_OPENSSL` and `#ifndef ENABLE_GSS` arms of `fe-secure.c`. That is enough
//! for `src/interfaces/libpq/test/libpq_testclient.c` and `t/002_api.pl`.
//! Connections, results and the rest follow in later slices of NAT-395.
//!
//! This is the one crate in the workspace that allows `unsafe`: a C ABI is
//! raw pointers. The pure half ([`abi`]) has none, and every shim is a thin
//! function at the edge.

// `#[no_mangle]` functions and the C names they carry are the whole point.
#![allow(unsafe_code, non_snake_case)]
// Proper nouns such as PostgreSQL fill every doc comment.
#![allow(clippy::doc_markdown)]

pub mod abi;
mod misc;
mod secure;

pub use misc::PG_VERSION_NUM;

/// `PGconn`, opaque to C (`libpq-fe.h:202`: `typedef struct pg_conn PGconn`).
///
/// No `PGconn` is ever created yet: `PQconnectdb` and its siblings are later
/// slices. The shims that take one today answer what C answers for *every*
/// connection in a build without SSL or GSSAPI, so none of them reads it.
pub struct PGconn {
    _unconstructed: [u8; 0],
}
