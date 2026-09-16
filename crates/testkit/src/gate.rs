//! The byte-diff gate: the same invocation through the reference C tool and
//! through ours, compared byte for byte.
//!
//! This is the method pgrust proves its own tools with, and the one
//! `docs/test-stealing.md` applies to every crate here: run both binaries on
//! identical arguments and identical stdin, apply the justified normalizations
//! in [`crate::normalize`] to both sides, then diff stdout, stderr and the
//! exit status. A gate never asserts that our output "looks right"; it asserts
//! that it is the reference's output.
//!
//! Data / Calculations / Actions:
//!
//! - [`Gate`] is the data describing an invocation, [`GateReport`] the verdict.
//! - [`compare`] is the whole comparison, pure over two [`CommandOutcome`]s and
//!   unit-tested without spawning anything.
//! - [`Gate::run`] is the only action: it spawns the two processes.
//!
//! When the reference binary is absent, [`Gate::for_tool`] yields `None` and
//! the caller prints [`crate::reference::skip_message`]: a skip is flagged,
//! never silent.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;

use crate::normalize::{self, Normalizer};
use crate::outcome::CommandOutcome;
use crate::{diff, reference, run};

/// How the reference side is labelled in a diff.
const REFERENCE_LABEL: &str = "reference";
/// How our side is labelled in a diff.
const CANDIDATE_LABEL: &str = "candidate";

/// One invocation to run through both binaries.
#[derive(Debug, Clone)]
pub struct Gate {
    /// The C PostgreSQL 18 tool, the authority.
    pub reference: PathBuf,
    /// Our Rust tool.
    pub candidate: PathBuf,
    /// Arguments handed to both, unchanged.
    pub args: Vec<OsString>,
    /// Bytes fed to both on stdin; empty means stdin is closed.
    pub stdin: Vec<u8>,
    /// Justified normalizations applied to both sides before diffing.
    pub normalizers: Vec<Normalizer>,
}

impl Gate {
    /// A gate between two known binaries, no arguments, no stdin, no
    /// normalization: the strictest possible comparison.
    #[must_use]
    pub fn new(reference: impl Into<PathBuf>, candidate: impl Into<PathBuf>) -> Self {
        Self {
            reference: reference.into(),
            candidate: candidate.into(),
            args: Vec::new(),
            stdin: Vec::new(),
            normalizers: Vec::new(),
        }
    }

    /// Locate the reference `tool` (see [`crate::reference::find`]) and gate
    /// `candidate` against it.
    ///
    /// `None` means the reference is not installed here; the caller must print
    /// [`crate::reference::skip_message`] and pass, so the skip is visible in
    /// the test output rather than a silently narrowed assertion.
    #[must_use]
    pub fn for_tool(tool: &str, candidate: impl Into<PathBuf>) -> Option<Self> {
        reference::find(tool).map(|reference| Self::new(reference, candidate))
    }

    /// Add one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add several arguments.
    #[must_use]
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    /// Feed these bytes to both binaries on stdin.
    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = stdin.into();
        self
    }

    /// Add one justified normalizer.
    #[must_use]
    pub fn normalizer(mut self, normalizer: Normalizer) -> Self {
        self.normalizers.push(normalizer);
        self
    }

    /// Add several justified normalizers.
    #[must_use]
    pub fn with_normalizers(mut self, normalizers: impl IntoIterator<Item = Normalizer>) -> Self {
        self.normalizers.extend(normalizers);
        self
    }

    /// Action: run both binaries on this invocation and compare them.
    ///
    /// # Errors
    /// The `io::Error` from spawning either binary.
    pub fn run(&self) -> std::io::Result<GateReport> {
        let reference = run::run_with_stdin(&self.reference, &self.args, &self.stdin)?;
        let candidate = run::run_with_stdin(&self.candidate, &self.args, &self.stdin)?;
        Ok(compare(&reference, &candidate, &self.normalizers))
    }

    /// Run the gate and fail the test with the full report unless it is clean.
    ///
    /// # Panics
    /// When either binary cannot be spawned, or the two disagree.
    pub fn assert_clean(&self) {
        let report = self
            .run()
            .unwrap_or_else(|err| panic!("{self}: could not run: {err}"));
        assert!(report.is_clean(), "{self}\n{report}");
    }
}

impl fmt::Display for Gate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "gate {} vs {}",
            self.reference.display(),
            self.candidate.display()
        )?;
        for arg in &self.args {
            write!(f, " {}", arg.to_string_lossy())?;
        }
        if !self.stdin.is_empty() {
            write!(f, " (stdin: {} bytes)", self.stdin.len())?;
        }
        if !self.normalizers.is_empty() {
            write!(f, " [normalized:")?;
            for normalizer in &self.normalizers {
                write!(f, " {}", normalizer.name)?;
            }
            write!(f, "]")?;
        }
        Ok(())
    }
}

