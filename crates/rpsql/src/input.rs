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

use liner::KeyMap as _;
use termion::input::TermRead as _;
use termion::raw::IntoRawMode as _;

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

    /// readline's `add_history(s)` (`input.c:163`): one finished entry.
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

/// `read_history(psql_history)` then `decode_history()` (`input.c:393`-`:394`):
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
///
/// It drives `liner::Editor` with the Emacs keymap itself rather than calling
/// `Context::read_line`, because that builds a fresh `stdin().keys()` for
/// every line, and termion's key iterator reads two bytes at a time and keeps
/// the second: when a line's Enter is the first of the two, the next line's
/// first byte died with the iterator (a pasted `\echo ab\n\warn cd\n` gave
/// `warn cd`). One iterator for the whole session keeps that byte, as
/// readline's one input stream does (ADR-0005, 2026-09-27 amendment).
///
/// Even one iterator keeps that second byte to itself until its next key, so
/// it reads through [`KeyBytes`], which never gives it a byte past the key
/// it is parsing.
pub struct LinerEditor {
    context: liner::Context,
    /// The session's only key source: see above.
    keys: termion::input::Keys<KeyBytes>,
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
            keys: KeyBytes.keys(),
            file,
            added: Vec::new(),
        }
    }

    /// `finishInput` (`input.c:540`) → `saveHistory(psql_history, histsize)`
    /// (`input.c:412`). Writing `/dev/null` is skipped, as upstream skips it.
    /// A new file is created `0600`, as `saveHistory` creates it
    /// (`input.c:452`), since entries can carry passwords; an existing
    /// file keeps its mode.
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
        write_history_file(&file, &saved_history(&existing, &self.added, histsize)).map_err(|err| {
            format!(
                "could not save history to file \"{}\": {}",
                file.display(),
                strerror(&err)
            )
        })
    }
}

/// The process's stdin as termion's key iterator should see it: one byte a
/// read, or two when the first is ESC. termion reads two bytes at a time to
/// tell a lone ESC from an escape sequence, and parks the second in its own
/// `leftover` when the first is a key by itself. Parked behind the Enter that
/// ends `COPY … FROM STDIN;`, that byte is the first of the COPY data, which
/// `StdinLines` then never sees: a pasted `xyz` row went in as `yz`, and the
/// `x` began the next query. Handing termion no second byte unless it follows
/// ESC leaves every byte past the key in stdin's buffer, where the COPY reader
/// and upstream's one `FILE *` find it. stdin is locked only for each read,
/// as `StdinLines` locks it.
pub struct KeyBytes;

impl std::io::Read for KeyBytes {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let mut stdin = std::io::stdin().lock();
        let avail = stdin.fill_buf()?;
        let n = match avail.first() {
            None => 0,
            Some(&0x1B) => avail.len().min(out.len()).min(2),
            Some(_) => out.len().min(1),
        };
        out[..n].copy_from_slice(&avail[..n]);
        stdin.consume(n);
        Ok(n)
    }
}

/// Replace the history file's contents, creating it `0600` if it is missing
/// (`input.c:452`'s `open(fname, O_CREAT | O_WRONLY | PG_BINARY, 0600)`).
fn write_history_file(file: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(file)?
        .write_all(contents)
}

/// `%m`: `strerror(errno)`, without the ` (os error N)` Rust adds.
fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .map_or_else(|| text.clone(), str::to_owned),
        None => text,
    }
}

impl LineEditor for LinerEditor {
    fn read_line(&mut self, prompt: &str) -> Fetched {
        let _ = std::io::stdout().flush();
        // Raw mode only while the line is edited: dropping `editor` at the
        // end restores the terminal, so a query runs with ISIG back on and a
        // Control-C is the SIGINT handler's.
        let Ok(out) = std::io::stdout().into_raw_mode() else {
            return Fetched::Eof;
        };
        let Ok(mut editor) =
            liner::Editor::new(out, liner::Prompt::from(prompt), None, &mut self.context)
        else {
            return Fetched::Eof;
        };
        let mut keymap = liner::Emacs::new();
        keymap.init(&mut editor);
        // `Context::handle_keys`, over the session's iterator.
        while let Some(Ok(key)) = self.keys.next() {
            match keymap.handle_key(key, &mut editor, &mut NoCompletion) {
                Ok(true) => return Fetched::Line(String::from(editor).into_bytes()),
                Ok(false) => {}
                Err(err) if err.kind() == ErrorKind::Interrupted => return Fetched::Interrupted,
                Err(_) => return Fetched::Eof,
            }
        }
        // End of input: readline hands back a partial line before `NULL`.
        let line = String::from(editor);
        if line.is_empty() {
            Fetched::Eof
        } else {
            Fetched::Line(line.into_bytes())
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
    fn a_new_history_file_is_0600_and_an_old_one_keeps_its_mode() {
        // `input.c:452`: `open(fname, O_CREAT | O_WRONLY | PG_BINARY, 0600)`.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("rpsql-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mode = |file: &Path| std::fs::metadata(file).unwrap().permissions().mode() & 0o777;

        let new = dir.join("new");
        let _ = std::fs::remove_file(&new);
        write_history_file(&new, b"a\n").unwrap();
        assert_eq!(mode(&new), 0o600);

        let old = dir.join("old");
        std::fs::write(&old, b"x\ny\n").unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o640)).unwrap();
        write_history_file(&old, b"a\n").unwrap();
        assert_eq!(mode(&old), 0o640);
        assert_eq!(std::fs::read(&old).unwrap(), b"a\n");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_save_error_is_rendered_as_percent_m() {
        let err = std::io::Error::from_raw_os_error(13);
        assert!(err.to_string().ends_with(" (os error 13)"));
        assert!(!strerror(&err).contains("os error"));
        assert_eq!(strerror(&std::io::Error::other("x")), "x");
    }

    #[test]
    fn an_invalid_utf8_byte_is_dropped_with_up_to_three_bytes_after_it() {
        // A divergence (docs/divergences.md): readline inserts the byte.
        // termion's key iterator gathers up to four bytes looking for a
        // character, then reports them as one unsupported event, which its
        // `Keys` skips; the Enter among them is lost too.
        let keys: Vec<_> = (&b"\xffab\ncd"[..]).keys().map(Result::unwrap).collect();
        assert_eq!(
            keys,
            [
                termion::event::Key::Char('c'),
                termion::event::Key::Char('d')
            ]
        );
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
