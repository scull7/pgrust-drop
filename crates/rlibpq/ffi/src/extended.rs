//! The blocking extended-query calls of `fe-exec.c`: `PQexecParams`,
//! `PQprepare`, `PQexecPrepared`, `PQdescribePrepared` and
//! `PQdescribePortal`.
//!
//! Each reads C's parallel arrays (`paramTypes`, `paramValues`,
//! `paramLengths`, `paramFormats`) into an `rlibpq` [`Params`], making the
//! argument checks `PQsendQueryParams` (`:1509`), `PQsendPrepare` (`:1553`),
//! `PQsendQueryPrepared` (`:1650`) and `PQsendQueryGuts` (`:1774`) make, and
//! hands the call to [`exec_with`], which is `PQexecStart` and
//! `PQexecFinish`.

use std::ffi::{c_char, c_int};

use rlibpq::{ArgumentError, Format, PQ_QUERY_PARAM_MAX_LIMIT, Params};

use crate::conn::{PGconn, Refused, c_bytes, exec_with};
use crate::result::PGresult;

/// Calculation: `nParams`, checked as `fe-exec.c:1527`, `:1573` and `:1667`
/// check it.
fn param_count(n_params: c_int) -> Result<usize, Refused> {
    usize::try_from(n_params)
        .ok()
        .filter(|&count| count <= PQ_QUERY_PARAM_MAX_LIMIT)
        .ok_or_else(|| Refused(ArgumentError::TooManyParameters.message()))
}

/// Calculation: a `paramFormats` entry or `resultFormat`. C tests a
/// parameter's format for "not 0" (`fe-exec.c:1856`), so every other value
/// is binary.
fn format(code: c_int) -> Format {
    if code == 0 {
        Format::Text
    } else {
        Format::Binary
    }
}

/// `n` entries of the C array `array`, or `None` for its NULL pointer.
///
/// # Safety
///
/// `array` is null or points at `n` readable `T`s that outlive `'a`.
unsafe fn c_array<'a, T>(array: *const T, n: usize) -> Option<&'a [T]> {
    // SAFETY: the caller's contract; a zero-length slice needs only a
    // non-null, aligned pointer, which a non-null C array is.
    (!array.is_null()).then(|| unsafe { std::slice::from_raw_parts(array, n) })
}

/// The parameters of one `PQexecParams` or `PQexecPrepared`, read from C.
struct CParams<'a> {
    values: Vec<Option<&'a [u8]>>,
    formats: Vec<Format>,
    result_format: Format,
}

impl CParams<'_> {
    fn params(&self) -> Params<'_> {
        Params {
            values: &self.values,
            formats: &self.formats,
            result_format: self.result_format,
        }
    }
}

/// Action: the Bind arguments, read as `PQsendQueryGuts` reads them
/// (`fe-exec.c:1849`-`:1882`).
///
/// A NULL `paramValues`, or a NULL entry in it, is SQL NULL. A text value
/// is its C string, `paramLengths` unread; a binary one is `paramLengths[i]`
/// bytes, and with no `paramLengths` the call is refused (`:1863`). C then
/// leaves in its output buffer whatever it had built so far — a
/// `PQexecParams` Parse, which goes out ahead of the next command — where
/// this sends nothing. A negative binary length makes C `memcpy` a wrapped
/// size after writing the length word; here that length word, -1, is what
/// is sent: SQL NULL.
///
/// # Safety
///
/// Each non-null array holds `n` entries; each non-null `paramValues` entry
/// is a NUL-terminated string when its format is text, and
/// `paramLengths[i]` readable bytes when binary.
unsafe fn read_params<'a>(
    n: usize,
    param_values: *const *const c_char,
    param_lengths: *const c_int,
    param_formats: *const c_int,
    result_format: c_int,
) -> Result<CParams<'a>, Refused> {
    // SAFETY: the caller's contract, for all three arrays.
    let (values, lengths, formats) = unsafe {
        (
            c_array(param_values, n),
            c_array(param_lengths, n),
            c_array(param_formats, n),
        )
    };
    let formats: Vec<Format> = formats
        .map(|formats| formats.iter().copied().map(format).collect())
        .unwrap_or_default();
    let mut read = Vec::with_capacity(n);
    for i in 0..n {
        let value = values.map_or(std::ptr::null(), |values| values[i]);
        if value.is_null() {
            read.push(None);
        } else if formats.get(i).is_some_and(|&f| f == Format::Binary) {
            let lengths = lengths
                .ok_or_else(|| Refused(b"length must be given for binary parameter".to_vec()))?;
            read.push(usize::try_from(lengths[i]).ok().map(|len| {
                // SAFETY: the caller's contract.
                unsafe { std::slice::from_raw_parts(value.cast::<u8>(), len) }
            }));
        } else {
            // SAFETY: the caller's contract.
            read.push(unsafe { c_bytes(value) });
        }
    }
    Ok(CParams {
        values: read,
        formats,
        result_format: format(result_format),
    })
}

/// `PQexecParams`, `fe-exec.c:2293`: `command` through the unnamed
/// statement, with out-of-line parameters; the last result, or NULL when
/// nothing was sent.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `command` is null
/// or a NUL-terminated string; the arrays are as [`read_params`] needs, and
/// a non-null `paramTypes` holds `nParams` OIDs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQexecParams(
    conn: *mut PGconn,
    command: *const c_char,
    nParams: c_int,
    paramTypes: *const u32,
    paramValues: *const *const c_char,
    paramLengths: *const c_int,
    paramFormats: *const c_int,
    resultFormat: c_int,
) -> *mut PGresult {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        exec_with(conn, |connection| {
            let command = c_bytes(command).ok_or_else(Refused::null_command)?;
            let n = param_count(nParams)?;
            let types = c_array(paramTypes, n).unwrap_or_default();
            let params = read_params(n, paramValues, paramLengths, paramFormats, resultFormat)?;
            Ok(connection.exec_params(command, types, &params.params()))
        })
    }
}

