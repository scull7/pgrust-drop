//! Sending a query and printing what came back: `src/bin/psql/common.c`.
//!
//! `SendQuery` (`common.c:1126`) is an action — it writes to a socket and to
//! two streams — so it is a thin shell here over two pure calculations:
//! [`echo_line`], which decides what `ECHO` puts on stdout before the query
//! runs, and [`crate::print::print_query`], which renders the result.

use std::io::Write;

use rlibpq::result::diag;
use rlibpq::{ConnectionError, ExecStatus, QueryResult, ResultError};

use crate::logging::{Level, log};
use crate::print::print_query;
use crate::settings::{Echo, PsqlSettings};
use crate::variables::VariableSpace;

/// The bytes libpq left in `conn->errorMessage`, kept as bytes.
///
/// `PQerrorMessage()` hands back a `char *` that psql writes with `%s`, so a
/// token the server sent in a non-UTF-8 client encoding reaches stderr as the
/// bytes C would have written. `rlibpq` goes to deliberate trouble to keep
/// them that way ([`rlibpq::ConnectionError::message`]); decoding them here
/// would replace every such byte with U+FFFD at the crate boundary.
///
/// This is rpsql's own error type rather than `rlibpq`'s so that an
/// [`Executor`] test double owes nothing to the transport — which is the
/// reason the trait exists at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMessage(Vec<u8>);

impl ErrorMessage {
    /// Wrap libpq's error buffer, without the `psql: error: ` prefix.
    #[must_use]
    pub fn new(message: impl Into<Vec<u8>>) -> Self {
        Self(message.into())
    }

    /// The message alone, as libpq left it.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The bytes psql writes to stderr: `pg_log_error`'s prefix, the message,
    /// and the newline that call adds (`logging.c:334`).
    #[must_use]
    pub fn rendered(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.0.len() + 14);
        out.extend_from_slice(b"psql: error: ");
        out.extend_from_slice(&self.0);
        out.push(b'\n');
        out
    }
}

impl From<ConnectionError> for ErrorMessage {
    fn from(err: ConnectionError) -> Self {
        Self(err.message())
    }
}

/// One thing the server said in answer to a query, in the order it said it.
#[derive(Debug, Clone)]
pub enum Reply {
    /// A NoticeResponse, which libpq hands to psql's `NoticeProcessor`
    /// (`common.c:281`) the moment it is parsed.
    Notice(ResultError),
    /// One `PQgetResult` result.
    Result(QueryResult),
    /// The connection broke: libpq's error buffer. Nothing follows it.
    Broken(ErrorMessage),
}

/// What psql can do with a query. An [`Executor`] is the only thing in the
/// crate that holds a connection, which keeps every other module testable
/// without a server.
pub trait Executor {
    /// `ExecQueryAndProcessResults` (`common.c:1581`), minus the printing: run
    /// `query` and hand back every notice and result it produced, in order.
    ///
    /// A notice comes before the result that was returned with it, because
    /// the notice processor printed it while `PQgetResult` was still parsing.
    /// A *failed query* is a `PGRES_FATAL_ERROR` result, as in libpq; a
    /// broken connection is a final [`Reply::Broken`].
    fn exec(&mut self, query: &[u8]) -> Vec<Reply>;

    /// `pset.db != NULL` (`mainloop.c:592`).
    fn connected(&self) -> bool;
}

/// What `ECHO` prints before a query runs, or `None`.
///
/// Only `PSQL_ECHO_QUERIES` echoes here (`common.c:1158`). `ECHO=all` is *not*
/// this function's job: it echoes each line of input in `MainLoop`
/// (`mainloop.c:360`) and each action in `main` (`startup.c:386`), both of
/// which happen before the query reaches `SendQuery`. Echoing it here as well
/// printed every query twice under `-a`.
#[must_use]
pub fn echo_line(query: &[u8], pset: &PsqlSettings) -> Option<Vec<u8>> {
    match pset.echo {
        Echo::Queries => Some(query.to_vec()),
        Echo::None | Echo::Errors | Echo::All => None,
    }
}

