//! rlibpq: the client side of libpq in pure Rust.
//!
//! Tracks PostgreSQL 18.6 `src/interfaces/libpq/` and the requirements in
//! pgrust issue #40: one codebase producing a native Rust crate and a drop-in
//! C-ABI `libpq.so`, with GSSAPI feature-gated and no libc/OpenSSL build-time
//! coupling.
//!
//! Nothing is implemented yet. Work is tracked in Linear NAT-388 … NAT-396,
//! starting with conninfo/URI parsing against `t/001_uri.pl`.
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
