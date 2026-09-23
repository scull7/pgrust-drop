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
//! When the reference binary is absent, [`Gate::for_tool_or_skip`] yields
//! `None` having *already* announced `SKIP (flagged, not silent)`, so the
//! caller cannot reach that arm with the skip unannounced: a skip is flagged,
//! never silent.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::env::Environment;
use crate::normalize::{self, Normalizer};
use crate::outcome::CommandOutcome;
use crate::{diff, reference, run};

/// Which of the two binaries a message is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The C PostgreSQL 18 tool, the authority.
    Reference,
    /// Our Rust tool.
    Candidate,
}

impl Side {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Side::Reference => "reference",
            Side::Candidate => "candidate",
        }
    }
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a gate could not produce a verdict.
///
/// Both binaries fail to spawn with the same bare `NotFound`, so the side and
/// the path are part of the error: "the candidate is not built" and "the
/// reference is not installed" need different fixes.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("could not run the {side} binary {}: {source}", path.display())]
    Spawn {
        side: Side,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Which streams a gate's verdict is allowed to rest on.
///
/// [`Scope::Everything`] is the default and the only scope a finished tool
/// should ever use. [`Scope::StderrAndStatus`] exists for a tool that is being
/// ported in chunks: the diagnostics of a failure can be made byte-exact long
/// before the progress output of the success path exists at all, and gating
/// the diagnostics now is strictly better than gating nothing.
///
/// It narrows the *verdict*, never the *report*: an out-of-scope stream is
/// still compared, still rendered, and [`GateReport::out_of_scope_difference`]
/// says so, which is what `assert_clean` announces on the process's own
/// stderr. A narrowing that cannot be seen in the log is the one AGENTS.md
/// forbids; this one is on screen every time it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// stdout, stderr and the exit status must all match.
    Everything,
    /// Only stderr and the exit status decide the verdict.
    StderrAndStatus {
        /// Why stdout is out of scope, printed with every flagged difference.
        because: &'static str,
    },
}

impl Scope {
    /// Does a stdout difference fail the gate?
    #[must_use]
    pub fn judges_stdout(self) -> bool {
        matches!(self, Scope::Everything)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Everything => f.write_str("stdout, stderr and exit status"),
            Scope::StderrAndStatus { because } => {
                write!(f, "stderr and exit status only ({because})")
            }
        }
    }
}

