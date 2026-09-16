//! End-to-end cover for the one action in the gate: spawning both binaries.
//!
//! The comparison itself is pure and unit-tested in `src/gate.rs`. These tests
//! only prove that two real processes are run with the same arguments and the
//! same stdin, and that their outcomes reach [`testkit::compare`]. They use
//! coreutils rather than PostgreSQL so they run everywhere; a missing tool
//! prints a flagged skip, never a silent pass.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;
use std::path::Path;

use testkit::Gate;

const CAT: &str = "/bin/cat";
const ECHO: &str = "/bin/echo";

fn available(tools: &[&str]) -> bool {
    let missing: Vec<&str> = tools
        .iter()
        .copied()
        .filter(|tool| !Path::new(tool).is_file())
        .collect();
    if missing.is_empty() {
        return true;
    }
    println!("SKIP (flagged, not silent): {missing:?} not present on this machine");
    false
}

#[test]
fn a_tool_gated_against_itself_is_clean() {
    if !available(&[CAT]) {
        return;
    }
    let report = Gate::new(CAT, CAT)
        .with_stdin(b"SELECT 1;\n\\q\n".to_vec())
        .run()
        .expect("run cat twice");
    assert!(report.is_clean(), "{report}");
}

#[test]
fn stdin_actually_reaches_both_children() {
    if !available(&[CAT, ECHO]) {
        return;
    }
    // cat echoes its stdin, echo ignores it: the gate must see the difference.
    let report = Gate::new(CAT, ECHO)
        .with_stdin(b"only cat repeats this\n".to_vec())
        .run()
        .expect("run cat and echo");
    let diff = report.stdout_diff.as_ref().expect("stdout differs");
    assert!(diff.contains("-only cat repeats this"), "{diff}");
    assert!(report.rc.matches(), "{report}");
}

#[test]
fn a_large_stdin_does_not_deadlock() {
    if !available(&[CAT]) {
        return;
    }
    // Comfortably past a 64 KiB pipe buffer, so the writer thread earns its keep.
    let mut payload = String::new();
    for line in 0..40_000 {
        writeln!(payload, "line {line}").expect("writing to a String is infallible");
    }
    let report = Gate::new(CAT, CAT)
        .with_stdin(payload.into_bytes())
        .run()
        .expect("run cat twice on a large input");
    assert!(report.is_clean(), "{report}");
}

#[test]
fn a_missing_binary_is_an_error_not_a_pass() {
    let error = Gate::new("/nonexistent/reference/initdb", CAT)
        .run()
        .expect_err("spawning a missing binary must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
