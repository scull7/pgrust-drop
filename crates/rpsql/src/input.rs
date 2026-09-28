//! Interactive input: `src/bin/psql/input.c`.
//!
//! Upstream reads a terminal through readline (`gets_interactive`,
//! `input.c:66`). This port reads it through a [`LineEditor`] (ADR-0005):
//! [`LinerEditor`] is the `redox_liner` one, and [`PlainEditor`] is the
//! `useReadline == false` path — prompt on stdout, then `gets_fromFile` —
//! that `-n` asks for. The history bookkeeping `MainLoop` does between
//! lines (`pg_append_history`, `pg_send_history`) and the history file's
//! name and contents are pure calculations here; reading and writing that
//! file are the editor's actions.

use std::ffi::OsStr;
use std::io::{BufRead, ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::mainloop::gets_from_file;
use crate::settings::HistControl;

/// `PSQLHISTORY` (`input.c:23`): the history file under the home directory.
pub const PSQLHISTORY: &str = ".psql_history";

/// `NL_IN_HISTORY` (`input.c:47`): what a newline inside a multi-line entry
/// becomes in the history file, so that it reloads as one entry.
pub const NL_IN_HISTORY: u8 = 0x01;

/// What one [`LineEditor::read_line`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// A line, its terminator stripped.
    Line(Vec<u8>),
    /// End of input: `readline()` returned `NULL` (control-D on an empty
    /// line, or the terminal went away).
    Eof,
    /// Control-C while waiting for input: upstream's SIGINT `siglongjmp`
    /// back into `MainLoop` (`mainloop.c:108`). The editor has already moved
    /// the terminal to a fresh line, which is the `putc('\n', stdout)` of
    /// `mainloop.c:125`, so `MainLoop` does not print one again.
    Interrupted,
}

/// A line editor for an interactive session (ADR-0005): readline's part of
/// `input.c`, kept small so the crate behind it can be swapped.
pub trait LineEditor {
    /// `gets_interactive(prompt, query_buf)` (`input.c:66`).
    fn read_line(&mut self, prompt: &str) -> Fetched;

    /// readline's `add_history(s)` (`input.c:171`): one finished entry.
    fn add_history(&mut self, entry: &[u8]);
}

/// `history_buf` (`mainloop.c:41`) and `pg_send_history`'s `prev_hist`
/// (`input.c:139`): the lines of an entry not yet handed to the editor, and
/// the last entry that was, for `HISTCONTROL=ignoredups`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryBuf {
    pending: Vec<u8>,
    prev: Option<Vec<u8>>,
}

impl HistoryBuf {
    /// `pg_append_history(s, history_buf)` (`input.c:114`): add a line,
    /// making sure it ends in a newline.
    pub fn append(&mut self, line: &[u8]) {
        self.pending.extend_from_slice(line);
        if line.last() != Some(&b'\n') {
            self.pending.push(b'\n');
        }
    }

    /// `pg_send_history(history_buf)` (`input.c:135`): the entry to hand to
    /// the editor, if `HISTCONTROL` keeps it, and the buffer emptied either
    /// way. Nothing is returned for an empty buffer, so extra calls are
    /// harmless.
    pub fn send(&mut self, histcontrol: HistControl) -> Option<Vec<u8>> {
        let mut entry = std::mem::take(&mut self.pending);
        // Trim any trailing `\n`s (`input.c:143`-`:146`).
        while entry.last() == Some(&b'\n') {
            entry.pop();
        }
        if entry.is_empty() {
            return None;
        }
        let ignore_space = matches!(
            histcontrol,
            HistControl::IgnoreSpace | HistControl::IgnoreBoth
        );
        let ignore_dups = matches!(
            histcontrol,
            HistControl::IgnoreDups | HistControl::IgnoreBoth
        );
        if (ignore_space && entry[0] == b' ')
            || (ignore_dups && self.prev.as_deref() == Some(&entry[..]))
        {
            return None;
        }
        self.prev = Some(entry.clone());
        Some(entry)
    }

    /// `resetPQExpBuffer(history_buf)`, after a Control-C at the prompt
    /// (`mainloop.c:115`).
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// The history file `initializeInput` settles on (`input.c:377`-`:396`):
/// the `HISTFILE` variable, else a non-empty `PSQL_HISTORY`, else
/// `~/.psql_history`; a leading `~` or `~/` is expanded (`expand_tilde`,
/// `common.c:2697`). `home` is `get_home_path` (`path.c:1022`): `$HOME`, when
/// set and not empty.
#[must_use]
pub fn history_file(
    histfile: Option<&str>,
    psql_history: Option<&OsStr>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let chosen: &OsStr = match (histfile, psql_history) {
        (Some(var), _) => OsStr::new(var),
        (None, Some(env)) if !env.is_empty() => env,
        (None, _) => return home.map(|home| home.join(PSQLHISTORY)),
    };
    Some(expand_tilde(Path::new(chosen), home))
}

/// `expand_tilde` (`common.c:2697`) for `~` and `~/…`. `~user` needs
/// `getpwnam`, which this port does not call, so it stays as written, as
/// upstream leaves it when `getpwnam` finds no such user.
fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) if rest.as_os_str().is_empty() => home.to_path_buf(),
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path.to_path_buf(),
    }
}

