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
//! `prompt.c`), enough of `print.c` to render the default aligned output, and
//! `help.c`'s three help texts ([`help`], NAT-399). NAT-400 adds `\pset`
//! ([`pset`], `command.c`'s `do_pset`) and grows [`print`] toward the whole of
//! `print.c`. NAT-403 adds `logging.c`'s prefixes ([`logging`]), `\timing`
//! and `\errverbose`. NAT-404 adds `\crosstabview` ([`crosstab`],
//! `crosstabview.c`), `\g`, `\gx`, `\parse`, `\bind`, `\bind_named` and
//! `\close_prepared` over the extended query protocol
//! ([`settings::SendMode`]), and the pipeline commands `\startpipeline`,
//! `\sendpipeline`, `\syncpipeline`, `\flush`, `\flushrequest`,
//! `\getresults` and `\endpipeline` on rlibpq's pipeline mode
//! ([`settings::PipelineCounters`]). NAT-405 adds Ctrl-C ([`cancel`],
//! `fe_utils/cancel.c`): a SIGINT cancels the running query. `\d` is
//! NAT-401's and interactive input is NAT-405's.
//!
//! Layout follows Data / Calculations / Actions: every module above is a pure
//! calculation over its inputs, and the only actions are [`connect`], the
//! SIGINT handler in [`cancel`] and the stream writing in [`run`].

#![deny(unsafe_code)]
// Pedantic clippy is on (CI passes `-W clippy::pedantic`). Two style lints are
// allowed here because crate attributes are the only level that outranks that
// flag: proper nouns such as PostgreSQL fill every doc comment (`doc_markdown`),
// and upstream-fidelity names repeat the module name on purpose
// (`module_name_repetitions`).
#![allow(clippy::doc_markdown, clippy::module_name_repetitions)]

pub mod cancel;
pub mod command;
pub mod common;
pub mod crosstab;
pub mod help;
pub mod logging;
pub mod mainloop;
pub mod print;
pub mod prompt;
pub mod pset;
pub mod scan;
pub mod settings;
pub mod slash;
pub mod startup;
pub mod variables;

use std::ffi::OsString;
use std::io::{IsTerminal as _, Write};
use std::process::ExitCode;

use rlibpq::{
    ConnInfo, Connection, ConnectionError, Env, ExecStatus, Filesystem, Params, PipelineStatus,
    QueryResult, ResultError, Stream,
};

use crate::command::{CommandResult, dispatch_slash};
use crate::common::{ErrorMessage, Executor, send_query};
use crate::mainloop::{LineSource, Lines, ReadLines, Session as LoopSession, main_loop};
use crate::scan::{ScanResult, Scanner};
use crate::settings::{EXIT_BADCONN, EXIT_FAILURE, EXIT_SUCCESS, EXIT_USER, SendMode};
use crate::startup::{Action, HelpTopic, Invocation, Session};
use crate::variables::VarView;

/// The psql version this port tracks (`PG_VERSION` in `pg_config.h`).
pub const PG_VERSION: &str = "18.6";

/// `showVersion()` (`startup.c:844`).
#[must_use]
pub fn version_line() -> String {
    format!("psql (PostgreSQL) {PG_VERSION}")
}

/// The text `--help[=topic]` prints.
///
/// Upstream calls `slashUsage()` while the options are still being parsed, so
/// there is no connection yet (`currently no connection`) and `\timing` is
/// off; the other `(currently …)` notes are the switches seen so far.
#[must_use]
pub fn help_text(topic: HelpTopic) -> String {
    match topic {
        HelpTopic::Options => help::usage(),
        HelpTopic::Commands(switches) => help::slash_usage(&help::SlashUsageState {
            switches,
            timing: false,
            currdb: None,
        }),
        HelpTopic::Variables => help::help_variables(),
    }
}

/// A live connection as an [`Executor`].
struct LiveExecutor {
    connection: Connection<Stream>,
    alive: bool,
    /// How many of the connection's notices have been handed out.
    notices_seen: usize,
}

impl LiveExecutor {
    /// `err` as psql sees it: an argument or state refused before anything
    /// was sent leaves the connection as it was; anything else broke it.
    fn failed(&mut self, err: ConnectionError) -> ErrorMessage {
        if !matches!(
            err,
            ConnectionError::Argument(_) | ConnectionError::Pipeline(_)
        ) {
            self.alive = false;
        }
        err.into()
    }
}

/// `\bind`'s parameters as libpq takes them: all text (`common.c:1616`).
fn text_params(params: &[String]) -> Vec<Option<&[u8]>> {
    params.iter().map(|p| Some(p.as_bytes())).collect()
}

