//! What a spawned command produced.

use std::process::Output;

/// Exit status, stdout and stderr of one command run.
///
/// Bytes, not strings: psql and initdb can legitimately emit non-UTF-8 when the
/// client encoding says so, and a gate must compare what was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// `None` when the process was killed by a signal.
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutcome {
    /// Build an outcome by hand, for unit tests of the checks.
    #[must_use]
    pub fn new(
        status: Option<i32>,
        stdout: impl Into<Vec<u8>>,
        stderr: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            status,
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    /// Exit status 0 with nothing on either stream.
    #[must_use]
    pub fn silent_success() -> Self {
        Self::new(Some(0), Vec::new(), Vec::new())
    }

    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.status == Some(0)
    }

    /// stdout decoded leniently for messages and regex checks.
    #[must_use]
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// stderr decoded leniently for messages and regex checks.
    #[must_use]
    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

impl From<Output> for CommandOutcome {
    fn from(output: Output) -> Self {
        Self {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}
