//! `PGresult` and the accessors of `fe-exec.c` that read one: `PQresultStatus`,
//! `PQresStatus`, `PQresultErrorMessage`, `PQresultErrorField`, `PQntuples`,
//! `PQnfields`, `PQbinaryTuples`, `PQfname`, `PQfnumber`, the column
//! metadata (`PQftable`, `PQftablecol`, `PQfformat`, `PQftype`, `PQfsize`,
//! `PQfmod`), `PQcmdStatus`, `PQoidStatus`, `PQoidValue`, `PQcmdTuples`,
//! `PQgetvalue`, `PQgetlength`, `PQgetisnull`, `PQnparams`, `PQparamtype` and
//! `PQclear`.
//!
//! A [`PGresult`] is built once from an `rlibpq` [`QueryResult`], with every
//! string C will read already NUL-terminated, so each accessor is a lookup.
//! The range checks are calculations that return the notice C would raise;
//! the shims raise it through the hooks the result carries
//! (`pqInternalNotice`, `fe-exec.c:944`).

use std::ffi::{CStr, c_char, c_int};
use std::ptr::null_mut;

use rlibpq::{ExecStatus, QueryResult};

use crate::ctext::CText;
use crate::notice::{NoticeHooks, internal_notice};

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

/// One column: `PGresAttDesc`, `libpq-fe.h:305`, with its name ready for C.
#[derive(Debug)]
struct Attr {
    /// `name`. `None` for a COPY result's columns: `getCopyStart` zeroes
    /// `attDescs` and fills in only the formats (`fe-protocol3.c:1732`,
    /// `:1747`), so C's `PQfname` answers NULL there.
    name: Option<CText>,
    tableid: u32,
    columnid: c_int,
    format: c_int,
    typid: u32,
    typlen: c_int,
    atttypmod: c_int,
}

/// `PGresult`, opaque to C (`libpq-fe.h:214`: `typedef struct pg_result
/// PGresult`).
#[derive(Debug)]
pub struct PGresult {
    status: ExecStatus,
    /// `attDescs`.
    attrs: Vec<Attr>,
    /// `paramDescs`: the parameter types of a described statement.
    params: Vec<u32>,
    /// `binary`, set only by a binary COPY (`getCopyStart`,
    /// `fe-protocol3.c:1719`).
    binary: bool,
    /// `tuples`; `None` is a NULL field.
    rows: Vec<Vec<Option<CText>>>,
    /// `null_field`, the empty string every NULL field's value points at.
    null_field: CText,
    cmd_status: CText,
    /// What `PQoidStatus` answers, worked out once from `cmd_status`.
    oid_status: CText,
    /// `errMsg`; `None` answers `""`, as `PQresultErrorMessage` does for a
    /// NULL `errMsg` (`fe-exec.c:3460`).
    error_message: Option<CText>,
    /// `errFields`, in the order the server sent them.
    error_fields: Vec<(u8, CText)>,
    /// `noticeHooks`: the connection's when the result was made.
    pub(crate) hooks: NoticeHooks,
}

impl PGresult {
    /// Calculation: `result` laid out for C, reporting its notices to
    /// `hooks`.
    pub(crate) fn from_result(result: &QueryResult, hooks: NoticeHooks) -> Self {
        let copy = matches!(
            result.status(),
            ExecStatus::CopyIn | ExecStatus::CopyOut | ExecStatus::CopyBoth
        );
        PGresult {
            status: result.status(),
            attrs: result
                .fields()
                .iter()
                .map(|field| Attr {
                    name: (!copy).then(|| CText::new(&field.name)),
                    tableid: field.tableid,
                    columnid: c_int::from(field.columnid),
                    format: c_int::from(field.format),
                    typid: field.typid,
                    typlen: c_int::from(field.typlen),
                    atttypmod: field.atttypmod,
                })
                .collect(),
            params: (0..result.nparams())
                .filter_map(|param| result.paramtype(param))
                .collect(),
            binary: result.binary_tuples(),
            rows: (0..result.ntuples())
                .map(|row| {
                    (0..result.nfields())
                        .map(|column| result.value(row, column).map(CText::new))
                        .collect()
                })
                .collect(),
            null_field: CText::default(),
            cmd_status: CText::new(result.command_status()),
            oid_status: CText::new(oid_status(result.command_status())),
            error_message: result.error().map(|_| CText::new(&result.error_message())),
            error_fields: result
                .error()
                .map(|error| {
                    error
                        .fields()
                        .iter()
                        .map(|(code, value)| (*code, CText::new(value)))
                        .collect()
                })
                .unwrap_or_default(),
            hooks,
        }
    }