impl Executor for LiveExecutor {
    /// The `switch (pset.send_mode)` of `ExecQueryAndProcessResults`
    /// (`common.c:1602`), outside pipeline mode: each extended mode is the
    /// blocking libpq call its `PQsend…` pairs with, and `\bind`'s
    /// parameters are all text with no types given (`common.c:1616`).
    fn exec(&mut self, query: &[u8], mode: &SendMode) -> Result<Vec<QueryResult>, ErrorMessage> {
        // `SetCancelConn(pset.db)` … `ResetCancelConn()` around the query, as
        // both `SendQuery` (`common.c:1173`, `:1309`) and `PSQLexec`
        // (`common.c:686`, `:690`) have it: a Ctrl-C meanwhile cancels it.
        cancel::set_cancel_conn(self.connection.get_cancel());
        let outcome = match mode {
            SendMode::Query => self.connection.exec(query),
            SendMode::ExtendedClose { statement } => {
                self.connection.close_prepared(statement.as_bytes())
            }
            SendMode::ExtendedParse { statement } => {
                self.connection.prepare(statement.as_bytes(), query, &[])
            }
            SendMode::ExtendedQueryParams { params } => {
                self.connection
                    .exec_params(query, &[], &Params::text(&text_params(params)))
            }
            SendMode::ExtendedQueryPrepared { statement, params } => self
                .connection
                .exec_prepared(statement.as_bytes(), &Params::text(&text_params(params))),
            // `send_query` sends these through `send`; the blocking path has
            // none, and says so as libpq would.
            SendMode::PipelineSync
            | SendMode::StartPipelineMode
            | SendMode::EndPipelineMode
            | SendMode::Flush
            | SendMode::FlushRequest
            | SendMode::GetResults => Err(ConnectionError::Pipeline(
                rlibpq::PipelineError::NotInPipelineMode,
            )),
        };
        cancel::reset_cancel_conn();
        outcome.map_err(|err| self.failed(err))
    }

    fn connected(&self) -> bool {
        self.alive
    }

    fn abandon(&mut self) {
        self.alive = false;
    }

    fn pipeline_status(&self) -> PipelineStatus {
        self.connection.pipeline_status()
    }

    /// The `switch (pset.send_mode)` of `ExecQueryAndProcessResults`
    /// (`common.c:1602`-`:1724`), each mode's non-blocking libpq call.
    fn send(&mut self, query: &[u8], mode: &SendMode) -> Result<(), ErrorMessage> {
        let outcome = match mode {
            // `common.c:1713`: in a pipeline a plain query goes through the
            // extended protocol, which is all a pipeline can carry.
            SendMode::Query if self.connection.pipeline_status() != PipelineStatus::Off => self
                .connection
                .send_query_params(query, &[], &Params::text(&[])),
            SendMode::Query => self.connection.send_query(query),
            SendMode::ExtendedClose { statement } => {
                self.connection.send_close_prepared(statement.as_bytes())
            }
            SendMode::ExtendedParse { statement } => {
                self.connection
                    .send_prepare(statement.as_bytes(), query, &[])
            }
            SendMode::ExtendedQueryParams { params } => {
                self.connection
                    .send_query_params(query, &[], &Params::text(&text_params(params)))
            }
            SendMode::ExtendedQueryPrepared { statement, params } => self
                .connection
                .send_query_prepared(statement.as_bytes(), &Params::text(&text_params(params))),
            SendMode::StartPipelineMode => self.connection.enter_pipeline_mode(),
            SendMode::EndPipelineMode => self.connection.pipeline_sync(),
            SendMode::PipelineSync => self.connection.send_pipeline_sync(),
            SendMode::Flush => self.connection.flush(),
            SendMode::FlushRequest => self.connection.send_flush_request(),
            SendMode::GetResults => Ok(()),
        };
        outcome.map_err(|err| self.failed(err))
    }

    fn get_result(&mut self) -> Result<Option<QueryResult>, ErrorMessage> {
        // `SendQuery`'s `SetCancelConn` covers the pipeline's results too
        // (`common.c:1173`, `:1309`): a Ctrl-C while one is awaited cancels.
        cancel::set_cancel_conn(self.connection.get_cancel());
        let outcome = self.connection.get_result();
        cancel::reset_cancel_conn();
        outcome.map_err(|err| self.failed(err))
    }

    fn exit_pipeline_mode(&mut self) -> Result<(), ErrorMessage> {
        self.connection
            .exit_pipeline_mode()
            .map_err(|err| self.failed(err))
    }

