//! The input loop: `src/bin/psql/mainloop.c`.
//!
//! `MainLoop()` (`mainloop.c:33`) reads lines, feeds them to the lexer, and
//! sends a statement whenever the lexer finds a semicolon. This port keeps the
//! same shape with two substitutions: lines come from an [`Input`] rather
//! than a `FILE *` — a [`LineSource`] for a script, a
//! [`crate::input::LineEditor`] at a terminal — and queries go to a
//! [`crate::common::Executor`]. A Ctrl-C stops a script (`mainloop.c:88`,
//! through [`Session::cancel_pressed`]); at the prompt it arrives from the
//! editor as [`Fetched::Interrupted`] instead of as a `siglongjmp`.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::command::{CommandResult, dispatch_slash};
use crate::common::{Executor, send_query};
use crate::input::{Fetched, HistoryBuf, LineEditor};
use crate::prompt::get_prompt;
use crate::scan::{PromptStatus, ScanResult, Scanner};
use crate::settings::{EXIT_BADCONN, EXIT_SUCCESS, EXIT_USER, PsqlSettings};
use crate::variables::{VarView, VariableSpace};

/// Where `MainLoop` reads: `MainLoop(FILE *source)`, with
/// `pset.cur_cmd_interactive = ((source == stdin) && !pset.notty)`
/// (`mainloop.c:65`) decided by the caller in choosing the variant.
pub enum Input<'a> {
    /// `gets_fromFile(source)` (`mainloop.c:170`): a script.
    Script(&'a mut dyn LineSource),
    /// `gets_interactive(get_prompt(…), query_buf)` (`mainloop.c:166`):
    /// stdin at a terminal.
    Terminal(&'a mut dyn LineEditor),
}

/// Where `MainLoop` gets its lines. `None` is end of input.
pub trait LineSource {
    /// One line, with its terminator already stripped (`gets_fromFile`,
    /// `input.c:186`).
    fn next_line(&mut self) -> Option<Vec<u8>>;
}

/// A [`LineSource`] over bytes already in memory, which is what `-f -` and the
/// tests both need.
pub struct Lines {
    lines: std::vec::IntoIter<Vec<u8>>,
}

impl Lines {
    /// Split `input` on `\n`, dropping a trailing empty piece so that a file
    /// ending in a newline does not produce one extra empty line.
    #[must_use]
    pub fn new(input: &[u8]) -> Self {
        let mut lines: Vec<Vec<u8>> = input.split(|&c| c == b'\n').map(<[u8]>::to_vec).collect();
        if lines.last().is_some_and(Vec::is_empty) {
            lines.pop();
        }
        Self {
            lines: lines.into_iter(),
        }
    }
}

impl LineSource for Lines {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        self.lines.next()
    }
}

/// A [`LineSource`] that reads as it goes, one `gets_fromFile` (`input.c:186`)
/// per line, so that a script on a pipe runs statement by statement while the
/// pipe is still open — `020_cancel.pl` never closes psql's stdin.
pub struct ReadLines<R> {
    reader: R,
    error: Option<std::io::Error>,
}

impl<R: BufRead> ReadLines<R> {
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            error: None,
        }
    }

    /// The read error that ended the input early, if one did: `gets_fromFile`
    /// reports it (`input.c:213`-`:215`) and then answers end of file.
    pub fn take_error(&mut self) -> Option<std::io::Error> {
        self.error.take()
    }
}

impl<R: BufRead> LineSource for ReadLines<R> {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        let mut line = Vec::new();
        match self.reader.read_until(b'\n', &mut line) {
            Ok(0) => None,
            Ok(_) => {
                // Only the one `\n` goes (`input.c:230`).
                if line.last() == Some(&b'\n') {
                    line.pop();
                }
                Some(line)
            }
            Err(err) => {
                self.error = Some(err);
                None
            }
        }
    }
}

/// The session state `MainLoop` mutates as it goes.
pub struct Session<'a> {
    /// `pset`
    pub pset: &'a mut PsqlSettings,
    /// `pset.vars`
    pub vars: &'a mut VariableSpace,
    /// `cancel_pressed` (`common.c:323`): [`crate::cancel::CANCEL_PRESSED`]
    /// in a live session, a flag of the test's own in a unit test.
    pub cancel_pressed: &'a AtomicBool,
}