/// The flag an out-of-scope difference carries, so it is greppable in a log
/// exactly like [`crate::reference::SKIP_FLAG`].
pub const OUT_OF_SCOPE_FLAG: &str = "OUT OF SCOPE (flagged, not silent)";

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
    /// Which streams the verdict rests on; [`Scope::Everything`] by default.
    pub scope: Scope,
    /// The environment both binaries are run in; inherited by default.
    ///
    /// Both sides get the same one — a gate whose two halves saw different
    /// `PGHOST`s would be comparing two different questions.
    pub env: Environment,
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
            scope: Scope::Everything,
            env: Environment::inherited(),
        }
    }

    /// Locate the reference `tool` (see [`crate::reference::find`]) and gate
    /// `candidate` against it, announcing the skip when it is not there.
    ///
    /// This is the constructor a gate should use. `None` means the reference
    /// is not installed here **and** [`crate::reference::skip`] has already
    /// put `SKIP (flagged, not silent)` on the process's own stderr, so the
    /// caller has nothing left to do but return. There is no way to reach the
    /// `None` arm with the skip unannounced: the announcement is a guarantee,
    /// not a call-site convention.
    ///
    /// ```no_run
    /// # use testkit::Gate;
    /// let Some(gate) = Gate::for_tool_or_skip("initdb", "target/debug/rinitdb") else {
    ///     return; // The flagged skip is already on stderr.
    /// };
    /// gate.arg("--version").assert_clean();
    /// ```
    #[must_use]
    pub fn for_tool_or_skip(tool: &str, candidate: impl Into<PathBuf>) -> Option<Self> {
        Self::for_located_tool(tool, candidate, reference::find, reference::skip)
    }

    /// The lookup and the announcement it owes, with both actions injected.
    ///
    /// Taking `find` and `announce_skip` as arguments is the same seam
    /// [`crate::reference::locate`] opens with `exists`: it lets the pairing
    /// this function exists to guarantee — absent reference implies announced
    /// skip — be asserted in a unit test, with no filesystem and no captured
    /// stderr to read back.
    fn for_located_tool(
        tool: &str,
        candidate: impl Into<PathBuf>,
        find: impl FnOnce(&str) -> Option<PathBuf>,
        announce_skip: impl FnOnce(&str),
    ) -> Option<Self> {
        let Some(reference) = find(tool) else {
            announce_skip(tool);
            return None;
        };
        Some(Self::new(reference, candidate))
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

    /// Judge this gate on stderr and the exit status alone, saying why.
    ///
    /// For an invocation whose *diagnostics* are ported but whose success-path
    /// stdout is not yet. The stdout difference is still computed and still
    /// shown; see [`Scope`].
    #[must_use]
    pub fn stderr_and_status_only(mut self, because: &'static str) -> Self {
        self.scope = Scope::StderrAndStatus { because };
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

    /// Run both binaries in this environment instead of the inherited one.
    #[must_use]
    pub fn with_env(mut self, env: Environment) -> Self {
        self.env = env;
        self
    }

    /// Action: run both binaries on this invocation and compare them.
    ///
    /// # Errors
    /// [`GateError::Spawn`], naming which binary could not be run.
    pub fn run(&self) -> Result<GateReport, GateError> {
        let reference = self.spawn(Side::Reference, &self.reference)?;
        let candidate = self.spawn(Side::Candidate, &self.candidate)?;
        Ok(compare(
            &reference,
            &candidate,
            &self.normalizers,
            self.scope,
        ))
    }

    fn spawn(&self, side: Side, bin: &Path) -> Result<CommandOutcome, GateError> {
        run::run_in(bin, &self.args, &self.stdin, &self.env).map_err(|source| GateError::Spawn {
            side,
            path: bin.to_path_buf(),
            source,
        })
    }

    /// Run the gate and fail the test with the full report unless it is clean.
    ///
    /// # Panics
    /// When either binary cannot be spawned, or the two disagree.
    pub fn assert_clean(&self) {
        let report = self
            .run()
            .unwrap_or_else(|err| panic!("{self}: could not run: {err}"));
        if let Some(difference) = report.out_of_scope_difference() {
            reference::announce_skip(&format!("{OUT_OF_SCOPE_FLAG}: {self}\n{difference}"));
        }
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
        if !self.env.is_inherited() {
            write!(f, " [env: {}]", self.env)?;
        }
        if !self.scope.judges_stdout() {
            write!(f, " [judged on: {}]", self.scope)?;
        }
        Ok(())
    }
}

/// What a gate made of one output stream.
///
/// An enum rather than an `Option<String>`: "matched", "differs, here is the
/// diff" and "differs but cannot be diffed as text" are three outcomes a
/// caller may want to branch on, and AGENTS.md rules out stringly-typed state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamDiff {
    /// Identical bytes, or identical after the justified normalizations.
    Match,
    /// Differs; the rendered `diff -U3`.
    Text(String),
    /// Differs, and at least one side is not valid UTF-8, so it cannot be
    /// diffed as text without `from_utf8_lossy` possibly mapping two different
    /// byte strings onto one.
    Binary {
        /// Offset of the first differing byte.
        at: usize,
        reference_len: usize,
        candidate_len: usize,
    },
}

impl StreamDiff {
    /// The two sides agree on this stream.
    #[must_use]
    pub fn matches(&self) -> bool {
        matches!(self, StreamDiff::Match)
    }

    /// The rendered diff, when there is one to show.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            StreamDiff::Text(diff) => Some(diff),
            _ => None,
        }
    }
}