/// `read_history(psql_history)` then `decode_history()` (`input.c:398`-`:400`):
/// one entry per line, `NL_IN_HISTORY` turned back into `\n`.
#[must_use]
pub fn decode_history(file: &[u8]) -> Vec<Vec<u8>> {
    let mut lines: Vec<&[u8]> = file.split(|&c| c == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
        .into_iter()
        .map(|line| {
            line.iter()
                .map(|&c| if c == NL_IN_HISTORY { b'\n' } else { c })
                .collect()
        })
        .collect()
}

/// What `saveHistory(fname, max_lines)` leaves in the file (`input.c:438`-`:462`),
/// the `history_truncate_file` + `append_history` branch: the file cut to
/// its last `max(max_lines - added, 0)` lines, then the last
/// `min(max_lines, added)` entries of this session appended, `\n` inside an
/// entry encoded as `NL_IN_HISTORY`. A negative `max_lines` (`HISTSIZE=-1`)
/// truncates nothing and appends everything.
#[must_use]
pub fn saved_history(file: &[u8], added: &[Vec<u8>], max_lines: i32) -> Vec<u8> {
    let (keep, append) = match usize::try_from(max_lines) {
        Ok(max) => (max.saturating_sub(added.len()), max.min(added.len())),
        Err(_) => (usize::MAX, added.len()),
    };
    let mut lines: Vec<&[u8]> = file.split(|&c| c == b'\n').collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut out = Vec::new();
    for line in &lines[lines.len().saturating_sub(keep)..] {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    for entry in &added[added.len() - append..] {
        out.extend(
            entry
                .iter()
                .map(|&c| if c == b'\n' { NL_IN_HISTORY } else { c }),
        );
        out.push(b'\n');
    }
    out
}

/// The `useReadline == false` arm of `gets_interactive` (`input.c:104`-`:106`):
/// the prompt on stdout, then `gets_fromFile(stdin)`. History is off
/// (`useHistory` stays false), so entries are dropped. A Control-C here is
/// the SIGINT handler's; see `docs/divergences.md`.
pub struct PlainEditor<R, W> {
    input: R,
    output: W,
}

impl<R: BufRead, W: Write> PlainEditor<R, W> {
    #[must_use]
    pub fn new(input: R, output: W) -> Self {
        Self { input, output }
    }
}

impl<R: BufRead, W: Write> LineEditor for PlainEditor<R, W> {
    fn read_line(&mut self, prompt: &str) -> Fetched {
        let _ = self.output.write_all(prompt.as_bytes());
        let _ = self.output.flush();
        // A read error ends the input, as `gets_fromFile` answers one
        // (`input.c:213`-`:215`).
        match gets_from_file(&mut self.input) {
            Ok(Some(line)) => Fetched::Line(line),
            Ok(None) | Err(_) => Fetched::Eof,
        }
    }

    fn add_history(&mut self, _entry: &[u8]) {}
}

/// No completions: tab completion (`tab-complete.in.c`) is a later issue.
struct NoCompletion;

impl liner::Completer for NoCompletion {
    fn completions(&mut self, _start: &str) -> Vec<String> {
        Vec::new()
    }
}

/// The readline arm, through `redox_liner` (ADR-0005).
pub struct LinerEditor {
    context: liner::Context,
    /// `psql_history`
    file: Option<PathBuf>,
    /// This session's entries, `history_lines_added` of them.
    added: Vec<Vec<u8>>,
}

impl LinerEditor {
    /// `initializeInput(1)` (`input.c:361`): an editor, with the history
    /// file's entries loaded when it can be read.
    #[must_use]
    pub fn initialize(file: Option<PathBuf>) -> Self {
        let mut context = liner::Context::new();
        // readline keeps every entry and leaves duplicates to HISTCONTROL.
        context.history.set_max_buffers_size(usize::MAX);
        context.history.append_duplicate_entries = true;
        if let Some(bytes) = file.as_deref().and_then(|f| std::fs::read(f).ok()) {
            for entry in decode_history(&bytes) {
                let _ = context
                    .history
                    .push(String::from_utf8_lossy(&entry).into_owned().into());
            }
        }
        Self {
            context,
            file,
            added: Vec::new(),
        }
    }

    /// `finishInput` (`input.c:535`) → `saveHistory(psql_history, histsize)`
    /// (`input.c:412`). Writing `/dev/null` is skipped, as upstream skips it.
    ///
    /// # Errors
    /// The message `pg_log_error` prints when the file cannot be written.
    pub fn finish(&mut self, histsize: i32) -> Result<(), String> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        if file == Path::new("/dev/null") {
            return Ok(());
        }
        let existing = std::fs::read(&file).unwrap_or_default();
        std::fs::write(&file, saved_history(&existing, &self.added, histsize)).map_err(|err| {
            format!(
                "could not save history to file \"{}\": {err}",
                file.display()
            )
        })
    }
}

impl LineEditor for LinerEditor {
    fn read_line(&mut self, prompt: &str) -> Fetched {
        let _ = std::io::stdout().flush();
        match self
            .context
            .read_line(liner::Prompt::from(prompt), None, &mut NoCompletion)
        {
            Ok(line) => Fetched::Line(line.into_bytes()),
            Err(err) if err.kind() == ErrorKind::Interrupted => Fetched::Interrupted,
            Err(_) => Fetched::Eof,
        }
    }

    fn add_history(&mut self, entry: &[u8]) {
        let _ = self
            .context
            .history
            .push(String::from_utf8_lossy(entry).into_owned().into());
        self.added.push(entry.to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_gathers_its_lines_and_loses_its_trailing_newlines() {
        // `pg_append_history` (`input.c:114`), `pg_send_history` (`:143`).
        let mut history = HistoryBuf::default();
        history.append(b"select 1");
        history.append(b"+ 1;\n");
        assert_eq!(
            history.send(HistControl::None),
            Some(b"select 1\n+ 1;".to_vec())
        );
        assert_eq!(history.send(HistControl::None), None, "the buffer is empty");
        history.append(b"");
        assert_eq!(
            history.send(HistControl::None),
            None,
            "a blank entry is none"
        );
    }

    #[test]
    fn histcontrol_drops_what_it_names() {
        // `input.c:152`-`:157`.
        let mut history = HistoryBuf::default();
        history.append(b" secret;");
        assert_eq!(history.send(HistControl::IgnoreSpace), None);
        history.append(b" shown;");
        assert!(history.send(HistControl::IgnoreDups).is_some());
        history.append(b" shown;");
        assert_eq!(history.send(HistControl::IgnoreDups), None);
        history.append(b" shown;");
        assert!(history.send(HistControl::None).is_some());
        history.append(b"a;");
        history.append(b"x");
        history.reset();
        history.append(b"a;");
        assert!(history.send(HistControl::IgnoreBoth).is_some());
        history.append(b"a;");
        assert_eq!(history.send(HistControl::IgnoreBoth), None);
    }

    #[test]
    fn the_history_file_is_histfile_then_psql_history_then_home() {
        // `input.c:377`-`:396`.
        let home = Path::new("/home/u");
        assert_eq!(
            history_file(Some("~/h"), Some(OsStr::new("/env")), Some(home)),
            Some(PathBuf::from("/home/u/h"))
        );
        assert_eq!(
            history_file(None, Some(OsStr::new("/env")), Some(home)),
            Some(PathBuf::from("/env"))
        );
        assert_eq!(
            history_file(None, Some(OsStr::new("")), Some(home)),
            Some(PathBuf::from("/home/u/.psql_history"))
        );
        assert_eq!(history_file(None, None, None), None);
        assert_eq!(
            history_file(Some("~"), None, Some(home)),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(
            history_file(Some("~bob/h"), None, Some(home)),
            Some(PathBuf::from("~bob/h"))
        );
    }

    #[test]
    fn a_multi_line_entry_survives_the_file() {
        // `encode_history` / `decode_history` (`input.c:305`, `:327`).
        let saved = saved_history(b"", &[b"select\n1;".to_vec()], 500);
        assert_eq!(saved, b"select\x011;\n");
        assert_eq!(decode_history(&saved), [b"select\n1;".to_vec()]);
    }

    #[test]
    fn saving_truncates_the_file_then_appends_this_session() {
        // `input.c:446`-`:458`.
        let file = b"a\nb\nc\n";
        let added = [b"d".to_vec(), b"e".to_vec()];
        assert_eq!(saved_history(file, &added, 3), b"c\nd\ne\n");
        assert_eq!(saved_history(file, &added, 1), b"e\n");
        assert_eq!(saved_history(file, &added, 0), b"");
        assert_eq!(saved_history(file, &added, -1), b"a\nb\nc\nd\ne\n");
    }

    #[test]
    fn the_plain_editor_prompts_then_reads_a_line() {
        let mut out = Vec::new();
        let mut editor = PlainEditor::new(&b"select 1;\n"[..], &mut out);
        assert_eq!(
            editor.read_line("db=# "),
            Fetched::Line(b"select 1;".to_vec())
        );
        assert_eq!(editor.read_line("db=# "), Fetched::Eof);
        assert_eq!(out, b"db=# db=# ");
    }
}
