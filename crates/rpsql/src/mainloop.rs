//! The input loop: `src/bin/psql/mainloop.c`.
//!
//! `MainLoop()` (`mainloop.c:33`) reads lines, feeds them to the lexer, and
//! sends a statement whenever the lexer finds a semicolon. This port keeps the
//! same shape with two substitutions: lines come from a [`LineSource`] rather
//! than a `FILE *`, and queries go to a [`crate::common::Executor`]. A Ctrl-C
//! stops a script (`mainloop.c:88`, through [`Session::cancel_pressed`]);
//! readline history and the SIGINT `siglongjmp` out of waiting for input are
//! interactive mode's (NAT-405) and are absent, not stubbed.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::command::{CommandResult, IncludeRequest, dispatch_slash};
use crate::common::{Executor, send_query, set_client_encoding};
use crate::logging;
use crate::scan::{PromptStatus, ScanResult, Scanner};
use crate::settings::{EXIT_BADCONN, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USER, PsqlSettings};
use crate::variables::{VarView, VariableSpace};

/// Where `MainLoop` gets its lines. `None` is end of input.
pub trait LineSource {
    /// One line, with its terminator already stripped (`gets_fromFile`,
    /// `input.c:186`).
    fn next_line(&mut self) -> Option<Vec<u8>>;

    /// The read error that ended the input early, if one did: `gets_fromFile`
    /// reports it (`input.c:213`-`:215`) and then answers end of file.
    fn take_error(&mut self) -> Option<std::io::Error> {
        None
    }
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
}

impl<R: BufRead> LineSource for ReadLines<R> {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        gets_from_file(&mut self.reader, &mut self.error)
    }

    fn take_error(&mut self) -> Option<std::io::Error> {
        self.error.take()
    }
}

/// [`ReadLines`] over the process's stdin, locked for one line at a time.
///
/// Upstream's `MainLoop(stdin)` and a nested `\i -` share one `FILE *`, and
/// so one buffer; `std`'s stdin has one buffer too, behind a lock that is
/// not reentrant. Taking it per line lets an included `-` read on from where
/// the outer loop stopped, as C does, where holding it for a whole loop
/// would deadlock the nested one.
#[derive(Default)]
pub struct StdinLines {
    error: Option<std::io::Error>,
}

impl LineSource for StdinLines {
    fn next_line(&mut self) -> Option<Vec<u8>> {
        gets_from_file(&mut std::io::stdin().lock(), &mut self.error)
    }

    fn take_error(&mut self) -> Option<std::io::Error> {
        self.error.take()
    }
}