    fn take_notices(&mut self) -> Vec<ResultError> {
        let notices = self.connection.notices()[self.notices_seen..].to_vec();
        self.notices_seen += notices.len();
        notices
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
///
/// `conninfo_array_parse` (`fe-connect.c:6466`, `:6602`), which
/// `PQconnectdbParams` runs: the keywords first, then the
/// defaults — a service file, then the environment — for whatever they left
/// unset, so a failed service lookup is the connection's error.
fn connect(session: &Session) -> Result<LiveExecutor, ErrorMessage> {
    let mut conninfo = ConnInfo::new();
    for (key, value) in connection_keywords(session) {
        // Every keyword here is a row of `PQconninfoOptions[]`, so an unknown
        // one is a bug in this function rather than in the command line.
        let _ = conninfo.set(key.as_bytes(), value.as_bytes());
    }
    if let Err(err) = conninfo.add_defaults(&Env::from_process(), &Filesystem) {
        return Err(ConnectionError::from(err).into());
    }
    match Connection::connect(&conninfo) {
        // The startup exchange's notices are not psql's to print: its
        // notice processor is installed after the connection is made.
        Ok(connection) => Ok(LiveExecutor {
            notices_seen: connection.notices().len(),
            connection,
            alive: true,
        }),
        Err(err) => Err(err.into()),
    }
}

/// Perform the invocation, writing to the given streams.
pub fn run(args: &[OsString], stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    match startup::plan(args) {
        Invocation::PrintVersion => {
            let _ = writeln!(stdout, "{}", version_line());
            ExitCode::SUCCESS
        }
        // `startup.c:704`-`:715`: the text on stdout, then `exit(EXIT_SUCCESS)`.
        Invocation::PrintHelp(topic) => {
            let _ = stdout.write_all(help_text(topic).as_bytes());
            ExitCode::SUCCESS
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

    // `startup.c:186`: judged on the process's own descriptors, not on the
    // streams this function writes to, which a test may have swapped.
    session.pset.notty = !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal();

    // `startup.c:216`: with no action and no tty, behave as if `-f -`.
    if session.actions.is_empty() && session.pset.notty {
        session.actions.push(Action::File(None));
    }
    // `startup.c:224`.
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
        Err(err) => {
            let _ = stderr.write_all(&err.rendered());
            return ExitCode::from(EXIT_BADCONN);
        }
    };
    // `startup.c:314`, once the connection is up.
    cancel::setup_cancel_handler();

    // The list is consumed here and never read again, so it moves out rather
    // than being cloned past the `&mut session` the loop needs.
    let actions = std::mem::take(&mut session.actions);
    let single_txn = session.single_txn;
    let mut code = EXIT_SUCCESS;

    // `-1`: wrap every action in one transaction (`startup.c:366`). A failed
    // BEGIN only stops the run under ON_ERROR_STOP, and then it skips the
    // actions *and* the COMMIT, which is what upstream's `goto error` does.
    let begun = !single_txn || psql_exec(&mut executor, b"BEGIN", &mut session.pset, stderr);
    if begun || !session.pset.on_error_stop {
        for action in &actions {
            code = run_action(action, &mut session, &mut executor, stdout, stderr);
            if code != EXIT_SUCCESS && session.pset.on_error_stop {
                break;
            }
        }
        if single_txn {
            // Roll back only when ON_ERROR_STOP made a failure fatal; other-
            // wise COMMIT, which the server itself turns into a rollback if
            // the transaction is already aborted (`startup.c:439`).
            let finish = single_txn_finish(code, session.pset.on_error_stop);
            if !psql_exec(&mut executor, finish, &mut session.pset, stderr)
                && session.pset.on_error_stop
            {
                code = EXIT_USER;
            }
        }
    } else {
        code = EXIT_USER;
    }

    let _ = executor.connection.terminate();
    ExitCode::from(code)
}

/// Which statement closes a `-1` transaction (`startup.c:439`).
///
/// Upstream rolls back only when `ON_ERROR_STOP` made a failure fatal — the
/// check "needs to match the one done a couple of lines above", which is the
/// one that breaks out of the action loop. Otherwise it commits, and a
/// transaction the server has already aborted turns that commit into a
/// rollback by itself.
#[must_use]
pub fn single_txn_finish(code: u8, on_error_stop: bool) -> &'static [u8] {
    if code != EXIT_SUCCESS && on_error_stop {
        b"ROLLBACK"
    } else {
        b"COMMIT"
    }
}

/// `PSQLexec()` (`common.c:657`): run a query psql issues for itself, printing
/// nothing on success and the server's error on failure.
fn psql_exec(
    executor: &mut LiveExecutor,
    query: &[u8],
    pset: &mut settings::PsqlSettings,
    stderr: &mut impl Write,
) -> bool {
    match executor.exec(query, &SendMode::Query) {
        Ok(results) => {
            let mut ok = true;
            for result in &results {
                // `AcceptResult`'s list of statuses that are not a failure
                // (`common.c:427`-`:435`).
                let accepted = matches!(
                    result.status(),
                    ExecStatus::CommandOk
                        | ExecStatus::TuplesOk
                        | ExecStatus::EmptyQuery
                        | ExecStatus::CopyIn
                        | ExecStatus::CopyOut
                );
                if !accepted {
                    // `AcceptResult` logs libpq's message at info level
                    // (`common.c:457`), and `ClearOrSaveResult` keeps the
                    // result for `\errverbose` (`common.c:694`).
                    logging::info(pset, result.error_message(), stderr);
                    common::clear_or_save_result(result, pset);
                    ok = false;
                }
            }
            ok
        }
        Err(err) => {
            logging::info(pset, err.as_bytes(), stderr);
            false
        }
    }
}

fn run_action(
    action: &Action,
    session: &mut Session,
    executor: &mut LiveExecutor,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    match action {
        // `ACT_SINGLE_QUERY` (`startup.c:382`).
        Action::SingleQuery(sql) => {
            session.pset.log_terse = true;
            if session.pset.echo == settings::Echo::All {
                let _ = writeln!(stdout, "{sql}");
            }
            if send_query(
                executor,
                sql.as_bytes(),
                &mut session.pset,
                &mut session.vars,
                stdout,
                stderr,
            ) {
                EXIT_SUCCESS
            } else {
                EXIT_FAILURE
            }
        }
        // `ACT_SINGLE_SLASH` (`startup.c:392`).
        Action::SingleSlash(text) => {
            session.pset.log_terse = true;
            if session.pset.echo == settings::Echo::All {
                let _ = writeln!(stdout, "{text}");
            }
            let mut scanner = Scanner::new();
            scanner.setup(format!("\\{text}").as_bytes(), true);
            let mut buf = Vec::new();
            let opens_a_command =
                scanner.scan(&mut buf, &VarView(&session.vars)).0 == ScanResult::Backslash;
            let status = if opens_a_command {
                dispatch_slash(
                    &mut scanner,
                    &mut session.pset,
                    &mut session.vars,
                    executor.pipeline_status(),
                    stdout,
                    stderr,
                )
            } else {
                CommandResult::Error
            };
            if status == CommandResult::Error {
                EXIT_FAILURE
            } else {
                EXIT_SUCCESS
            }
        }
        // `ACT_FILE` (`startup.c:418`).
        Action::File(name) => process_file(name.as_deref(), session, executor, stdout, stderr),
    }
}

/// Where `process_file()` reads from, and what it calls the input in
/// messages (`command.c:4927`-`:4966`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFile<'a> {
    /// No `-f` at all: stdin, and no locus, so messages are terse.
    Stdin,
    /// `-f -`: stdin, called `<stdin>` "for future error messages".
    StdinNamed,
    /// `-f NAME`.
    Path(&'a str),
}

impl<'a> InputFile<'a> {
    /// Calculation: the `filename` argument's three cases.
    #[must_use]
    pub fn from_arg(name: Option<&'a str>) -> Self {
        match name {
            None => InputFile::Stdin,
            Some("-") => InputFile::StdinNamed,
            Some(path) => InputFile::Path(path),
        }
    }

    /// The name `pset.inputfile` takes while the file is read.
    #[must_use]
    pub fn inputfile(&self) -> Option<&'a str> {
        match self {
            InputFile::Stdin => None,
            InputFile::StdinNamed => Some("<stdin>"),
            InputFile::Path(path) => Some(path),
        }
    }
}

/// `process_file()` (`command.c:4920`): read the input, run it through
/// `MainLoop` with `pset.inputfile` naming it, then restore the name and the
/// logging mode that goes with it.
fn process_file(
    name: Option<&str>,
    session: &mut Session,
    executor: &mut LiveExecutor,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    let input = InputFile::from_arg(name);
    // A file is read whole; stdin is read a line at a time as `MainLoop`
    // asks for it, so a statement runs as soon as it arrives on a pipe that
    // stays open.
    let bytes = match input {
        InputFile::Path(path) => match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                logging::error(&session.pset, format!("{path}: {err}"), stderr);
                return EXIT_FAILURE;
            }
        },
        InputFile::Stdin | InputFile::StdinNamed => None,
    };

    let old = std::mem::replace(
        &mut session.pset.inputfile,
        input.inputfile().map(str::to_owned),
    );
    // `command.c:4970`.
    session.pset.log_terse = session.pset.inputfile.is_none();
    let result = if let Some(bytes) = bytes {
        run_main_loop(&mut Lines::new(&bytes), session, executor, stdout, stderr)
    } else {
        let mut source = ReadLines::new(std::io::stdin().lock());
        let code = run_main_loop(&mut source, session, executor, stdout, stderr);
        if let Some(err) = source.take_error() {
            // `input.c:215`, inside `MainLoop`, so under this input's name.
            logging::error(
                &session.pset,
                format!("could not read from input file: {err}"),
                stderr,
            );
        }
        code
    };
    session.pset.inputfile = old;
    // `command.c:4979`.
    session.pset.log_terse = session.pset.inputfile.is_none();
    result
}

