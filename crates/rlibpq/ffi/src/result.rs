//! `PGresult` and the accessors of `fe-exec.c` that read one: `PQresultStatus`,
//! `PQresStatus`, `PQresultErrorMessage`, `PQntuples`, `PQnfields`, `PQfname`,
//! `PQcmdStatus`, `PQgetvalue`, `PQgetlength`, `PQgetisnull` and `PQclear`.
//!
//! A [`PGresult`] is built once from an `rlibpq` [`QueryResult`], with every
//! string C will read already NUL-terminated, so each accessor is a lookup.
//! The range checks are calculations that return the notice C would raise;
//! the shims print it, as the default notice processor does.

use std::ffi::{CStr, c_char, c_int};
use std::io::Write as _;
use std::ptr::null_mut;

use rlibpq::{ExecStatus, QueryResult};

use crate::ctext::CText;

/// `pgresStatus[]`, `fe-exec.c:33`-`:47`: what `PQresStatus` answers,
/// indexed by `ExecStatusType` (`libpq-fe.h:122`).
const PGRES_STATUS: [&CStr; 13] = [
    c"PGRES_EMPTY_QUERY",
    c"PGRES_COMMAND_OK",
    c"PGRES_TUPLES_OK",
    c"PGRES_COPY_OUT",
    c"PGRES_COPY_IN",
    c"PGRES_BAD_RESPONSE",
    c"PGRES_NONFATAL_ERROR",
    c"PGRES_FATAL_ERROR",
    c"PGRES_COPY_BOTH",
    c"PGRES_SINGLE_TUPLE",
    c"PGRES_PIPELINE_SYNC",
    c"PGRES_PIPELINE_ABORTED",
    c"PGRES_TUPLES_CHUNK",
];

/// Calculation: `status` as the `ExecStatusType` value `libpq-fe.h:122`
/// gives it, `PGRES_EMPTY_QUERY = 0` counting up.
pub(crate) const fn exec_status_code(status: ExecStatus) -> c_int {
    match status {
        ExecStatus::EmptyQuery => 0,
        ExecStatus::CommandOk => 1,
        ExecStatus::TuplesOk => 2,
        ExecStatus::CopyOut => 3,
        ExecStatus::CopyIn => 4,
        ExecStatus::BadResponse => 5,
        ExecStatus::NonfatalError => 6,
        ExecStatus::FatalError => 7,
        ExecStatus::CopyBoth => 8,
        ExecStatus::SingleTuple => 9,
        ExecStatus::PipelineSync => 10,
        ExecStatus::PipelineAborted => 11,
        ExecStatus::TuplesChunk => 12,
    }
}

/// Calculation: `PQresStatus`'s answer for any `int`, `fe-exec.c:3450`.
fn res_status(status: c_int) -> &'static CStr {
    usize::try_from(status)
        .ok()
        .and_then(|index| PGRES_STATUS.get(index))
        .copied()
        .unwrap_or(c"invalid ExecStatusType code")
}

/// Calculation: a count as the `int` C reports it.
fn c_count(count: usize) -> c_int {
    c_int::try_from(count).unwrap_or(c_int::MAX)
}

/// `PGresult`, opaque to C (`libpq-fe.h:214`: `typedef struct pg_result
/// PGresult`).
#[derive(Debug)]
pub struct PGresult {
    status: ExecStatus,
    /// `attDescs[i].name`. `None` for a COPY result's columns: `getCopyStart`
    /// zeroes `attDescs` and fills in only the formats (`fe-protocol3.c:1732`,
    /// `:1747`), so C's `PQfname` answers NULL there.
    fnames: Vec<Option<CText>>,
    /// `tuples`; `None` is a NULL field.
    rows: Vec<Vec<Option<CText>>>,
    /// `null_field`, the empty string every NULL field's value points at.
    null_field: CText,
    cmd_status: CText,
    /// `errMsg`; `None` answers `""`, as `PQresultErrorMessage` does for a
    /// NULL `errMsg` (`fe-exec.c:3460`).
    error_message: Option<CText>,
}

