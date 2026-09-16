//! Actions: spawn a binary and hand its outcome to the pure checks.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

use crate::CommandOutcome;
use crate::checks::{self, Violation};

/// Run `bin` with `args`, capturing both streams. stdin is closed so a tool
/// that would otherwise read a terminal (psql) does not hang.
///
/// # Errors
/// The `io::Error` from spawning, typically "No such file or directory".
pub fn run<I, S>(bin: &Path, args: I) -> std::io::Result<CommandOutcome>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map(CommandOutcome::from)
}

/// `program_help_ok('initdb')`: runs `bin --help` and asserts the upstream
/// conditions. Panics with the full violation list, like `Test::More` diag.
///
/// # Panics
/// When the binary cannot be spawned or any check fails.
pub fn program_help_ok(bin: &Path) {
    let outcome = must_run(bin, ["--help"]);
    assert_clean(bin, "--help", &checks::program_help(&outcome));
}

/// `program_version_ok('initdb')`: runs `bin --version`.
///
/// # Panics
/// When the binary cannot be spawned or any check fails.
pub fn program_version_ok(bin: &Path) {
    let outcome = must_run(bin, ["--version"]);
    assert_clean(bin, "--version", &checks::program_version(&outcome));
}

/// `program_options_handling_ok('initdb')`: runs `bin --not-a-valid-option`.
///
/// # Panics
/// When the binary cannot be spawned or any check fails.
pub fn program_options_handling_ok(bin: &Path) {
    let outcome = must_run(bin, ["--not-a-valid-option"]);
    assert_clean(
        bin,
        "--not-a-valid-option",
        &checks::program_options_handling(&outcome),
    );
}

fn must_run<const N: usize>(bin: &Path, args: [&str; N]) -> CommandOutcome {
    run(bin, args).unwrap_or_else(|err| panic!("could not run {}: {err}", bin.display()))
}

fn assert_clean(bin: &Path, arg: &str, violations: &[Violation]) {
    assert!(
        violations.is_empty(),
        "{} {arg}:\n{}",
        bin.display(),
        violations
            .iter()
            .map(|v| format!("  - {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
