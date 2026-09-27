//! Sending a query and printing what came back: `src/bin/psql/common.c`.
//!
//! `SendQuery` (`common.c:1126`) is an action — it writes to a socket and to
//! two streams — so it is a thin shell here over two pure calculations:
//! [`echo_line`], which decides what `ECHO` puts on stdout before the query
//! runs, and [`crate::print::print_query`], which renders the result.
//!
//! `ExecQueryAndProcessResults` (`common.c:1581`) has two halves here. Outside
//! a pipeline a query is one blocking [`Executor::exec`] and its results are
//! printed afterwards. In a pipeline, and for the commands that drive one,
//! [`exec_pipelined`] ports the `PQsend…` / `PQgetResult` loop itself,
//! because there the counters psql keeps (`settings.h:126`) decide how many
//! results to read, and reading one too many would block.

use std::io::Write;
use std::time::Instant;

use rlibpq::{ConnectionError, ExecStatus, PipelineStatus, QueryResult, ResultError};

use crate::crosstab::{CtvArgs, print_result_in_crosstab};
use crate::logging;
use crate::print::print_query;
use crate::settings::{Echo, PipelineCounters, PsqlSettings, SendMode};
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

/// What psql can do with a query. An [`Executor`] is the only thing in the
/// crate that holds a connection, which keeps every other module testable
/// without a server.
///
/// [`Executor::exec`] is the whole of a query outside a pipeline. The rest
/// are the libpq calls a pipeline is driven with, one for one; their default
/// bodies are an executor that never enters pipeline mode.
pub trait Executor {
    /// `ExecQueryAndProcessResults` (`common.c:1581`), minus the printing: send
    /// `query` the way `mode` says (`common.c:1602`) and hand back every
    /// result it produced. `\close_prepared` sends no query text, and the
    /// query is then ignored.
    ///
    /// # Errors
    /// The connection broke, and [`ErrorMessage`] is libpq's error buffer. A
    /// *failed query* is not an error: it comes back as a `PGRES_FATAL_ERROR`
    /// result, as in libpq.
    fn exec(&mut self, query: &[u8], mode: &SendMode) -> Result<Vec<QueryResult>, ErrorMessage>;

    /// `pset.db != NULL` (`mainloop.c:592`).
    fn connected(&self) -> bool;

    /// Give the connection up: [`Executor::connected`] is false from then
    /// on, which is how a non-interactive `MainLoop` comes to exit with
    /// `EXIT_BADCONN`, as psql's own `exit(EXIT_BADCONN)` does.
    fn abandon(&mut self);

    /// `PQpipelineStatus`.
    fn pipeline_status(&self) -> PipelineStatus {
        PipelineStatus::Off
    }

    /// The libpq call each `mode` makes in `ExecQueryAndProcessResults`'
    /// `switch` (`common.c:1602`-`:1724`), without waiting for a result:
    /// a `PQsend…` for a statement (`PQsendQueryParams` with no parameters
    /// for a plain query in a pipeline, `:1716`), `PQenterPipelineMode`,
    /// `PQpipelineSync`, `PQsendPipelineSync`, `PQflush` or
    /// `PQsendFlushRequest`. `GetResults` sends nothing and is not passed.
    ///
    /// # Errors
    /// libpq refused the call, and [`ErrorMessage`] is its error buffer —
    /// `cannot send pipeline when not in pipeline mode`, say — or the
    /// connection broke.
    fn send(&mut self, query: &[u8], mode: &SendMode) -> Result<(), ErrorMessage> {
        let _ = (query, mode);
        Err(ErrorMessage::new(
            b"this connection has no pipeline mode".to_vec(),
        ))
    }

    /// `PQgetResult`: the next result, or `None` for the NULL that ends a
    /// command.
    ///
    /// # Errors
    /// The connection broke.
    fn get_result(&mut self) -> Result<Option<QueryResult>, ErrorMessage> {
        Ok(None)
    }

    /// `PQexitPipelineMode`.
    ///
    /// # Errors
    /// libpq refused: results are still to be read, or a command is busy.
    fn exit_pipeline_mode(&mut self) -> Result<(), ErrorMessage> {
        Ok(())
    }

