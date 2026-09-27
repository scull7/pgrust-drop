//! Where query output goes: `pset.queryFout`, `\o`, `\g`'s file or pipe, and
//! the shell status a closed pipe leaves behind.
//!
//! Ports `openQueryOutputFile` and `setQFout` (`common.c:58`, `:146`),
//! `SetShellResultVariables` (`common.c:518`), and `wait_result_to_str` and
//! `wait_result_to_exit_code` (`src/common/wait_error.c:33`, `:138`).
//! `popen(command, …)` is `/bin/sh -c command` with one end of a pipe, as
//! POSIX specifies it, and `pclose` is the wait for that shell.
//!
//! The calculations — what a wait status means — are pure functions over the
//! raw status; opening, writing and waiting are the thin actions around them.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};

use crate::copy::strerror;
use crate::logging;
use crate::settings::PsqlSettings;
use crate::variables::VariableSpace;

/// Which end of the pipe `popen` hands back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopenMode {
    /// `"r"`: psql reads the command's stdout.
    Read,
    /// `"w"`: psql writes the command's stdin.
    Write,
}

/// `popen(command, mode)`: `/bin/sh -c command`, with the command's stdin
/// (for [`PopenMode::Write`]) or stdout (for [`PopenMode::Read`]) a pipe to
/// psql and its other streams psql's own.
///
/// # Errors
/// The shell could not be started.
pub fn popen(command: &[u8], mode: PopenMode) -> std::io::Result<Child> {
    let mut shell = Command::new("/bin/sh");
    shell.arg("-c").arg(OsStr::from_bytes(command));
    match mode {
        PopenMode::Read => shell.stdout(Stdio::piped()),
        PopenMode::Write => shell.stdin(Stdio::piped()),
    };
    shell.spawn()
}

/// `pclose()`: close psql's end of the pipe and wait for the shell.
///
/// # Errors
/// The wait itself failed, which `pclose` reports as `-1` with `errno` set.
pub fn pclose(mut child: Child) -> std::io::Result<i32> {
    drop(child.stdin.take());
    drop(child.stdout.take());
    child.wait().map(ExitStatus::into_raw)
}

/// A stream psql opened for output (`FILE *` from `fopen` or `popen`).
pub enum OutputFile {
    /// `fopen(fname, "w")`.
    File(BufWriter<File>),
    /// `popen(fname + 1, "w")`.
    Pipe {
        /// The shell's stdin, buffered as a stdio stream is.
        stdin: BufWriter<ChildStdin>,
        /// The shell.
        child: Child,
    },
}

impl OutputFile {
    /// The open half of `openQueryOutputFile` (`common.c:58`) for a name
    /// that is not empty: a leading `|` runs the rest as a shell command.
    ///
    /// # Errors
    /// The file could not be created or the shell not started.
    ///
    /// # Panics
    /// Never: `popen` pipes the stdin it was asked to.
    pub fn open(fname: &[u8]) -> std::io::Result<Self> {
        if let Some(command) = fname.strip_prefix(b"|") {
            let mut child = popen(command, PopenMode::Write)?;
            let stdin = child.stdin.take().expect("popen(\"w\") pipes stdin");
            Ok(Self::Pipe {
                stdin: BufWriter::new(stdin),
                child,
            })
        } else {
            File::create(OsStr::from_bytes(fname)).map(|f| Self::File(BufWriter::new(f)))
        }
    }

    /// `fclose` or `pclose`: for a pipe, the wait status
    /// `SetShellResultVariables` is given; for a file, `None`. Neither `\o`
    /// nor `\g` looks at `fclose`'s result, so neither does this.
    #[must_use]
    pub fn close(self) -> Option<i32> {
        match self {
            Self::File(mut file) => {
                let _ = file.flush();
                None
            }
            Self::Pipe { mut stdin, child } => {
                let _ = stdin.flush();
                drop(stdin);
                Some(pclose(child).unwrap_or(-1))
            }
        }
    }
}

impl Write for OutputFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::File(file) => file.write(buf),
            Self::Pipe { stdin, .. } => stdin.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            Self::Pipe { stdin, .. } => stdin.flush(),
        }
    }
}

/// What `openQueryOutputFile` opened: `*fout`.
pub enum Opened {
    /// No name, or an empty one: stdout.
    Stdout,
    /// A file or a pipe.
    File(OutputFile),
}

/// psql's stdout and `pset.queryFout`, which is stdout until `-o` or `\o`
/// names a file or a pipe.
///
/// Upstream keeps the two apart by the stream each `printf`/`fprintf` names:
/// query results, command tags, `\qecho` and COPY OUT data go to
/// `pset.queryFout`; `ECHO`, `\timing`, `\echo` and the listings of `\set`
/// and `\pset` go to stdout.
pub struct Output<'a> {
    /// psql's own stdout.
    pub stdout: &'a mut dyn Write,
    /// `pset.queryFout` when it is not stdout.
    query_fout: Option<OutputFile>,
}