/// `MainLoop` over `source` in this session, with the process's
/// `cancel_pressed`.
fn run_main_loop(
    source: &mut dyn LineSource,
    session: &mut Session,
    executor: &mut LiveExecutor,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> u8 {
    let mut loop_session = LoopSession {
        pset: &mut session.pset,
        vars: &mut session.vars,
        cancel_pressed: &cancel::CANCEL_PRESSED,
    };
    main_loop(source, &mut loop_session, executor, stdout, stderr)
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
    fn every_help_topic_goes_to_stdout_and_exits_zero() {
        // `001_basic.pl`'s `--help=foo` loop, and `program_help_ok`: exit 0,
        // stdout non-empty, stderr empty.
        for (arg, starts) in [
            ("--help", "psql is the PostgreSQL interactive terminal.\n"),
            (
                "--help=options",
                "psql is the PostgreSQL interactive terminal.\n",
            ),
            ("--help=commands", "General\n"),
            ("--help=variables", "List of specially treated variables\n"),
        ] {
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                run(&[OsString::from(arg)], &mut out, &mut err),
                ExitCode::SUCCESS,
                "{arg}"
            );
            assert!(String::from_utf8(out).unwrap().starts_with(starts), "{arg}");
            assert!(err.is_empty(), "{arg}");
        }
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
    fn a_single_transaction_commits_unless_on_error_stop_made_it_fatal() {
        // `startup.c:439`. Without ON_ERROR_STOP upstream still COMMITs after
        // a failed statement; the server has aborted the transaction, so the
        // commit discards the work anyway. With ON_ERROR_STOP it ROLLBACKs.
        assert_eq!(single_txn_finish(EXIT_SUCCESS, false), b"COMMIT");
        assert_eq!(single_txn_finish(EXIT_SUCCESS, true), b"COMMIT");
        assert_eq!(single_txn_finish(EXIT_FAILURE, false), b"COMMIT");
        assert_eq!(single_txn_finish(EXIT_FAILURE, true), b"ROLLBACK");
    }

    #[test]
    fn single_transaction_is_carried_from_the_option_table() {
        let Invocation::Run(session) = startup::plan(&[
            OsString::from("-1"),
            OsString::from("-c"),
            OsString::from("select 1"),
        ]) else {
            panic!("expected a session");
        };
        assert!(session.single_txn);

        let Invocation::Run(session) = startup::plan(&[
            OsString::from("--single-transaction"),
            OsString::from("-c"),
            OsString::from("select 1"),
        ]) else {
            panic!("expected a session");
        };
        assert!(session.single_txn);
    }

    #[test]
    fn process_file_names_its_input_the_way_upstream_does() {
        // `command.c:4927`-`:4966`: no `-f` reads stdin with no name, and so
        // logs tersely; `-f -` reads stdin as `<stdin>`.
        assert_eq!(InputFile::from_arg(None).inputfile(), None);
        assert_eq!(InputFile::from_arg(Some("-")), InputFile::StdinNamed);
        assert_eq!(InputFile::from_arg(Some("-")).inputfile(), Some("<stdin>"));
        assert_eq!(
            InputFile::from_arg(Some("a.sql")).inputfile(),
            Some("a.sql")
        );
        // Not canonicalized yet (`command.c:4934`; docs/divergences.md).
        assert_eq!(
            InputFile::from_arg(Some("./a.sql")).inputfile(),
            Some("./a.sql")
        );
    }
}
