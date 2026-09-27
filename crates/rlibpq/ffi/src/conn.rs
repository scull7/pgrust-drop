//! `PGconn` and the blocking calls of `fe-connect.c` and `fe-exec.c` that
//! open, use and close one: `PQconnectdb`, `PQstatus`, `PQerrorMessage`,
//! `PQexec` and `PQfinish`; and [`exec_with`], the `PQexecStart` /
//! `PQexecFinish` frame the extended-query calls share with `PQexec`.
//!
//! The connection itself is an `rlibpq` [`Connection`]; this module keeps
//! what C reads beside it — the status, `conn->errorMessage` as a C string —
//! and turns what a call returned into what C's `PGconn` would then hold.

use std::ffi::{CStr, c_char, c_int};
use std::io::Write as _;
use std::ptr::null_mut;

use rlibpq::{
    Connection, ConnectionError, ContextVisibility, Env, ExecStatus, Filesystem, QueryResult,
    Verbosity, parse_conninfo,
};

use crate::ctext::CText;
use crate::result::PGresult;

/// `ConnStatusType`, `libpq-fe.h:82`, as far as a blocking connection ever
/// leaves it: `CONNECTION_OK` (0) or `CONNECTION_BAD` (1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnStatus {
    Ok,
    Bad,
}

impl ConnStatus {
    const fn code(self) -> c_int {
        match self {
            ConnStatus::Ok => 0,
            ConnStatus::Bad => 1,
        }
    }
}

/// `PGconn`, opaque to C (`libpq-fe.h:202`: `typedef struct pg_conn PGconn`).
#[derive(Debug)]
pub struct PGconn {
    /// The open connection; `None` once it is `CONNECTION_BAD`.
    connection: Option<Connection>,
    status: ConnStatus,
    /// `conn->errorMessage`.
    error_message: CText,
    /// How many of the connection's notices have been handed to the notice
    /// processor already.
    notices_reported: usize,
}

impl PGconn {
    /// Calculation: the `PGconn` a connection attempt leaves.
    fn from_attempt(attempt: Result<Connection, Vec<u8>>) -> Self {
        match attempt {
            Ok(connection) => PGconn {
                connection: Some(connection),
                status: ConnStatus::Ok,
                error_message: CText::default(),
                notices_reported: 0,
            },
            Err(message) => PGconn {
                connection: None,
                status: ConnStatus::Bad,
                error_message: CText::new(&message),
                notices_reported: 0,
            },
        }
    }

    /// Action: hand every notice the server sent since the last call to the
    /// notice processor. With no `PQsetNoticeReceiver` or
    /// `PQsetNoticeProcessor` exported yet it is always the default, which
    /// prints `PQresultErrorMessage` of the notice to stderr
    /// (`defaultNoticeReceiver`, `fe-connect.c:7842`; `defaultNoticeProcessor`,
    /// `:7857`).
    fn report_notices(&mut self) {
        let Some(connection) = &self.connection else {
            return;
        };
        let notices = &connection.notices()[self.notices_reported..];
        let mut stderr = std::io::stderr().lock();
        for notice in notices {
            let _ = stderr.write_all(&notice.message(
                ExecStatus::NonfatalError,
                Verbosity::default(),
                ContextVisibility::default(),
            ));
        }
        self.notices_reported += notices.len();
    }
}

/// Calculation: `message` as `libpq_append_conn_error` leaves it in
/// `conn->errorMessage`, ending in the newline it adds (`fe-misc.c:1539`).
/// `rlibpq`'s messages carry that newline for some errors and not others.
fn with_newline(mut message: Vec<u8>) -> Vec<u8> {
    if !message.ends_with(b"\n") {
        message.push(b'\n');
    }
    message
}

/// What one `PQexec` leaves behind.
#[derive(Debug)]
struct ExecOutcome {
    /// What `PQexec` returns; `None` is NULL, "the query was not even sent".
    result: Option<PGresult>,
    /// `conn->errorMessage` afterwards.
    error_message: Vec<u8>,
    /// The connection is gone, so the status is now `CONNECTION_BAD`.
    lost: bool,
}

