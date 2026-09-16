//! Pure checks over a [`CommandOutcome`], one per upstream TAP helper.
//!
//! Each function returns every violation it finds rather than the first, so a
//! failing test reports the whole picture the way `Test::More` does.

use std::fmt;

use crate::CommandOutcome;
use crate::pattern::Pattern;

/// `program_help_ok` (Utils.pm:927): "Most output actually tries to aim for
/// 80", the convention enforced is 95 columns.
pub const HELP_MAX_LINE_LENGTH: usize = 95;

/// One failed TAP assertion, worded like the upstream test name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Expected exit status 0; the outcome carried this status instead.
    ExpectedSuccess(Option<i32>),
    /// Expected a nonzero exit status; got 0.
    ExpectedFailure,
    /// The named stream should have carried something.
    EmptyStream(Stream),
    /// The named stream should have been empty; the bytes it carried.
    NonEmptyStream(Stream, String),
    /// Help lines longer than [`HELP_MAX_LINE_LENGTH`].
    LongHelpLines(Vec<String>),
    /// The named stream did not match the pattern the stolen test carries;
    /// the pattern and what the stream held instead.
    NoMatch(Stream, Pattern, String),
}

/// Which output stream a violation talks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Stream::Stdout => "stdout",
            Stream::Stderr => "stderr",
        })
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::ExpectedSuccess(Some(code)) => write!(f, "expected exit code 0, got {code}"),
            Violation::ExpectedSuccess(None) => {
                f.write_str("expected exit code 0, process was killed by a signal")
            }
            Violation::ExpectedFailure => f.write_str("expected a nonzero exit code, got 0"),
            Violation::EmptyStream(stream) => write!(f, "expected output on {stream}, got nothing"),
            Violation::NonEmptyStream(stream, text) => {
                write!(f, "expected nothing on {stream}, got:\n{text}")
            }
            Violation::NoMatch(stream, pattern, text) => {
                write!(f, "{stream} does not match /{pattern}/, it held:\n{text}")
            }
            Violation::LongHelpLines(lines) => {
                writeln!(
                    f,
                    "these help lines are too long (>{HELP_MAX_LINE_LENGTH}):"
                )?;
                for line in lines {
                    writeln!(f, "  {line}")?;
                }
                Ok(())
            }
        }
    }
}

/// `program_help_ok($cmd)`: `--help` exits 0, writes to stdout, nothing to
/// stderr, and no stdout line exceeds [`HELP_MAX_LINE_LENGTH`] characters.
#[must_use]
pub fn program_help(outcome: &CommandOutcome) -> Vec<Violation> {
    let mut violations = success_on_stdout_only(outcome);
    let long_lines: Vec<String> = outcome
        .stdout_text()
        .lines()
        .filter(|line| line.chars().count() > HELP_MAX_LINE_LENGTH)
        .map(str::to_owned)
        .collect();
    if !long_lines.is_empty() {
        violations.push(Violation::LongHelpLines(long_lines));
    }
    violations
}

/// `program_version_ok($cmd)`: `--version` exits 0, writes to stdout, nothing
/// to stderr.
#[must_use]
pub fn program_version(outcome: &CommandOutcome) -> Vec<Violation> {
    success_on_stdout_only(outcome)
}

/// `program_options_handling_ok($cmd)`: `--not-a-valid-option` exits nonzero
/// and prints an error message on stderr.
#[must_use]
pub fn program_options_handling(outcome: &CommandOutcome) -> Vec<Violation> {
    let mut violations = Vec::new();
    if outcome.succeeded() {
        violations.push(Violation::ExpectedFailure);
    }
    if outcome.stderr.is_empty() {
        violations.push(Violation::EmptyStream(Stream::Stderr));
    }
    violations
}

/// `command_ok($cmd)` (Utils.pm:866): the command exits 0. Upstream looks at
/// nothing else — `run_log` reports only success or failure — so neither
/// stream is asserted on here either.
#[must_use]
pub fn command_ok(outcome: &CommandOutcome) -> Vec<Violation> {
    if outcome.succeeded() {
        Vec::new()
    } else {
        vec![Violation::ExpectedSuccess(outcome.status)]
    }
}

/// `command_fails($cmd)` (Utils.pm:883): the command exits nonzero.
#[must_use]
pub fn command_fails(outcome: &CommandOutcome) -> Vec<Violation> {
    if outcome.succeeded() {
        vec![Violation::ExpectedFailure]
    } else {
        Vec::new()
    }
}

