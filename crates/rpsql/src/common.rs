//! Sending a query and printing what came back: `src/bin/psql/common.c`.
//!
//! `SendQuery` (`common.c:1180`) is an action — it writes to a socket and to
//! two streams — so it is a thin shell here over two pure calculations:
//! [`echo_line`], which decides what `ECHO` puts on stdout before the query
//! runs, and [`crate::print::print_query`], which renders the result.

use std::io::Write;

use rlibpq::{ExecStatus, QueryResult};

use crate::print::print_query;
use crate::settings::{Echo, PsqlSettings};

/// What psql can do with a query. An [`Executor`] is the only thing in the
/// crate that holds a connection, which keeps every other module testable
/// without a server.
pub trait Executor {
    /// `PQexec` plus `ProcessResult` (`common.c:1428`): run `query` and hand
    /// back every result it produced.
    ///
    /// # Errors
    /// The connection broke. A *failed query* is not an error: it comes back
    /// as a `PGRES_FATAL_ERROR` result, as in libpq.
    fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, String>;

    /// `pset.db != NULL` (`mainloop.c:557`).
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

/// `PrintQueryStatus()` (`common.c:344`): the command tag, unless this is a
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

/// `SendQuery()` (`common.c:1180`), for the simple-query path.
///
/// Returns whether the query succeeded, which is what `MainLoop` tests against
/// `ON_ERROR_STOP`.
pub fn send_query(
    executor: &mut dyn Executor,
    query: &[u8],
    pset: &PsqlSettings,
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

    let results = match executor.exec(query) {
        Ok(results) => results,
        Err(message) => {
            let _ = write!(stderr, "{message}");
            return false;
        }
    };

    let mut ok = true;
    let last = results.len().saturating_sub(1);
    for (i, result) in results.iter().enumerate() {
        let is_last = i == last;
        if !(is_last || pset.show_all_results) {
            continue;
        }
        match result.status() {
            ExecStatus::TuplesOk => {
                match print_query(result, &pset.popt) {
                    Ok(text) => {
                        let _ = stdout.write_all(&text);
                    }
                    Err(err) => {
                        let _ = writeln!(stderr, "psql: error: {err}");
                        ok = false;
                    }
                }
                if let Some(status) = query_status_line(result, pset) {
                    let _ = stdout.write_all(&status);
                }
            }
            ExecStatus::CommandOk => {
                if let Some(status) = query_status_line(result, pset) {
                    let _ = stdout.write_all(&status);
                }
            }
            ExecStatus::EmptyQuery => {}
            _ => {
                // `pqBuildErrorMessage3`'s rendering, at the configured
                // verbosity (`common.c:594`).
                let message = match result.error() {
                    Some(error) => {
                        error.message(result.status(), pset.verbosity, pset.show_context)
                    }
                    None => result.error_message(),
                };
                let _ = stderr.write_all(&message);
                ok = false;
            }
        }
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlibpq::{Backend, FieldDescription, QueryRunner, ResultError, TransactionStatus};

    struct Replay(Vec<Vec<QueryResult>>);

    impl Executor for Replay {
        fn exec(&mut self, _query: &[u8]) -> Result<Vec<QueryResult>, String> {
            Ok(self.0.remove(0))
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
        let mut executor = Replay(vec![results]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let ok = send_query(&mut executor, b"select 1", pset, &mut out, &mut err);
        (
            ok,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
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
        // `PrintQueryStatus` (`common.c:352`).
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
        let (ok, out, err) = run(runner.into_results(), &PsqlSettings::default());
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(err, "ERROR:  syntax error at or near \"selec\"\n");
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
    fn an_all_whitespace_query_is_not_sent() {
        struct Never;
        impl Executor for Never {
            fn exec(&mut self, _query: &[u8]) -> Result<Vec<QueryResult>, String> {
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
            &mut out,
            &mut err
        ));
    }
}