    /// Calculation: the `PGRES_FATAL_ERROR` result `pqPrepareAsyncResult`
    /// (`fe-exec.c:857`) makes from `conn->errorMessage` when a query ends
    /// with no result from the server, such as a lost connection. It has a
    /// message but no fields.
    pub(crate) fn fatal_error(message: &[u8], hooks: NoticeHooks) -> Self {
        PGresult {
            status: ExecStatus::FatalError,
            attrs: Vec::new(),
            params: Vec::new(),
            binary: false,
            rows: Vec::new(),
            null_field: CText::default(),
            cmd_status: CText::default(),
            oid_status: CText::default(),
            error_message: Some(CText::new(message)),
            error_fields: Vec::new(),
            hooks,
        }
    }

    /// Calculation: the notice `pqInternalNotice` (`fe-exec.c:944`) makes of
    /// `text`: `PGRES_NONFATAL_ERROR`, the text as the primary message with
    /// severity `NOTICE` (`:968`-`:970`), and the text and a newline as the
    /// message (`:977`-`:979`).
    pub(crate) fn internal_notice(text: &[u8], hooks: NoticeHooks) -> Self {
        PGresult {
            status: ExecStatus::NonfatalError,
            error_message: Some(CText::new(&[text, b"\n"].concat())),
            error_fields: vec![
                (b'M', CText::new(text)),
                (b'S', CText::new(b"NOTICE")),
                (b'V', CText::new(b"NOTICE")),
            ],
            ..PGresult::fatal_error(b"", hooks)
        }
    }

    /// Is it `PGRES_FATAL_ERROR`?
    pub(crate) fn is_fatal_error(&self) -> bool {
        self.status == ExecStatus::FatalError
    }

    fn ntuples(&self) -> c_int {
        c_count(self.rows.len())
    }

    fn nfields(&self) -> c_int {
        c_count(self.attrs.len())
    }

    /// Calculation: `check_field_number`, `fe-exec.c:3541`: the column, or
    /// the notice C raises for it.
    fn field(&self, field_num: c_int) -> Result<&Attr, Vec<u8>> {
        usize::try_from(field_num)
            .ok()
            .and_then(|column| self.attrs.get(column))
            .ok_or_else(|| {
                format!(
                    "column number {field_num} is out of range 0..{}",
                    i64::from(self.nfields()) - 1
                )
                .into_bytes()
            })
    }

    /// Calculation: `check_tuple_field_number`, `fe-exec.c:3556`: the field,
    /// or the notice C raises for it, the row checked first.
    fn cell(&self, tup_num: c_int, field_num: c_int) -> Result<Option<&CText>, Vec<u8>> {
        let row = usize::try_from(tup_num)
            .ok()
            .and_then(|row| self.rows.get(row))
            .ok_or_else(|| {
                format!(
                    "row number {tup_num} is out of range 0..{}",
                    i64::from(self.ntuples()) - 1
                )
                .into_bytes()
            })?;
        self.field(field_num)?;
        Ok(row[usize::try_from(field_num).unwrap_or_default()].as_ref())
    }

    /// Calculation: `check_param_number`, `fe-exec.c:3579`: the parameter's
    /// type, or the notice C raises for it.
    fn param(&self, param_num: c_int) -> Result<u32, Vec<u8>> {
        usize::try_from(param_num)
            .ok()
            .and_then(|param| self.params.get(param))
            .copied()
            .ok_or_else(|| {
                format!(
                    "parameter number {param_num} is out of range 0..{}",
                    i64::from(c_count(self.params.len())) - 1
                )
                .into_bytes()
            })
    }
}

