//! Conformance helpers for pgrust-drop.
//!
//! Ports of the PostgreSQL 18.6 TAP helpers in
//! `src/test/perl/PostgreSQL/Test/Utils.pm` so the upstream `t/*.pl` files
//! translate one assertion at a time into Rust integration tests, plus the
//! reference-binary discovery that every byte-diff gate needs.
//!
//! Layout follows Data / Calculations / Actions:
//!
//! - [`CommandOutcome`] is the data a spawned command produced.
//! - [`checks`] are pure functions from an outcome to a list of [`Violation`]s;
//!   they are unit-tested without spawning anything.
//! - [`run`] and the `*_ok` wrappers are the only places that touch a process.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod checks;
pub mod outcome;
pub mod reference;
pub mod run;

pub use checks::Violation;
pub use outcome::CommandOutcome;
pub use run::{program_help_ok, program_options_handling_ok, program_version_ok, run};