/// `PrintQueryStatus()` (`common.c:996`): the command tag, unless this is a
/// `TUPLES_OK` result that did not come from a RETURNING clause.
#[must_use]
pub fn query_status_line(result: &QueryResult, pset: &PsqlSettings) -> Option<Vec<u8>> {
    let cmdstatus = result.command_status();
    if result.status() == ExecStatus::TuplesOk {
        let returning = [b"INSERT".as_slice(), b"UPDATE", b"DELETE", b"MERGE"]
            .iter()
            .any(|tag| cmdstatus.starts_with(tag));
        if !returning {
            return None;
        }
    }
    if pset.quiet {
        return None;
    }
    let mut line = cmdstatus.to_vec();
    line.push(b'\n');
    Some(line)
}

/// `NoticeProcessor()` (`common.c:281`): `pg_log_info` of the notice as libpq
/// rendered it, with the connection's verbosity and context settings, as a
/// `PGRES_NONFATAL_ERROR` result (`pqGetErrorNotice3`, `fe-protocol3.c:1011`).
pub fn notice_processor(notice: &ResultError, pset: &PsqlSettings, stderr: &mut dyn Write) {
    let message = notice.message(ExecStatus::NonfatalError, pset.verbosity, pset.show_context);
    log(stderr, pset, Level::Info, &message);
}

/// `AcceptResult()`'s statuses that are not a failure (`common.c:427`-`:433`).
#[must_use]
pub fn accept_result(result: &QueryResult) -> bool {
    matches!(
        result.status(),
        ExecStatus::CommandOk
            | ExecStatus::TuplesOk
            | ExecStatus::TuplesChunk
            | ExecStatus::EmptyQuery
            | ExecStatus::CopyIn
            | ExecStatus::CopyOut
            | ExecStatus::PipelineSync
    )
}

/// The special variables one query's outcome sets (`SetResultVariables`,
/// `common.c:478`), as `(name, value)` pairs in upstream's order.
///
/// `result` is `None` for a broken connection, where libpq's synthesized
/// result carries no fields: the SQLSTATE and message are then empty
/// (`common.c:496`).
#[must_use]
pub fn result_variables(
    result: Option<&QueryResult>,
    success: bool,
) -> Vec<(&'static str, String)> {
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    if let (true, Some(result)) = (success, result) {
        let ntuples = result.cmd_tuples();
        return vec![
            ("ERROR", "false".to_string()),
            ("SQLSTATE", "00000".to_string()),
            (
                "ROW_COUNT",
                if ntuples.is_empty() {
                    "0".to_string()
                } else {
                    text(ntuples)
                },
            ),
        ];
    }
    let error = result.and_then(QueryResult::error);
    let code = error
        .and_then(|e| e.field(diag::SQLSTATE))
        .map_or_else(String::new, text);
    let mesg = error
        .and_then(|e| e.field(diag::MESSAGE_PRIMARY))
        .map_or_else(String::new, text);
    vec![
        ("ERROR", "true".to_string()),
        ("SQLSTATE", code.clone()),
        ("ROW_COUNT", "0".to_string()),
        ("LAST_ERROR_SQLSTATE", code),
        ("LAST_ERROR_MESSAGE", mesg),
    ]
}

/// `SetResultVariables()` (`common.c:478`): the action over
/// [`result_variables`].
fn set_result_variables(vars: &mut VariableSpace, result: Option<&QueryResult>, success: bool) {
    for (name, value) in result_variables(result, success) {
        // None of these names has a hook, so the assignment cannot be
        // refused.
        let _ = vars.set(name, Some(&value));
    }
}