/// `PQprepare`, `fe-exec.c:2323`: Parse `query` as the statement
/// `stmtName`; COMMAND_OK or the server's error, or NULL when nothing was
/// sent.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `stmtName` and
/// `query` are null or NUL-terminated strings; a non-null `paramTypes`
/// holds `nParams` OIDs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQprepare(
    conn: *mut PGconn,
    stmtName: *const c_char,
    query: *const c_char,
    nParams: c_int,
    paramTypes: *const u32,
) -> *mut PGresult {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        exec_with(conn, |connection| {
            let statement = c_bytes(stmtName).ok_or_else(Refused::null_statement)?;
            let query = c_bytes(query).ok_or_else(Refused::null_command)?;
            let n = param_count(nParams)?;
            // `fe-exec.c:1590`: the types only when there are parameters and
            // a types array.
            let types = c_array(paramTypes, n).unwrap_or_default();
            Ok(connection.prepare(statement, query, types))
        })
    }
}

/// `PQexecPrepared`, `fe-exec.c:2340`: run the prepared statement
/// `stmtName`; the last result, or NULL when nothing was sent.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `stmtName` is null
/// or a NUL-terminated string; the arrays are as [`read_params`] needs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQexecPrepared(
    conn: *mut PGconn,
    stmtName: *const c_char,
    nParams: c_int,
    paramValues: *const *const c_char,
    paramLengths: *const c_int,
    paramFormats: *const c_int,
    resultFormat: c_int,
) -> *mut PGresult {
    // SAFETY: the caller's contract, for every pointer read below.
    unsafe {
        exec_with(conn, |connection| {
            let statement = c_bytes(stmtName).ok_or_else(Refused::null_statement)?;
            let n = param_count(nParams)?;
            let params = read_params(n, paramValues, paramLengths, paramFormats, resultFormat)?;
            Ok(connection.exec_prepared(statement, &params.params()))
        })
    }
}

/// `PQdescribePrepared`, `fe-exec.c:2472`: a COMMAND_OK result whose
/// parameters and columns describe the statement. A NULL name is the
/// unnamed statement (`PQsendTypedCommand`, `:2610`-`:2612`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `stmt` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQdescribePrepared(
    conn: *mut PGconn,
    stmt: *const c_char,
) -> *mut PGresult {
    // SAFETY: the caller's contract.
    unsafe {
        exec_with(conn, |connection| {
            Ok(connection.describe_prepared(c_bytes(stmt).unwrap_or_default()))
        })
    }
}

/// `PQdescribePortal`, `fe-exec.c:2491`: a COMMAND_OK result whose columns
/// describe the portal, such as a cursor `DECLARE` made. A NULL name is the
/// unnamed portal.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `portal` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQdescribePortal(
    conn: *mut PGconn,
    portal: *const c_char,
) -> *mut PGresult {
    // SAFETY: the caller's contract.
    unsafe {
        exec_with(conn, |connection| {
            Ok(connection.describe_portal(c_bytes(portal).unwrap_or_default()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_count_outside_0_to_65535_is_refused_with_c_text() {
        assert_eq!(param_count(0), Ok(0));
        assert_eq!(param_count(65_535), Ok(65_535));
        for refused in [-1, 65_536] {
            assert_eq!(
                param_count(refused),
                Err(Refused(
                    b"number of parameters must be between 0 and 65535".to_vec()
                ))
            );
        }
    }

    #[test]
    fn every_nonzero_format_is_binary() {
        assert_eq!(format(0), Format::Text);
        assert_eq!(format(1), Format::Binary);
        assert_eq!(format(2), Format::Binary);
        assert_eq!(format(-1), Format::Binary);
    }

    #[test]
    fn params_read_as_pq_send_query_guts_reads_them() {
        let text = c"joe's place";
        let binary = [0u8, 0, 0, 2];
        let values = [
            text.as_ptr(),
            std::ptr::null(),
            binary.as_ptr().cast::<c_char>(),
        ];
        let lengths = [99, 99, 4];
        let formats = [0, 1, 1];
        // SAFETY: three entries each, the text NUL-terminated, the binary
        // value four bytes.
        let read =
            unsafe { read_params(3, values.as_ptr(), lengths.as_ptr(), formats.as_ptr(), 1) }
                .expect("readable");
        assert_eq!(
            read.values,
            [Some(&b"joe's place"[..]), None, Some(&binary[..])]
        );
        assert_eq!(read.formats, [Format::Text, Format::Binary, Format::Binary]);
        assert_eq!(read.result_format, Format::Binary);

        // SAFETY: as above, with no lengths.
        let refused =
            unsafe { read_params(3, values.as_ptr(), std::ptr::null(), formats.as_ptr(), 0) };
        assert_eq!(
            refused.err(),
            Some(Refused(
                b"length must be given for binary parameter".to_vec()
            ))
        );

        // SAFETY: NULL arrays are all NULL values, text, nothing read.
        let nulls =
            unsafe { read_params(2, std::ptr::null(), std::ptr::null(), std::ptr::null(), 0) }
                .expect("readable");
        assert_eq!(nulls.values, [None, None]);
        assert!(nulls.formats.is_empty());
    }
}
