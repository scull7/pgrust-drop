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
//!
//! On top of those, [`gate`] is the byte-diff gate itself: one invocation
//! through the reference C tool and through ours, compared byte for byte after
//! the justified normalizations in [`normalize`], with [`diff`] rendering any
//! mismatch as `diff -U3`.

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod checks;
pub mod diff;
pub mod files;
pub mod gate;
pub mod normalize;
pub mod outcome;
pub mod pattern;
pub mod reference;
pub mod run;

pub use checks::Violation;
pub use files::{Entry, EntryKind, ModeViolation, slurp_file};
pub use gate::{Gate, GateError, GateReport, RcCheck, Scope, Side, StreamDiff};
pub use normalize::Normalizer;
pub use outcome::CommandOutcome;
pub use pattern::{Pattern, PatternError};
pub use run::{
    command_fails, command_fails_like, command_like, command_ok, program_help_ok,
    program_options_handling_ok, program_version_ok, run, run_with_stdin,
};

#[cfg(unix)]
pub use files::{check_mode_recursive, check_mode_recursive_ok};