/// `gets_fromFile()` (`input.c:186`) for a script: one line, or `None` at end
/// of file or on a read error, which is kept in `error`.
fn gets_from_file(reader: &mut dyn BufRead, error: &mut Option<std::io::Error>) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    match reader.read_until(b'\n', &mut line) {
        Ok(0) => None,
        Ok(_) => {
            // Only the one `\n` goes (`input.c:230`).
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            Some(line)
        }
        Err(err) => {
            *error = Some(err);
            None
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
    source: &mut dyn LineSource,
    session: &mut Session<'_>,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let mut scanner = Scanner::new();
    let mut query_buf: Vec<u8> = Vec::new();
    let mut previous_buf: Vec<u8> = Vec::new();
    let mut success_result = EXIT_SUCCESS;
    let mut slash_status = CommandResult::Unknown;
    let mut die_on_error = false;
    let mut success;

    // Save the prior command source's line (`mainloop.c:60`); `\i` runs a
    // loop inside this one.
    let prev_lineno = session.pset.lineno;
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

        let Some(line) = source.next_line() else {
            if let Some(err) = source.take_error() {
                // `input.c:215`, before this line is counted; a read error
                // fails this loop (`mainloop.c:172`).
                success_result = EXIT_FAILURE;
                logging::error(
                    session.pset,
                    format!(
                        "could not read from input file: {}",
                        logging::strerror(&err)
                    ),
                    stderr,
                );
            }
            break;
        };
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
            success_result = EXIT_FAILURE;
            break;
        }

        // No further processing of empty lines, unless within a literal.
        if line.is_empty() && !scanner.in_quote() {
            continue;
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

        while success || !die_on_error {
            let scan_result = scanner.scan(&mut query_buf, &VarView(session.vars)).0;
            if scan_result == ScanResult::Eol {
                session.pset.stmt_lineno += 1;
            }

            if scan_result == ScanResult::Semicolon
                || (scan_result == ScanResult::Eol && session.pset.singleline)
            {
                success = send_query(
                    executor,
                    &query_buf,
                    session.pset,
                    session.vars,
                    stdout,
                    stderr,
                );
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
                // buffer untouched (`mainloop.c:475`).
                if Some(query_buf.len()) == added_nl_pos {
                    query_buf.pop();
                }
                added_nl_pos = None;

                slash_status = dispatch_slash(
                    &mut scanner,
                    session.pset,
                    session.vars,
                    executor.pipeline_status(),
                    stdout,
                    stderr,
                );
                slash_status = perform(slash_status, session, executor, stdout, stderr);
                success = slash_status != CommandResult::Error;
                session.pset.stmt_lineno = 1;

                match slash_status {
                    CommandResult::Send => {
                        // `copy_previous_query()` (`command.c:3850`), which
                        // `exec_command` applies to every command that sends
                        // (`command.c:488`): an empty buffer sends the
                        // previous query again.
                        if query_buf.is_empty() {
                            query_buf.clone_from(&previous_buf);
                        }
                        success = send_query(
                            executor,
                            &query_buf,
                            session.pset,
                            session.vars,
                            stdout,
                            stderr,
                        );
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
        let ok = send_query(
            executor,
            &query_buf,
            session.pset,
            session.vars,
            stdout,
            stderr,
        );
        if !ok && die_on_error {
            success_result = EXIT_USER;
        } else if !executor.connected() {
            success_result = EXIT_BADCONN;
        }
    }

    // `mainloop.c:662`.
    session.pset.lineno = prev_lineno;
    success_result
}

/// Where `process_file()` reads from, and what it calls the input in
/// messages (`command.c:4927`-`:4966`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFile {
    /// No `-f` at all: stdin, and no locus, so messages are terse.
    Stdin,
    /// `-f -` or `\i -`: stdin, called `<stdin>` "for future error messages".
    StdinNamed,
    /// A file, by the name [`crate::path::include_path`] resolved.
    Path(String),
}

impl InputFile {
    /// Calculation: the `filename` argument's three cases, and for a file the
    /// name it is opened and reported by, given `inputfile`, the one being
    /// read now (`command.c:4927`-`:4950`).
    #[must_use]
    pub fn resolve(
        filename: Option<&str>,
        use_relative_path: bool,
        inputfile: Option<&str>,
    ) -> Self {
        match filename {
            None => InputFile::Stdin,
            Some("-") => InputFile::StdinNamed,
            Some(name) => InputFile::Path(crate::path::include_path(
                name,
                use_relative_path,
                inputfile,
            )),
        }
    }

    /// The name `pset.inputfile` takes while the file is read.
    #[must_use]
    pub fn inputfile(&self) -> Option<&str> {
        match self {
            InputFile::Stdin => None,
            InputFile::StdinNamed => Some("<stdin>"),
            InputFile::Path(path) => Some(path),
        }
    }
}

/// `process_file()` (`command.c:4920`): open the input, run it through
/// `MainLoop` with `pset.inputfile` naming it, then restore the name and the
/// logging mode that goes with it. Returns `MainLoop`'s exit status, or
/// `EXIT_FAILURE` when the file cannot be opened.
///
/// Every input is read a line at a time as `MainLoop` asks for it, so a
/// statement on a pipe that stays open runs as soon as it arrives.
pub fn process_file(
    filename: Option<&str>,
    use_relative_path: bool,
    session: &mut Session<'_>,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let input = InputFile::resolve(
        filename,
        use_relative_path,
        session.pset.inputfile.as_deref(),
    );
    // Action: `fopen(filename, PG_BINARY_R)` (`command.c:4953`).
    let mut source: Box<dyn LineSource> = match &input {
        InputFile::Path(path) => match std::fs::File::open(path) {
            Ok(file) => Box::new(ReadLines::new(std::io::BufReader::new(file))),
            Err(err) => {
                logging::error(
                    session.pset,
                    format!("{path}: {}", logging::strerror(&err)),
                    stderr,
                );
                return EXIT_FAILURE;
            }
        },
        InputFile::Stdin | InputFile::StdinNamed => Box::new(StdinLines::default()),
    };

    let old = std::mem::replace(
        &mut session.pset.inputfile,
        input.inputfile().map(str::to_owned),
    );
    // `command.c:4970`.
    session.pset.log_terse = session.pset.inputfile.is_none();
    let result = main_loop(source.as_mut(), session, executor, stdout, stderr);
    session.pset.inputfile = old;
    // `command.c:4979`.
    session.pset.log_terse = session.pset.inputfile.is_none();
    result
}

/// The part of a backslash command that needs the connection, which
/// [`dispatch_slash`] leaves to its caller: `\i`'s file and `\encoding`'s
/// `PQsetClientEncoding`. Every other result passes through.
pub fn perform(
    status: CommandResult,
    session: &mut Session<'_>,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    match status {
        CommandResult::Include(request) => include(&request, session, executor, stdout, stderr),
        // `exec_command_encoding` returns PSQL_CMD_SKIP_LINE whether or not
        // the encoding was set (`command.c:1636`).
        CommandResult::SetEncoding(encoding) => {
            set_client_encoding(executor, &encoding, session.pset, session.vars, stderr);
            CommandResult::SkipLine
        }
        status => status,
    }
}

/// The rest of `exec_command_include()` (`command.c:2084`): run the file
/// `\i` or `\ir` named, and answer as the command.
pub fn include(
    request: &IncludeRequest,
    session: &mut Session<'_>,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let code = process_file(
        Some(&request.filename),
        request.use_relative_path,
        session,
        executor,
        stdout,
        stderr,
    );
    if code == EXIT_SUCCESS {
        CommandResult::SkipLine
    } else {
        CommandResult::Error
    }
}

