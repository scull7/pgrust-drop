//! The calls of `fe-exec.c` that send a command without waiting for it and
//! collect what comes back: `PQsendQuery`, `PQsendQueryParams`,
//! `PQsendPrepare`, `PQsendQueryPrepared`, `PQsendDescribePrepared` and
//! `PQsendDescribePortal`; `PQgetResult`, `PQconsumeInput`, `PQisBusy` and
//! `PQnotifies`; and `PQsetnonblocking`, `PQisnonblocking` and `PQflush`.
//!
//! Each is a thin shim over the `rlibpq` [`Connection`] call of the same
//! name. What the `PGconn` keeps beside it — `conn->errorMessage`, the
//! notices and notifications parsed, a connection lost — is
//! [`PGconn`]'s. A call that parses input hands the notices it parsed to
//! the receiver once it is done with the `PGconn`.

use std::ffi::{c_char, c_int};
use std::ptr::null_mut;

use rlibpq::{AsyncStatus, Connection, ConnectionError, ExecStatus, Flush};

use crate::alloc::{malloc, until_nul};
use crate::conn::{AfterLoss, ConnStatus, Notices, PGconn, Refused, c_bytes, with_newline};
use crate::extended::{c_array, param_count, read_params};
use crate::result::PGresult;

/// `PGnotify`, `libpq-fe.h:228`: one notification, as `PQnotifies` hands
/// it to C.
#[repr(C)]
#[derive(Debug)]
pub struct PGnotify {
    /// The channel.
    pub relname: *mut c_char,
    /// The notifying server process.
    pub be_pid: c_int,
    /// The payload.
    pub extra: *mut c_char,
    /// C libpq's list link, NULL once handed out (`fe-exec.c:2700`).
    pub next: *mut PGnotify,
}

/// Action: a notification laid out as `getNotify` lays it out
/// (`fe-protocol3.c:1675`-`:1689`): one `malloc`'d block holding the struct
/// and then the channel and the payload, NUL-terminated, so a single
/// `PQfreemem` frees it all. Null when `malloc` fails.
fn malloc_notify(be_pid: c_int, channel: &[u8], payload: &[u8]) -> *mut PGnotify {
    let (channel, payload) = (until_nul(channel), until_nul(payload));
    let size = size_of::<PGnotify>() + channel.len() + payload.len() + 2;
    // SAFETY: `malloc` has no preconditions. What it returns is aligned for
    // any type, `PGnotify` included.
    let notify = unsafe { malloc(size) }.cast::<PGnotify>();
    if notify.is_null() {
        return null_mut();
    }
    // SAFETY: the block holds the struct and then `channel.len() + 1` and
    // `payload.len() + 1` bytes, and overlaps neither slice.
    unsafe {
        let relname = notify.add(1).cast::<u8>();
        std::ptr::copy_nonoverlapping(channel.as_ptr(), relname, channel.len());
        relname.add(channel.len()).write(0);
        let extra = relname.add(channel.len() + 1);
        std::ptr::copy_nonoverlapping(payload.as_ptr(), extra, payload.len());
        extra.add(payload.len()).write(0);
        notify.write(PGnotify {
            relname: relname.cast(),
            be_pid,
            extra: extra.cast(),
            next: null_mut(),
        });
    }
    notify
}

/// Action: `parseInput` (`fe-exec.c:2037`): parse what has been read,
/// reading nothing. A parse failure loses the connection, `then` as
/// [`PGconn::lose`] takes it. The notices parsed are returned, to be
/// delivered.
fn parse_input(conn: &mut PGconn) -> Notices {
    let Some(connection) = conn.connection.as_mut() else {
        return Notices::default();
    };
    match connection.is_busy() {
        Ok(_) => conn.collect(),
        Err(err) => {
            let notices = conn.collect();
            conn.lose(&err, AfterLoss::ErrorResult);
            notices
        }
    }
}

/// Action: [`PGconn::send_with`] over a C `conn`: 0 for NULL
/// (`PQsendQueryStart`, `fe-exec.c:1692`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
unsafe fn send(
    conn: *mut PGconn,
    send: impl FnOnce(&mut Connection) -> Result<Result<(), ConnectionError>, Refused>,
) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_mut() }.map_or(0, |conn| conn.send_with(send))
}

/// `PQsendQuery`, `fe-exec.c:1433`: send `query` and return; 1 when it
/// went out, 0 with `PQerrorMessage` saying why not.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `query` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendQuery(conn: *mut PGconn, query: *const c_char) -> c_int {
    // SAFETY: the caller's contract.
    unsafe {
        send(conn, |connection| {
            // `PQsendQueryInternal`, `fe-exec.c:1453`-`:1457`.
            let query = c_bytes(query).ok_or_else(Refused::null_command)?;
            Ok(connection.send_query(query))
        })
    }
}