/// `command_like($cmd, $expected_stdout)` (Utils.pm:1006): exit 0, nothing on
/// stderr, and stdout matches the pattern.
#[must_use]
pub fn command_like(outcome: &CommandOutcome, expected_stdout: &Pattern) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !outcome.succeeded() {
        violations.push(Violation::ExpectedSuccess(outcome.status));
    }
    if !outcome.stderr.is_empty() {
        violations.push(Violation::NonEmptyStream(
            Stream::Stderr,
            outcome.stderr_text(),
        ));
    }
    let stdout = outcome.stdout_text();
    if !expected_stdout.is_match(&stdout) {
        violations.push(Violation::NoMatch(
            Stream::Stdout,
            expected_stdout.clone(),
            stdout,
        ));
    }
    violations
}

/// `command_fails_like($cmd, $expected_stderr)` (Utils.pm:1059): exit nonzero
/// and stderr matches the pattern. Upstream does not assert on stdout here.
#[must_use]
pub fn command_fails_like(outcome: &CommandOutcome, expected_stderr: &Pattern) -> Vec<Violation> {
    let mut violations = command_fails(outcome);
    let stderr = outcome.stderr_text();
    if !expected_stderr.is_match(&stderr) {
        violations.push(Violation::NoMatch(
            Stream::Stderr,
            expected_stderr.clone(),
            stderr,
        ));
    }
    violations
}

