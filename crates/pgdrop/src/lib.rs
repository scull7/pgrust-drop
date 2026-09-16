//! pgdrop: one binary, busybox-style.
//!
//! `pgdrop initdb …`, `pgdrop psql …`, `pgdrop postgres …`, `pgdrop start …`,
//! plus symlinks named `initdb` / `psql` / `postgres` that select the applet
//! from `argv[0]` (Linear NAT-406). Applet arguments pass through untouched so
//! the upstream test suites see the upstream command lines.
//!
//! `pgdrop install-links DIR` ([`install`], Linear NAT-416) is what creates
//! those symlinks.
//!
//! The pgrust server (NAT-407) and `start` (NAT-409) are not embedded yet.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod dispatch;
pub mod install;