/// `MainLoop()` (`mainloop.c:33`). Returns the process exit status.
// The body is one `for (;;)` of upstream's, statement for statement; cutting
// it up would hide that correspondence without removing a single branch.
#[allow(clippy::too_many_lines)]
pub fn main_loop(
    mut input: Input<'_>,
    session: &mut Session<'_>,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let mut scanner = Scanner::new();
    let mut query_buf: Vec<u8> = Vec::new();
    let mut previous_buf: Vec<u8> = Vec::new();
    let mut history_buf = HistoryBuf::default();
    let mut success_result = EXIT_SUCCESS;
    let mut slash_status = CommandResult::Unknown;
    let mut prompt_status = PromptStatus::Ready;
    let mut count_eof = 0;
    let mut die_on_error = false;
    let mut success;
    let mut line_saved_in_history;

    // Establish the new source (`mainloop.c:58`-`:66`), putting the prior
    // one's back on the way out.
    let prev_cmd_interactive = session.pset.cur_cmd_interactive;
    let prev_lineno = session.pset.lineno;
    session.pset.cur_cmd_interactive = matches!(input, Input::Terminal(_));
    session.pset.lineno = 0;
    session.pset.stmt_lineno = 1;

    'lines: while success_result == EXIT_SUCCESS {
        // Clean up after a previous Control-C (`mainloop.c:88`-`:100`): it
        // stops a script, and is forgotten at an interactive prompt.
        if session.cancel_pressed.load(Ordering::SeqCst) {
            if !session.pset.cur_cmd_interactive {
                success_result = EXIT_USER;
                break;
            }
            session.cancel_pressed.store(false, Ordering::SeqCst);
        }

        let _ = stdout.flush();

        // Get another line (`mainloop.c:147`-`:173`).
        let fetched = match &mut input {
            Input::Terminal(editor) => {
                // May need to reset prompt, eg after \r command.
                if query_buf.is_empty() {
                    prompt_status = PromptStatus::Ready;
                }
                let prompt =
                    get_prompt(prompt_status, session.pset, &executor.prompt_facts(), true);
                editor.read_line(&prompt)
            }
            Input::Script(source) => source.next_line().map_or(Fetched::Eof, Fetched::Line),
        };

        let line = match fetched {
            Fetched::Line(line) => line,
            // Control-C at the prompt: what the `siglongjmp` lands on
            // (`mainloop.c:108`-`:136`). There is no `\if` stack yet
            // (NAT-402), so no `\if: escaped`.
            Fetched::Interrupted => {
                scanner.finish();
                scanner.reset();
                query_buf.clear();
                history_buf.reset();
                count_eof = 0;
                slash_status = CommandResult::Unknown;
                prompt_status = PromptStatus::Ready;
                session.pset.stmt_lineno = 1;
                session.cancel_pressed.store(false, Ordering::SeqCst);
                continue;
            }
            // No more input. Time to quit, or \i done (`mainloop.c:182`).
            Fetched::Eof => {
                if session.pset.cur_cmd_interactive {
                    // This tries to mimic bash's IGNOREEOF feature.
                    count_eof += 1;
                    if count_eof < session.pset.ignoreeof {
                        if !session.pset.quiet {
                            let _ =
                                writeln!(stdout, "Use \"\\q\" to leave {}.", session.pset.progname);
                        }
                        continue;
                    }
                    let _ = writeln!(stdout, "{}", if session.pset.quiet { "" } else { "\\q" });
                }
                break;
            }
        };

        count_eof = 0;
        session.pset.lineno += 1;

        // Detect attempts to run custom-format dumps as SQL (`mainloop.c:209`).
        if session.pset.lineno == 1
            && !session.pset.cur_cmd_interactive
            && line.starts_with(b"PGDMP")
        {
            let _ = stdout.write_all(
                b"The input is a PostgreSQL custom-format dump.\n\
                  Use the pg_restore command-line client to restore this dump to a database.\n\n",
            );
            success_result = crate::settings::EXIT_FAILURE;
            break;
        }

        // No further processing of empty lines, unless within a literal.
        if line.is_empty() && !scanner.in_quote() {
            continue;
        }

        // Recognize "help", "quit", "exit" only in interactive mode
        // (`mainloop.c:228`-`:340`).
        if session.pset.cur_cmd_interactive {
            match assistance_word(&line) {
                Some(Assistance::Help) if query_buf.is_empty() => {
                    let _ = stdout.write_all(HELP_TEXT.as_bytes());
                    let _ = stdout.flush();
                    continue;
                }
                Some(Assistance::Help) => {
                    let _ = writeln!(
                        stdout,
                        "Use \\? for help or press control-C to clear the input buffer."
                    );
                }
                Some(Assistance::ExitOrQuit) if query_buf.is_empty() => {
                    let _ = stdout.flush();
                    success_result = EXIT_SUCCESS;
                    break;
                }
                Some(Assistance::ExitOrQuit) => {
                    let _ = writeln!(
                        stdout,
                        "{}",
                        if backslash_q_is_active(prompt_status) {
                            "Use \\q to quit."
                        } else {
                            "Use control-D to quit."
                        }
                    );
                }
                Some(Assistance::BackslashQ)
                    if !query_buf.is_empty() && !backslash_q_is_active(prompt_status) =>
                {
                    let _ = writeln!(stdout, "Use control-D to quit.");
                }
                _ => {}
            }
        }

        // ECHO=all echoes the input line, unless interactive (`mainloop.c:360`).
        if session.pset.echo == crate::settings::Echo::All && !session.pset.cur_cmd_interactive {
            let _ = stdout.write_all(&line);
            let _ = stdout.write_all(b"\n");
        }

        // Insert newlines into the query buffer between source lines.
        let mut added_nl_pos = if query_buf.is_empty() {
            None
        } else {
            query_buf.push(b'\n');
            Some(query_buf.len())
        };

        // Setting this will not have effect until the next line.
        die_on_error = session.pset.on_error_stop;

        scanner.setup(&line, true);
        success = true;
        line_saved_in_history = false;

        while success || !die_on_error {
            let (scan_result, prompt_tmp) = scanner.scan(&mut query_buf, &VarView(session.vars));
            prompt_status = prompt_tmp;
            if scan_result == ScanResult::Eol {
                session.pset.stmt_lineno += 1;
            }

            if scan_result == ScanResult::Semicolon
                || (scan_result == ScanResult::Eol && session.pset.singleline)
            {
                // Save line in history (`mainloop.c:429`).
                if session.pset.cur_cmd_interactive && !line_saved_in_history {
                    history_buf.append(&line);
                    send_history(&mut history_buf, &mut input, session.pset);
                    line_saved_in_history = true;
                }

                success = send_query(executor, &query_buf, session.pset, stdout, stderr);
                slash_status = if success {
                    CommandResult::Send
                } else {
                    CommandResult::Error
                };
                session.pset.stmt_lineno = 1;
                std::mem::swap(&mut previous_buf, &mut query_buf);
                query_buf.clear();
                added_nl_pos = None;
            } else if scan_result == ScanResult::Backslash {
                // A line holding only a backslash command leaves the query
                // buffer untouched, and forces out any previous lines as a
                // history entry of their own (`mainloop.c:475`).
                if Some(query_buf.len()) == added_nl_pos {
                    query_buf.pop();
                    send_history(&mut history_buf, &mut input, session.pset);
                }
                added_nl_pos = None;

                // Save backslash command in history (`mainloop.c:491`).
                if session.pset.cur_cmd_interactive && !line_saved_in_history {
                    history_buf.append(&line);
                    send_history(&mut history_buf, &mut input, session.pset);
                    line_saved_in_history = true;
                }

                slash_status =
                    dispatch_slash(&mut scanner, session.pset, session.vars, stdout, stderr);
                success = slash_status != CommandResult::Error;
                session.pset.stmt_lineno = 1;

                match slash_status {
                    CommandResult::Send => {
                        success = send_query(executor, &query_buf, session.pset, stdout, stderr);
                        std::mem::swap(&mut previous_buf, &mut query_buf);
                        query_buf.clear();
                    }
                    CommandResult::Terminate => break,
                    _ => {}
                }
            }

            if matches!(scan_result, ScanResult::Incomplete | ScanResult::Eol) {
                break;
            }
        }

        // Add line to pending history if we didn't do so already; flush it
        // out while the query buffer is empty (`mainloop.c:570`).
        if session.pset.cur_cmd_interactive {
            if !line_saved_in_history {
                history_buf.append(&line);
            }
            if query_buf.is_empty() {
                send_history(&mut history_buf, &mut input, session.pset);
            }
        }

        scanner.finish();

        if slash_status == CommandResult::Terminate {
            success_result = EXIT_SUCCESS;
            break 'lines;
        }
        if !session.pset.cur_cmd_interactive {
            if !success && die_on_error {
                success_result = EXIT_USER;
            } else if !executor.connected() {
                success_result = EXIT_BADCONN;
            }
        }
    }

    // A non-semicolon-terminated query at end of file is still processed
    // (`mainloop.c:598`).
    if !query_buf.is_empty() && !session.pset.cur_cmd_interactive && success_result == EXIT_SUCCESS
    {
        let ok = send_query(executor, &query_buf, session.pset, stdout, stderr);
        if !ok && die_on_error {
            success_result = EXIT_USER;
        } else if !executor.connected() {
            success_result = EXIT_BADCONN;
        }
    }

    // Restore the prior command source (`mainloop.c:660`-`:662`).
    session.pset.cur_cmd_interactive = prev_cmd_interactive;
    session.pset.lineno = prev_lineno;

    success_result
}