impl<'a> Output<'a> {
    /// Output with `pset.queryFout = stdout` (`startup.c:158`).
    pub fn new(stdout: &'a mut dyn Write) -> Self {
        Self {
            stdout,
            query_fout: None,
        }
    }

    /// `pset.queryFout`.
    pub fn query_fout(&mut self) -> &mut dyn Write {
        match &mut self.query_fout {
            Some(file) => file,
            None => &mut *self.stdout,
        }
    }

    /// `pset.queryFout == stdout`.
    #[must_use]
    pub fn query_fout_is_stdout(&self) -> bool {
        self.query_fout.is_none()
    }

    /// `fflush(NULL)`, which upstream calls before every `popen` so that
    /// what psql wrote so far comes out before the command's own output.
    pub fn flush_all(&mut self) {
        let _ = self.stdout.flush();
        if let Some(file) = &mut self.query_fout {
            let _ = file.flush();
        }
    }

    /// `setQFout()` (`common.c:146`): open `fname` (stdout for `None` or an
    /// empty name), and only then close the old file or pipe, whose shell
    /// status goes to `SHELL_ERROR` and `SHELL_EXIT_CODE`. On failure the
    /// error is logged and nothing changes.
    pub fn set_query_fout(
        &mut self,
        fname: Option<&[u8]>,
        pset: &PsqlSettings,
        vars: &mut VariableSpace,
        stderr: &mut dyn Write,
    ) -> bool {
        let new = match self.open_query_output_file(fname, pset, stderr) {
            Some(Opened::Stdout) => None,
            Some(Opened::File(file)) => Some(file),
            None => return false,
        };
        if let Some(old) = std::mem::replace(&mut self.query_fout, new)
            && let Some(status) = old.close()
        {
            set_shell_result_variables(vars, status);
        }
        true
    }

    /// `openQueryOutputFile()` (`common.c:58`), or `None` when the file or
    /// pipe could not be opened, which has been logged as `"%s: %m"` over
    /// the name as typed, `|` and all.
    pub fn open_query_output_file(
        &mut self,
        fname: Option<&[u8]>,
        pset: &PsqlSettings,
        stderr: &mut dyn Write,
    ) -> Option<Opened> {
        let Some(fname) = fname.filter(|f| !f.is_empty()) else {
            return Some(Opened::Stdout);
        };
        if fname.starts_with(b"|") {
            self.flush_all();
        }
        match OutputFile::open(fname) {
            Ok(file) => Some(Opened::File(file)),
            Err(err) => {
                let mut message = fname.to_vec();
                message.extend_from_slice(b": ");
                message.extend_from_slice(strerror(&err).as_bytes());
                logging::error(pset, message, stderr);
                None
            }
        }
    }

    /// `setQFout(NULL)` at the end of `main` (`startup.c:477`): close a file
    /// or pipe `-o` or `\o` left open, waiting for the pipe's command.
    pub fn close(&mut self) {
        if let Some(old) = self.query_fout.take() {
            let _ = old.close();
        }
    }
}

impl Drop for Output<'_> {
    /// Whatever way `main` ends, a pipe is flushed and its command waited
    /// for, as `exit()` flushes every stdio stream.
    fn drop(&mut self) {
        self.close();
    }
}

/// `SetShellResultVariables()` (`common.c:518`).
pub fn set_shell_result_variables(vars: &mut VariableSpace, wait_result: i32) {
    let error = if wait_result == 0 { "false" } else { "true" };
    let _ = vars.set("SHELL_ERROR", Some(error));
    let code = wait_result_to_exit_code(wait_result).to_string();
    let _ = vars.set("SHELL_EXIT_CODE", Some(&code));
}

/// `wait_result_to_exit_code()` (`wait_error.c:138`): the shell's `$?` for
/// a wait status — the exit code, `128 +` the signal, or `-1` passed
/// through.
#[must_use]
pub fn wait_result_to_exit_code(exit_status: i32) -> i32 {
    if exit_status == -1 {
        return -1;
    }
    let status = ExitStatus::from_raw(exit_status);
    if let Some(code) = status.code() {
        return code;
    }
    if let Some(signal) = status.signal() {
        return 128 + signal;
    }
    -1
}

/// `wait_result_to_str()` (`wait_error.c:33`): why a child process ended.
/// `errno_text` is what `%m` expands to for a status of `-1`.
#[must_use]
pub fn wait_result_to_str(exit_status: i32, errno_text: &str) -> String {
    if exit_status == -1 {
        return errno_text.to_owned();
    }
    let status = ExitStatus::from_raw(exit_status);
    if let Some(code) = status.code() {
        return match code {
            126 => "command not executable".to_owned(),
            127 => "command not found".to_owned(),
            _ => format!("child process exited with exit code {code}"),
        };
    }
    if let Some(signal) = status.signal() {
        return format!(
            "child process was terminated by signal {signal}: {}",
            pg_strsignal(signal)
        );
    }
    format!("child process exited with unrecognized status {exit_status}")
}

