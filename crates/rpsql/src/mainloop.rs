//! The input loop: `src/bin/psql/mainloop.c`.
//!
//! `MainLoop()` (`mainloop.c:32`) reads lines, feeds them to the lexer, and
//! sends a statement whenever the lexer finds a semicolon. This port keeps the
//! same shape with two substitutions: lines come from a [`LineSource`] rather
//! than a `FILE *`, and queries go to a [`crate::common::Executor`]. Readline
//! history and the SIGINT `siglongjmp` belong to NAT-405 and are absent, not
//! stubbed.

use std::io::Write;

use crate::command::{CommandContext, CommandResult, handle_slash_cmds};
use crate::common::{Executor, send_query};
use crate::scan::{PromptStatus, QuoteType, ScanResult, Scanner, VariableSource};
use crate::settings::{EXIT_BADCONN, EXIT_SUCCESS, EXIT_USER, PsqlSettings};
use crate::variables::VariableSpace;

/// Where `MainLoop` gets its lines. `None` is end of input.
pub trait LineSource {
    /// One line, with its terminator already stripped (`gets_fromFile`,
    /// `input.c:150`).
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

/// A read-only view of the variable space for the lexer's `:name` callback.
struct VarView(VariableSpace);

impl VariableSource for VarView {
    fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String> {
        let value = self.0.get(name)?.to_string();
        Some(match quote {
            QuoteType::Plain | QuoteType::ShellArg => value,
            QuoteType::SqlLiteral => crate::variables::escape_literal(&value),
            QuoteType::SqlIdent => crate::variables::escape_identifier(&value),
        })
    }
}

/// The session state `MainLoop` mutates as it goes.
pub struct Session<'a> {
    /// `pset`
    pub pset: &'a mut PsqlSettings,
    /// `pset.vars`
    pub vars: &'a mut VariableSpace,
}

/// `MainLoop()` (`mainloop.c:32`). Returns the process exit status.
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

    session.pset.lineno = 0;
    session.pset.stmt_lineno = 1;

    'lines: while success_result == EXIT_SUCCESS {
        let Some(line) = source.next_line() else {
            break;
        };
        session.pset.lineno += 1;

        // Detect attempts to run custom-format dumps as SQL (`mainloop.c:200`).
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

        // ECHO=all echoes the input line, unless interactive (`mainloop.c:355`).
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
            let scan_result = {
                let view = VarView(session.vars.clone());
                scanner.scan(&mut query_buf, &view).0
            };
            if scan_result == ScanResult::Eol {
                session.pset.stmt_lineno += 1;
            }

            if scan_result == ScanResult::Semicolon
                || (scan_result == ScanResult::Eol && session.pset.singleline)
            {
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
                // buffer untouched (`mainloop.c:475`).
                if Some(query_buf.len()) == added_nl_pos {
                    query_buf.pop();
                }
                added_nl_pos = None;

                slash_status = {
                    // The lexer reads the variable space while the command
                    // writes to it; upstream aliases one global for both, so
                    // the read side works from the state the C lexer would
                    // have seen.
                    let view = VarView(session.vars.clone());
                    let pset = session.pset.clone();
                    let mut ctx = CommandContext {
                        pset: &pset,
                        vars: session.vars,
                    };
                    handle_slash_cmds(&mut scanner, &mut ctx, &view, stdout, stderr)
                };
                *session.pset = session.vars.settings(session.pset);
                success = slash_status != CommandResult::Error;
                session.pset.stmt_lineno = 1;

                match slash_status {
                    CommandResult::Send => {
                        success = send_query(executor, &query_buf, session.pset, stdout, stderr);
                        std::mem::swap(&mut previous_buf, &mut query_buf);
                        query_buf.clear();
                    }
                    CommandResult::Terminate => break,
                    CommandResult::Connect(_) => {
                        // Reconnection is an action the caller owns; this loop
                        // reports it rather than pretending it happened.
                        let _ = writeln!(
                            stderr,
                            "psql: error: \\connect is not implemented yet (Linear NAT-405)"
                        );
                        success = false;
                        slash_status = CommandResult::Error;
                    }
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
    // (`mainloop.c:600`).
    if !query_buf.is_empty() && !session.pset.cur_cmd_interactive && success_result == EXIT_SUCCESS
    {
        let ok = send_query(executor, &query_buf, session.pset, stdout, stderr);
        if !ok && die_on_error {
            success_result = EXIT_USER;
        } else if !executor.connected() {
            success_result = EXIT_BADCONN;
        }
    }

    success_result
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
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
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
        // `mainloop.c:600`.
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
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
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
        let mut session = Session {
            pset: &mut pset,
            vars: &mut vars,
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
        // `mainloop.c:200`.
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
    fn blank_lines_are_skipped_but_not_inside_a_literal() {
        let out = run("select 'a\n\nb';\n", PsqlSettings::default());
        assert_eq!(out.seen, ["select 'a\n\nb';"]);
    }
}