/// `PQsendQueryParams`, `fe-exec.c:1509`: [`PQsendQuery`] with
/// out-of-line parameters, as `PQexecParams` takes them.
///
/// # Safety
///
/// As for `PQexecParams`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendQueryParams(
    conn: *mut PGconn,
    command: *const c_char,
    nParams: c_int,
    paramTypes: *const u32,
    paramValues: *const *const c_char,
    paramLengths: *const c_int,
    paramFormats: *const c_int,
    resultFormat: c_int,
) -> c_int {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        send(conn, |connection| {
            let command = c_bytes(command).ok_or_else(Refused::null_command)?;
            let n = param_count(nParams)?;
            let types = c_array(paramTypes, n).unwrap_or_default();
            let params = read_params(n, paramValues, paramLengths, paramFormats, resultFormat)?;
            Ok(connection.send_query_params(command, types, &params.params()))
        })
    }
}

/// `PQsendPrepare`, `fe-exec.c:1553`: send the Parse `PQprepare` waits
/// for, and return.
///
/// # Safety
///
/// As for `PQprepare`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendPrepare(
    conn: *mut PGconn,
    stmtName: *const c_char,
    query: *const c_char,
    nParams: c_int,
    paramTypes: *const u32,
) -> c_int {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        send(conn, |connection| {
            let statement = c_bytes(stmtName).ok_or_else(Refused::null_statement)?;
            let query = c_bytes(query).ok_or_else(Refused::null_command)?;
            let n = param_count(nParams)?;
            let types = c_array(paramTypes, n).unwrap_or_default();
            Ok(connection.send_prepare(statement, query, types))
        })
    }
}

/// `PQsendQueryPrepared`, `fe-exec.c:1650`: run the prepared statement
/// `stmtName` without waiting.
///
/// # Safety
///
/// As for `PQexecPrepared`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendQueryPrepared(
    conn: *mut PGconn,
    stmtName: *const c_char,
    nParams: c_int,
    paramValues: *const *const c_char,
    paramLengths: *const c_int,
    paramFormats: *const c_int,
    resultFormat: c_int,
) -> c_int {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        send(conn, |connection| {
            let statement = c_bytes(stmtName).ok_or_else(Refused::null_statement)?;
            let n = param_count(nParams)?;
            let params = read_params(n, paramValues, paramLengths, paramFormats, resultFormat)?;
            Ok(connection.send_query_prepared(statement, &params.params()))
        })
    }
}

/// `PQsendDescribePrepared`, `fe-exec.c:2508`: a NULL name is the unnamed
/// statement (`PQsendTypedCommand`, `:2611`-`:2612`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `stmt` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendDescribePrepared(conn: *mut PGconn, stmt: *const c_char) -> c_int {
    // SAFETY: the caller's contract.
    unsafe {
        send(conn, |connection| {
            Ok(connection.send_describe_prepared(c_bytes(stmt).unwrap_or_default()))
        })
    }
}

/// `PQsendDescribePortal`, `fe-exec.c:2521`: a NULL name is the unnamed
/// portal.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `portal` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsendDescribePortal(conn: *mut PGconn, portal: *const c_char) -> c_int {
    // SAFETY: the caller's contract.
    unsafe {
        send(conn, |connection| {
            Ok(connection.send_describe_portal(c_bytes(portal).unwrap_or_default()))
        })
    }
}

/// `PQgetResult`, `fe-exec.c:2079`: the next result of the command
/// running, waiting for it if need be; NULL once the command is done, and
/// for NULL.
///
/// An error result adds its message to `conn->errorMessage`
/// (`pqGetErrorNotice3`, `fe-protocol3.c:995`). A connection that breaks
/// while the result is awaited is lost, and the result is the
/// `PGRES_FATAL_ERROR` `pqPrepareAsyncResult` makes of the error
/// (`fe-exec.c:2114`-`:2121`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQgetResult(conn: *mut PGconn) -> *mut PGresult {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return null_mut();
    };
    if let Some(result) = conn.take_error_result() {
        return Box::into_raw(Box::new(result));
    }
    let Some(connection) = conn.connection.as_mut() else {
        return null_mut();
    };
    let next = connection.get_result();
    let notices = conn.collect();
    let result = match next {
        Ok(Some(result)) => {
            if result.status() == ExecStatus::FatalError {
                conn.report_error(&result.error_message());
            }
            Some(PGresult::from_result(&result, conn.hooks))
        }
        Ok(None) => None,
        Err(err) => {
            conn.lose(&err, AfterLoss::ErrorResult);
            // A command was running, so `lose` left the error result.
            conn.take_error_result()
        }
    };
    notices.deliver();
    result.map_or(null_mut(), |result| Box::into_raw(Box::new(result)))
}