/// `pg_strsignal()` (`src/port/pgstrsignal.c`), which is `strsignal`, for
/// the signals whose description glibc, musl and macOS agree on; macOS's
/// `strsignal` appends the number (`Terminated: 15`). The rest read
/// `unrecognized signal`, `pg_strsignal`'s own fallback; see
/// `docs/divergences.md`.
#[must_use]
pub fn pg_strsignal(signal: i32) -> String {
    let Some(description) = signal_description(signal) else {
        return "unrecognized signal".to_owned();
    };
    if cfg!(target_os = "macos") {
        format!("{description}: {signal}")
    } else {
        description.to_owned()
    }
}

/// The text the C libraries share for `signal`, if it is one of the eight.
fn signal_description(signal: i32) -> Option<&'static str> {
    Some(match signal {
        1 => "Hangup",
        2 => "Interrupt",
        3 => "Quit",
        9 => "Killed",
        11 => "Segmentation fault",
        13 => "Broken pipe",
        14 => "Alarm clock",
        15 => "Terminated",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wait status `waitpid` reports for `exit(code)`.
    fn exited(code: i32) -> i32 {
        code << 8
    }

    #[test]
    fn an_exit_code_is_the_shells_dollar_question_mark() {
        assert_eq!(wait_result_to_exit_code(0), 0);
        assert_eq!(wait_result_to_exit_code(exited(3)), 3);
        assert_eq!(wait_result_to_exit_code(exited(127)), 127);
        // A signal is 128 plus its number, and -1 passes through.
        assert_eq!(wait_result_to_exit_code(15), 143);
        assert_eq!(wait_result_to_exit_code(-1), -1);
    }

    #[test]
    fn a_wait_status_reads_the_way_wait_error_c_words_it() {
        assert_eq!(
            wait_result_to_str(exited(1), ""),
            "child process exited with exit code 1"
        );
        assert_eq!(
            wait_result_to_str(exited(126), ""),
            "command not executable"
        );
        assert_eq!(wait_result_to_str(exited(127), ""), "command not found");
        let terminated = if cfg!(target_os = "macos") {
            "child process was terminated by signal 15: Terminated: 15"
        } else {
            "child process was terminated by signal 15: Terminated"
        };
        assert_eq!(wait_result_to_str(15, ""), terminated);
        assert_eq!(
            wait_result_to_str(-1, "No child processes"),
            "No child processes"
        );
    }

    #[test]
    fn shell_error_is_false_only_for_a_clean_exit() {
        let mut vars = VariableSpace::new();
        set_shell_result_variables(&mut vars, 0);
        assert_eq!(vars.get("SHELL_ERROR"), Some("false"));
        assert_eq!(vars.get("SHELL_EXIT_CODE"), Some("0"));
        set_shell_result_variables(&mut vars, exited(2));
        assert_eq!(vars.get("SHELL_ERROR"), Some("true"));
        assert_eq!(vars.get("SHELL_EXIT_CODE"), Some("2"));
        set_shell_result_variables(&mut vars, 9);
        assert_eq!(vars.get("SHELL_EXIT_CODE"), Some("137"));
    }

    #[test]
    fn a_pipe_is_waited_for_and_its_status_kept() {
        let mut pipe = OutputFile::open(b"|cat >/dev/null; exit 4").unwrap();
        pipe.write_all(b"data\n").unwrap();
        assert_eq!(pipe.close(), Some(exited(4)));
        let pipe = OutputFile::open(b"|true").unwrap();
        assert_eq!(pipe.close(), Some(0));
    }

    #[test]
    fn set_query_fout_swaps_the_stream_and_closes_the_old_pipe() {
        let dir = std::env::temp_dir().join(format!("rpsql-output-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("o.out");
        let pset = PsqlSettings::default();
        let mut vars = VariableSpace::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        {
            let mut out = Output::new(&mut stdout);
            assert!(out.query_fout_is_stdout());
            assert!(out.set_query_fout(
                Some(path.as_os_str().as_bytes()),
                &pset,
                &mut vars,
                &mut stderr
            ));
            out.query_fout().write_all(b"to the file\n").unwrap();
            assert!(out.set_query_fout(Some(b"|exit 3"), &pset, &mut vars, &mut stderr));
            // Closing a file sets nothing.
            assert_eq!(vars.get("SHELL_EXIT_CODE"), None);
            assert!(out.set_query_fout(None, &pset, &mut vars, &mut stderr));
            assert_eq!(vars.get("SHELL_EXIT_CODE"), Some("3"));
            out.query_fout().write_all(b"to stdout\n").unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"to the file\n");
        assert_eq!(stdout, b"to stdout\n");
        assert!(stderr.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_logged_and_changes_nothing() {
        let pset = PsqlSettings::default();
        let mut vars = VariableSpace::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut out = Output::new(&mut stdout);
        assert!(!out.set_query_fout(
            Some(b"/nonexistent/dir/file"),
            &pset,
            &mut vars,
            &mut stderr
        ));
        assert!(out.query_fout_is_stdout());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "psql: error: /nonexistent/dir/file: No such file or directory\n"
        );
    }
}