impl PGresult {
    /// Calculation: `result` laid out for C.
    pub(crate) fn from_result(result: &QueryResult) -> Self {
        let copy = matches!(
            result.status(),
            ExecStatus::CopyIn | ExecStatus::CopyOut | ExecStatus::CopyBoth
        );
        PGresult {
            status: result.status(),
            fnames: result
                .fields()
                .iter()
                .map(|field| (!copy).then(|| CText::new(&field.name)))
                .collect(),
            rows: (0..result.ntuples())
                .map(|row| {
                    (0..result.nfields())
                        .map(|column| result.value(row, column).map(CText::new))
                        .collect()
                })
                .collect(),
            null_field: CText::default(),
            cmd_status: CText::new(result.command_status()),
            error_message: result.error().map(|_| CText::new(&result.error_message())),
        }
    }

    /// Calculation: the `PGRES_FATAL_ERROR` result `pqPrepareAsyncResult`
    /// (`fe-exec.c:857`) makes from `conn->errorMessage` when a query ends
    /// with no result from the server, such as a lost connection.
    pub(crate) fn fatal_error(message: &[u8]) -> Self {
        PGresult {
            status: ExecStatus::FatalError,
            fnames: Vec::new(),
            rows: Vec::new(),
            null_field: CText::default(),
            cmd_status: CText::default(),
            error_message: Some(CText::new(message)),
        }
    }

    fn ntuples(&self) -> c_int {
        c_count(self.rows.len())
    }

    fn nfields(&self) -> c_int {
        c_count(self.fnames.len())
    }

    /// Calculation: `check_field_number`, `fe-exec.c:3541`: the column, or
    /// the notice C raises for it.
    fn field(&self, field_num: c_int) -> Result<usize, String> {
        usize::try_from(field_num)
            .ok()
            .filter(|&column| column < self.fnames.len())
            .ok_or_else(|| {
                format!(
                    "column number {field_num} is out of range 0..{}",
                    i64::from(self.nfields()) - 1
                )
            })
    }

    /// Calculation: `check_tuple_field_number`, `fe-exec.c:3556`: the field,
    /// or the notice C raises for it, the row checked first.
    fn cell(&self, tup_num: c_int, field_num: c_int) -> Result<Option<&CText>, String> {
        let row = usize::try_from(tup_num)
            .ok()
            .and_then(|row| self.rows.get(row))
            .ok_or_else(|| {
                format!(
                    "row number {tup_num} is out of range 0..{}",
                    i64::from(self.ntuples()) - 1
                )
            })?;
        let column = self.field(field_num)?;
        Ok(row[column].as_ref())
    }
}

/// Action: raise `notice` through the result's notice hooks.
///
/// `pqInternalNotice` (`fe-exec.c:944`) makes the text the primary message
/// plus a newline (`:979`); with no `PQsetNoticeReceiver` or
/// `PQsetNoticeProcessor` exported yet, the hooks are always the defaults,
/// which print it to stderr (`defaultNoticeProcessor`, `fe-connect.c:7857`).
fn internal_notice(notice: &str) {
    let _ = writeln!(std::io::stderr(), "{notice}");
}

/// The `PGresult` behind `res`, or `None` for NULL.
///
/// # Safety
///
/// `res` is null or a live pointer this crate returned.
unsafe fn borrow<'a>(res: *const PGresult) -> Option<&'a PGresult> {
    // SAFETY: the caller's contract.
    unsafe { res.as_ref() }
}

/// `PQresultStatus`, `fe-exec.c:3442`: `PGRES_FATAL_ERROR` for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQresultStatus(res: *const PGresult) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(exec_status_code(ExecStatus::FatalError), |res| {
        exec_status_code(res.status)
    })
}

/// `PQresStatus`, `fe-exec.c:3450`.
#[unsafe(no_mangle)]
pub extern "C" fn PQresStatus(status: c_int) -> *mut c_char {
    res_status(status).as_ptr().cast_mut()
}

/// `PQresultErrorMessage`, `fe-exec.c:3458`: `""` for NULL or no error.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQresultErrorMessage(res: *const PGresult) -> *mut c_char {
    // SAFETY: the caller's contract.
    match unsafe { borrow(res) }.and_then(|res| res.error_message.as_ref()) {
        Some(message) => message.as_ptr(),
        None => c"".as_ptr().cast_mut(),
    }
}

/// `PQntuples`, `fe-exec.c:3512`: 0 for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQntuples(res: *const PGresult) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(0, PGresult::ntuples)
}

/// `PQnfields`, `fe-exec.c:3520`: 0 for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQnfields(res: *const PGresult) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(0, PGresult::nfields)
}

