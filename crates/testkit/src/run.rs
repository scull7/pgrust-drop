//! Actions: spawn a binary and hand its outcome to the pure checks.

use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

/// Run `bin` with `args`, feeding `stdin` to it and capturing both streams.
///
/// Empty `stdin` closes the child's stdin, exactly as [`run`] does. Otherwise
/// the bytes are written from a helper thread while the parent drains stdout
/// and stderr, so a tool that writes more than a pipe buffer before reading
/// all of its input cannot deadlock the gate. A child that exits before
/// consuming everything (psql with `-c`, initdb rejecting an option) breaks
/// the pipe; that is the child's business, not a test failure, so `EPIPE` is
/// not reported.
///
/// # Errors
/// The `io::Error` from spawning, or from writing to the child's stdin.
///
/// # Panics
/// If the stdin-writing thread panics.
pub fn run_with_stdin<I, S>(bin: &Path, args: I, stdin: &[u8]) -> std::io::Result<CommandOutcome>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    if stdin.is_empty() {
        return run(bin, args);
    }
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut pipe = child.stdin.take().expect("stdin was piped");
    let bytes = stdin.to_vec();
    let writer = std::thread::spawn(move || pipe.write_all(&bytes));
    let output = child.wait_with_output()?;
    match writer.join().expect("stdin writer panicked") {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {}
        Err(err) => return Err(err),
    }
    Ok(CommandOutcome::from(output))
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