/// Shared shape of `program_help_ok` and `program_version_ok`.
fn success_on_stdout_only(outcome: &CommandOutcome) -> Vec<Violation> {
    let mut violations = Vec::new();
    if !outcome.succeeded() {
        violations.push(Violation::ExpectedSuccess(outcome.status));
    }
    if outcome.stdout.is_empty() {
        violations.push(Violation::EmptyStream(Stream::Stdout));
    }
    if !outcome.stderr.is_empty() {
        violations.push(Violation::NonEmptyStream(
            Stream::Stderr,
            outcome.stderr_text(),
        ));
    }
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> CommandOutcome {
        CommandOutcome::new(Some(0), stdout, "")
    }

    #[test]
    fn help_accepts_the_upstream_shape() {
        assert!(program_help(&ok("initdb initializes a cluster.\n\nUsage:\n")).is_empty());
    }

    #[test]
    fn help_reports_every_failure_at_once() {
        let outcome = CommandOutcome::new(Some(1), "", "boom");
        assert_eq!(
            program_help(&outcome),
            vec![
                Violation::ExpectedSuccess(Some(1)),
                Violation::EmptyStream(Stream::Stdout),
                Violation::NonEmptyStream(Stream::Stderr, "boom".to_owned()),
            ]
        );
    }

    #[test]
    fn help_flags_lines_over_ninety_five_columns() {
        let long = "x".repeat(HELP_MAX_LINE_LENGTH + 1);
        let fine = "y".repeat(HELP_MAX_LINE_LENGTH);
        let outcome = ok(&format!("{fine}\n{long}\n"));
        assert_eq!(
            program_help(&outcome),
            vec![Violation::LongHelpLines(vec![long])]
        );
    }

    #[test]
    fn help_counts_characters_not_bytes() {
        // 95 multibyte characters are within the limit even though they are more bytes.
        let outcome = ok(&"é".repeat(HELP_MAX_LINE_LENGTH));
        assert!(program_help(&outcome).is_empty());
    }

    #[test]
    fn help_treats_a_signal_death_as_failure() {
        let outcome = CommandOutcome::new(None, "help", "");
        assert_eq!(
            program_help(&outcome),
            vec![Violation::ExpectedSuccess(None)]
        );
    }

    #[test]
    fn version_accepts_a_version_line() {
        assert!(program_version(&ok("initdb (PostgreSQL) 18.6\n")).is_empty());
    }

    #[test]
    fn version_rejects_stderr_noise() {
        let outcome = CommandOutcome::new(Some(0), "v", "warning");
        assert_eq!(
            program_version(&outcome),
            vec![Violation::NonEmptyStream(
                Stream::Stderr,
                "warning".to_owned()
            )]
        );
    }

    #[test]
    fn options_handling_wants_failure_with_a_message() {
        let good = CommandOutcome::new(Some(2), "", "error: unexpected argument\n");
        assert!(program_options_handling(&good).is_empty());

        let silent_failure = CommandOutcome::new(Some(1), "", "");
        assert_eq!(
            program_options_handling(&silent_failure),
            vec![Violation::EmptyStream(Stream::Stderr)]
        );

        let accepted = CommandOutcome::new(Some(0), "", "");
        assert_eq!(
            program_options_handling(&accepted),
            vec![
                Violation::ExpectedFailure,
                Violation::EmptyStream(Stream::Stderr)
            ]
        );
    }

    #[test]
    fn command_ok_only_cares_about_the_exit_status() {
        // run_log reports success or failure and nothing else, so noise on
        // either stream is not this assertion's business.
        let noisy = CommandOutcome::new(Some(0), "creating directory", "warning: fsync");
        assert!(command_ok(&noisy).is_empty());
        assert_eq!(
            command_ok(&CommandOutcome::new(Some(1), "", "")),
            vec![Violation::ExpectedSuccess(Some(1))]
        );
        assert_eq!(
            command_ok(&CommandOutcome::new(None, "", "")),
            vec![Violation::ExpectedSuccess(None)]
        );
    }

    #[test]
    fn command_fails_wants_any_nonzero_status() {
        assert!(command_fails(&CommandOutcome::new(Some(1), "", "initdb: error:")).is_empty());
        // Killed by a signal is a failure too, as it is for IPC::Run.
        assert!(command_fails(&CommandOutcome::new(None, "", "")).is_empty());
        assert_eq!(
            command_fails(&CommandOutcome::silent_success()),
            vec![Violation::ExpectedFailure]
        );
    }

    fn pattern(source: &str) -> Pattern {
        Pattern::new(source).expect("pattern compiles")
    }

    #[test]
    fn command_like_wants_success_a_clean_stderr_and_a_match() {
        // command_like([ 'pg_controldata', $datadir ],
        //     qr/Data page checksum version:.*1/, 'checksums are enabled');
        let expected = pattern("Data page checksum version:.*1");
        let good = CommandOutcome::new(Some(0), "Data page checksum version:    1\n", "");
        assert!(command_like(&good, &expected).is_empty());

        let wrong_value = CommandOutcome::new(Some(0), "Data page checksum version:    0\n", "");
        assert_eq!(
            command_like(&wrong_value, &expected),
            vec![Violation::NoMatch(
                Stream::Stdout,
                expected.clone(),
                "Data page checksum version:    0\n".to_owned()
            )]
        );

        let failed = CommandOutcome::new(Some(1), "", "pg_controldata: error: boom");
        assert_eq!(
            command_like(&failed, &expected),
            vec![
                Violation::ExpectedSuccess(Some(1)),
                Violation::NonEmptyStream(Stream::Stderr, "pg_controldata: error: boom".to_owned()),
                Violation::NoMatch(Stream::Stdout, expected, String::new()),
            ]
        );
    }

    #[test]
    fn command_fails_like_wants_failure_and_a_matching_stderr() {
        // command_fails_like([ 'initdb', '--set', 'foo' ], qr/initdb: error: /, …)
        let expected = pattern("initdb: error: locale must be specified if provider is icu");
        let good = CommandOutcome::new(
            Some(1),
            "",
            "initdb: error: locale must be specified if provider is icu\n",
        );
        assert!(command_fails_like(&good, &expected).is_empty());

        // The right exit status with the wrong message is still a failure:
        // this is the check that stops a tool failing for the wrong reason.
        let wrong_message = CommandOutcome::new(Some(1), "", "initdb: error: out of memory\n");
        assert_eq!(
            command_fails_like(&wrong_message, &expected),
            vec![Violation::NoMatch(
                Stream::Stderr,
                expected.clone(),
                "initdb: error: out of memory\n".to_owned()
            )]
        );

        let succeeded = CommandOutcome::silent_success();
        assert_eq!(
            command_fails_like(&succeeded, &expected),
            vec![
                Violation::ExpectedFailure,
                Violation::NoMatch(Stream::Stderr, expected, String::new()),
            ]
        );
    }

    #[test]
    fn violations_render_readably() {
        let text = Violation::LongHelpLines(vec!["abc".to_owned()]).to_string();
        assert!(text.contains("too long"));
        assert!(text.contains("  abc"));
        assert_eq!(
            Violation::ExpectedFailure.to_string(),
            "expected a nonzero exit code, got 0"
        );
        let no_match = Violation::NoMatch(
            Stream::Stderr,
            pattern(r"initdb: error: \d+"),
            "ok\n".to_owned(),
        )
        .to_string();
        assert!(no_match.contains(r"/initdb: error: \d+/"), "{no_match}");
        assert!(no_match.contains("ok"), "{no_match}");
    }
}