    /// The notices received since the last call, oldest first: what libpq
    /// hands to psql's `NoticeProcessor` (`common.c:281`) as it parses them.
    fn take_notices(&mut self) -> Vec<ResultError> {
        Vec::new()
    }
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

/// `PrintTiming()` (`common.c:598`): `\timing`'s line for a query that took
/// `elapsed_msec`, broken down into minutes, hours and days from one second
/// up.
#[must_use]
// `(int) minutes` and friends: whole, non-negative values that fit, as
// upstream's casts assume.
#[allow(clippy::cast_possible_truncation)]
pub fn timing_line(elapsed_msec: f64) -> String {
    if elapsed_msec < 1000.0 {
        return format!("Time: {elapsed_msec:.3} ms\n");
    }
    let mut seconds = elapsed_msec / 1000.0;
    let mut minutes = (seconds / 60.0).floor();
    seconds -= 60.0 * minutes;
    if minutes < 60.0 {
        return format!(
            "Time: {elapsed_msec:.3} ms ({:02}:{seconds:06.3})\n",
            minutes as i32
        );
    }
    let mut hours = (minutes / 60.0).floor();
    minutes -= 60.0 * hours;
    if hours < 24.0 {
        return format!(
            "Time: {elapsed_msec:.3} ms ({:02}:{:02}:{seconds:06.3})\n",
            hours as i32, minutes as i32
        );
    }
    let days = (hours / 24.0).floor();
    hours -= 24.0 * days;
    format!(
        "Time: {elapsed_msec:.3} ms ({days:.0} d {:02}:{:02}:{seconds:06.3})\n",
        hours as i32, minutes as i32
    )
}

/// `ClearOrSaveResult()` (`common.c:560`): keep an error result for
/// `\errverbose`, drop anything else.
pub fn clear_or_save_result(result: &QueryResult, pset: &mut PsqlSettings) {
    if matches!(
        result.status(),
        ExecStatus::NonfatalError | ExecStatus::FatalError
    ) {
        pset.last_error_result = Some(result.clone());
    }
}

/// `SetPipelineVariables()` (`common.c:536`): the three variables that
/// publish the counters, as `%d` writes them.
#[must_use]
pub fn pipeline_variables(counters: &PipelineCounters) -> [(&'static str, String); 3] {
    [
        ("PIPELINE_SYNC_COUNT", counters.piped_syncs.to_string()),
        (
            "PIPELINE_COMMAND_COUNT",
            counters.piped_commands.to_string(),
        ),
        (
            "PIPELINE_RESULT_COUNT",
            counters.available_results.to_string(),
        ),
    ]
}

/// Action: [`pipeline_variables`] into `vars`. None of the three has a hook,
/// so setting one cannot fail.
fn set_pipeline_variables(counters: &PipelineCounters, vars: &mut VariableSpace) {
    for (name, value) in pipeline_variables(counters) {
        let _ = vars.set(name, Some(&value));
    }
}

/// `AcceptResult()`'s verdict (`common.c:418`), without its logging: the
/// statuses that are not a failure.
#[must_use]
pub fn accept_result(status: ExecStatus) -> bool {
    matches!(
        status,
        ExecStatus::CommandOk
            | ExecStatus::TuplesOk
            | ExecStatus::TuplesChunk
            | ExecStatus::EmptyQuery
            | ExecStatus::CopyIn
            | ExecStatus::CopyOut
            | ExecStatus::PipelineSync
    )
}

/// `PQresultErrorMessage`: `pqBuildErrorMessage3`'s rendering of a failed
/// result at the configured verbosity (`common.c:1831`). Empty for a
/// `PGRES_PIPELINE_ABORTED` result, which carries no error.
fn result_error_message(result: &QueryResult, pset: &PsqlSettings) -> Vec<u8> {
    match result.error() {
        Some(error) => error.message(result.status(), pset.verbosity, pset.show_context),
        None => result.error_message(),
    }
}

/// Action: psql's `NoticeProcessor` (`common.c:281`), `pg_log_info` of what
/// libpq built for each notice, at the connection's verbosity.
fn print_notices(executor: &mut dyn Executor, pset: &PsqlSettings, stderr: &mut dyn Write) {
    for notice in executor.take_notices() {
        let message = notice.message(ExecStatus::NonfatalError, pset.verbosity, pset.show_context);
        logging::info(pset, &message, stderr);
    }
}

/// `SendQuery()` (`common.c:1126`).
///
/// Returns whether the query succeeded, which is what `MainLoop` tests against
/// `ON_ERROR_STOP`. `pset` is written because a failed result is kept for
/// `\errverbose`.
///
/// The one-shot requests a backslash command leaves for this query —
/// `\crosstabview`'s, `\bind`'s and its siblings' send mode, and the print
/// options `\g (…)` or `\gx` saved — are cleared on the way out whatever
/// happened (`common.c:1311`-`:1341`). Here the first two are taken on the
/// way in, which nothing between the two can tell apart, and the saved print
/// options are put back on the way out.
pub fn send_query(
    executor: &mut dyn Executor,
    query: &[u8],
    pset: &mut PsqlSettings,
    vars: &mut VariableSpace,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let crosstab = pset.crosstab.take();
    let mode = std::mem::take(&mut pset.send_mode);
    let ok = if mode.is_pipeline_control() || executor.pipeline_status() != PipelineStatus::Off {
        if let Some(line) = echo_line(query, pset) {
            let _ = stdout.write_all(&line);
            let _ = stdout.write_all(b"\n");
        }
        exec_pipelined(
            executor,
            query,
            &mode,
            crosstab.as_ref(),
            pset,
            vars,
            stdout,
            stderr,
        )
    } else {
        send_query_with(
            executor,
            query,
            &mode,
            crosstab.as_ref(),
            pset,
            stdout,
            stderr,
        )
    };
    // `restorePsetInfo` (`common.c:1319`).
    if let Some(saved) = pset.gsavepopt.take() {
        pset.popt = saved;
    }
    ok
}

/// [`send_query`] once its one-shot requests are taken, outside a pipeline.
fn send_query_with(
    executor: &mut dyn Executor,
    query: &[u8],
    mode: &SendMode,
    crosstab: Option<&CtvArgs>,
    pset: &mut PsqlSettings,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    // An empty simple query comes back as PGRES_EMPTY_QUERY, which prints
    // nothing, so it is not sent. Every extended mode is: an empty Parse is
    // still a statement, and `\close_prepared` sends no query at all.
    if *mode == SendMode::Query && query.iter().all(u8::is_ascii_whitespace) {
        return true;
    }
    if let Some(line) = echo_line(query, pset) {
        let _ = stdout.write_all(&line);
        let _ = stdout.write_all(b"\n");
    }

    // `common.c:1128`: whether to time is decided before the query runs.
    let timing = pset.timing;
    // `ExecQueryAndProcessResults` takes its "after" once the last result has
    // arrived and before it is printed (`common.c:2197`); `exec` returns only
    // when every result has arrived, so this is the same interval.
    let before = Instant::now();
    let results = executor.exec(query, mode);
    let elapsed_msec = before.elapsed().as_secs_f64() * 1000.0;
    // The whole query has been read, so every notice it drew comes first.
    print_notices(executor, pset, stderr);

    let ok = match results {
        Ok(results) => process_results(&results, crosstab, pset, stdout, stderr),
        // libpq's message, at info level like the server's errors
        // (`common.c:1834`): `001_basic.pl:147` expects
        // `psql:<stdin>:2: server closed the connection unexpectedly`.
        Err(err) => {
            logging::info(pset, err.as_bytes(), stderr);
            false
        }
    };

    // `common.c:1286`: the timing line follows success and failure alike,
    // which is what `001_basic.pl:95` tests.
    if timing {
        let _ = stdout.write_all(timing_line(elapsed_msec).as_bytes());
    }
    ok
}

/// The printing half of `ExecQueryAndProcessResults()` (`common.c:1581`).
fn process_results(
    results: &[QueryResult],
    crosstab: Option<&CtvArgs>,
    pset: &mut PsqlSettings,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let mut ok = true;
    let last = results.len().saturating_sub(1);
    for (i, result) in results.iter().enumerate() {
        let is_last = i == last;
        if accept_result(result.status()) {
            ok &= print_query_result(result, is_last, crosstab, pset, stdout, stderr);
        } else {
            // `common.c:1825`-`:1843`: an error is reported whether or not it
            // is the last result, as `PQresultErrorMessage` renders it at the
            // configured verbosity, and kept for `\errverbose`.
            let message = result_error_message(result, pset);
            if !message.is_empty() {
                logging::info(pset, &message, stderr);
            }
            clear_or_save_result(result, pset);
            ok = false;
        }
    }
    ok
}

/// `PrintQueryResult()` (`common.c:1043`), for a result `AcceptResult`
/// passed: print the rows and the status line, or pivot the last result for
/// `\crosstabview`.
fn print_query_result(
    result: &QueryResult,
    last: bool,
    crosstab: Option<&CtvArgs>,
    pset: &PsqlSettings,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    if !(last || pset.show_all_results) {
        return true;
    }
    match result.status() {
        ExecStatus::TuplesOk => {
            let printed = match crosstab {
                Some(args) if last => {
                    print_result_in_crosstab(result, args, &pset.popt).map_err(|err| err.message())
                }
                _ => print_query(result, &pset.popt).map_err(|err| err.to_string().into_bytes()),
            };
            let ok = match printed {
                Ok(text) => {
                    let _ = stdout.write_all(&text);
                    true
                }
                Err(message) => {
                    logging::error(pset, &message, stderr);
                    false
                }
            };
            if let Some(status) = query_status_line(result, pset) {
                let _ = stdout.write_all(&status);
            }
            ok
        }
        ExecStatus::CommandOk => {
            if let Some(status) = query_status_line(result, pset) {
                let _ = stdout.write_all(&status);
            }
            true
        }
        _ => true,
    }
}

/// Where the pipeline loop stands after a libpq call: the connection broke,
/// which ends the loop with libpq's message.
struct ConnectionLost(ErrorMessage);

/// Action: `PQgetResult`, then the notices parsing it drew, which psql's
/// notice processor prints as libpq meets them — before the result.
fn fetch(
    executor: &mut dyn Executor,
    pset: &PsqlSettings,
    stderr: &mut dyn Write,
) -> Result<Option<QueryResult>, ConnectionLost> {
    let result = executor.get_result();
    print_notices(executor, pset, stderr);
    result.map_err(ConnectionLost)
}

/// `discardAbortedPipelineResults()` (`common.c:1478`): read and drop the
/// results of an aborted pipeline, up to its next sync, a fatal error, or
/// the last result there is to read without blocking.
fn discard_aborted_pipeline_results(
    executor: &mut dyn Executor,
    pset: &mut PsqlSettings,
    stderr: &mut dyn Write,
) -> Result<Option<QueryResult>, ConnectionLost> {
    loop {
        let res = fetch(executor, pset, stderr)?;
        match res.as_ref().map(QueryResult::status) {
            // A synchronisation point; the caller decrements the sync counter.
            Some(ExecStatus::PipelineSync) => return Ok(res),
            // A FATAL error from the backend: consume the end of the current
            // query, and let the outer loop report it.
            Some(ExecStatus::FatalError) => {
                let _ = fetch(executor, pset, stderr)?;
                return Ok(res);
            }
            Some(_) => {}
            // A query was processed. An error from the Sync itself is not
            // counted in available_results, hence the guards.
            None => {
                if !executor.connected() {
                    return Ok(None);
                }
                let c = &mut pset.pipeline;
                c.available_results = c.available_results.saturating_sub(1);
                c.requested_results = c.requested_results.saturating_sub(1);
            }
        }
        let c = &pset.pipeline;
        if c.requested_results == 0 {
            // Every requested result is read.
            return Ok(res);
        }
        if c.available_results == 0 && c.piped_syncs == 0 {
            // Nothing more to read and no sync to stop at: the pipeline stays
            // aborted.
            return Ok(res);
        }
    }
}

/// The pipeline half of `ExecQueryAndProcessResults()` (`common.c:1581`):
/// everything `SendQuery` does once a pipeline is on, or a pipeline command
/// asks for one.
///
/// The counters decide what is read. A command sent in a pipeline is only
/// counted (`piped_commands`); a sync or a flush request makes the ones
/// before it available; `\getresults` and `\endpipeline` then read up to
/// `requested_results` of them, syncs included, and print each as it comes.
// One function, as upstream's is: the `switch` and the result loop share
// `success`, `end_pipeline` and the counters, statement for statement.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn exec_pipelined(
    executor: &mut dyn Executor,
    query: &[u8],
    mode: &SendMode,
    crosstab: Option<&CtvArgs>,
    pset: &mut PsqlSettings,
    vars: &mut VariableSpace,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let in_pipeline = |executor: &dyn Executor| executor.pipeline_status() != PipelineStatus::Off;
    let mut end_pipeline = false;

    // `common.c:1602`-`:1724`.
    let sent = match mode {
        SendMode::GetResults => {
            let c = &mut pset.pipeline;
            if c.available_results == 0 && c.piped_syncs == 0 {
                // PQgetResult() would block: nothing was synced or flushed.
                c.requested_results = 0;
                logging::info(pset, b"No pending results to get", stderr);
                Ok(false)
            } else {
                // Cap the request to the results known to be there.
                let known = c.available_results + c.piped_syncs;
                if c.requested_results == 0 || c.requested_results > known {
                    c.requested_results = known;
                }
                Ok(true)
            }
        }
        // `success = PQflush(pset.db)` (`common.c:1672`): PQflush returns 0
        // on success, so `\flush` reports failure, silently, unless the
        // flush failed (-1). Ported as upstream has it.
        SendMode::Flush => Ok(executor.send(query, mode).is_err()),
        _ => executor.send(query, mode).map(|()| true),
    };
    print_notices(executor, pset, stderr);
    let success = match sent {
        Ok(success) => success,
        Err(err) => {
            // `pg_log_info("%s", PQerrorMessage(pset.db))` (`common.c:1731`).
            if !err.as_bytes().is_empty() {
                logging::info(pset, err.as_bytes(), stderr);
            }
            false
        }
    };
    if success && in_pipeline(executor) {
        let c = &mut pset.pipeline;
        match mode {
            SendMode::Query
            | SendMode::ExtendedClose { .. }
            | SendMode::ExtendedParse { .. }
            | SendMode::ExtendedQueryParams { .. }
            | SendMode::ExtendedQueryPrepared { .. } => c.piped_commands += 1,
            SendMode::EndPipelineMode => {
                // All queued commands are to be processed: the Sync makes
                // them available, and every result is wanted.
                end_pipeline = true;
                c.piped_syncs += 1;
                c.available_results += c.piped_commands;
                c.piped_commands = 0;
                c.requested_results = c.available_results + c.piped_syncs;
            }
            SendMode::PipelineSync => {
                c.piped_syncs += 1;
                c.available_results += c.piped_commands;
                c.piped_commands = 0;
            }
            SendMode::FlushRequest => {
                c.available_results += c.piped_commands;
                c.piped_commands = 0;
            }
            SendMode::StartPipelineMode | SendMode::Flush | SendMode::GetResults => {}
        }
    }
    if !success {
        set_pipeline_variables(&pset.pipeline, vars);
        return false;
    }

    if pset.pipeline.requested_results == 0 && !end_pipeline && in_pipeline(executor) {
        // In a pipeline, and nothing to read yet (`common.c:1740`).
        set_pipeline_variables(&pset.pipeline, vars);
        return true;
    }

    let mut success = true;
    let mut broken = None;
    let mut result = match fetch(executor, pset, stderr) {
        Ok(result) => result,
        Err(err) => {
            broken = Some(err);
            None
        }
    };
    // `common.c:1818`-`:2249`.
    while let Some(current) = result.take() {
        let status = current.status();
        if !accept_result(status) {
            let error = result_error_message(&current, pset);
            if !error.is_empty() {
                logging::info(pset, &error, stderr);
            }
            clear_or_save_result(&current, pset);
            success = false;
            if status == ExecStatus::PipelineAborted {
                logging::info(pset, b"Pipeline aborted, command did not run", stderr);
            }
            // Within a pipeline, everything up to the next sync is aborted:
            // read past it, or stop where there is nothing more.
            let next =
                if (end_pipeline || pset.pipeline.requested_results > 0) && in_pipeline(executor) {
                    discard_aborted_pipeline_results(executor, pset, stderr)
                } else {
                    fetch(executor, pset, stderr)
                };
            match next {
                Ok(next) => result = next,
                Err(err) => broken = Some(err),
            }
            continue;
        }

        if matches!(status, ExecStatus::CopyIn | ExecStatus::CopyOut) && in_pipeline(executor) {
            // `common.c:1919`: COPY breaks a pipeline's synchronisation in
            // ways psql cannot track, so upstream gives the connection up.
            logging::info(
                pset,
                b"COPY in a pipeline is not supported, aborting connection",
                stderr,
            );
            executor.abandon();
            return false;
        }

        if status == ExecStatus::PipelineSync {
            let c = &mut pset.pipeline;
            c.piped_syncs = c.piped_syncs.saturating_sub(1);
            c.requested_results = c.requested_results.saturating_sub(1);
            // Past a synchronisation point, what follows prints again.
            success = true;
            if end_pipeline && c.piped_syncs == 0 {
                success &= executor.exit_pipeline_mode().is_ok();
            }
        } else if in_pipeline(executor) {
            let c = &mut pset.pipeline;
            c.available_results = c.available_results.saturating_sub(1);
            c.requested_results = c.requested_results.saturating_sub(1);
        }

        // Is this the last result? Outside a pipeline the next PQgetResult
        // says; in one, the NULL that ends this command is consumed first,
        // and the next result is only asked for if it was requested.
        let next = if in_pipeline(executor) {
            let mut next = Ok(None);
            if status != ExecStatus::PipelineSync {
                next = fetch(executor, pset, stderr);
            }
            if pset.pipeline.requested_results > 0 && matches!(next, Ok(None)) {
                next = fetch(executor, pset, stderr);
            }
            next
        } else {
            fetch(executor, pset, stderr)
        };
        let next = match next {
            Ok(next) => next,
            Err(err) => {
                broken = Some(err);
                None
            }
        };
        let last = next.is_none();

        // A sync has nothing to print.
        if status != ExecStatus::PipelineSync && success {
            success &= print_query_result(&current, last, crosstab, pset, stdout, stderr);
        }
        result = next;
    }

    if end_pipeline && broken.is_none() {
        // An error in a Sync's own processing can leave syncs unread; read
        // them before leaving pipeline mode (`common.c:2254`).
        while pset.pipeline.piped_syncs > 0 {
            match fetch(executor, pset, stderr) {
                Ok(Some(remaining)) => {
                    if remaining.status() == ExecStatus::PipelineSync {
                        pset.pipeline.piped_syncs -= 1;
                    }
                }
                Ok(None) if executor.connected() => {}
                Ok(None) => break,
                Err(err) => {
                    broken = Some(err);
                    break;
                }
            }
        }
    }
    if end_pipeline {
        pset.pipeline = PipelineCounters::default();
        if in_pipeline(executor) {
            let _ = executor.exit_pipeline_mode();
        }
    }
    set_pipeline_variables(&pset.pipeline, vars);

    if let Some(ConnectionLost(err)) = broken {
        // At info level, as the non-pipeline path reports it (`common.c:1834`).
        logging::info(pset, err.as_bytes(), stderr);
        return false;
    }
    success
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlibpq::{Backend, FieldDescription, QueryRunner, ResultError, TransactionStatus};

    struct Replay(Vec<Vec<QueryResult>>);

    impl Executor for Replay {
        fn exec(
            &mut self,
            _query: &[u8],
            _mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            Ok(self.0.remove(0))
        }
        fn connected(&self) -> bool {
            true
        }
        fn abandon(&mut self) {}
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
        let ok = send_query(
            &mut executor,
            b"select 1",
            &mut pset.clone(),
            &mut VariableSpace::new(),
            &mut out,
            &mut err,
        );
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
        // Terse, as under `-c`: the server's text alone (`common.c:1834`).
        let pset = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        let (ok, out, err) = run(runner.into_results(), &pset);
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
    fn a_broken_connection_reaches_stderr_with_its_bytes_unchanged() {
        // The whole reason `ErrorMessage` carries bytes: `0xC3 0x28` is not
        // valid UTF-8, and C psql writes those two bytes. Decoding the message
        // anywhere between the socket and stderr turns them into U+FFFD.
        struct Broken;
        impl Executor for Broken {
            fn exec(
                &mut self,
                _query: &[u8],
                _mode: &SendMode,
            ) -> Result<Vec<QueryResult>, ErrorMessage> {
                Err(ErrorMessage::new(b"no such database \"\xc3\x28\"".to_vec()))
            }
            fn connected(&self) -> bool {
                false
            }
            fn abandon(&mut self) {}
        }

        let mut out = Vec::new();
        let mut err = Vec::new();
        let ok = send_query(
            &mut Broken,
            b"select 1",
            &mut PsqlSettings::default(),
            &mut VariableSpace::new(),
            &mut out,
            &mut err,
        );

        assert!(!ok);
        assert_eq!(err, b"psql: no such database \"\xc3\x28\"\n");
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
            fn exec(
                &mut self,
                _query: &[u8],
                _mode: &SendMode,
            ) -> Result<Vec<QueryResult>, ErrorMessage> {
                panic!("an empty query must not reach the server");
            }
            fn connected(&self) -> bool {
                true
            }
            fn abandon(&mut self) {}
        }
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(send_query(
            &mut Never,
            b"  \n ",
            &mut PsqlSettings::default(),
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
    }

    fn select_error() -> Vec<QueryResult> {
        let error = ResultError::new(vec![
            (b'S', b"ERROR".to_vec()),
            (b'C', b"42703".to_vec()),
            (b'M', b"column \"error\" does not exist".to_vec()),
        ]);
        let mut runner = QueryRunner::new();
        runner.push(Backend::ErrorResponse(error)).unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results()
    }

    #[test]
    fn timing_is_milliseconds_alone_below_one_second() {
        // `PrintTiming` (`common.c:605`).
        assert_eq!(timing_line(0.0), "Time: 0.000 ms\n");
        assert_eq!(timing_line(12.3456), "Time: 12.346 ms\n");
        assert_eq!(timing_line(999.9994), "Time: 999.999 ms\n");
    }

    #[test]
    fn timing_breaks_a_longer_interval_into_its_parts() {
        // `common.c:618`-`:640`.
        assert_eq!(timing_line(1000.0), "Time: 1000.000 ms (00:01.000)\n");
        assert_eq!(timing_line(61_500.25), "Time: 61500.250 ms (01:01.500)\n");
        assert_eq!(
            timing_line(3_600_000.0),
            "Time: 3600000.000 ms (01:00:00.000)\n"
        );
        assert_eq!(
            timing_line(90_061_001.0),
            "Time: 90061001.000 ms (1 d 01:01:01.001)\n"
        );
    }

    #[test]
    fn timing_follows_the_result_and_a_failure_too() {
        // `common.c:1286`; `001_basic.pl:95` requires the line after an error.
        let pset = PsqlSettings {
            timing: true,
            ..PsqlSettings::default()
        };
        let (ok, out, _) = run(one_row(), &pset);
        assert!(ok);
        assert!(out.starts_with(" ?column? \n"), "{out}");
        assert!(out.lines().last().unwrap().starts_with("Time: "), "{out}");
        assert!(out.ends_with(" ms\n"), "{out}");

        let (ok, out, _) = run(select_error(), &pset);
        assert!(!ok);
        assert!(out.starts_with("Time: ") && out.ends_with(" ms\n"), "{out}");
    }

    #[test]
    fn without_timing_there_is_no_time_line() {
        let (_, out, _) = run(one_row(), &PsqlSettings::default());
        assert!(!out.contains("Time:"), "{out}");
    }

    #[test]
    fn a_failed_result_is_kept_for_errverbose_and_a_later_success_keeps_it() {
        // `ClearOrSaveResult` (`common.c:560`) replaces the saved result only
        // with another error.
        let mut executor = Replay(vec![select_error(), one_row()]);
        let mut pset = PsqlSettings::default();
        let mut vars = VariableSpace::new();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert!(!send_query(
            &mut executor,
            b"select error",
            &mut pset,
            &mut vars,
            &mut out,
            &mut err
        ));
        let saved = pset.last_error_result.clone().expect("the error is kept");
        assert_eq!(saved.status(), ExecStatus::FatalError);
        assert!(send_query(
            &mut executor,
            b"select 1",
            &mut pset,
            &mut vars,
            &mut out,
            &mut err
        ));
        assert_eq!(pset.last_error_result, Some(saved));
    }

    #[test]
    fn a_server_error_under_a_file_carries_the_locus() {
        // `001_basic.pl:175`: `psql:<stdin>:1: ERROR:  …`, no `error:` label,
        // because psql logs the server's text at info level (`common.c:1834`).
        let pset = PsqlSettings {
            inputfile: Some("<stdin>".into()),
            lineno: 1,
            ..PsqlSettings::default()
        };
        let (_, _, err) = run(select_error(), &pset);
        assert_eq!(
            err,
            "psql:<stdin>:1: ERROR:  column \"error\" does not exist\n"
        );
    }

    fn three_columns(rows: &[[&str; 3]]) -> Vec<QueryResult> {
        let field = |name: &str| FieldDescription {
            name: name.as_bytes().to_vec(),
            tableid: 0,
            columnid: 0,
            typid: 25,
            typlen: -1,
            atttypmod: -1,
            format: 0,
        };
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(vec![
                field("x"),
                field("y"),
                field("v"),
            ]))
            .unwrap();
        for row in rows {
            runner
                .push(Backend::DataRow(
                    row.iter().map(|c| Some(c.as_bytes().to_vec())).collect(),
                ))
                .unwrap();
        }
        runner
            .push(Backend::CommandComplete(b"SELECT".to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results()
    }

    /// Records the mode each query was sent in.
    struct Modes(Vec<(Vec<u8>, SendMode)>);

    impl Executor for Modes {
        fn exec(
            &mut self,
            query: &[u8],
            mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            self.0.push((query.to_vec(), mode.clone()));
            Ok(command_ok(""))
        }
        fn connected(&self) -> bool {
            true
        }
        fn abandon(&mut self) {}
    }

    #[test]
    fn the_send_mode_reaches_the_executor_once_and_is_then_reset() {
        // `clean_extended_state()` in `SendQuery`'s cleanup (`common.c:1325`).
        let bind = SendMode::ExtendedQueryParams {
            params: vec!["foo".into()],
        };
        let mut pset = PsqlSettings {
            send_mode: bind.clone(),
            quiet: true,
            ..PsqlSettings::default()
        };
        let mut executor = Modes(Vec::new());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert!(send_query(
            &mut executor,
            b"SELECT $1 ",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert!(send_query(
            &mut executor,
            b"SELECT 1",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(
            executor.0,
            [
                (b"SELECT $1 ".to_vec(), bind),
                (b"SELECT 1".to_vec(), SendMode::Query)
            ]
        );
        assert_eq!(pset.send_mode, SendMode::Query);
    }

    #[test]
    fn an_extended_command_is_sent_even_with_an_empty_query() {
        // `\close_prepared` sends no query text at all, and an empty Parse
        // is still a statement; only an empty simple query stays home.
        let close = SendMode::ExtendedClose {
            statement: "stmt2".into(),
        };
        let mut pset = PsqlSettings {
            send_mode: close.clone(),
            quiet: true,
            ..PsqlSettings::default()
        };
        let mut executor = Modes(Vec::new());
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert!(send_query(
            &mut executor,
            b"",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert!(send_query(
            &mut executor,
            b"",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(executor.0, [(Vec::new(), close)]);
    }

    #[test]
    fn print_options_g_saved_are_restored_after_the_query() {
        // `restorePsetInfo` in `SendQuery`'s cleanup (`common.c:1319`).
        let saved = PsqlSettings::default().popt;
        let mut pset = PsqlSettings::default();
        pset.popt.topt.expanded = crate::settings::Expanded::On;
        pset.gsavepopt = Some(saved.clone());
        let mut executor = Replay(vec![one_row()]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(send_query(
            &mut executor,
            b"select 1",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "-[ RECORD 1 ]\n?column? | 1\n\n"
        );
        assert_eq!(pset.popt, saved);
        assert_eq!(pset.gsavepopt, None);
    }

    #[test]
    fn a_crosstab_request_pivots_the_next_result_only() {
        // `common.c:1060`, and the one-shot reset at `common.c:1341`.
        let mut pset = PsqlSettings {
            crosstab: Some(crate::crosstab::CtvArgs::default()),
            ..PsqlSettings::default()
        };
        let mut executor = Replay(vec![
            three_columns(&[["1", "a", "*a"], ["1", "b", "*b"]]),
            three_columns(&[["1", "a", "*a"]]),
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(send_query(
            &mut executor,
            b"q",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            " x | a  | b  \n---+----+----\n 1 | *a | *b\n(1 row)\n\n"
        );
        assert_eq!(pset.crosstab, None);

        let mut out = Vec::new();
        assert!(send_query(
            &mut executor,
            b"q",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(
            String::from_utf8(out).unwrap(),
            " x | y | v  \n---+---+----\n 1 | a | *a\n(1 row)\n\n"
        );
        assert!(err.is_empty());
    }

    #[test]
    fn a_crosstab_that_fails_logs_and_fails_the_query() {
        let mut pset = PsqlSettings {
            crosstab: Some(crate::crosstab::CtvArgs::default()),
            log_terse: true,
            ..PsqlSettings::default()
        };
        let mut executor = Replay(vec![three_columns(&[["1", "a", "*"], ["1", "a", "*a"]])]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        assert!(!send_query(
            &mut executor,
            b"q",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert!(out.is_empty());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "\\crosstabview: query result contains multiple data values for row \"1\", column \"a\"\n"
        );
        assert_eq!(pset.crosstab, None);
    }

    /// libpq's pipeline, scripted: the sends are recorded, and each
    /// `PQgetResult` answers the next scripted result or NULL.
    struct Pipe {
        status: PipelineStatus,
        sent: Vec<SendMode>,
        results: std::collections::VecDeque<Option<QueryResult>>,
        abandoned: bool,
    }

    impl Pipe {
        fn new(results: Vec<Option<QueryResult>>) -> Self {
            Self {
                status: PipelineStatus::Off,
                sent: Vec::new(),
                results: results.into(),
                abandoned: false,
            }
        }
    }

    impl Executor for Pipe {
        fn exec(
            &mut self,
            _query: &[u8],
            _mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            panic!("a pipeline is never sent through PQexec");
        }
        fn connected(&self) -> bool {
            !self.abandoned
        }
        fn abandon(&mut self) {
            self.abandoned = true;
        }
        fn pipeline_status(&self) -> PipelineStatus {
            self.status
        }
        fn send(&mut self, _query: &[u8], mode: &SendMode) -> Result<(), ErrorMessage> {
            match mode {
                SendMode::StartPipelineMode => self.status = PipelineStatus::On,
                SendMode::EndPipelineMode | SendMode::PipelineSync
                    if self.status == PipelineStatus::Off =>
                {
                    return Err(ErrorMessage::new(
                        b"cannot send pipeline when not in pipeline mode".to_vec(),
                    ));
                }
                _ => {}
            }
            self.sent.push(mode.clone());
            Ok(())
        }
        fn get_result(&mut self) -> Result<Option<QueryResult>, ErrorMessage> {
            Ok(self.results.pop_front().flatten())
        }
        fn exit_pipeline_mode(&mut self) -> Result<(), ErrorMessage> {
            self.status = PipelineStatus::Off;
            Ok(())
        }
    }

    /// One `SendQuery` of `mode`, as `pg_regress` would log it.
    fn pipe(
        executor: &mut Pipe,
        mode: SendMode,
        pset: &mut PsqlSettings,
        vars: &mut VariableSpace,
    ) -> (bool, String, String) {
        pset.send_mode = mode;
        pset.log_terse = true;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let ok = send_query(executor, b"SELECT $1 ", pset, vars, &mut out, &mut err);
        (
            ok,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn bind() -> SendMode {
        SendMode::ExtendedQueryParams {
            params: vec!["1".into()],
        }
    }

    fn counts(vars: &VariableSpace) -> [&str; 3] {
        [
            "PIPELINE_COMMAND_COUNT",
            "PIPELINE_SYNC_COUNT",
            "PIPELINE_RESULT_COUNT",
        ]
        .map(|name| vars.get(name).unwrap_or("<unset>"))
    }

    #[test]
    fn the_pipeline_variables_count_commands_syncs_and_results() {
        // `psql_pipeline.sql:43`-`:58`, "Send multiple syncs", up to its
        // second set of `\echo`s: nothing is read yet.
        let mut executor = Pipe::new(vec![]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        for mode in [
            SendMode::StartPipelineMode,
            bind(),
            SendMode::PipelineSync,
            SendMode::PipelineSync,
            bind(),
            SendMode::PipelineSync,
            bind(),
        ] {
            let (ok, out, err) = pipe(&mut executor, mode, &mut pset, &mut vars);
            assert!(ok);
            assert_eq!((out.as_str(), err.as_str()), ("", ""));
        }
        assert_eq!(counts(&vars), ["1", "3", "2"]);
        assert_eq!(executor.status, PipelineStatus::On);
    }

    #[test]
    fn endpipeline_reads_every_result_then_leaves_pipeline_mode() {
        let mut executor = Pipe::new(vec![
            Some(one_row().remove(0)),
            None,
            Some(one_row().remove(0)),
            None,
            Some(QueryResult::new(ExecStatus::PipelineSync)),
        ]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        for mode in [SendMode::StartPipelineMode, bind(), bind()] {
            assert!(pipe(&mut executor, mode, &mut pset, &mut vars).0);
        }
        let (ok, out, err) = pipe(
            &mut executor,
            SendMode::EndPipelineMode,
            &mut pset,
            &mut vars,
        );
        assert!(ok);
        let table = " ?column? \n----------\n        1\n(1 row)\n\n";
        assert_eq!(out, format!("{table}{table}"));
        assert_eq!(err, "");
        assert_eq!(executor.status, PipelineStatus::Off);
        assert!(executor.results.is_empty(), "every result is read");
        assert_eq!(pset.pipeline, PipelineCounters::default());
        assert_eq!(counts(&vars), ["0", "0", "0"]);
    }

    #[test]
    fn an_error_aborts_the_rest_of_the_pipeline_silently_up_to_the_sync() {
        // `psql_pipeline.sql:218`-`:222`: the error is printed once; the aborted
        // command after it is discarded, not reported
        // (`discardAbortedPipelineResults`, `common.c:1478`).
        let error = QueryResult::with_error(
            ExecStatus::FatalError,
            ResultError::new(vec![(b'S', b"ERROR".to_vec()), (b'M', b"boom".to_vec())]),
        );
        let mut executor = Pipe::new(vec![
            Some(error),
            None,
            Some(QueryResult::new(ExecStatus::PipelineAborted)),
            None,
            Some(QueryResult::new(ExecStatus::PipelineSync)),
        ]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        for mode in [SendMode::StartPipelineMode, bind(), bind()] {
            assert!(pipe(&mut executor, mode, &mut pset, &mut vars).0);
        }
        let (ok, out, err) = pipe(
            &mut executor,
            SendMode::EndPipelineMode,
            &mut pset,
            &mut vars,
        );
        // Past the sync, success is reset (`common.c:2136`).
        assert!(ok);
        assert_eq!(out, "");
        assert_eq!(err, "ERROR:  boom\n");
        assert!(executor.results.is_empty());
        assert_eq!(executor.status, PipelineStatus::Off);
    }

    #[test]
    fn getresults_reads_only_what_was_asked_and_reports_an_aborted_command() {
        // `psql_pipeline.sql:328`-`:340`: `\getresults 1` past an aborted
        // command prints "Pipeline aborted, command did not run".
        let mut executor = Pipe::new(vec![
            Some(QueryResult::new(ExecStatus::PipelineAborted)),
            None,
        ]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        executor.status = PipelineStatus::Aborted;
        pset.pipeline = PipelineCounters {
            piped_commands: 0,
            piped_syncs: 1,
            available_results: 2,
            requested_results: 1,
        };
        let (ok, out, err) = pipe(&mut executor, SendMode::GetResults, &mut pset, &mut vars);
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(err, "Pipeline aborted, command did not run\n");
        assert_eq!(counts(&vars), ["0", "1", "1"]);
        assert!(executor.results.is_empty());
    }

    #[test]
    fn getresults_with_nothing_synced_or_flushed_would_block_and_is_refused() {
        // `common.c:1688`, `psql_pipeline.sql:135`.
        let mut executor = Pipe::new(vec![]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        pset.pipeline.requested_results = 4;
        let (ok, out, err) = pipe(&mut executor, SendMode::GetResults, &mut pset, &mut vars);
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(err, "No pending results to get\n");
        assert_eq!(pset.pipeline.requested_results, 0);
        assert!(executor.sent.is_empty(), "\\getresults sends nothing");
    }

    #[test]
    fn endpipeline_outside_a_pipeline_reports_libpq_s_refusal() {
        // `psql_pipeline.sql:207`-`:208`.
        let mut executor = Pipe::new(vec![]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        let (ok, out, err) = pipe(
            &mut executor,
            SendMode::EndPipelineMode,
            &mut pset,
            &mut vars,
        );
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(err, "cannot send pipeline when not in pipeline mode\n");
        assert_eq!(counts(&vars), ["0", "0", "0"]);
    }

    #[test]
    fn flush_reports_failure_silently_as_upstream_does() {
        // `common.c:1672`: `success = PQflush(pset.db)`, and PQflush returns
        // 0 when it succeeds.
        let mut executor = Pipe::new(vec![]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        assert!(
            pipe(
                &mut executor,
                SendMode::StartPipelineMode,
                &mut pset,
                &mut vars
            )
            .0
        );
        let (ok, out, err) = pipe(&mut executor, SendMode::Flush, &mut pset, &mut vars);
        assert!(!ok);
        assert_eq!((out.as_str(), err.as_str()), ("", ""));
        assert_eq!(executor.sent.last(), Some(&SendMode::Flush));
    }

    #[test]
    fn the_pipeline_variables_start_at_zero() {
        // `startup.c:208`-`:211` seeds them from zeroed counters.
        assert_eq!(
            pipeline_variables(&PipelineCounters::default()),
            [
                ("PIPELINE_SYNC_COUNT", "0".to_string()),
                ("PIPELINE_COMMAND_COUNT", "0".to_string()),
                ("PIPELINE_RESULT_COUNT", "0".to_string()),
            ]
        );
    }

    #[test]
    fn copy_in_a_pipeline_gives_the_connection_up() {
        // `common.c:1919`-`:1943`: upstream logs and exits with
        // EXIT_BADCONN rather than drive a COPY inside a pipeline.
        let mut executor = Pipe::new(vec![Some(QueryResult::new(ExecStatus::CopyIn))]);
        let (mut pset, mut vars) = (PsqlSettings::default(), VariableSpace::new());
        for mode in [SendMode::StartPipelineMode, bind()] {
            assert!(pipe(&mut executor, mode, &mut pset, &mut vars).0);
        }
        let (ok, out, err) = pipe(
            &mut executor,
            SendMode::EndPipelineMode,
            &mut pset,
            &mut vars,
        );
        assert!(!ok);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "COPY in a pipeline is not supported, aborting connection\n"
        );
        assert!(!executor.connected());
    }

    #[test]
    fn notices_are_printed_before_the_results_of_the_query_that_drew_them() {
        // psql's `NoticeProcessor` (`common.c:281`) is `pg_log_info`. Outside
        // a pipeline the whole query is read before anything is printed, so
        // its notices all come first (`docs/divergences.md`).
        struct Noisy(Vec<ResultError>);
        impl Executor for Noisy {
            fn exec(
                &mut self,
                _query: &[u8],
                _mode: &SendMode,
            ) -> Result<Vec<QueryResult>, ErrorMessage> {
                Ok(one_row())
            }
            fn connected(&self) -> bool {
                true
            }
            fn abandon(&mut self) {}
            fn take_notices(&mut self) -> Vec<ResultError> {
                std::mem::take(&mut self.0)
            }
        }
        let mut executor = Noisy(vec![ResultError::new(vec![
            (b'S', b"WARNING".to_vec()),
            (
                b'M',
                b"SET LOCAL can only be used in transaction blocks".to_vec(),
            ),
        ])]);
        let mut pset = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert!(send_query(
            &mut executor,
            b"select 1",
            &mut pset,
            &mut VariableSpace::new(),
            &mut out,
            &mut err
        ));
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "WARNING:  SET LOCAL can only be used in transaction blocks\n"
        );
        assert!(executor.0.is_empty(), "each notice is printed once");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            " ?column? \n----------\n        1\n(1 row)\n\n"
        );
    }
}
