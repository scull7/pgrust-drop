//! rpsql: psql in Rust.
//!
//! Tracks PostgreSQL 18.6 `src/bin/psql/`, ported fresh from the C sources
//! (ADR-0003: nothing is copied from pgrust's Rust psql) on top of `rlibpq`.
//! Gates: regress `psql.sql` / `psql_crosstab.sql` / `psql_pipeline.sql`,
//! `t/001_basic.pl`, `t/020_cancel.pl`, and a byte-diff of the same input
//! through PGDG psql 18 (the method pgrust uses for its own psql).
//!
//! What is here is the skeleton (NAT-398): the statement lexer
//! ([`scan`], `psqlscan.l`), the backslash lexer ([`slash`],
//! `psqlscanslash.l`), the variable repository ([`variables`],
//! `variables.c`), the settings ([`settings`], `settings.h`), the option table
//! ([`startup`], `startup.c`), the input loop ([`mainloop`], `mainloop.c`),
//! `SendQuery` ([`common`], `common.c`), the four backslash commands this
//! issue names ([`command`], `command.c`), the prompt renderer ([`prompt`],
//! `prompt.c`) and enough of `print.c` to render the default aligned output.
//! `--help` is NAT-399's, the rest of `print.c` is NAT-400's, `\d` is
//! NAT-401's and interactive input is NAT-405's.
//!
//! Layout follows Data / Calculations / Actions: every module above is a pure
//! calculation over its inputs, and the only actions are [`connect`] and the
//! stream writing in [`run`].

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod command;
pub mod common;
pub mod mainloop;
pub mod print;
pub mod prompt;
pub mod scan;
pub mod settings;
pub mod slash;
pub mod startup;
pub mod variables;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use rlibpq::{Connection, Env, QueryResult, Stream, conndefaults};

use crate::command::{CommandContext, CommandResult, handle_slash_cmds};
use crate::common::{Executor, send_query};
use crate::mainloop::{Lines, Session as LoopSession, main_loop};
use crate::scan::{QuoteType, ScanResult, Scanner, VariableSource};
use crate::settings::{EXIT_BADCONN, EXIT_FAILURE, EXIT_SUCCESS};
use crate::startup::{Action, Invocation, Session};

/// The psql version this port tracks (`PG_VERSION` in `pg_config.h`).
pub const PG_VERSION: &str = "18.6";

/// `showVersion()` (`startup.c:832`).
#[must_use]
pub fn version_line() -> String {
    format!("psql (PostgreSQL) {PG_VERSION}")
}

/// A live connection as an [`Executor`].
struct LiveExecutor {
    connection: Connection<Stream>,
    alive: bool,
}

impl Executor for LiveExecutor {
    fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, String> {
        match self.connection.exec(query) {
            Ok(results) => Ok(results),
            Err(err) => {
                self.alive = false;
                Err(format!(
                    "psql: error: {}\n",
                    String::from_utf8_lossy(&err.message())
                ))
            }
        }
    }

    fn connected(&self) -> bool {
        self.alive
    }
}

/// Read-only view of the variable space for the lexer callback.
struct VarView(variables::VariableSpace);

impl VariableSource for VarView {
    fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String> {
        let value = self.0.get(name)?.to_string();
        Some(match quote {
            QuoteType::Plain | QuoteType::ShellArg => value,
            QuoteType::SqlLiteral => variables::escape_literal(&value),
            QuoteType::SqlIdent => variables::escape_identifier(&value),
        })
    }
}

/// The connection keyword/value array `main()` builds (`startup.c:254`).
#[must_use]
pub fn connection_keywords(session: &Session) -> Vec<(String, String)> {
    let mut keywords = Vec::new();
    for (key, value) in [
        ("host", &session.host),
        ("port", &session.port),
        ("user", &session.username),
        ("dbname", &session.dbname),
    ] {
        if let Some(value) = value {
            keywords.push((key.to_string(), value.clone()));
        }
    }
    keywords.push((
        "fallback_application_name".to_string(),
        session.pset.progname.clone(),
    ));
    keywords
}

/// Action: open the connection this session asks for.
fn connect(session: &Session) -> Result<LiveExecutor, String> {
    let mut conninfo = conndefaults(&Env::from_process());
    for (key, value) in connection_keywords(session) {
        // Every keyword here is a row of `PQconninfoOptions[]`, so an unknown
        // one is a bug in this function rather than in the command line.
        let _ = conninfo.set(key.as_bytes(), value.as_bytes());
    }
    match Connection::connect(&conninfo) {
        Ok(connection) => Ok(LiveExecutor {
            connection,
            alive: true,
        }),
        Err(err) => Err(format!(
            "psql: error: {}\n",
            String::from_utf8_lossy(&err.message())
        )),
    }
}

/// Perform the invocation, writing to the given streams.
pub fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    match startup::plan(args) {
        Invocation::PrintVersion => {
            let _ = writeln!(stdout, "{}", version_line());
            ExitCode::SUCCESS
        }
        Invocation::PrintHelp(_) => {
            // `usage()`, `slashUsage()` and `helpVariables()` must be
            // byte-identical to upstream, which is NAT-399's whole issue.
            let _ = writeln!(
                stderr,
                "psql: error: --help is not implemented yet (Linear NAT-399)"
            );
            ExitCode::from(EXIT_FAILURE)
        }
        Invocation::Unparsable(rendered) => {
            let _ = stderr.write_all(rendered.as_bytes());
            // usage-rs's parse errors exit 2 (ADR-0004).
            ExitCode::from(2)
        }
        Invocation::Run(session) => run_session(*session, stdout, stderr),
    }
}