/// Calculation: `PQfnumber`'s folding of `field_name`, `fe-exec.c:3672`-
/// `:3702`: outside double quotes a letter is lowered, inside them it is
/// kept, and a doubled quote inside them is one quote.
///
/// `pg_tolower` (`src/port/pgstrcasecmp.c:122`) lowers `A`-`Z`, and a
/// high-bit byte only where the C library's `isupper` says so, which in the
/// C locale a program starts in is never; this port lowers ASCII only. C's
/// fast path for a name already all lower case (`:3645`-`:3659`) is the
/// same answer, since folding such a name changes nothing.
fn fold_field_name(field_name: &[u8]) -> Vec<u8> {
    let mut folded = Vec::with_capacity(field_name.len());
    let mut in_quotes = false;
    let mut bytes = field_name.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if in_quotes {
            if byte == b'"' {
                if bytes.next_if_eq(&b'"').is_some() {
                    folded.push(b'"');
                } else {
                    in_quotes = false;
                }
            } else {
                folded.push(byte);
            }
        } else if byte == b'"' {
            in_quotes = true;
        } else {
            folded.push(byte.to_ascii_lowercase());
        }
    }
    folded
}

/// Calculation: `PQfnumber`, `fe-exec.c:3620`: the first column whose name
/// is `field_name` folded, or -1. An empty name is -1 before any folding
/// (`:3637`); a column with no name (a COPY result's) never matches.
fn fnumber(res: &PGresult, field_name: &[u8]) -> c_int {
    if field_name.is_empty() {
        return -1;
    }
    let folded = fold_field_name(field_name);
    res.attrs
        .iter()
        .position(|attr| {
            attr.name
                .as_ref()
                .is_some_and(|name| name.bytes() == folded)
        })
        .map_or(-1, c_count)
}

/// Calculation: `PQoidStatus`, `fe-exec.c:3796`: the digits after an
/// `INSERT ` tag, at most 23 of them (`buf[24]`), else nothing.
fn oid_status(cmd_status: &[u8]) -> &[u8] {
    let Some(rest) = cmd_status.strip_prefix(b"INSERT ") else {
        return b"";
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    &rest[..digits.min(23)]
}

/// Calculation: `PQoidValue`, `fe-exec.c:3824`: the OID of an `INSERT oid
/// rows` tag, `InvalidOid` (0) for anything else.
///
/// `strtoul` saturates at `ULONG_MAX` and C then casts to `Oid`; on every
/// target this crate builds for `unsigned long` is 64 bits, so the answer
/// is the low 32 bits of the saturated value.
fn oid_value(cmd_status: &[u8]) -> u32 {
    let Some(rest) = cmd_status.strip_prefix(b"INSERT ") else {
        return 0;
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 || !matches!(rest.get(digits), None | Some(b' ')) {
        return 0;
    }
    let value = rest[..digits].iter().fold(0u64, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit - b'0')))
            .unwrap_or(u64::MAX)
    });
    // `(Oid) result`: the conversion keeps the low 32 bits.
    #[allow(clippy::cast_possible_truncation)]
    let oid = value as u32;
    oid
}

/// Calculation: `PQcmdTuples`, `fe-exec.c:3853`: where in `cmd_status` the
/// row count starts, `None` for a tag that carries none, or the notice C
/// raises for a tag it cannot read.
fn cmd_tuples(cmd_status: &[u8]) -> Result<Option<usize>, Vec<u8>> {
    let start = if let Some(rest) = cmd_status.strip_prefix(b"INSERT ") {
        // "INSERT: skip oid and space" (`:3864`-`:3869`).
        match rest.iter().position(|&byte| byte == b' ') {
            Some(space) => 7 + space + 1,
            None => return Err(cannot_interpret(cmd_status)),
        }
    } else if [&b"SELECT "[..], b"DELETE ", b"UPDATE "]
        .iter()
        .any(|tag| cmd_status.starts_with(tag))
    {
        7
    } else if cmd_status.starts_with(b"FETCH ") || cmd_status.starts_with(b"MERGE ") {
        6
    } else if cmd_status.starts_with(b"MOVE ") || cmd_status.starts_with(b"COPY ") {
        5
    } else {
        return Ok(None);
    };
    // "at least one digit, nothing else" (`:3884`-`:3892`).
    let count = &cmd_status[start..];
    if count.is_empty() || !count.iter().all(u8::is_ascii_digit) {
        return Err(cannot_interpret(cmd_status));
    }
    Ok(Some(start))
}