/// The two exit statuses, `None` when a process was killed by a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RcCheck {
    pub reference: Option<i32>,
    pub candidate: Option<i32>,
}

impl RcCheck {
    #[must_use]
    pub fn matches(&self) -> bool {
        self.reference == self.candidate
    }
}

impl fmt::Display for RcCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "exit status: {} vs {}",
            describe_status(self.reference),
            describe_status(self.candidate)
        )
    }
}

fn describe_status(status: Option<i32>) -> String {
    status.map_or_else(|| "killed by a signal".to_owned(), |code| code.to_string())
}

/// What the gate found: a rendered diff per stream (`None` when that stream
/// matched) and the two exit statuses.
#[derive(Debug, Clone)]
pub struct GateReport {
    pub stdout_diff: Option<String>,
    pub stderr_diff: Option<String>,
    pub rc: RcCheck,
}

impl GateReport {
    /// Both streams identical (after normalization) and the same exit status.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.stdout_diff.is_none() && self.stderr_diff.is_none() && self.rc.matches()
    }
}

impl fmt::Display for GateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return f.write_str("gate clean: stdout, stderr and exit status all match");
        }
        if !self.rc.matches() {
            writeln!(f, "{}", self.rc)?;
        }
        for diff in [self.stdout_diff.as_ref(), self.stderr_diff.as_ref()]
            .into_iter()
            .flatten()
        {
            writeln!(f, "{diff}")?;
        }
        Ok(())
    }
}

/// Calculation: the whole gate verdict, pure over the two outcomes.
///
/// A stream is compared as bytes first, so an identical stream costs nothing
/// and needs no valid UTF-8. Only a mismatch is decoded, normalized and
/// diffed; a mismatching stream that is not valid UTF-8 is reported as such
/// rather than lossily decoded, because lossy decoding could turn two
/// different byte strings into one and pass a gate that should fail.
#[must_use]
pub fn compare(
    reference: &CommandOutcome,
    candidate: &CommandOutcome,
    normalizers: &[Normalizer],
) -> GateReport {
    GateReport {
        stdout_diff: compare_stream("stdout", &reference.stdout, &candidate.stdout, normalizers),
        stderr_diff: compare_stream("stderr", &reference.stderr, &candidate.stderr, normalizers),
        rc: RcCheck {
            reference: reference.status,
            candidate: candidate.status,
        },
    }
}

fn compare_stream(
    stream: &str,
    reference: &[u8],
    candidate: &[u8],
    normalizers: &[Normalizer],
) -> Option<String> {
    if reference == candidate {
        return None;
    }
    let (Ok(reference_text), Ok(candidate_text)) =
        (str::from_utf8(reference), str::from_utf8(candidate))
    else {
        return Some(binary_mismatch(stream, reference, candidate));
    };
    let reference_text = normalize::apply_all(reference_text, normalizers);
    let candidate_text = normalize::apply_all(candidate_text, normalizers);
    diff::unified(
        &reference_text,
        &candidate_text,
        &format!("{REFERENCE_LABEL} {stream}"),
        &format!("{CANDIDATE_LABEL} {stream}"),
    )
}