impl fmt::Display for StreamDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamDiff::Match => Ok(()),
            StreamDiff::Text(diff) => f.write_str(diff),
            StreamDiff::Binary {
                at,
                reference_len,
                candidate_len,
            } => write!(
                f,
                "differs and is not valid UTF-8, so it cannot be diffed as text: \
                 first difference at byte {at} (reference {reference_len} bytes, \
                 candidate {candidate_len} bytes)"
            ),
        }
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
    pub stdout_diff: StreamDiff,
    pub stderr_diff: StreamDiff,
    pub rc: RcCheck,
    /// Which streams this verdict rests on.
    pub scope: Scope,
}

impl GateReport {
    /// Every stream the [`Scope`] judges is identical (after normalization)
    /// and the exit statuses agree.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        let stdout_ok = self.stdout_diff.matches() || !self.scope.judges_stdout();
        stdout_ok && self.stderr_diff.matches() && self.rc.matches()
    }

    /// A rendered difference the [`Scope`] chose not to judge, if there is one.
    ///
    /// The caller prints it; a narrowed gate that hides what it narrowed is
    /// the failure mode AGENTS.md rules out.
    #[must_use]
    pub fn out_of_scope_difference(&self) -> Option<String> {
        if self.scope.judges_stdout() || self.stdout_diff.matches() {
            return None;
        }
        Some(format!("{}\nstdout {}", self.scope, self.stdout_diff))
    }
}