/// Calculation: `PQcmdTuples`'s notice, `fe-exec.c:3896`.
fn cannot_interpret(cmd_status: &[u8]) -> Vec<u8> {
    let mut notice = b"could not interpret result from server: ".to_vec();
    notice.extend_from_slice(cmd_status);
    notice
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
        Ok(attr) => attr.name.as_ref().map_or(null_mut(), CText::as_ptr),
        Err(notice) => {
            internal_notice(&res.hooks, &notice);
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
            internal_notice(&res.hooks, &notice);
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
            internal_notice(&res.hooks, &notice);
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
            internal_notice(&res.hooks, &notice);
            1
        }
    }
}

/// `PQbinaryTuples`, `fe-exec.c:3528`: 1 for a binary COPY, 0 otherwise
/// and for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQbinaryTuples(res: *const PGresult) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(0, |res| c_int::from(res.binary))
}

/// `PQfnumber`, `fe-exec.c:3620`: the column named `field_name`, folded as
/// an SQL identifier is; -1 for no match, NULL or `""`.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library; `field_name` is
/// null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfnumber(res: *const PGresult, field_name: *const c_char) -> c_int {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return -1;
    };
    if field_name.is_null() {
        return -1;
    }
    // SAFETY: the caller's contract.
    fnumber(res, unsafe { CStr::from_ptr(field_name) }.to_bytes())
}

/// Action: one `PGresAttDesc` member of column `field_num`, or `default`
/// for NULL and for a column out of range (with its notice), as every
/// `PQf*` accessor from `fe-exec.c:3717` to `:3781` does.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
unsafe fn attr_member<T>(
    res: *const PGresult,
    field_num: c_int,
    default: T,
    member: impl FnOnce(&Attr) -> T,
) -> T {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return default;
    };
    match res.field(field_num) {
        Ok(attr) => member(attr),
        Err(notice) => {
            internal_notice(&res.hooks, &notice);
            default
        }
    }
}

/// `PQftable`, `fe-exec.c:3717`: the OID of the table the column comes
/// from, `InvalidOid` (0) when it is not a plain column.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQftable(res: *const PGresult, field_num: c_int) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.tableid) }
}

/// `PQftablecol`, `fe-exec.c:3728`: the column's number in that table.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQftablecol(res: *const PGresult, field_num: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.columnid) }
}

/// `PQfformat`, `fe-exec.c:3739`: 0 for text, 1 for binary.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfformat(res: *const PGresult, field_num: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.format) }
}

/// `PQftype`, `fe-exec.c:3750`: the column's type OID.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQftype(res: *const PGresult, field_num: c_int) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.typid) }
}

/// `PQfsize`, `fe-exec.c:3761`: the type's `typlen`, negative for a
/// variable-length type.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfsize(res: *const PGresult, field_num: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.typlen) }
}

/// `PQfmod`, `fe-exec.c:3772`: the column's type modifier.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfmod(res: *const PGresult, field_num: c_int) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { attr_member(res, field_num, 0, |attr| attr.atttypmod) }
}

/// `PQoidStatus`, `fe-exec.c:3796`: an `INSERT`'s OID as a string, `""`
/// otherwise and for NULL.
///
/// C returns a `static` buffer each call overwrites; this returns the
/// result's own copy, which lives as long as the result.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQoidStatus(res: *const PGresult) -> *mut c_char {
    // SAFETY: the caller's contract.
    match unsafe { borrow(res) } {
        Some(res) => res.oid_status.as_ptr(),
        None => c"".as_ptr().cast_mut(),
    }
}