fn binary_mismatch(stream: &str, reference: &[u8], candidate: &[u8]) -> String {
    let at = reference
        .iter()
        .zip(candidate)
        .position(|(left, right)| left != right)
        .unwrap_or_else(|| reference.len().min(candidate.len()));
    format!(
        "{stream} differs and is not valid UTF-8, so it cannot be diffed as text: \
         first difference at byte {at} ({REFERENCE_LABEL} {} bytes, {CANDIDATE_LABEL} {} bytes)",
        reference.len(),
        candidate.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::{DEFAULT, TIMING};

    fn outcome(status: i32, stdout: &str, stderr: &str) -> CommandOutcome {
        CommandOutcome::new(Some(status), stdout.as_bytes(), stderr.as_bytes())
    }

    #[test]
    fn identical_outcomes_are_clean() {
        let theirs = outcome(0, "initdb (PostgreSQL) 18.6\n", "");
        let report = compare(&theirs, &theirs.clone(), &[]);
        assert!(report.is_clean(), "{report}");
        assert_eq!(report.stdout_diff, None);
        assert_eq!(report.stderr_diff, None);
        assert!(report.rc.matches());
    }

    #[test]
    fn a_stdout_difference_is_reported_as_a_unified_diff() {
        let theirs = outcome(0, "initdb (PostgreSQL) 18.6\n", "");
        let ours = outcome(0, "rinitdb (PostgreSQL) 18.6\n", "");
        let report = compare(&theirs, &ours, &[]);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.expect("stdout differs");
        assert!(
            diff.starts_with("--- reference stdout\n+++ candidate stdout\n"),
            "{diff}"
        );
        assert!(diff.contains("-initdb (PostgreSQL) 18.6"), "{diff}");
        assert!(diff.contains("+rinitdb (PostgreSQL) 18.6"), "{diff}");
    }

    #[test]
    fn a_stderr_difference_is_reported_separately() {
        let theirs = outcome(1, "", "initdb: error: invalid option\n");
        let ours = outcome(1, "", "error: unexpected argument\n");
        let report = compare(&theirs, &ours, &[]);
        assert_eq!(report.stdout_diff, None);
        assert!(report.stderr_diff.is_some());
        assert!(report.rc.matches());
        assert!(!report.is_clean());
    }

    #[test]
    fn a_differing_exit_status_alone_fails_the_gate() {
        let theirs = outcome(1, "", "");
        let ours = outcome(2, "", "");
        let report = compare(&theirs, &ours, &[]);
        assert_eq!(report.stdout_diff, None);
        assert_eq!(report.stderr_diff, None);
        assert!(!report.rc.matches());
        assert!(!report.is_clean());
        assert!(
            report.to_string().contains("exit status: 1 vs 2"),
            "{report}"
        );
    }

    #[test]
    fn a_signal_death_never_matches_an_exit_code() {
        let theirs = CommandOutcome::new(None, "", "");
        let ours = CommandOutcome::new(Some(0), "", "");
        let report = compare(&theirs, &ours, &[]);
        assert!(!report.rc.matches());
        assert!(
            report.to_string().contains("killed by a signal"),
            "{report}"
        );
    }

    #[test]
    fn a_normalized_difference_passes_the_gate() {
        let theirs = outcome(0, "SELECT 1\nTime: 1.234 ms\n", "");
        let ours = outcome(0, "SELECT 1\nTime: 9.876 ms\n", "");
        assert!(
            !compare(&theirs, &ours, &[]).is_clean(),
            "unnormalized gates must fail"
        );
        assert!(compare(&theirs, &ours, &[TIMING]).is_clean());
        assert!(compare(&theirs, &ours, &DEFAULT).is_clean());
    }

    #[test]
    fn a_normalizer_does_not_hide_a_real_difference_on_the_same_line() {
        let theirs = outcome(0, "Timing is on.\nTime: 1.0 ms\n", "");
        let ours = outcome(0, "Timing is off.\nTime: 2.0 ms\n", "");
        let report = compare(&theirs, &ours, &DEFAULT);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.expect("stdout differs");
        assert!(diff.contains("-Timing is on."), "{diff}");
        assert!(
            !diff.contains("Time: 1.0 ms"),
            "the timing line is normalized away: {diff}"
        );
    }

    #[test]
    fn a_trailing_newline_difference_fails_the_gate() {
        let theirs = outcome(0, "initdb (PostgreSQL) 18.6\n", "");
        let ours = outcome(0, "initdb (PostgreSQL) 18.6", "");
        let report = compare(&theirs, &ours, &DEFAULT);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.expect("stdout differs");
        assert!(diff.contains("\\ No newline at end of file"), "{diff}");
    }

    #[test]
    fn non_utf8_output_is_flagged_instead_of_decoded_lossily() {
        // Both decode to U+FFFD under from_utf8_lossy; a gate must not call
        // two different byte strings equal.
        let theirs = CommandOutcome::new(Some(0), vec![0xff], Vec::new());
        let ours = CommandOutcome::new(Some(0), vec![0xfe], Vec::new());
        let report = compare(&theirs, &ours, &DEFAULT);
        assert!(!report.is_clean());
        let message = report.stdout_diff.expect("stdout differs");
        assert!(message.contains("not valid UTF-8"), "{message}");
        assert!(message.contains("byte 0"), "{message}");
    }

    #[test]
    fn identical_non_utf8_output_is_still_clean() {
        let theirs = CommandOutcome::new(Some(0), vec![0xff, 0xfe], Vec::new());
        let report = compare(&theirs, &theirs.clone(), &DEFAULT);
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn the_gate_describes_its_invocation() {
        let gate = Gate::new("/usr/lib/postgresql/18/bin/initdb", "target/debug/rinitdb")
            .arg("--help")
            .with_stdin(b"\\q\n".to_vec())
            .with_normalizers(DEFAULT);
        let shown = gate.to_string();
        assert!(
            shown.contains("/usr/lib/postgresql/18/bin/initdb"),
            "{shown}"
        );
        assert!(shown.contains("target/debug/rinitdb"), "{shown}");
        assert!(shown.contains("--help"), "{shown}");
        assert!(shown.contains("stdin: 3 bytes"), "{shown}");
        assert!(shown.contains("timing"), "{shown}");
    }

    #[test]
    fn for_tool_is_none_when_the_reference_is_absent() {
        assert!(Gate::for_tool("no-such-postgres-tool", "target/debug/rinitdb").is_none());
    }
}
