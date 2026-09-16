//! End-to-end cover for the spawning half of the `command_*` helpers.
//!
//! The verdicts themselves are pure and unit-tested in `src/checks.rs`; these
//! tests only prove that a real process is run and that its exit status and
//! streams reach those checks. They use coreutils and `/bin/sh` rather than
//! PostgreSQL so they run everywhere; a missing tool prints a flagged skip,
//! never a silent pass.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::Path;

use testkit::{Pattern, reference};

const ECHO: &str = "/bin/echo";
const FALSE: &str = "/bin/false";
const SH: &str = "/bin/sh";

fn available(tools: &[&str]) -> bool {
    let missing: Vec<&str> = tools
        .iter()
        .copied()
        .filter(|tool| !Path::new(tool).is_file())
        .collect();
    if missing.is_empty() {
        return true;
    }
    reference::announce_skip(&format!(
        "{}: {missing:?} not present on this machine",
        reference::SKIP_FLAG
    ));
    false
}

fn pattern(source: &str) -> Pattern {
    Pattern::new(source).expect("pattern compiles")
}

#[test]
fn command_ok_passes_on_a_zero_exit() {
    if !available(&[ECHO]) {
        return;
    }
    testkit::command_ok(Path::new(ECHO), ["creating directory ... ok"]);
}

#[test]
fn command_fails_passes_on_a_nonzero_exit() {
    if !available(&[FALSE]) {
        return;
    }
    testkit::command_fails(Path::new(FALSE), std::iter::empty::<&str>());
}

#[test]
fn command_like_reaches_the_real_stdout() {
    if !available(&[ECHO]) {
        return;
    }
    testkit::command_like(
        Path::new(ECHO),
        ["Data page checksum version:    1"],
        // qr/Data page checksum version:.*1/ (t/001_initdb.pl)
        &pattern("Data page checksum version:.*1"),
    );
}

#[test]
fn command_fails_like_reaches_the_real_stderr() {
    if !available(&[SH]) {
        return;
    }
    testkit::command_fails_like(
        Path::new(SH),
        [
            "-c",
            "echo 'initdb: error: locale \"x\" unknown' >&2; exit 1",
        ],
        &pattern(r#"initdb: error: locale "x" unknown"#),
    );
}

#[test]
fn a_command_that_should_have_failed_panics_with_the_reason() {
    if !available(&[ECHO]) {
        return;
    }
    let panic = std::panic::catch_unwind(|| {
        testkit::command_fails(Path::new(ECHO), ["this exits 0"]);
    })
    .expect_err("echo exits 0, so command_fails must panic");
    let message = panic
        .downcast_ref::<String>()
        .map_or_else(String::new, Clone::clone);
    assert!(
        message.contains("expected a nonzero exit code"),
        "{message}"
    );
    // The failure names the command line it ran, the way upstream's
    // "# Running: …" does.
    assert!(message.contains("this exits 0"), "{message}");
}

#[test]
fn a_stdout_that_does_not_match_panics_with_the_pattern_and_the_output() {
    if !available(&[ECHO]) {
        return;
    }
    let panic = std::panic::catch_unwind(|| {
        testkit::command_like(
            Path::new(ECHO),
            ["Data page checksum version:    0"],
            &pattern("Data page checksum version:.*1"),
        );
    })
    .expect_err("checksums are off, so command_like must panic");
    let message = panic
        .downcast_ref::<String>()
        .map_or_else(String::new, Clone::clone);
    assert!(message.contains("does not match"), "{message}");
    assert!(message.contains("version:    0"), "{message}");
}