/// `PQoidValue`, `fe-exec.c:3824`: an `INSERT`'s OID, `InvalidOid` (0)
/// otherwise and for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQoidValue(res: *const PGresult) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(0, |res| oid_value(res.cmd_status.bytes()))
}

/// `PQcmdTuples`, `fe-exec.c:3853`: the row count in the command tag, a
/// pointer into `PQcmdStatus`'s string; `""` for a tag without one, for
/// NULL, and for a tag it cannot read (with its notice).
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQcmdTuples(res: *mut PGresult) -> *mut c_char {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return c"".as_ptr().cast_mut();
    };
    match cmd_tuples(res.cmd_status.bytes()) {
        // SAFETY: `start` is within the tag, whose NUL follows it.
        Ok(Some(start)) => unsafe { res.cmd_status.as_ptr().add(start) },
        Ok(None) => c"".as_ptr().cast_mut(),
        Err(notice) => {
            internal_notice(&res.hooks, &notice);
            c"".as_ptr().cast_mut()
        }
    }
}

/// `PQnparams`, `fe-exec.c:3946`: how many parameters a described statement
/// takes; 0 for NULL.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQnparams(res: *const PGresult) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }.map_or(0, |res| c_count(res.params.len()))
}

/// `PQparamtype`, `fe-exec.c:3957`: the type OID of parameter `param_num`;
/// `InvalidOid` (0) for NULL and for a parameter out of range (with its
/// notice).
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQparamtype(res: *const PGresult, param_num: c_int) -> u32 {
    // SAFETY: the caller's contract.
    let Some(res) = (unsafe { borrow(res) }) else {
        return 0;
    };
    res.param(param_num).unwrap_or_else(|notice| {
        internal_notice(&res.hooks, &notice);
        0
    })
}