/// Calculation: `PQexecFinish` (`fe-exec.c:2427`) over what
/// [`Connection::exec`] returned.
///
/// The last result is the one returned (`:2444`-`:2455`), and
/// `conn->errorMessage`, cleared when the query began, has accumulated the
/// message of every error result on the way (`:2433`-`:2436`). A refusal
/// before anything was sent is NULL, the refusal the error message. Any
/// other failure lost the connection: C then returns the `PGRES_FATAL_ERROR`
/// result `pqPrepareAsyncResult` (`fe-exec.c:857`) builds from the error
/// message, and the connection is `CONNECTION_BAD`.
fn exec_outcome(outcome: Result<Vec<QueryResult>, ConnectionError>) -> ExecOutcome {
    match outcome {
        Ok(results) => ExecOutcome {
            error_message: results
                .iter()
                .filter(|result| result.status() == ExecStatus::FatalError)
                .flat_map(QueryResult::error_message)
                .collect(),
            result: results.last().map(PGresult::from_result),
            lost: false,
        },
        Err(err @ (ConnectionError::Pipeline(_) | ConnectionError::Argument(_))) => ExecOutcome {
            result: None,
            error_message: with_newline(err.message()),
            lost: false,
        },
        Err(err) => {
            let message = with_newline(err.message());
            ExecOutcome {
                result: Some(PGresult::fatal_error(&message)),
                error_message: message,
                lost: true,
            }
        }
    }
}

/// Action: `PQconnectdb`'s work — parse, fill in the defaults from the
/// environment and the service files, connect and authenticate — or the
/// error message it leaves.
fn connect(conninfo: &[u8]) -> Result<Connection, Vec<u8>> {
    let mut info = parse_conninfo(conninfo).map_err(|err| with_newline(err.message()))?;
    info.add_defaults(&Env::from_process(), &Filesystem)
        .map_err(|err| with_newline(err.message()))?;
    Connection::connect(&info).map_err(|err| with_newline(err.message()))
}

/// `PQconnectdb`, `fe-connect.c:820`: connect, blocking, and return the
/// `PGconn` whatever happened; `PQstatus` and `PQerrorMessage` tell.
///
/// A NULL `conninfo` is taken as `""`.
///
/// # Safety
///
/// `conninfo` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectdb(conninfo: *const c_char) -> *mut PGconn {
    // SAFETY: the caller's contract.
    let conninfo = unsafe { c_bytes(conninfo) }.unwrap_or_default();
    let mut conn = PGconn::from_attempt(connect(conninfo));
    conn.report_notices();
    Box::into_raw(Box::new(conn))
}

/// `PQfinish`, `fe-connect.c:5301`: send Terminate if the connection is up
/// (`sendTerminateConn`, `:5233`), close it and free the `PGconn`; nothing
/// for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfinish(conn: *mut PGconn) {
    if conn.is_null() {
        return;
    }
    // SAFETY: every `PGconn` this crate hands out is `Box::into_raw`.
    let conn = unsafe { Box::from_raw(conn) };
    if let Some(mut connection) = conn.connection {
        // "Ignore any error" (`:5236`-`:5237`).
        let _ = connection.terminate();
    }
}

/// `PQstatus`, `fe-connect.c:7575`: `CONNECTION_BAD` for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQstatus(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_ref() }
        .map_or(ConnStatus::Bad, |conn| conn.status)
        .code()
}

/// `PQerrorMessage`, `fe-connect.c:7638`: the most recent error, `""` when
/// there is none.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQerrorMessage(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    match unsafe { conn.as_ref() } {
        Some(conn) => conn.error_message.as_ptr(),
        None => c"connection pointer is NULL\n".as_ptr().cast_mut(),
    }
}

/// Why `PQsendQueryParams` and its siblings sent nothing: an argument they
/// check once the connection is known to be idle, the message without the
/// newline `libpq_append_conn_error` adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) Vec<u8>);

impl Refused {
    /// `fe-exec.c:1524`, `:1570`: "command string is a null pointer".
    pub(crate) fn null_command() -> Self {
        Refused(b"command string is a null pointer".to_vec())
    }

    /// `fe-exec.c:1565`, `:1664`: "statement name is a null pointer".
    pub(crate) fn null_statement() -> Self {
        Refused(b"statement name is a null pointer".to_vec())
    }
}