/// `PrintQueryResult()` (`common.c:1043`), for the statuses a simple query
/// produces: the rows and the status line of the last result, or of every
/// result under `SHOW_ALL_RESULTS`.
fn print_query_result(
    result: &QueryResult,
    last: bool,
    pset: &PsqlSettings,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let shown = last || pset.show_all_results;
    let mut success = true;
    match result.status() {
        ExecStatus::TuplesOk if shown => {
            match print_query(result, &pset.popt) {
                Ok(text) => {
                    let _ = stdout.write_all(&text);
                }
                Err(err) => {
                    log(stderr, pset, Level::Error, err.to_string());
                    success = false;
                }
            }
            if let Some(status) = query_status_line(result, pset) {
                let _ = stdout.write_all(&status);
            }
        }
        ExecStatus::CommandOk if shown => {
            if let Some(status) = query_status_line(result, pset) {
                let _ = stdout.write_all(&status);
            }
        }
        _ => {}
    }
    success
}

/// `SendQuery()` (`common.c:1126`), for the simple-query path.
///
/// Returns whether the query succeeded, which is what `MainLoop` tests against
/// `ON_ERROR_STOP`. `ERROR`, `SQLSTATE`, `ROW_COUNT` and the `LAST_ERROR_*`
/// pair are set in `vars` from the outcome, as upstream sets them for every
/// query the user typed.
pub fn send_query(
    executor: &mut dyn Executor,
    query: &[u8],
    pset: &PsqlSettings,
    vars: &mut VariableSpace,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    if query.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    if let Some(line) = echo_line(query, pset) {
        let _ = stdout.write_all(&line);
        let _ = stdout.write_all(b"\n");
    }

    // `ExecQueryAndProcessResults()` (`common.c:1581`). A result is handled
    // only once the next one has been fetched (`:2164`), so every notice
    // parsed up to then is already on stderr: hold each result back until
    // the next result, or the end, is reached.
    let mut success = true;
    let mut pending: Option<QueryResult> = None;
    for reply in executor.exec(query) {
        match reply {
            Reply::Notice(notice) => notice_processor(&notice, pset, stderr),
            Reply::Result(result) => {
                if let Some(previous) = pending.take() {
                    success = handle_result(&previous, false, success, pset, vars, stdout, stderr);
                }
                pending = Some(result);
            }
            Reply::Broken(err) => {
                if let Some(previous) = pending.take() {
                    let _ = handle_result(&previous, false, success, pset, vars, stdout, stderr);
                }
                let _ = stderr.write_all(&err.rendered());
                set_result_variables(vars, None, false);
                return false;
            }
        }
    }
    if let Some(last) = pending {
        success = handle_result(&last, true, success, pset, vars, stdout, stderr);
    }
    success
}