/// `PQresultErrorField`, `fe-exec.c:3497`: one field of the error or
/// notice, NULL when it is absent or `res` is NULL.
///
/// C keeps `errFields` newest first (`pqSaveMessageField`, `fe-exec.c:1079`),
/// so of a code the server sent twice the last one sent is found.
///
/// # Safety
///
/// `res` is null or a live `PGresult` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQresultErrorField(res: *const PGresult, fieldcode: c_int) -> *mut c_char {
    // SAFETY: the caller's contract.
    unsafe { borrow(res) }
        .and_then(|res| {
            res.error_fields
                .iter()
                .rev()
                .find(|(code, _)| c_int::from(*code) == fieldcode)
        })
        .map_or(null_mut(), |(_, value)| value.as_ptr())
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
        let res = PGresult::fatal_error(b"boom\n", NoticeHooks::NONE);
        assert_eq!(
            res.cell(0, 0),
            Err(b"row number 0 is out of range 0..-1".to_vec())
        );
        assert_eq!(
            res.field(-2).map(|_| ()),
            Err(b"column number -2 is out of range 0..-1".to_vec())
        );
        assert_eq!(
            res.error_message.as_ref().map(CText::bytes),
            Some(&b"boom\n"[..])
        );
    }

    fn named(names: &[&[u8]]) -> PGresult {
        let mut res = PGresult::fatal_error(b"", NoticeHooks::NONE);
        res.attrs = names
            .iter()
            .map(|name| Attr {
                name: Some(CText::new(name)),
                tableid: 0,
                columnid: 0,
                format: 0,
                typid: 0,
                typlen: 0,
                atttypmod: 0,
            })
            .collect();
        res
    }

    #[test]
    fn fold_field_name_lowers_outside_quotes_and_undoubles_inside() {
        assert_eq!(fold_field_name(b"FooBar"), b"foobar");
        assert_eq!(fold_field_name(b"\"FooBar\""), b"FooBar");
        assert_eq!(fold_field_name(b"\"a\"\"B\""), b"a\"B");
        // `fe-exec.c:3664`-`:3666` say so: partial quoting is not rejected.
        assert_eq!(fold_field_name(b"foo\"BAR\"FOO"), b"fooBARfoo");
        assert_eq!(fold_field_name(b"\xc4X"), b"\xc4x");
    }

    #[test]
    fn fnumber_finds_the_first_column_with_the_folded_name() {
        let res = named(&[b"a", b"B", b"a", b""]);
        assert_eq!(fnumber(&res, b"a"), 0);
        assert_eq!(fnumber(&res, b"A"), 0);
        assert_eq!(fnumber(&res, b"B"), -1);
        assert_eq!(fnumber(&res, b"\"B\""), 1);
        assert_eq!(fnumber(&res, b""), -1);
        assert_eq!(fnumber(&res, b"\"\""), 3);
        assert_eq!(fnumber(&res, b"c"), -1);
    }

    #[test]
    fn oid_status_and_oid_value_read_an_insert_tag() {
        assert_eq!(oid_status(b"INSERT 0 1"), b"0");
        assert_eq!(oid_status(b"INSERT 16384 1"), b"16384");
        assert_eq!(oid_status(b"UPDATE 1"), b"");
        assert_eq!(
            oid_status(b"INSERT 123456789012345678901234567 1").len(),
            23
        );
        assert_eq!(oid_value(b"INSERT 16384 1"), 16384);
        assert_eq!(oid_value(b"INSERT 16384"), 16384);
        assert_eq!(oid_value(b"INSERT 0 1"), 0);
        assert_eq!(oid_value(b"INSERT x 1"), 0);
        assert_eq!(oid_value(b"INSERT 12x 1"), 0);
        assert_eq!(oid_value(b"DELETE 3"), 0);
        // `(Oid) strtoul(...)`: the low 32 bits, saturated at ULONG_MAX.
        assert_eq!(oid_value(b"INSERT 4294967297 1"), 1);
        assert_eq!(oid_value(b"INSERT 99999999999999999999 1"), u32::MAX);
    }

    #[test]
    fn cmd_tuples_finds_the_count_or_the_notice() {
        let at = |tag: &[u8]| cmd_tuples(tag).map(|start| start.map(|start| tag[start..].to_vec()));
        assert_eq!(at(b"INSERT 0 2"), Ok(Some(b"2".to_vec())));
        assert_eq!(at(b"SELECT 10"), Ok(Some(b"10".to_vec())));
        assert_eq!(at(b"UPDATE 0"), Ok(Some(b"0".to_vec())));
        assert_eq!(at(b"DELETE 3"), Ok(Some(b"3".to_vec())));
        assert_eq!(at(b"FETCH 4"), Ok(Some(b"4".to_vec())));
        assert_eq!(at(b"MERGE 5"), Ok(Some(b"5".to_vec())));
        assert_eq!(at(b"MOVE 6"), Ok(Some(b"6".to_vec())));
        assert_eq!(at(b"COPY 7"), Ok(Some(b"7".to_vec())));
        assert_eq!(at(b"CREATE TABLE"), Ok(None));
        assert_eq!(at(b""), Ok(None));
        assert_eq!(
            at(b"INSERT 0"),
            Err(b"could not interpret result from server: INSERT 0".to_vec())
        );
        assert_eq!(
            at(b"SELECT "),
            Err(b"could not interpret result from server: SELECT ".to_vec())
        );
        assert_eq!(
            at(b"UPDATE 1x"),
            Err(b"could not interpret result from server: UPDATE 1x".to_vec())
        );
    }

    #[test]
    fn a_parameter_out_of_range_raises_c_s_notice() {
        let mut res = PGresult::fatal_error(b"", NoticeHooks::NONE);
        res.params = vec![23, 25];
        assert_eq!(res.param(1), Ok(25));
        assert_eq!(
            res.param(2),
            Err(b"parameter number 2 is out of range 0..1".to_vec())
        );
        assert_eq!(
            res.param(-1),
            Err(b"parameter number -1 is out of range 0..1".to_vec())
        );
    }
}