/// `pg_send_history(history_buf)` (`input.c:135`): hand the entry to the
/// editor. A script has no editor, and upstream's `useHistory` is false.
fn send_history(history_buf: &mut HistoryBuf, input: &mut Input<'_>, pset: &PsqlSettings) {
    if let Some(entry) = history_buf.send(pset.histcontrol)
        && let Input::Terminal(editor) = input
    {
        editor.add_history(&entry);
    }
}

/// What `help` prints on an empty query buffer (`mainloop.c:298`-`:303`).
const HELP_TEXT: &str = "You are using psql, the command-line interface to PostgreSQL.\n\
Type:  \\copyright for distribution terms\n       \
\\h for help with SQL commands\n       \
\\? for help with psql commands\n       \
\\g or terminate with semicolon to execute query\n       \
\\q to quit\n";

/// One of the words `MainLoop` answers only at a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assistance {
    /// `help`
    Help,
    /// `exit` or `quit`
    ExitOrQuit,
    /// `\q`, reported only where it is not a command.
    BackslashQ,
}

/// Calculation: `mainloop.c:230`-`:282`. The word must start the line, in
/// any case; `help`, `exit` and `quit` count only when nothing but white
/// space and at most one semicolon follow, while `\q` counts regardless.
#[must_use]
pub fn assistance_word(line: &[u8]) -> Option<Assistance> {
    let prefix =
        |word: &[u8]| line.len() >= word.len() && line[..word.len()].eq_ignore_ascii_case(word);
    let (found, rest) = if prefix(b"help") {
        (Assistance::Help, &line[4..])
    } else if prefix(b"exit") || prefix(b"quit") {
        (Assistance::ExitOrQuit, &line[4..])
    } else if line.starts_with(b"\\q") {
        return Some(Assistance::BackslashQ);
    } else {
        return None;
    };
    // C's `isspace` in the C locale.
    let space = |c: &u8| matches!(c, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r');
    let mut rest = rest;
    while rest.first().is_some_and(space) {
        rest = &rest[1..];
    }
    if rest.first() == Some(&b';') {
        rest = &rest[1..];
    }
    while rest.first().is_some_and(space) {
        rest = &rest[1..];
    }
    rest.is_empty().then_some(found)
}

/// Whether `\q` would be read as a command in this prompt state
/// (`mainloop.c:314`-`:316`, `:335`-`:337`): not inside a quote or comment.
fn backslash_q_is_active(status: PromptStatus) -> bool {
    matches!(
        status,
        PromptStatus::Ready | PromptStatus::Continue | PromptStatus::Paren
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ErrorMessage;
    use rlibpq::{Backend, QueryResult, QueryRunner, TransactionStatus};

    /// An executor that records the queries it was asked to run and answers
    /// each with a CommandComplete.
    struct Recorder {
        seen: Vec<String>,
        fail: Vec<bool>,
        connected: bool,
    }

    impl Recorder {
        fn new() -> Self {
            Self {
                seen: Vec::new(),
                fail: Vec::new(),
                connected: true,
            }
        }
    }

    impl Executor for Recorder {
        fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, ErrorMessage> {
            self.seen.push(String::from_utf8_lossy(query).into_owned());
            let fails = self.fail.first().copied().unwrap_or(false);
            if !self.fail.is_empty() {
                self.fail.remove(0);
            }
            let mut runner = QueryRunner::new();
            if fails {
                runner
                    .push(Backend::ErrorResponse(rlibpq::ResultError::new(vec![
                        (b'S', b"ERROR".to_vec()),
                        (b'M', b"boom".to_vec()),
                    ])))
                    .unwrap();
            } else {
                runner
                    .push(Backend::CommandComplete(b"OK".to_vec()))
                    .unwrap();
            }
            runner
                .push(Backend::ReadyForQuery(TransactionStatus::Idle))
                .unwrap();
            Ok(runner.into_results())
        }

        fn connected(&self) -> bool {
            self.connected
        }
    }

    struct Outcome {
        code: u8,
        stdout: String,
        stderr: String,
        seen: Vec<String>,
    }

    fn run(input: &str, pset: PsqlSettings) -> Outcome {
        let mut pset = pset;
        let mut vars = VariableSpace::new();
        let cancel_pressed = AtomicBool::new(false);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &cancel_pressed,
        };
        let mut executor = Recorder::new();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = main_loop(
            Input::Script(&mut Lines::new(input.as_bytes())),
            &mut session,
            &mut executor,
            &mut stdout,
            &mut stderr,
        );
        Outcome {
            code,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            seen: executor.seen,
        }
    }

    #[test]
    fn each_semicolon_sends_one_statement() {
        let out = run("select 1;\nselect 2;\n", PsqlSettings::default());
        assert_eq!(out.seen, ["select 1;", "select 2;"]);
        assert_eq!(out.code, EXIT_SUCCESS);
    }

    #[test]
    fn a_statement_spanning_lines_is_joined_with_newlines() {
        let out = run("select\n1;\n", PsqlSettings::default());
        assert_eq!(out.seen, ["select\n1;"]);
    }

    #[test]
    fn a_trailing_statement_without_a_semicolon_is_still_sent() {
        // `mainloop.c:598`.
        let out = run("select 1", PsqlSettings::default());
        assert_eq!(out.seen, ["select 1"]);
    }

    #[test]
    fn a_backslash_command_on_its_own_line_leaves_the_query_buffer_alone() {
        // `mainloop.c:475`.
        let out = run("select 1\n\\echo hi\n;\n", PsqlSettings::default());
        assert_eq!(out.stdout.lines().next(), Some("hi"));
        assert_eq!(out.seen, ["select 1\n;"]);
    }

    #[test]
    fn quit_ends_the_loop_before_the_rest_of_the_input() {
        let out = run("\\q\nselect 1;\n", PsqlSettings::default());
        assert!(out.seen.is_empty());
        assert_eq!(out.code, EXIT_SUCCESS);
    }

    #[test]
    fn on_error_stop_stops_at_the_first_failure() {
        let mut pset = PsqlSettings {
            on_error_stop: true,
            ..PsqlSettings::default()
        };
        let mut vars = VariableSpace::new();
        vars.set("ON_ERROR_STOP", Some("on")).unwrap();
        let cancel_pressed = AtomicBool::new(false);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &cancel_pressed,
        };
        let mut executor = Recorder::new();
        executor.fail = vec![false, true, false];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = main_loop(
            Input::Script(&mut Lines::new(b"select 1;\nselect 2;\nselect 3;\n")),
            &mut session,
            &mut executor,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_USER);
        assert_eq!(executor.seen, ["select 1;", "select 2;"]);
    }

    #[test]
    fn without_on_error_stop_the_loop_carries_on() {
        let mut pset = PsqlSettings {
            on_error_stop: false,
            ..PsqlSettings::default()
        };
        let mut vars = VariableSpace::new();
        let cancel_pressed = AtomicBool::new(false);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &cancel_pressed,
        };
        let mut executor = Recorder::new();
        executor.fail = vec![true, false];
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = main_loop(
            Input::Script(&mut Lines::new(b"select 1;\nselect 2;\n")),
            &mut session,
            &mut executor,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(code, EXIT_SUCCESS);
        assert_eq!(executor.seen.len(), 2);
    }

    #[test]
    fn echo_all_prints_every_input_line_exactly_once() {
        // `mainloop.c:360` echoes the input line and `SendQuery` does not
        // (`common.c:1158`); echoing in both printed every query twice, and a
        // `starts_with` assertion did not notice.
        let pset = PsqlSettings {
            echo: crate::settings::Echo::All,
            ..PsqlSettings::default()
        };
        let out = run("select 1;\nselect 2;\n", pset);
        assert_eq!(out.stdout.matches("select 1;").count(), 1, "{}", out.stdout);
        assert_eq!(out.stdout.matches("select 2;").count(), 1, "{}", out.stdout);
        assert!(out.stdout.starts_with("select 1;\n"), "{}", out.stdout);
    }

    #[test]
    fn singleline_mode_sends_at_end_of_line() {
        let pset = PsqlSettings {
            singleline: true,
            ..PsqlSettings::default()
        };
        let out = run("select 1\n", pset);
        assert_eq!(out.seen, ["select 1"]);
    }

    #[test]
    fn a_custom_format_dump_is_detected_on_the_first_line() {
        // `mainloop.c:209`.
        let out = run("PGDMP\x01\x02\n", PsqlSettings::default());
        assert_eq!(out.code, crate::settings::EXIT_FAILURE);
        assert!(
            out.stdout
                .starts_with("The input is a PostgreSQL custom-format dump.")
        );
        assert!(out.seen.is_empty());
    }

    #[test]
    fn a_set_inside_the_loop_changes_the_settings_it_reads() {
        let out = run("\\set ECHO all\nselect 1;\n", PsqlSettings::default());
        assert!(out.stdout.contains("select 1;"), "{}", out.stdout);
        assert_eq!(out.stderr, "");
    }

    #[test]
    fn a_control_c_stops_a_script_before_its_next_line() {
        // `mainloop.c:88`-`:96`: "You get here if you stopped a script with
        // Ctrl-C."
        let mut pset = PsqlSettings::default();
        let mut vars = VariableSpace::new();
        let cancel_pressed = AtomicBool::new(true);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &cancel_pressed,
        };
        let mut executor = Recorder::new();
        let code = main_loop(
            Input::Script(&mut Lines::new(b"select 1;\nselect 2")),
            &mut session,
            &mut executor,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(code, EXIT_USER);
        assert!(
            executor.seen.is_empty(),
            "nothing is sent, not even the unterminated statement at the end: {:?}",
            executor.seen
        );
        assert!(
            cancel_pressed.load(Ordering::SeqCst),
            "a script leaves the flag set"
        );
    }

    /// A terminal: the lines to type, and what the loop showed and kept.
    struct Keyboard {
        keys: std::collections::VecDeque<Fetched>,
        prompts: Vec<String>,
        history: Vec<String>,
    }

    impl Keyboard {
        fn new(keys: Vec<Fetched>) -> Self {
            Self {
                keys: keys.into(),
                prompts: Vec::new(),
                history: Vec::new(),
            }
        }
    }

    impl LineEditor for Keyboard {
        fn read_line(&mut self, prompt: &str) -> Fetched {
            self.prompts.push(prompt.to_string());
            self.keys.pop_front().unwrap_or(Fetched::Eof)
        }

        fn add_history(&mut self, entry: &[u8]) {
            self.history
                .push(String::from_utf8_lossy(entry).into_owned());
        }
    }

    fn line(text: &str) -> Fetched {
        Fetched::Line(text.as_bytes().to_vec())
    }

    /// A superuser connected to `postgres`, so PROMPT1 is `postgres=# `.
    struct Connected(Recorder);

    impl Executor for Connected {
        fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, ErrorMessage> {
            self.0.exec(query)
        }

        fn connected(&self) -> bool {
            self.0.connected()
        }

        fn prompt_facts(&self) -> crate::prompt::PromptFacts {
            crate::prompt::PromptFacts {
                dbname: Some("postgres".to_string()),
                superuser: true,
                ..crate::prompt::PromptFacts::default()
            }
        }
    }

    struct Typed {
        code: u8,
        stdout: String,
        seen: Vec<String>,
        keyboard: Keyboard,
        cancel_pressed: bool,
    }

    fn type_in(keys: Vec<Fetched>, pset: PsqlSettings, cancel_pressed: bool) -> Typed {
        let mut pset = pset;
        let mut vars = VariableSpace::new();
        let flag = AtomicBool::new(cancel_pressed);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &flag,
        };
        let mut keyboard = Keyboard::new(keys);
        let mut executor = Connected(Recorder::new());
        let mut stdout = Vec::new();
        let code = main_loop(
            Input::Terminal(&mut keyboard),
            &mut session,
            &mut executor,
            &mut stdout,
            &mut Vec::new(),
        );
        assert!(!pset.cur_cmd_interactive, "the prior source is restored");
        Typed {
            code,
            stdout: String::from_utf8(stdout).unwrap(),
            seen: executor.0.seen,
            keyboard,
            cancel_pressed: flag.load(Ordering::SeqCst),
        }
    }

    #[test]
    fn an_interactive_session_forgets_a_control_c_and_reads_on() {
        // `mainloop.c:99`.
        let typed = type_in(vec![line("select 1;")], PsqlSettings::default(), true);
        assert_eq!(typed.code, EXIT_SUCCESS);
        assert_eq!(typed.seen, ["select 1;"]);
        assert!(!typed.cancel_pressed);
    }

    #[test]
    fn the_prompt_follows_the_lexer() {
        // `mainloop.c:151`-`:167`: PROMPT1 on an empty buffer, PROMPT2 while
        // a statement or a literal is open.
        let typed = type_in(
            vec![line("select"), line("'a"), line("';"), line("select 2;")],
            PsqlSettings::default(),
            false,
        );
        assert_eq!(
            typed.keyboard.prompts,
            [
                "postgres=# ",
                "postgres-# ",
                "postgres'# ",
                "postgres=# ",
                "postgres=# "
            ]
        );
        assert_eq!(typed.seen, ["select\n'a\n';", "select 2;"]);
    }

    #[test]
    fn control_c_at_the_prompt_throws_the_query_away() {
        // `mainloop.c:108`-`:122`: the buffer, the lexer and the pending
        // history entry are reset, and the session reads on.
        let typed = type_in(
            vec![line("select"), Fetched::Interrupted, line("select 2;")],
            PsqlSettings::default(),
            false,
        );
        assert_eq!(typed.seen, ["select 2;"]);
        assert_eq!(
            typed.keyboard.prompts,
            ["postgres=# ", "postgres-# ", "postgres=# ", "postgres=# "]
        );
        assert_eq!(typed.keyboard.history, ["select 2;"]);
        assert_eq!(typed.code, EXIT_SUCCESS);
    }

    #[test]
    fn end_of_input_at_a_terminal_prints_backslash_q() {
        // `mainloop.c:184`-`:197`.
        let typed = type_in(vec![], PsqlSettings::default(), false);
        assert_eq!(typed.stdout, "\\q\n");
        let quiet = PsqlSettings {
            quiet: true,
            ..PsqlSettings::default()
        };
        assert_eq!(type_in(vec![], quiet, false).stdout, "\n");
    }

    #[test]
    fn ignoreeof_needs_that_many_control_ds() {
        // `mainloop.c:187`-`:193`.
        let pset = PsqlSettings {
            ignoreeof: 2,
            ..PsqlSettings::default()
        };
        let typed = type_in(vec![Fetched::Eof], pset, false);
        assert_eq!(typed.stdout, "Use \"\\q\" to leave psql.\n\\q\n");
        assert_eq!(typed.keyboard.prompts.len(), 2);
    }

    #[test]
    fn an_interactive_session_does_not_send_what_is_left_at_the_end() {
        // `mainloop.c:598`-`:602`.
        let typed = type_in(vec![line("select 1")], PsqlSettings::default(), false);
        assert!(typed.seen.is_empty());
    }

    #[test]
    fn history_keeps_statements_and_backslash_commands_apart() {
        // `mainloop.c:429`, `:484`-`:496`, `:570`-`:576`.
        let typed = type_in(
            vec![
                line("select 1"),
                line("+ 1;"),
                line("select"),
                line("\\echo hi"),
                line("2;"),
                line(""),
                line("\\q"),
            ],
            PsqlSettings::default(),
            false,
        );
        assert_eq!(
            typed.keyboard.history,
            ["select 1\n+ 1;", "select", "\\echo hi", "2;", "\\q"]
        );
        assert_eq!(typed.seen, ["select 1\n+ 1;", "select\n2;"]);
    }

    #[test]
    fn help_quit_and_exit_are_words_only_at_a_terminal() {
        // `mainloop.c:228`-`:340`.
        let typed = type_in(
            vec![line("help"), line("quit")],
            PsqlSettings::default(),
            false,
        );
        assert_eq!(typed.stdout, HELP_TEXT);
        assert_eq!(typed.keyboard.prompts.len(), 2, "quit ends the session");

        let typed = type_in(
            vec![
                line("select"),
                line("help"),
                line("exit;"),
                line("'"),
                line("quit"),
            ],
            PsqlSettings::default(),
            false,
        );
        assert_eq!(
            typed.stdout,
            "Use \\? for help or press control-C to clear the input buffer.\n\
             Use \\q to quit.\n\
             OK\n\
             Use control-D to quit.\n\
             \\q\n"
        );
        assert_eq!(typed.seen, ["select\nhelp\nexit;"]);
    }

    #[test]
    fn the_assistance_words_are_read_as_upstream_reads_them() {
        assert_eq!(assistance_word(b"HELP"), Some(Assistance::Help));
        assert_eq!(assistance_word(b"help ; "), Some(Assistance::Help));
        assert_eq!(assistance_word(b"help;;"), None);
        assert_eq!(assistance_word(b" help"), None);
        assert_eq!(assistance_word(b"helpme"), None);
        assert_eq!(assistance_word(b"Quit"), Some(Assistance::ExitOrQuit));
        assert_eq!(assistance_word(b"exit;"), Some(Assistance::ExitOrQuit));
        assert_eq!(assistance_word(b"\\q extra"), Some(Assistance::BackslashQ));
        assert_eq!(assistance_word(b"select"), None);
    }

    #[test]
    fn reading_as_it_goes_yields_the_lines_reading_it_all_first_does() {
        for input in [
            &b""[..],
            b"\n",
            b"a",
            b"a\n",
            b"a\n\n",
            b"a\nb",
            b"a\r\nb\n",
            b"\n\nselect 1;\n\\q\n",
        ] {
            let mut whole = Lines::new(input);
            let mut streamed = ReadLines::new(input);
            loop {
                let (w, s) = (whole.next_line(), streamed.next_line());
                assert_eq!(w, s, "{:?}", String::from_utf8_lossy(input));
                if w.is_none() {
                    break;
                }
            }
            assert!(streamed.take_error().is_none());
        }
    }

    #[test]
    fn reading_as_it_goes_does_not_wait_for_the_end_of_the_input() {
        /// A reader that has one line and then fails the test if read again:
        /// a pipe whose writer is still waiting for the answer.
        struct OneLineThenBlock(Option<&'static [u8]>);
        impl std::io::Read for OneLineThenBlock {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let line = self.0.take().expect("read past the first line");
                buf[..line.len()].copy_from_slice(line);
                Ok(line.len())
            }
        }
        let mut source = ReadLines::new(std::io::BufReader::new(OneLineThenBlock(Some(
            b"select pg_sleep(180);\n",
        ))));
        assert_eq!(
            source.next_line().as_deref(),
            Some(&b"select pg_sleep(180);"[..])
        );
    }

    #[test]
    fn a_read_error_ends_the_input_and_is_kept_for_the_report() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("boom"))
            }
        }
        let mut source = ReadLines::new(std::io::BufReader::new(Broken));
        assert_eq!(source.next_line(), None);
        assert_eq!(
            source.take_error().map(|e| e.to_string()).as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn blank_lines_are_skipped_but_not_inside_a_literal() {
        let out = run("select 'a\n\nb';\n", PsqlSettings::default());
        assert_eq!(out.seen, ["select 'a\n\nb';"]);
    }
}