/// `PQconsumeInput`, `fe-exec.c:2001`: read whatever the server has sent,
/// without waiting and without parsing it; 1, or 0 when the read failed,
/// which loses the connection (`pqReadData`, `fe-misc.c:833`-`:842`), and
/// for NULL. On a connection that is not open it is `pqReadData`'s
/// "connection not open" (`fe-misc.c:621`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconsumeInput(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return 0;
    };
    let Some(connection) = conn.connection.as_mut() else {
        conn.append_error(b"connection not open\n");
        return 0;
    };
    match connection.consume_input() {
        Ok(()) => 1,
        Err(err) => {
            // What was read before the failure is parsed rather than
            // dropped with the connection: its notices and notifications
            // are still delivered.
            let notices = parse_input(conn);
            conn.lose(&err, AfterLoss::Wait);
            notices.deliver();
            0
        }
    }
}

/// `PQisBusy`, `fe-exec.c:2048`: parse what has been read, and answer
/// whether `PQgetResult` would wait: 1 while a command runs and its result
/// is not complete, 0 otherwise and for NULL or a lost connection
/// (`:2064`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQisBusy(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return 0;
    };
    let notices = parse_input(conn);
    let busy = conn
        .connection
        .as_ref()
        .is_some_and(|connection| connection.async_status() == AsyncStatus::Busy);
    notices.deliver();
    c_int::from(busy)
}

/// `PQnotifies`, `fe-exec.c:2684`: parse what has been read, and hand out
/// the oldest notification not yet handed out, for the caller to
/// `PQfreemem`; NULL when there is none, and for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQnotifies(conn: *mut PGconn) -> *mut PGnotify {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return null_mut();
    };
    let notices = parse_input(conn);
    let next = conn.notifies.pop_front();
    notices.deliver();
    next.map_or(null_mut(), |(be_pid, channel, payload)| {
        malloc_notify(be_pid, &channel, &payload)
    })
}

/// `PQsetnonblocking`, `fe-exec.c:3975`: 0 once the connection is in the
/// mode asked for, -1 for NULL, a connection that is not up, and a flush
/// that failed or — leaving non-blocking mode — could not send everything.
/// The error is cleared first unless a command is running (`:3997`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsetnonblocking(conn: *mut PGconn, arg: c_int) -> c_int {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return -1;
    };
    let nonblocking = arg != 0;
    let (current, idle) = match &conn.connection {
        Some(connection) => (
            connection.is_nonblocking(),
            connection.async_status() == AsyncStatus::Idle,
        ),
        None => return -1,
    };
    if nonblocking == current {
        return 0;
    }
    if idle {
        conn.clear_error();
    }
    let outcome = conn
        .connection
        .as_mut()
        .map(|connection| connection.set_nonblocking(nonblocking));
    match outcome {
        Some(Ok(())) => 0,
        Some(Err(ConnectionError::FlushPending)) | None => -1,
        Some(Err(err)) => {
            conn.append_error(&with_newline(err.message()));
            -1
        }
    }
}

/// `PQisnonblocking`, `fe-exec.c:4014`: 1 in non-blocking mode; 0
/// otherwise, and for NULL or a connection that is not up.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQisnonblocking(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let conn = unsafe { conn.as_ref() };
    c_int::from(
        conn.filter(|conn| conn.status == ConnStatus::Ok)
            .and_then(|conn| conn.connection.as_ref())
            .is_some_and(Connection::is_nonblocking),
    )
}

/// `PQflush`, `fe-exec.c:4031`: send what is buffered; 0 when all of it
/// went, 1 when a non-blocking connection could not send it all yet, -1
/// for a failure and for NULL or a connection that is not up.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQflush(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return -1;
    };
    let Some(connection) = conn.connection.as_mut() else {
        return -1;
    };
    match connection.flush() {
        Ok(Flush::Done) => 0,
        Ok(Flush::Pending) => 1,
        Err(err) => {
            conn.append_error(&with_newline(err.message()));
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::CStr;

    /// `getNotify`'s layout: the strings follow the struct in one block,
    /// each NUL-terminated, and the link is NULL.
    #[test]
    fn a_notification_is_one_malloc_block_pqfreemem_frees() {
        let notify = malloc_notify(42, b"tbl2", b"payload");
        assert!(!notify.is_null());
        // SAFETY: `notify` is the block just built.
        unsafe {
            let read = &*notify;
            assert_eq!(read.be_pid, 42);
            assert!(read.next.is_null());
            assert_eq!(read.relname, notify.add(1).cast::<c_char>());
            assert_eq!(CStr::from_ptr(read.relname).to_bytes(), b"tbl2");
            assert_eq!(read.extra, read.relname.add(5));
            assert_eq!(CStr::from_ptr(read.extra).to_bytes(), b"payload");
            crate::misc::PQfreemem(notify.cast());
        }
    }
}