/// The prompt the loop would show, for the interactive mode NAT-405 adds.
#[must_use]
pub fn prompt_status_for(result: ScanResult) -> PromptStatus {
    match result {
        ScanResult::Semicolon | ScanResult::Backslash => PromptStatus::Ready,
        _ => PromptStatus::Continue,
    }
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
        fn exec(
            &mut self,
            query: &[u8],
            _mode: &crate::settings::SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
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

        fn abandon(&mut self) {
            self.connected = false;
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
            &mut Lines::new(input.as_bytes()),
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
    fn a_sending_command_on_an_empty_buffer_sends_the_previous_query_again() {
        // `copy_previous_query` (`command.c:3850`), which
        // `psql_crosstab.sql:24`'s `\crosstabview` on its own line relies on.
        let out = run(
            "select 1;\n\\crosstabview\nselect 2 \\crosstabview\n",
            PsqlSettings::default(),
        );
        assert_eq!(out.seen, ["select 1;", "select 1;", "select 2 "]);
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
            &mut Lines::new(b"select 1;\nselect 2;\nselect 3;\n"),
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
            &mut Lines::new(b"select 1;\nselect 2;\n"),
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
            &mut Lines::new(b"select 1;\nselect 2"),
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

    #[test]
    fn an_interactive_session_forgets_a_control_c_and_reads_on() {
        // `mainloop.c:99`.
        let mut pset = PsqlSettings {
            cur_cmd_interactive: true,
            ..PsqlSettings::default()
        };
        let mut vars = VariableSpace::new();
        let cancel_pressed = AtomicBool::new(true);
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
            cancel_pressed: &cancel_pressed,
        };
        let mut executor = Recorder::new();
        let code = main_loop(
            &mut Lines::new(b"select 1;\n"),
            &mut session,
            &mut executor,
            &mut Vec::new(),
            &mut Vec::new(),
        );
        assert_eq!(code, EXIT_SUCCESS);
        assert_eq!(executor.seen, ["select 1;"]);
        assert!(!cancel_pressed.load(Ordering::SeqCst));
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

    #[test]
    fn process_file_names_its_input_the_way_upstream_does() {
        // `command.c:4927`-`:4966`: no `-f` reads stdin with no name, and so
        // logs tersely; `-f -` reads stdin as `<stdin>`; a file is known by
        // its canonical name (`command.c:4934`).
        let resolve = |name| InputFile::resolve(name, false, None);
        assert_eq!(resolve(None).inputfile(), None);
        assert_eq!(resolve(Some("-")), InputFile::StdinNamed);
        assert_eq!(resolve(Some("-")).inputfile(), Some("<stdin>"));
        assert_eq!(resolve(Some("a.sql")).inputfile(), Some("a.sql"));
        assert_eq!(resolve(Some("./a.sql")).inputfile(), Some("a.sql"));
        // `\ir -` is stdin too: `-` is tested before any path arithmetic.
        assert_eq!(
            InputFile::resolve(Some("-"), true, Some("sub/a.sql")),
            InputFile::StdinNamed
        );
    }

    /// A directory of its own for one test's scripts, removed when dropped.
    struct Scripts(std::path::PathBuf);

    impl Scripts {
        fn new(test: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("rpsql-mainloop-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            Self(dir)
        }

        /// Write `text` to `name` and return its absolute path.
        fn file(&self, name: &str, text: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path.to_str().unwrap().to_owned()
        }
    }

    impl Drop for Scripts {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `process_file(main, false)` as `-f main` runs it.
    fn run_file(main: &str) -> Outcome {
        let mut pset = PsqlSettings::default();
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
        let code = process_file(
            Some(main),
            false,
            &mut session,
            &mut executor,
            &mut stdout,
            &mut stderr,
        );
        assert_eq!(pset.inputfile, None, "process_file restores the name");
        Outcome {
            code,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            seen: executor.seen,
        }
    }

    #[test]
    fn i_runs_the_file_in_place_and_the_outer_file_reads_on_at_its_own_line() {
        // `process_file` saves and restores `pset.inputfile`
        // (`command.c:4968`, `:4977`) and `MainLoop` the line number
        // (`mainloop.c:60`, `:662`), so the outer file's next message has
        // its own name and line.
        let dir = Scripts::new("i-in-place");
        let a = dir.file("sub/a.sql", "select 'a';\n\\warn in a\n");
        let main = dir.file(
            "main.sql",
            &format!("select 1;\n\\i {a}\n\\warn back\nselect 2;\n"),
        );
        let out = run_file(&main);
        assert_eq!(out.seen, ["select 1;", "select 'a';", "select 2;"]);
        assert_eq!(out.stderr, "in a\nback\n");
        assert_eq!(out.code, EXIT_SUCCESS);
    }

    #[test]
    fn ir_resolves_a_relative_name_beside_the_file_being_read() {
        let dir = Scripts::new("ir-relative");
        dir.file("sub/b.sql", "select 'b';\n");
        dir.file("sub/a.sql", "\\ir b.sql\n\\ir ./../sub/b.sql\n");
        let main = dir.file("main.sql", "\\ir sub/a.sql\n");
        let out = run_file(&main);
        assert_eq!(out.seen, ["select 'b';", "select 'b';"]);
        assert_eq!(out.stderr, "");
    }

    #[test]
    fn a_missing_file_is_reported_at_the_line_that_named_it_and_is_an_error() {
        // `pg_log_error("%s: %m", filename)` (`command.c:4957`), under the
        // including file's locus; the name is the canonical one.
        let dir = Scripts::new("missing");
        let main = dir.file(
            "main.sql",
            "select 1;\n\\i ./sub/../nosuch.sql\nselect 2;\n",
        );
        let out = run_file(&main);
        assert_eq!(
            out.stderr,
            format!("psql:{main}:2: error: nosuch.sql: No such file or directory\n")
        );
        assert_eq!(out.seen, ["select 1;", "select 2;"]);
        assert_eq!(out.code, EXIT_SUCCESS);
    }

    #[test]
    fn a_message_inside_an_included_file_names_that_file_by_its_canonical_name() {
        let dir = Scripts::new("locus");
        let a = dir.file("sub/a.sql", "\n\\nosuch\n");
        let main = dir.file("main.sql", &format!("\\i {a}/../a.sql\n\\nosuch\n"));
        let out = run_file(&main);
        // `{a}/../a.sql` canonicalizes to `{a}`'s directory's `a.sql`.
        let canonical = crate::path::canonicalize_path(&format!("{a}/../a.sql"));
        assert_eq!(
            out.stderr,
            format!(
                "psql:{canonical}:2: error: invalid command \\nosuch\n\
                 psql:{main}:2: error: invalid command \\nosuch\n"
            )
        );
    }

    #[test]
    fn quit_in_an_included_file_ends_only_that_file() {
        // `mainloop.c`'s PSQL_CMD_TERMINATE breaks out of the loop it is in.
        let dir = Scripts::new("quit");
        let a = dir.file("a.sql", "\\q\nselect 'not run';\n");
        let main = dir.file("main.sql", &format!("\\i {a}\nselect 2;\n"));
        let out = run_file(&main);
        assert_eq!(out.seen, ["select 2;"]);
    }

    #[test]
    fn on_error_stop_in_an_included_file_stops_the_including_one_too() {
        // The file's EXIT_USER makes `\i` fail (`command.c:2084`), and
        // ON_ERROR_STOP makes that fatal in turn.
        let dir = Scripts::new("on-error-stop");
        let a = dir.file("a.sql", "\\nosuch\nselect 'not run';\n");
        let main = dir.file(
            "main.sql",
            &format!("\\set ON_ERROR_STOP on\n\\i {a}\nselect 2;\n"),
        );
        let out = run_file(&main);
        assert!(out.seen.is_empty(), "{:?}", out.seen);
        assert_eq!(out.code, EXIT_USER);
    }

    #[test]
    fn an_unreadable_input_is_reported_inside_its_own_loop() {
        // A directory opens and then fails to read: `gets_fromFile`'s
        // message, under the directory's name before its first line
        // (`input.c:215`, `logging.c:261`).
        let dir = Scripts::new("unreadable");
        let sub = dir.0.join("sub");
        let sub = sub.to_str().unwrap();
        let main = dir.file("main.sql", &format!("\\i {sub}\nselect 2;\n"));
        let out = run_file(&main);
        assert_eq!(
            out.stderr,
            format!("psql:{sub}: error: could not read from input file: Is a directory\n")
        );
        assert_eq!(out.seen, ["select 2;"]);
        // The inner loop fails (`mainloop.c:172`), so `\i` is an error;
        // without ON_ERROR_STOP the outer file reads on and ends well.
        assert_eq!(out.code, EXIT_SUCCESS);
    }

    #[test]
    fn a_read_error_fails_the_loop_that_hit_it() {
        // `-f <dir>`: `gets_fromFile` fails and `MainLoop` returns
        // `EXIT_FAILURE` (`mainloop.c:172`-`:173`).
        let dir = Scripts::new("unreadable-top");
        let sub = dir.0.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let out = run_file(sub.to_str().unwrap());
        assert_eq!(out.code, EXIT_FAILURE);
        assert!(out.seen.is_empty());
    }
}