/// `PQfname`, `fe-exec.c:3598`: NULL for a NULL result, a column out of
/// range (with its notice) or a COPY result's column.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfname(res: *const PGresult, field_num: c_int) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return null_mut();
    };
    match res.field(field_num) {
        Ok(column) => res.fnames[column]
            .as_ref()
            .map_or(null_mut(), CText::as_ptr),
        Err(notice) => {
            internal_notice(&notice);
            null_mut()
        }
    }
}

/// `PQcmdStatus`, `fe-exec.c:3783`: the command tag, `""` when the result
/// has none, NULL for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQcmdStatus(res: *mut PGresult) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(null_mut(), |res| res.cmd_status.as_ptr())
}

/// `PQgetvalue`, `fe-exec.c:3907`: the field's text, `""` for a NULL field,
/// NULL for a NULL result or a row or column out of range (with its notice).
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQgetvalue(
    res: *const PGresult,
    tup_num: c_int,
    field_num: c_int,
) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return null_mut();
    };
    match res.cell(tup_num, field_num) {
        Ok(value) => value.unwrap_or(&res.null_field).as_ptr(),
        Err(notice) => {
            internal_notice(&notice);
            null_mut()
        }
    }
}

/// `PQgetlength`, `fe-exec.c:3918`: the value's length in bytes, 0 for a
/// NULL field and for anything `PQgetvalue` answers NULL for.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQgetlength(
    res: *const PGresult,
    tup_num: c_int,
    field_num: c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return 0;
    };
    match res.cell(tup_num, field_num) {
        Ok(value) => value.map_or(0, |value| c_count(value.bytes().len())),
        Err(notice) => {
            internal_notice(&notice);
            0
        }
    }
}

/// `PQgetisnull`, `fe-exec.c:3932`: 1 for a NULL field, and "pretend it is
/// null" for anything `PQgetvalue` answers NULL for.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQgetisnull(
    res: *const PGresult,
    tup_num: c_int,
    field_num: c_int,
) -> c_int {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return 1;
    };
    match res.cell(tup_num, field_num) {
        Ok(value) => c_int::from(value.is_none()),
        Err(notice) => {
            internal_notice(&notice);
            1
        }
    }
}

/// `PQclear`, `fe-exec.c:727`: free the result; nothing for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQclear(res: *mut PGresult) {
    if !res.is_null() {
        // SAFETY: every `PGresult` this crate hands out is `Box::into_raw`.
        drop(unsafe { Box::from_raw(res) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY_STATUS: [ExecStatus; 13] = [
        ExecStatus::EmptyQuery,
        ExecStatus::CommandOk,
        ExecStatus::TuplesOk,
        ExecStatus::CopyOut,
        ExecStatus::CopyIn,
        ExecStatus::BadResponse,
        ExecStatus::NonfatalError,
        ExecStatus::FatalError,
        ExecStatus::CopyBoth,
        ExecStatus::SingleTuple,
        ExecStatus::PipelineSync,
        ExecStatus::PipelineAborted,
        ExecStatus::TuplesChunk,
    ];

    /// `libpq-fe.h:122`-`:143` numbers the statuses in declaration order, and
    /// `pgresStatus[]` names them in the same order.
    #[test]
    fn every_status_has_its_libpq_fe_h_code_and_pgres_status_name() {
        for (code, status) in EVERY_STATUS.into_iter().enumerate() {
            assert_eq!(exec_status_code(status), c_int::try_from(code).unwrap());
            assert_eq!(
                res_status(exec_status_code(status)).to_bytes(),
                status.as_str().as_bytes()
            );
        }
        assert_eq!(res_status(13), c"invalid ExecStatusType code");
        assert_eq!(res_status(-1), c"invalid ExecStatusType code");
    }

    #[test]
    fn a_range_check_names_the_row_before_the_column() {
        let res = PGresult::fatal_error(b"boom\n");
        assert_eq!(
            res.cell(0, 0),
            Err("row number 0 is out of range 0..-1".to_string())
        );
        assert_eq!(
            res.field(-2),
            Err("column number -2 is out of range 0..-1".to_string())
        );
        assert_eq!(
            res.error_message.as_ref().map(CText::bytes),
            Some(&b"boom\n"[..])
        );
    }
}