/// One pass of `ExecQueryAndProcessResults`'s result loop
/// (`common.c:1818`-`:2231`): report a failed result, or print a good one
/// while nothing has failed yet, and set the result variables from the last.
/// Returns the loop's `success` after this result.
fn handle_result(
    result: &QueryResult,
    last: bool,
    success: bool,
    pset: &PsqlSettings,
    vars: &mut VariableSpace,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    if !accept_result(result) {
        // `PQresultErrorMessage`: `pqBuildErrorMessage3`'s rendering, at the
        // configured verbosity (`common.c:1831`).
        let message = match result.error() {
            Some(error) => error.message(result.status(), pset.verbosity, pset.show_context),
            None => result.error_message(),
        };
        // `pg_log_info("%s", error)`, when there is one (`common.c:1833`-`:1834`).
        if !message.is_empty() {
            log(stderr, pset, Level::Info, &message);
        }
        set_result_variables(vars, Some(result), false);
        return false;
    }
    let success = success && print_query_result(result, last, pset, stdout, stderr);
    if last {
        set_result_variables(vars, Some(result), success);
    }
    success
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlibpq::{Backend, ContextVisibility, FieldDescription, QueryRunner, TransactionStatus};

    struct Replay(Vec<Vec<Reply>>);

    impl Executor for Replay {
        fn exec(&mut self, _query: &[u8]) -> Vec<Reply> {
            self.0.remove(0)
        }
        fn connected(&self) -> bool {
            true
        }
    }

    fn one_row() -> Vec<QueryResult> {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(vec![FieldDescription {
                name: b"?column?".to_vec(),
                tableid: 0,
                columnid: 0,
                typid: 23,
                typlen: 4,
                atttypmod: -1,
                format: 0,
            }]))
            .unwrap();
        runner
            .push(Backend::DataRow(vec![Some(b"1".to_vec())]))
            .unwrap();
        runner
            .push(Backend::CommandComplete(b"SELECT 1".to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results()
    }

    fn command_ok(tag: &str) -> Vec<QueryResult> {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::CommandComplete(tag.as_bytes().to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results()
    }

    fn run(results: Vec<QueryResult>, pset: &PsqlSettings) -> (bool, String, String) {
        let (ok, out, err, _) = run_replies(
            results.into_iter().map(Reply::Result).collect(),
            pset,
            &mut VariableSpace::new(),
        );
        (ok, out, err)
    }

    /// `send_query` over `replies`, with stdout and stderr as one stream the
    /// way pg_regress's `2>&1` has them, then each on its own.
    fn run_replies(
        replies: Vec<Reply>,
        pset: &PsqlSettings,
        vars: &mut VariableSpace,
    ) -> (bool, String, String, String) {
        let mut executor = Replay(vec![replies]);
        let both = Shared::default();
        let mut out = Tee(Vec::new(), both.clone());
        let mut err = Tee(Vec::new(), both.clone());
        let ok = send_query(&mut executor, b"select 1", pset, vars, &mut out, &mut err);
        (
            ok,
            String::from_utf8(out.0).unwrap(),
            String::from_utf8(err.0).unwrap(),
            String::from_utf8(both.0.borrow().clone()).unwrap(),
        )
    }

    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);

    /// A stream kept on its own and copied into a [`Shared`] one.
    struct Tee(Vec<u8>, Shared);

    impl Write for Tee {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.extend_from_slice(buf);
            self.1.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn server_error(fields: &[(u8, &[u8])]) -> ResultError {
        ResultError::new(fields.iter().map(|(c, v)| (*c, v.to_vec())).collect())
    }

    fn error_result(fields: &[(u8, &[u8])]) -> QueryResult {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::ErrorResponse(server_error(fields)))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results().remove(0)
    }

    fn empty_query() -> QueryResult {
        let mut runner = QueryRunner::new();
        runner.push(Backend::EmptyQueryResponse).unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results().remove(0)
    }

    const SYNTAX_ERROR: &[(u8, &[u8])] = &[
        (b'S', b"ERROR"),
        (b'V', b"ERROR"),
        (b'C', b"42601"),
        (b'M', b"syntax error at or near \";\""),
        (b'P', b"15"),
    ];

    fn variables(vars: &VariableSpace) -> Vec<(&'static str, Option<String>)> {
        [
            "ERROR",
            "SQLSTATE",
            "ROW_COUNT",
            "LAST_ERROR_MESSAGE",
            "LAST_ERROR_SQLSTATE",
        ]
        .into_iter()
        .map(|name| (name, vars.get(name).map(str::to_string)))
        .collect()
    }

    /// psql.sql:1177-1208, `-- tests for special result variables`, one
    /// query at a time: a working query, a syntax error, an empty query that
    /// keeps the `LAST_ERROR_*` pair, and another error that replaces it.
    #[test]
    fn each_query_sets_the_special_result_variables() {
        let pset = PsqlSettings::default();
        let mut vars = VariableSpace::new();
        let some = |v: &str| Some(v.to_string());

        let mut two = one_row();
        let mut runner = QueryRunner::new();
        runner.push(Backend::RowDescription(vec![])).unwrap();
        runner
            .push(Backend::CommandComplete(b"SELECT 2".to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        two[0] = runner.into_results().remove(0);
        let _ = run_replies(vec![Reply::Result(two.remove(0))], &pset, &mut vars);
        assert_eq!(
            variables(&vars),
            [
                ("ERROR", some("false")),
                ("SQLSTATE", some("00000")),
                ("ROW_COUNT", some("2")),
                ("LAST_ERROR_MESSAGE", None),
                ("LAST_ERROR_SQLSTATE", None),
            ]
        );

        let _ = run_replies(
            vec![Reply::Result(error_result(SYNTAX_ERROR))],
            &pset,
            &mut vars,
        );
        let after_error = [
            ("ERROR", some("true")),
            ("SQLSTATE", some("42601")),
            ("ROW_COUNT", some("0")),
            ("LAST_ERROR_MESSAGE", some("syntax error at or near \";\"")),
            ("LAST_ERROR_SQLSTATE", some("42601")),
        ];
        assert_eq!(variables(&vars), after_error);

        let _ = run_replies(vec![Reply::Result(empty_query())], &pset, &mut vars);
        assert_eq!(
            variables(&vars),
            [
                ("ERROR", some("false")),
                ("SQLSTATE", some("00000")),
                ("ROW_COUNT", some("0")),
                after_error[3].clone(),
                after_error[4].clone(),
            ],
            "must have kept previous values (psql.sql:1198)"
        );

        let _ = run_replies(
            command_ok("CREATE TABLE")
                .into_iter()
                .map(Reply::Result)
                .collect(),
            &pset,
            &mut vars,
        );
        assert_eq!(vars.get("ROW_COUNT"), Some("0"), "a tag with no count");
    }

    #[test]
    fn a_message_that_is_not_utf8_is_stored_lossily() {
        // docs/divergences.md: the variable space holds strings.
        let result = error_result(&[(b'S', b"ERROR"), (b'C', b"XX000"), (b'M', b"bad \xff")]);
        let vars = result_variables(Some(&result), false);
        assert_eq!(vars[4], ("LAST_ERROR_MESSAGE", "bad \u{fffd}".to_string()));
    }

    /// `common.c:496`: an error with no SQLSTATE — libpq's own, such as a lost
    /// connection — sets it and the message empty.
    #[test]
    fn a_broken_connection_sets_an_empty_sqlstate() {
        let mut vars = VariableSpace::new();
        let (ok, _, _, _) = run_replies(
            vec![Reply::Broken(ErrorMessage::new(
                b"server closed the connection".to_vec(),
            ))],
            &PsqlSettings::default(),
            &mut vars,
        );
        assert!(!ok);
        let some = |v: &str| Some(v.to_string());
        assert_eq!(
            variables(&vars),
            [
                ("ERROR", some("true")),
                ("SQLSTATE", some("")),
                ("ROW_COUNT", some("0")),
                ("LAST_ERROR_MESSAGE", some("")),
                ("LAST_ERROR_SQLSTATE", some("")),
            ]
        );
    }

    /// psql.sql:1142-1163, `-- SHOW_CONTEXT`: a notice is rendered with the
    /// connection's settings as a NONFATAL result, so its CONTEXT line shows
    /// only under `always`, while the error's shows under `errors` too.
    #[test]
    fn show_context_decides_the_context_of_notices_and_errors_apart() {
        let notice = server_error(&[
            (b'S', b"NOTICE"),
            (b'V', b"NOTICE"),
            (b'C', b"00000"),
            (b'M', b"foo"),
            (b'W', b"PL/pgSQL function inline_code_block line 3 at RAISE"),
        ]);
        let error = error_result(&[
            (b'S', b"ERROR"),
            (b'V', b"ERROR"),
            (b'C', b"P0001"),
            (b'M', b"bar"),
            (b'W', b"PL/pgSQL function inline_code_block line 4 at RAISE"),
        ]);
        let mut expected = [
            "NOTICE:  foo\nERROR:  bar\n",
            "NOTICE:  foo\nERROR:  bar\nCONTEXT:  PL/pgSQL function inline_code_block line 4 at RAISE\n",
            "NOTICE:  foo\nCONTEXT:  PL/pgSQL function inline_code_block line 3 at RAISE\n\
             ERROR:  bar\nCONTEXT:  PL/pgSQL function inline_code_block line 4 at RAISE\n",
        ]
        .into_iter();
        for show_context in [
            ContextVisibility::Never,
            ContextVisibility::Errors,
            ContextVisibility::Always,
        ] {
            let pset = PsqlSettings {
                log_terse: true,
                show_context,
                ..PsqlSettings::default()
            };
            let (ok, out, err, _) = run_replies(
                vec![Reply::Notice(notice.clone()), Reply::Result(error.clone())],
                &pset,
                &mut VariableSpace::new(),
            );
            assert!(!ok);
            assert_eq!(out, "");
            assert_eq!(err, expected.next().unwrap(), "{show_context:?}");
        }
    }

    /// `common.c:2164`: a result is printed only after the next one has been
    /// fetched, so a notice that came between two results is already on
    /// stderr when the first is printed.
    #[test]
    fn a_notice_between_two_results_is_printed_before_the_first() {
        let pset = PsqlSettings {
            log_terse: true,
            show_all_results: true,
            ..PsqlSettings::default()
        };
        let notice = server_error(&[(b'S', b"NOTICE"), (b'M', b"between")]);
        let mut replies: Vec<Reply> = command_ok("CREATE TABLE")
            .into_iter()
            .map(Reply::Result)
            .collect();
        replies.push(Reply::Notice(notice));
        replies.extend(command_ok("DROP TABLE").into_iter().map(Reply::Result));
        let (ok, _, _, both) = run_replies(replies, &pset, &mut VariableSpace::new());
        assert!(ok);
        assert_eq!(both, "NOTICE:  between\nCREATE TABLE\nDROP TABLE\n");
    }

    #[test]
    fn a_select_prints_the_table_and_nothing_else() {
        let (ok, out, err) = run(one_row(), &PsqlSettings::default());
        assert!(ok);
        assert_eq!(out, " ?column? \n----------\n        1\n(1 row)\n\n");
        assert_eq!(err, "");
    }

    #[test]
    fn a_command_prints_its_tag() {
        let (ok, out, _) = run(command_ok("CREATE TABLE"), &PsqlSettings::default());
        assert!(ok);
        assert_eq!(out, "CREATE TABLE\n");
    }

    #[test]
    fn quiet_suppresses_the_tag() {
        let pset = PsqlSettings {
            quiet: true,
            ..PsqlSettings::default()
        };
        let (_, out, _) = run(command_ok("CREATE TABLE"), &pset);
        assert_eq!(out, "");
    }

    #[test]
    fn a_plain_select_tag_is_not_printed_but_a_returning_one_is() {
        // `PrintQueryStatus` (`common.c:996`).
        let pset = PsqlSettings::default();
        let results = one_row();
        assert_eq!(query_status_line(&results[0], &pset), None);

        let mut runner = QueryRunner::new();
        runner.push(Backend::RowDescription(vec![])).unwrap();
        runner
            .push(Backend::CommandComplete(b"INSERT 0 1".to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        let results = runner.into_results();
        assert_eq!(
            query_status_line(&results[0], &pset),
            Some(b"INSERT 0 1\n".to_vec())
        );
    }

    #[test]
    fn an_error_result_goes_to_stderr_and_fails() {
        let error = ResultError::new(vec![
            (b'S', b"ERROR".to_vec()),
            (b'C', b"42601".to_vec()),
            (b'M', b"syntax error at or near \"selec\"".to_vec()),
        ]);
        let mut runner = QueryRunner::new();
        runner.push(Backend::ErrorResponse(error)).unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        let results = runner.into_results();
        // Terse, as under `-c` or a pipe: the server's text alone.
        let terse = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        let (ok, out, err) = run(results.clone(), &terse);
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(err, "ERROR:  syntax error at or near \"selec\"\n");
        // Under `-f`, `pg_log_info` names the file and line (`common.c:457`).
        let file = PsqlSettings {
            inputfile: Some("x.sql".into()),
            lineno: 4,
            ..PsqlSettings::default()
        };
        let (_, _, err) = run(results, &file);
        assert_eq!(
            err,
            "psql:x.sql:4: ERROR:  syntax error at or near \"selec\"\n"
        );
    }

    #[test]
    fn echo_queries_prints_the_query_first() {
        let pset = PsqlSettings {
            echo: Echo::Queries,
            ..PsqlSettings::default()
        };
        assert_eq!(echo_line(b"select 1", &pset), Some(b"select 1".to_vec()));
        let (_, out, _) = run(one_row(), &pset);
        assert!(out.starts_with("select 1\n ?column?"), "{out}");
    }

    #[test]
    fn echo_all_does_not_echo_here_because_its_caller_already_did() {
        // `common.c:1158`: SendQuery echoes only for PSQL_ECHO_QUERIES.
        // MainLoop and the `-c` action loop own the ECHO=all echo, so doing it
        // here too printed the query twice.
        let pset = PsqlSettings {
            echo: Echo::All,
            ..PsqlSettings::default()
        };
        assert_eq!(echo_line(b"select 1", &pset), None);
        let (_, out, _) = run(one_row(), &pset);
        assert!(out.starts_with(" ?column?"), "{out}");
    }

    #[test]
    fn echo_none_and_echo_errors_print_nothing_beforehand() {
        for echo in [Echo::None, Echo::Errors] {
            let pset = PsqlSettings {
                echo,
                ..PsqlSettings::default()
            };
            assert_eq!(echo_line(b"select 1", &pset), None);
        }
    }

    #[test]
    fn a_broken_connection_reaches_stderr_with_its_bytes_unchanged() {
        // The whole reason `ErrorMessage` carries bytes: `0xC3 0x28` is not
        // valid UTF-8, and C psql writes those two bytes. Decoding the message
        // anywhere between the socket and stderr turns them into U+FFFD.
        struct Broken;
        impl Executor for Broken {
            fn exec(&mut self, _query: &[u8]) -> Vec<Reply> {
                vec![Reply::Broken(ErrorMessage::new(
                    b"no such database \"\xc3\x28\"".to_vec(),
                ))]
            }
            fn connected(&self) -> bool {
                false
            }
        }

        let mut out = Vec::new();
        let mut err = Vec::new();
        let ok = send_query(
            &mut Broken,
            b"select 1",
            &PsqlSettings::default(),
            &mut VariableSpace::new(),
            &mut out,
            &mut err,
        );

        assert!(!ok);
        assert_eq!(err, b"psql: error: no such database \"\xc3\x28\"\n");
        assert!(!err.contains(&0xEF), "no U+FFFD may appear: {err:?}");
    }

    #[test]
    fn a_libpq_error_keeps_its_bytes_on_the_way_into_an_error_message() {
        // `rlibpq` keeps the server's tokens as bytes on purpose; the
        // conversion at this crate's boundary must not undo that.
        let error = ResultError::new(vec![
            (b'S', b"FATAL".to_vec()),
            (b'C', b"3D000".to_vec()),
            (b'M', b"database \"\xff\xfe\" does not exist".to_vec()),
        ]);

        let message = ErrorMessage::from(rlibpq::ConnectionError::Server(Box::new(error)));

        assert!(
            message.as_bytes().windows(2).any(|w| w == b"\xff\xfe"),
            "{:?}",
            message.as_bytes()
        );
    }

    #[test]
    fn a_rendered_error_is_the_prefix_the_message_and_one_newline() {
        let message = ErrorMessage::new(b"connection refused".to_vec());
        assert_eq!(message.rendered(), b"psql: error: connection refused\n");
    }

    #[test]
    fn an_all_whitespace_query_is_not_sent() {
        struct Never;
        impl Executor for Never {
            fn exec(&mut self, _query: &[u8]) -> Vec<Reply> {
                panic!("an empty query must not reach the server");
            }
            fn connected(&self) -> bool {
                true
            }
        }
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(send_query(
            &mut Never,
            b"  \n ",
            &PsqlSettings::default(),
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
    }
}