impl fmt::Display for GateReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return write!(f, "gate clean, judged on {}", self.scope);
        }
        if !self.rc.matches() {
            writeln!(f, "{}", self.rc)?;
        }
        for (stream, diff) in [("stdout", &self.stdout_diff), ("stderr", &self.stderr_diff)] {
            match diff {
                StreamDiff::Match => {}
                StreamDiff::Text(text) => writeln!(f, "{text}")?,
                StreamDiff::Binary { .. } => writeln!(f, "{stream} {diff}")?,
            }
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
    scope: Scope,
) -> GateReport {
    GateReport {
        scope,
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
) -> StreamDiff {
    if reference == candidate {
        return StreamDiff::Match;
    }
    let (Ok(reference_text), Ok(candidate_text)) =
        (str::from_utf8(reference), str::from_utf8(candidate))
    else {
        return binary_mismatch(reference, candidate);
    };
    let reference_text = normalize::apply_all(reference_text, normalizers);
    let candidate_text = normalize::apply_all(candidate_text, normalizers);
    diff::unified(
        &reference_text,
        &candidate_text,
        &format!("{} {stream}", Side::Reference),
        &format!("{} {stream}", Side::Candidate),
    )
    .map_or(StreamDiff::Match, StreamDiff::Text)
}

fn binary_mismatch(reference: &[u8], candidate: &[u8]) -> StreamDiff {
    StreamDiff::Binary {
        at: reference
            .iter()
            .zip(candidate)
            .position(|(left, right)| left != right)
            .unwrap_or_else(|| reference.len().min(candidate.len())),
        reference_len: reference.len(),
        candidate_len: candidate.len(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::normalize::{DEFAULT, TIMING};
    use crate::reference::RefPolicy;

    fn outcome(status: i32, stdout: &str, stderr: &str) -> CommandOutcome {
        CommandOutcome::new(Some(status), stdout.as_bytes(), stderr.as_bytes())
    }

    #[test]
    fn identical_outcomes_are_clean() {
        let theirs = outcome(0, "initdb (PostgreSQL) 18.6\n", "");
        let report = compare(&theirs, &theirs.clone(), &[], Scope::Everything);
        assert!(report.is_clean(), "{report}");
        assert_eq!(report.stdout_diff, StreamDiff::Match);
        assert_eq!(report.stderr_diff, StreamDiff::Match);
        assert!(report.rc.matches());
    }

    #[test]
    fn a_stdout_difference_is_reported_as_a_unified_diff() {
        let theirs = outcome(0, "initdb (PostgreSQL) 18.6\n", "");
        let ours = outcome(0, "rinitdb (PostgreSQL) 18.6\n", "");
        let report = compare(&theirs, &ours, &[], Scope::Everything);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.text().expect("stdout differs as text");
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
        let report = compare(&theirs, &ours, &[], Scope::Everything);
        assert_eq!(report.stdout_diff, StreamDiff::Match);
        assert!(!report.stderr_diff.matches());
        assert!(report.rc.matches());
        assert!(!report.is_clean());
    }

    #[test]
    fn a_differing_exit_status_alone_fails_the_gate() {
        let theirs = outcome(1, "", "");
        let ours = outcome(2, "", "");
        let report = compare(&theirs, &ours, &[], Scope::Everything);
        assert_eq!(report.stdout_diff, StreamDiff::Match);
        assert_eq!(report.stderr_diff, StreamDiff::Match);
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
        let report = compare(&theirs, &ours, &[], Scope::Everything);
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
            !compare(&theirs, &ours, &[], Scope::Everything).is_clean(),
            "unnormalized gates must fail"
        );
        assert!(compare(&theirs, &ours, &[TIMING], Scope::Everything).is_clean());
        assert!(compare(&theirs, &ours, &DEFAULT, Scope::Everything).is_clean());
    }

    #[test]
    fn a_normalizer_does_not_hide_a_real_difference_on_the_same_line() {
        let theirs = outcome(0, "Timing is on.\nTime: 1.0 ms\n", "");
        let ours = outcome(0, "Timing is off.\nTime: 2.0 ms\n", "");
        let report = compare(&theirs, &ours, &DEFAULT, Scope::Everything);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.text().expect("stdout differs as text");
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
        let report = compare(&theirs, &ours, &DEFAULT, Scope::Everything);
        assert!(!report.is_clean());
        let diff = report.stdout_diff.text().expect("stdout differs as text");
        assert!(diff.contains("\\ No newline at end of file"), "{diff}");
    }

    #[test]
    fn non_utf8_output_is_flagged_instead_of_decoded_lossily() {
        // Both decode to U+FFFD under from_utf8_lossy; a gate must not call
        // two different byte strings equal.
        let theirs = CommandOutcome::new(Some(0), vec![0xff], Vec::new());
        let ours = CommandOutcome::new(Some(0), vec![0xfe], Vec::new());
        let report = compare(&theirs, &ours, &DEFAULT, Scope::Everything);
        assert!(!report.is_clean());
        assert_eq!(
            report.stdout_diff,
            StreamDiff::Binary {
                at: 0,
                reference_len: 1,
                candidate_len: 1
            }
        );
        let shown = report.to_string();
        assert!(
            shown.contains("stdout differs and is not valid UTF-8"),
            "{shown}"
        );
    }

    #[test]
    fn identical_non_utf8_output_is_still_clean() {
        let theirs = CommandOutcome::new(Some(0), vec![0xff, 0xfe], Vec::new());
        let report = compare(&theirs, &theirs.clone(), &DEFAULT, Scope::Everything);
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
    fn an_absent_reference_announces_its_skip_before_yielding_none() {
        let announced = RefCell::new(Vec::new());

        let gate = Gate::for_located_tool(
            "initdb",
            "target/debug/rinitdb",
            |_| None,
            |tool| announced.borrow_mut().push(tool.to_owned()),
        );

        assert!(gate.is_none());
        assert_eq!(announced.into_inner(), ["initdb"]);
    }

    #[test]
    fn a_located_reference_is_gated_without_any_skip() {
        let announced = RefCell::new(Vec::new());

        let gate = Gate::for_located_tool(
            "initdb",
            "target/debug/rinitdb",
            |tool| Some(Path::new("/ref/bin").join(tool)),
            |tool| announced.borrow_mut().push(tool.to_owned()),
        );

        let gate = gate.expect("a located reference yields a gate");
        assert_eq!(gate.reference, PathBuf::from("/ref/bin/initdb"));
        assert_eq!(gate.candidate, PathBuf::from("target/debug/rinitdb"));
        assert!(announced.into_inner().is_empty());
    }

    #[test]
    fn for_tool_or_skip_follows_the_active_policy_when_the_reference_is_absent() {
        // The real wiring, announcement included. Which arm runs is decided by
        // the policy this process was started under, so the test asserts that
        // arm rather than assuming the permissive default: locally the
        // `SKIP (flagged, not silent)` line this prints on stderr is the
        // mechanism working, and in CI, where PGDROP_REQUIRE_REF is set, the
        // same absent reference failing the gate is the mechanism working.
        let attempt = std::panic::catch_unwind(|| {
            Gate::for_tool_or_skip("no-such-postgres-tool", "target/debug/rinitdb").is_none()
        });

        match reference::policy() {
            RefPolicy::Skip => {
                assert!(attempt.expect("the permissive policy announces, it never panics"));
            }
            RefPolicy::Require => {
                let panic = attempt.expect_err("the strict policy fails the gate");
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "(panic payload is not a String)".to_owned());
                assert!(message.contains(reference::REQUIRE_REF_ENV), "{message}");
                assert!(message.contains("no-such-postgres-tool"), "{message}");
            }
        }
    }

    #[test]
    fn a_narrowed_scope_still_fails_on_stderr_and_on_the_exit_status() {
        let scope = Scope::StderrAndStatus {
            because: "cluster creation lands later",
        };
        let theirs = outcome(1, "creating directory ... ok\n", "initdb: error: boom\n");

        let wrong_stderr = outcome(1, "creating directory ... ok\n", "initdb: error: bang\n");
        assert!(!compare(&theirs, &wrong_stderr, &[], scope).is_clean());

        let wrong_status = outcome(2, "creating directory ... ok\n", "initdb: error: boom\n");
        assert!(!compare(&theirs, &wrong_status, &[], scope).is_clean());
    }

    #[test]
    fn a_narrowed_scope_forgives_stdout_but_reports_it() {
        let scope = Scope::StderrAndStatus {
            because: "cluster creation lands later",
        };
        let theirs = outcome(1, "creating directory ... ok\n", "initdb: error: boom\n");
        let ours = outcome(1, "", "initdb: error: boom\n");
        let report = compare(&theirs, &ours, &[], scope);

        assert!(report.is_clean(), "{report}");
        let flagged = report
            .out_of_scope_difference()
            .expect("the stdout difference is still reported");
        assert!(
            flagged.contains("cluster creation lands later"),
            "{flagged}"
        );
        assert!(flagged.contains("creating directory ... ok"), "{flagged}");
        // The same invocation under the default scope is a failure.
        assert!(!compare(&theirs, &ours, &[], Scope::Everything).is_clean());
    }

    #[test]
    fn nothing_is_flagged_when_the_out_of_scope_stream_matches_anyway() {
        let scope = Scope::StderrAndStatus { because: "any" };
        let theirs = outcome(1, "same\n", "initdb: error: boom\n");
        let report = compare(&theirs, &theirs.clone(), &[], scope);
        assert_eq!(report.out_of_scope_difference(), None);
        assert_eq!(
            compare(&theirs, &theirs.clone(), &[], Scope::Everything).out_of_scope_difference(),
            None
        );
    }

    #[test]
    fn the_gate_line_says_when_the_verdict_is_narrowed() {
        let gate = Gate::new("/ref/initdb", "/our/rinitdb")
            .arg("--sync-only")
            .stderr_and_status_only("cluster creation lands later");
        let line = gate.to_string();
        assert!(
            line.contains("judged on: stderr and exit status only"),
            "{line}"
        );
        assert!(line.contains("cluster creation lands later"), "{line}");
        assert!(
            !Gate::new("/ref/initdb", "/our/rinitdb")
                .to_string()
                .contains("judged on")
        );
    }
}