fn run_session(mut session: Session, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    for warning in &session.warnings {
        if !session.pset.quiet {
            let _ = writeln!(stderr, "psql: warning: {warning}");
        }
    }

    // `startup.c:216`: with no action and no tty, behave as if `-f -`.
    if session.actions.is_empty() && session.pset.notty {
        session.actions.push(Action::File(None));
    }
    // `startup.c:222`.
    if session.single_txn && session.actions.is_empty() {
        let _ = writeln!(
            stderr,
            "psql: error: -1 can only be used in non-interactive mode"
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    if session.actions.is_empty() {
        let _ = writeln!(
            stderr,
            "psql: error: interactive mode is not implemented yet (Linear NAT-405)"
        );
        return ExitCode::from(EXIT_FAILURE);
    }
    if session.list_dbs || session.output.is_some() || session.logfilename.is_some() {
        let _ = writeln!(
            stderr,
            "psql: error: -l, -o and -L are not implemented yet (Linear NAT-401, NAT-403)"
        );
        return ExitCode::from(EXIT_FAILURE);
    }

    let mut executor = match connect(&session) {
        Ok(executor) => executor,
        Err(message) => {
            let _ = stderr.write_all(message.as_bytes());
            return ExitCode::from(EXIT_BADCONN);
        }
    };

    let mut code = EXIT_SUCCESS;
    for action in session.actions.clone() {
        code = run_action(&action, &mut session, &mut executor, stdout, stderr);
        if code != EXIT_SUCCESS && session.pset.on_error_stop {
            break;
        }
    }
    let _ = executor.connection.terminate();
    ExitCode::from(code)
}

fn run_action(
    action: &Action,
    session: &mut Session,
    executor: &mut LiveExecutor,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    match action {
        // `ACT_SINGLE_QUERY` (`startup.c:370`).
        Action::SingleQuery(sql) => {
            if session.pset.echo == settings::Echo::All {
                let _ = writeln!(stdout, "{sql}");
            }
            if send_query(executor, sql.as_bytes(), &session.pset, stdout, stderr) {
                EXIT_SUCCESS
            } else {
                EXIT_FAILURE
            }
        }
        // `ACT_SINGLE_SLASH` (`startup.c:380`).
        Action::SingleSlash(text) => {
            if session.pset.echo == settings::Echo::All {
                let _ = writeln!(stdout, "{text}");
            }
            let mut scanner = Scanner::new();
            scanner.setup(format!("\\{text}").as_bytes(), true);
            let mut buf = Vec::new();
            // The lexer reads the variable space while the command writes to
            // it, which upstream gets away with because both are the same
            // global. The read side works from a snapshot taken before
            // dispatch, which is the state the C lexer would have seen.
            let view = VarView(session.vars.clone());
            let status = if scanner.scan(&mut buf, &view).0 == ScanResult::Backslash {
                let pset = session.pset.clone();
                let mut ctx = CommandContext {
                    pset: &pset,
                    vars: &mut session.vars,
                };
                handle_slash_cmds(&mut scanner, &mut ctx, &view, stdout, stderr)
            } else {
                CommandResult::Error
            };
            session.pset = session.vars.settings(&session.pset);
            if status == CommandResult::Error {
                EXIT_FAILURE
            } else {
                EXIT_SUCCESS
            }
        }
        // `ACT_FILE` (`startup.c:403`): `None` is stdin.
        Action::File(name) => {
            let read = if let Some(name) = name {
                std::fs::read(name).map_err(|err| format!("{name}: {err}"))
            } else {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut std::io::stdin(), &mut bytes)
                    .map(|_| bytes)
                    .map_err(|err| err.to_string())
            };
            let input = match read {
                Ok(bytes) => bytes,
                Err(message) => {
                    let _ = writeln!(stderr, "psql: error: {message}");
                    return EXIT_FAILURE;
                }
            };
            let mut loop_session = LoopSession {
                pset: &mut session.pset,
                vars: &mut session.vars,
            };
            let mut stdout_ref: &mut dyn Write = stdout;
            let mut stderr_ref: &mut dyn Write = stderr;
            main_loop(
                &mut Lines::new(&input),
                &mut loop_session,
                executor,
                &mut stdout_ref,
                &mut stderr_ref,
            )
        }
    }
}

/// The exit statuses `main()` can return (`settings.h:192`), re-exported so
/// the integration tests name them rather than numbering them.
pub mod exit {
    pub use crate::settings::{EXIT_BADCONN, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USER};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_matches_upstream_shape() {
        assert_eq!(version_line(), "psql (PostgreSQL) 18.6");
    }

    #[test]
    fn version_is_printed_for_argv_one() {
        let args = [OsString::from("--version")];
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert_eq!(run(&args, &mut out, &mut err), ExitCode::SUCCESS);
        assert_eq!(String::from_utf8(out).unwrap(), "psql (PostgreSQL) 18.6\n");
        assert!(err.is_empty());
    }

    #[test]
    fn the_connection_array_is_the_one_main_builds() {
        let Invocation::Run(session) = startup::plan(&[
            OsString::from("-h"),
            OsString::from("srv"),
            OsString::from("-U"),
            OsString::from("bob"),
            OsString::from("-c"),
            OsString::from("select 1"),
        ]) else {
            panic!("expected a session");
        };
        let keywords = connection_keywords(&session);
        assert!(keywords.contains(&("host".to_string(), "srv".to_string())));
        assert!(keywords.contains(&("user".to_string(), "bob".to_string())));
        assert!(
            keywords
                .iter()
                .any(|(k, v)| k == "fallback_application_name" && v == "psql")
        );
    }

    #[test]
    fn exit_statuses_are_reexported() {
        assert_eq!(
            (
                exit::EXIT_SUCCESS,
                exit::EXIT_FAILURE,
                exit::EXIT_BADCONN,
                exit::EXIT_USER
            ),
            (0, 1, 2, 3)
        );
    }
}
