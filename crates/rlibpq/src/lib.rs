//! rlibpq: the client side of libpq in pure Rust.
//!
//! Tracks PostgreSQL 18.6 `src/interfaces/libpq/` and the requirements in
//! pgrust issue #40: one codebase producing a native Rust crate and a drop-in
//! C-ABI `libpq.so`, with GSSAPI feature-gated and no libc/OpenSSL build-time
//! coupling.
//!
//! What is here so far is the connection-string front end: `PQconninfoOptions[]`
//! and the two parsers that fill a working copy of it, proved against
//! `t/001_uri.pl`. The rest is tracked in Linear NAT-389 … NAT-396.
//!
//! The C ABI layer will need `unsafe`; the pure-Rust core must not, so the
//! crate denies it until that layer exists as its own module.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod conninfo;
mod cstr;
pub mod error;
pub mod pg_config;
pub mod regress;
mod text;
pub mod uri;

pub use conninfo::{
    CONNINFO_OPTIONS, ConnInfo, ConnOption, ConnOptionDef, Dispchar, Env, UnknownKeyword,
    conndefaults, parse_conninfo, parse_keyword_value, recognized_connection_string,
    uri_prefix_length,
};
pub use error::ConnError;
pub use regress::regress_report;
pub use text::RawText;
pub use uri::{parse_uri, uri_decode};
