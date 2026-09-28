//! The accessor functions for `PGconn` (`fe-connect.c:7415`-`:7680`): what
//! the connection was asked for — `PQdb`, `PQuser`, `PQpass`, `PQhost`,
//! `PQport`, `PQtty`, `PQoptions`, `PQconninfo` — and what the server told
//! it — `PQtransactionStatus`, `PQparameterStatus`, `PQserverVersion`,
//! `PQbackendPID` — and `PQsocket`.
//!
//! Every string returned points into the `PGconn` and lives as long as the
//! field it reads, as in C; the caller never frees one. `PQconninfo`'s
//! array is the exception, `malloc`'d for `PQconninfoFree`.

use std::ffi::{CStr, c_char, c_int};
use std::os::fd::AsRawFd as _;
use std::ptr::null_mut;

use rlibpq::{AsyncStatus, ConnInfo, TransactionStatus};

use crate::conn::{ConnStatus, PGconn};
use crate::conninfo::{PQconninfoOption, to_c};
use crate::ctext::CText;

/// `PGTransactionStatusType`, `libpq-fe.h:146`: `PQTRANS_IDLE` (0),
/// `PQTRANS_ACTIVE` (1), `PQTRANS_INTRANS` (2), `PQTRANS_INERROR` (3),
/// `PQTRANS_UNKNOWN` (4).
const fn transaction_status_code(status: TransactionStatus) -> c_int {
    match status {
        TransactionStatus::Idle => 0,
        TransactionStatus::InTransaction => 2,
        TransactionStatus::InError => 3,
        TransactionStatus::Unknown => 4,
    }
}

/// `PQTRANS_ACTIVE`: "a command is in progress".
const PQTRANS_ACTIVE: c_int = 1;
/// `PQTRANS_UNKNOWN`: "cannot determine status".
const PQTRANS_UNKNOWN: c_int = 4;

/// The pointer C reads for an option field: NULL when it holds nothing.
fn field(text: Option<&CText>) -> *mut c_char {
    text.map_or(null_mut(), CText::as_ptr)
}

/// A string C returns from its read-only data, such as `PQtty`'s `""`.
fn literal(text: &'static CStr) -> *mut c_char {
    text.as_ptr().cast_mut()
}

/// The option field `keyword` of `conn`: NULL for a NULL `conn`, and NULL
/// when the field holds nothing.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
unsafe fn option_field(conn: *const PGconn, keyword: &str) -> *mut c_char {
    // SAFETY: the caller's contract.
    let options = unsafe { conn.as_ref() }.and_then(|conn| conn.options.as_ref());
    field(options.and_then(|options| options.text(keyword)))
}

/// `PQconninfo`, `fe-connect.c:7415`: the connection's options as a
/// `PQconninfoOption` array, each row holding what the field it names
/// holds. NULL for a NULL `conn`, or when memory runs out.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconninfo(conn: *mut PGconn) -> *mut PQconninfoOption {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_ref() }) else {
        return null_mut();
    };
    match &conn.options {
        Some(options) => to_c(&options.info),
        None => to_c(&ConnInfo::new()),
    }
}

/// `PQdb`, `fe-connect.c:7472`: `conn->dbName`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQdb(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { option_field(conn, "dbname") }
}

/// `PQuser`, `fe-connect.c:7480`: `conn->pguser`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQuser(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { option_field(conn, "user") }
}

/// `PQpass`, `fe-connect.c:7488`: the password given, `""` when there is
/// none. C would first answer the host's password from the password file;
/// `rlibpq` reads no password file, so there never is one.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQpass(conn: *const PGconn) -> *mut c_char {
    if conn.is_null() {
        return null_mut();
    }
    // SAFETY: the caller's contract.
    match unsafe { option_field(conn, "password") } {
        password if password.is_null() => literal(c""),
        password => password,
    }
}

/// `PQhost`, `fe-connect.c:7505`: the host entry's `host`, else its
/// `hostaddr`, else `""`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQhost(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    field(unsafe { conn.as_ref() }.map(|conn| &conn.host))
}

/// `PQport`, `fe-connect.c:7541`: the host entry's port, else
/// `DEF_PGPORT_STR`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQport(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    field(unsafe { conn.as_ref() }.map(|conn| &conn.port))
}

/// `PQtty`, `fe-connect.c:7559`: `""`, kept for compatibility.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQtty(conn: *const PGconn) -> *mut c_char {
    if conn.is_null() {
        null_mut()
    } else {
        literal(c"")
    }
}

/// `PQoptions`, `fe-connect.c:7567`: `conn->pgoptions`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQoptions(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { option_field(conn, "options") }
}

/// `PQtransactionStatus`, `fe-connect.c:7583`: `PQTRANS_UNKNOWN` unless the
/// connection is up, `PQTRANS_ACTIVE` while a command is in progress (a
/// COPY, after a blocking call), else what the last ReadyForQuery said.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQtransactionStatus(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let Some(connection) = (unsafe { conn.as_ref() })
        .filter(|conn| conn.status == ConnStatus::Ok)
        .and_then(|conn| conn.connection.as_ref())
    else {
        return PQTRANS_UNKNOWN;
    };
    if connection.async_status() == AsyncStatus::Idle {
        transaction_status_code(connection.transaction_status())
    } else {
        PQTRANS_ACTIVE
    }
}

/// `PQparameterStatus`, `fe-connect.c:7593`: the value the server last
/// reported for `paramName`, NULL when it reported none, or for a NULL
/// argument.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `param_name` is
/// null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQparameterStatus(
    conn: *const PGconn,
    param_name: *const c_char,
) -> *const c_char {
    // SAFETY: the caller's contract, for both arguments.
    let (Some(conn), Some(name)) = (unsafe { conn.as_ref() }, unsafe {
        crate::conn::c_bytes(param_name)
    }) else {
        return null_mut();
    };
    field(
        conn.parameters
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| value),
    )
}

/// `PQserverVersion`, `fe-connect.c:7628`: the server's version as an
/// integer, 0 when the connection is `CONNECTION_BAD`.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQserverVersion(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_ref() }
        .filter(|conn| conn.status == ConnStatus::Ok)
        .and_then(|conn| conn.connection.as_ref())
        .map_or(0, rlibpq::Connection::server_version)
}

/// `PQsocket`, `fe-connect.c:7664`: the socket's descriptor, -1 when there
/// is none.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsocket(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_ref() }
        .and_then(|conn| conn.connection.as_ref())
        .map_or(-1, |connection| connection.socket().as_raw_fd())
}

/// `PQbackendPID`, `fe-connect.c:7674`: the server process's PID, 0 unless
/// the connection is up.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQbackendPID(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_ref() }
        .filter(|conn| conn.status == ConnStatus::Ok)
        .and_then(|conn| conn.connection.as_ref())
        .map_or(0, rlibpq::Connection::backend_pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_status_codes_are_libpq_fe_h_s() {
        assert_eq!(transaction_status_code(TransactionStatus::Idle), 0);
        assert_eq!(transaction_status_code(TransactionStatus::InTransaction), 2);
        assert_eq!(transaction_status_code(TransactionStatus::InError), 3);
        assert_eq!(
            transaction_status_code(TransactionStatus::Unknown),
            PQTRANS_UNKNOWN
        );
    }
}