/// Action: the shape every blocking `PQexec*` shares — `PQexecStart`,
/// `PQsendQueryStart`, the call's own work, then `PQexecFinish` — with
/// `send` checking the call's arguments and running it on the open
/// connection.
///
/// NULL for a NULL `conn` (`PQexecStart`, `fe-exec.c:2365`), for a
/// connection that is not up (`PQsendQueryStart`, `:1704`-`:1708`) and for
/// an argument `send` refuses; otherwise the last result.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
pub(crate) unsafe fn exec_with(
    conn: *mut PGconn,
    send: impl FnOnce(&mut Connection) -> Result<Result<Vec<QueryResult>, ConnectionError>, Refused>,
) -> *mut PGresult {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return null_mut();
    };
    // `PQexecStart`, `fe-exec.c:2374`: a new query cycle clears the error.
    conn.error_message = CText::default();
    let Some(connection) = conn.connection.as_mut() else {
        conn.error_message = CText::new(b"no connection to the server\n");
        return null_mut();
    };
    let outcome = match send(connection) {
        Ok(outcome) => exec_outcome(outcome),
        Err(Refused(message)) => {
            conn.error_message = CText::new(&with_newline(message));
            return null_mut();
        }
    };
    conn.report_notices();
    conn.error_message = CText::new(&outcome.error_message);
    if outcome.lost {
        conn.connection = None;
        conn.status = ConnStatus::Bad;
    }
    outcome
        .result
        .map_or(null_mut(), |result| Box::into_raw(Box::new(result)))
}

/// The bytes of a C string argument, or `None` for NULL.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string that outlives `'a`.
pub(crate) unsafe fn c_bytes<'a>(text: *const c_char) -> Option<&'a [u8]> {
    // SAFETY: the caller's contract.
    (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_bytes())
}

/// `PQexec`, `fe-exec.c:2279`: send `query` and wait for every result;
/// return the last, or NULL when nothing was sent.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `query` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQexec(conn: *mut PGconn, query: *const c_char) -> *mut PGresult {
    // SAFETY: the caller's contract.
    unsafe {
        exec_with(conn, |connection| {
            // The argument check of `PQsendQueryInternal`,
            // `fe-exec.c:1453`-`:1457`.
            let query = c_bytes(query).ok_or_else(Refused::null_command)?;
            Ok(connection.exec(query))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rlibpq::{PipelineError, ResultError};

    fn error_result(message: &[u8]) -> QueryResult {
        QueryResult::with_error(
            ExecStatus::FatalError,
            ResultError::new(vec![(b'S', b"ERROR".to_vec()), (b'M', message.to_vec())]),
        )
    }

    #[test]
    fn with_newline_ends_the_message_in_exactly_one_newline() {
        assert_eq!(with_newline(b"boom".to_vec()), b"boom\n");
        assert_eq!(with_newline(b"boom\n".to_vec()), b"boom\n");
    }

    #[test]
    fn exec_returns_the_last_result_and_every_error_message() {
        let outcome = exec_outcome(Ok(vec![
            QueryResult::new(ExecStatus::TuplesOk),
            error_result(b"division by zero"),
        ]));
        assert!(!outcome.lost);
        assert_eq!(outcome.error_message, b"ERROR:  division by zero\n");
        let result = outcome.result.expect("the last result");
        // SAFETY: `result` is a live `PGresult`.
        assert_eq!(
            unsafe { crate::result::PQresultStatus(&raw const result) },
            7
        );

        let fine = exec_outcome(Ok(vec![QueryResult::new(ExecStatus::CommandOk)]));
        assert_eq!(fine.error_message, b"");
    }

    #[test]
    fn a_refusal_sends_nothing_and_a_broken_connection_is_lost() {
        let refused = exec_outcome(Err(PipelineError::ExecDuringCopyBoth.into()));
        assert!(refused.result.is_none());
        assert!(!refused.lost);
        assert!(refused.error_message.ends_with(b"\n"));

        let lost = exec_outcome(Err(ConnectionError::ServerClosedConnection));
        assert!(lost.lost);
        assert_eq!(
            lost.error_message,
            with_newline(ConnectionError::ServerClosedConnection.message())
        );
        assert!(lost.result.is_some());
    }

    #[test]
    fn a_failed_attempt_is_bad_with_its_message() {
        let conn = PGconn::from_attempt(Err(b"no\n".to_vec()));
        assert_eq!(conn.status.code(), 1);
        assert_eq!(conn.error_message.bytes(), b"no\n");
        assert_eq!(ConnStatus::Ok.code(), 0);
    }
}
