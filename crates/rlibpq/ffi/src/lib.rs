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
//! Then the connection-free half of `fe-connect.c`'s option handling:
//! `PQconndefaults`, `PQconninfoParse` and `PQconninfoFree` over the public
//! [`PQconninfoOption`] layout, enough for
//! `src/interfaces/libpq/test/libpq_uri_regress.c` and `t/001_uri.pl`.
//! Then the opaque [`PGconn`] and [`PGresult`] and the blocking calls over
//! them — `PQconnectdb`, `PQstatus`, `PQerrorMessage`, `PQexec`, `PQfinish`
//! and the result accessors — enough for `src/test/examples/testlibpq.c`.
//! Then the extended-query calls — `PQexecParams`, `PQprepare`,
//! `PQexecPrepared`, `PQdescribePrepared`, `PQdescribePortal` — and the rest
//! of a result's metadata, enough for `src/test/examples/testlibpq3.c`.
//! The rest follows in later slices of NAT-395.
//!
//! This is the one crate in the workspace that allows `unsafe`: a C ABI is
//! raw pointers. The pure half ([`abi`]) has none, and every shim is a thin
//! function at the edge.

// `#[no_mangle]` functions and the C names they carry are the whole point.
#![allow(unsafe_code, non_snake_case)]
// Proper nouns such as PostgreSQL fill every doc comment.
#![allow(clippy::doc_markdown)]

pub mod abi;
mod alloc;
mod conn;
mod conninfo;
mod ctext;
mod extended;
mod misc;
mod result;
mod secure;

pub use conn::PGconn;
pub use conninfo::PQconninfoOption;
pub use misc::PG_VERSION_NUM;
pub use result::PGresult;
