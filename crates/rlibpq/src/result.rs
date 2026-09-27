//! `PGresult` as data: the status, the row description, the rows, and the
//! broken-down error fields.
//!
//! Ported from `src/interfaces/libpq/fe-exec.c` (the `PGresult` struct and its
//! accessors) and `fe-protocol3.c` (`pqGetErrorNotice3`, `:899`, which fills
//! the error fields, `pqBuildErrorMessage3`, `:1031`, which renders them, and
//! `reportErrorPosition`, `:1202`, which draws the syntax cursor).
//! Everything here is a calculation over bytes; nothing touches a socket.

use crate::encoding::Encoding;

/// `ExecStatusType`, `libpq-fe.h:122`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecStatus {
    EmptyQuery,
    CommandOk,
    TuplesOk,
    CopyOut,
    CopyIn,
    BadResponse,
    NonfatalError,
    FatalError,
    CopyBoth,
    SingleTuple,
    PipelineSync,
    PipelineAborted,
    TuplesChunk,
}

impl ExecStatus {
    /// `pgresStatus[]`, `fe-exec.c:33` — what `PQresStatus` returns.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ExecStatus::EmptyQuery => "PGRES_EMPTY_QUERY",
            ExecStatus::CommandOk => "PGRES_COMMAND_OK",
            ExecStatus::TuplesOk => "PGRES_TUPLES_OK",
            ExecStatus::CopyOut => "PGRES_COPY_OUT",
            ExecStatus::CopyIn => "PGRES_COPY_IN",
            ExecStatus::BadResponse => "PGRES_BAD_RESPONSE",
            ExecStatus::NonfatalError => "PGRES_NONFATAL_ERROR",
            ExecStatus::FatalError => "PGRES_FATAL_ERROR",
            ExecStatus::CopyBoth => "PGRES_COPY_BOTH",
            ExecStatus::SingleTuple => "PGRES_SINGLE_TUPLE",
            ExecStatus::PipelineSync => "PGRES_PIPELINE_SYNC",
            ExecStatus::PipelineAborted => "PGRES_PIPELINE_ABORTED",
            ExecStatus::TuplesChunk => "PGRES_TUPLES_CHUNK",
        }
    }
}

/// The `PG_DIAG_*` field codes, `postgres_ext.h:55`-`:72`.
pub mod diag {
    pub const SEVERITY: u8 = b'S';
    pub const SEVERITY_NONLOCALIZED: u8 = b'V';
    pub const SQLSTATE: u8 = b'C';
    pub const MESSAGE_PRIMARY: u8 = b'M';
    pub const MESSAGE_DETAIL: u8 = b'D';
    pub const MESSAGE_HINT: u8 = b'H';
    pub const STATEMENT_POSITION: u8 = b'P';
    pub const INTERNAL_POSITION: u8 = b'p';
    pub const INTERNAL_QUERY: u8 = b'q';
    pub const CONTEXT: u8 = b'W';
    pub const SCHEMA_NAME: u8 = b's';
    pub const TABLE_NAME: u8 = b't';
    pub const COLUMN_NAME: u8 = b'c';
    pub const DATATYPE_NAME: u8 = b'd';
    pub const CONSTRAINT_NAME: u8 = b'n';
    pub const SOURCE_FILE: u8 = b'F';
    pub const SOURCE_LINE: u8 = b'L';
    pub const SOURCE_FUNCTION: u8 = b'R';
}

/// `PGVerbosity`, `libpq-fe.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    Terse,
    #[default]
    Default,
    Verbose,
    Sqlstate,
}

/// `PGContextVisibility`, `libpq-fe.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextVisibility {
    Never,
    #[default]
    Errors,
    Always,
}

/// The broken-down fields of an ErrorResponse or NoticeResponse, in the order
/// the server sent them (`pqSaveMessageField`, `fe-exec.c:1066`), with the
/// two things the `PGresult` holding them keeps for drawing a syntax cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultError {
    fields: Vec<(u8, Vec<u8>)>,
    /// `res->errQuery`: the text of the command the error is about, kept
    /// only when there is a `PG_DIAG_STATEMENT_POSITION` to point into it
    /// (`fe-protocol3.c:966`).
    err_query: Option<Vec<u8>>,
    /// `res->client_encoding`, copied from the connection when the result is
    /// made (`fe-exec.c:193`): what the cursor measures characters in.
    client_encoding: Encoding,
}

impl ResultError {
    /// Fields alone: no query kept, and SQL_ASCII, which is what
    /// `PQmakeEmptyPGresult` gives a result made without a connection
    /// (`fe-exec.c:234`).
    #[must_use]
    pub fn new(fields: Vec<(u8, Vec<u8>)>) -> Self {
        Self {
            fields,
            ..Self::default()
        }
    }

    /// The same fields with `res->errQuery` set to `query`.
    #[must_use]
    pub fn with_err_query(mut self, query: Vec<u8>) -> Self {
        self.err_query = Some(query);
        self
    }

    /// The same fields with `res->client_encoding` set to `encoding`.
    #[must_use]
    pub fn with_client_encoding(mut self, encoding: Encoding) -> Self {
        self.client_encoding = encoding;
        self
    }

    /// `res->errQuery`.
    #[must_use]
    pub fn err_query(&self) -> Option<&[u8]> {
        self.err_query.as_deref()
    }

    /// `res->client_encoding`.
    #[must_use]
    pub fn client_encoding(&self) -> Encoding {
        self.client_encoding
    }

    /// `PQresultErrorField`, `fe-exec.c:3497`.
    #[must_use]
    pub fn field(&self, code: u8) -> Option<&[u8]> {
        self.fields
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, v)| v.as_slice())
    }

    #[must_use]
    pub fn fields(&self) -> &[(u8, Vec<u8>)] {
        &self.fields
    }

    /// `conn->last_sqlstate`, saved at `fe-protocol3.c:954`.
    #[must_use]
    pub fn sqlstate(&self) -> Option<&[u8]> {
        self.field(diag::SQLSTATE)
    }

    /// `pqBuildErrorMessage3`, `fe-protocol3.c:1031` — what
    /// `PQresultErrorMessage` returns, trailing newline included, with the
    /// `LINE n:` and caret lines of the syntax cursor whenever there is a
    /// query to draw them over and the verbosity is not terse.
    #[must_use]
    // `valf` / `vall` below are upstream's own names for the source file and
    // source line fields (`fe-protocol3.c:1174`).
    #[allow(clippy::similar_names)]
    pub fn message(
        &self,
        status: ExecStatus,
        verbosity: Verbosity,
        show_context: ContextVisibility,
    ) -> Vec<u8> {
        let mut msg = Vec::new();
        let mut verbosity = verbosity;

        if let Some(val) = self.field(diag::SEVERITY) {
            msg.extend_from_slice(val);
            msg.extend_from_slice(b":  ");
        }

        if verbosity == Verbosity::Sqlstate {
            // fe-protocol3.c:1063 — SQLSTATE and nothing else, or fall back.
            if let Some(val) = self.field(diag::SQLSTATE) {
                msg.extend_from_slice(val);
                msg.push(b'\n');
                return msg;
            }
            verbosity = Verbosity::Terse;
        }

        if verbosity == Verbosity::Verbose
            && let Some(val) = self.field(diag::SQLSTATE)
        {
            msg.extend_from_slice(val);
            msg.extend_from_slice(b": ");
        }
        if let Some(val) = self.field(diag::MESSAGE_PRIMARY) {
            msg.extend_from_slice(val);
        }
        // fe-protocol3.c:1089-:1126 — a position becomes a cursor over the
        // query it points into when there is one and the verbosity is not
        // terse, and " at character %s" text otherwise. A statement position
        // points into the command sent (`res->errQuery`), an internal one into
        // `PG_DIAG_INTERNAL_QUERY`.
        let mut cursor: Option<(&[u8], i32)> = None;
        let (position, query) = match self.field(diag::STATEMENT_POSITION) {
            Some(val) => (Some(val), self.err_query()),
            None => (
                self.field(diag::INTERNAL_POSITION),
                self.field(diag::INTERNAL_QUERY),
            ),
        };
        if let Some(val) = position {
            match query {
                Some(query) if verbosity != Verbosity::Terse => cursor = Some((query, atoi(val))),
                _ => {
                    msg.extend_from_slice(b" at character ");
                    msg.extend_from_slice(val);
                }
            }
        }
        msg.push(b'\n');

        if verbosity != Verbosity::Terse {
            // fe-protocol3.c:1129.
            if let Some((query, querypos)) = cursor
                && querypos > 0
            {
                report_error_position(&mut msg, query, querypos, self.client_encoding);
            }
            for (code, label) in [
                (diag::MESSAGE_DETAIL, &b"DETAIL:  "[..]),
                (diag::MESSAGE_HINT, &b"HINT:  "[..]),
                (diag::INTERNAL_QUERY, &b"QUERY:  "[..]),
            ] {
                if let Some(val) = self.field(code) {
                    msg.extend_from_slice(label);
                    msg.extend_from_slice(val);
                    msg.push(b'\n');
                }
            }
            if (show_context == ContextVisibility::Always
                || (show_context == ContextVisibility::Errors && status == ExecStatus::FatalError))
                && let Some(val) = self.field(diag::CONTEXT)
            {
                msg.extend_from_slice(b"CONTEXT:  ");
                msg.extend_from_slice(val);
                msg.push(b'\n');
            }
        }

        if verbosity == Verbosity::Verbose {
            for (code, label) in [
                (diag::SCHEMA_NAME, &b"SCHEMA NAME:  "[..]),
                (diag::TABLE_NAME, &b"TABLE NAME:  "[..]),
                (diag::COLUMN_NAME, &b"COLUMN NAME:  "[..]),
                (diag::DATATYPE_NAME, &b"DATATYPE NAME:  "[..]),
                (diag::CONSTRAINT_NAME, &b"CONSTRAINT NAME:  "[..]),
            ] {
                if let Some(val) = self.field(code) {
                    msg.extend_from_slice(label);
                    msg.extend_from_slice(val);
                    msg.push(b'\n');
                }
            }

            // fe-protocol3.c:1174 — LOCATION, from up to three fields. The
            // names are upstream's `valf` (file) and `vall` (line).
            let valf = self.field(diag::SOURCE_FILE);
            let vall = self.field(diag::SOURCE_LINE);
            let val = self.field(diag::SOURCE_FUNCTION);
            if val.is_some() || valf.is_some() || vall.is_some() {
                msg.extend_from_slice(b"LOCATION:  ");
                if let Some(val) = val {
                    msg.extend_from_slice(val);
                    msg.extend_from_slice(b", ");
                }
                if let (Some(valf), Some(vall)) = (valf, vall) {
                    msg.extend_from_slice(valf);
                    msg.push(b':');
                    msg.extend_from_slice(vall);
                }
                msg.push(b'\n');
            }
        }

        msg
    }
}

/// C's `atoi` over a position field: leading white space, an optional sign,
/// then digits up to the first non-digit. The server sends a plain positive
/// decimal; anything past `i32` saturates where C's behaviour is undefined.
fn atoi(s: &[u8]) -> i32 {
    let mut rest = s;
    while let [c, tail @ ..] = rest
        && matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
    {
        rest = tail;
    }
    let negative = rest.first() == Some(&b'-');
    if matches!(rest.first(), Some(b'-' | b'+')) {
        rest = &rest[1..];
    }
    let mut n: i32 = 0;
    for &c in rest.iter().take_while(|c| c.is_ascii_digit()) {
        let digit = i32::from(c - b'0');
        n = if negative {
            n.saturating_mul(10).saturating_sub(digit)
        } else {
            n.saturating_mul(10).saturating_add(digit)
        };
    }
    n
}

/// `DISPLAY_SIZE`, `fe-protocol3.c:1204`: screen width limit, in columns.
const DISPLAY_SIZE: usize = 60;
/// `MIN_RIGHT_CUT`, `fe-protocol3.c:1205`: how far to try to keep the cursor
/// from the end of the line.
const MIN_RIGHT_CUT: usize = 10;

/// `reportErrorPosition`, `fe-protocol3.c:1202`: append the `LINE n: …` line
/// holding the 1-based character position `loc` of `query`, and under it a
/// caret at that character's screen column. A position past the end of the
/// query appends nothing.
///
/// Characters are measured in `encoding`: each one's byte length is
/// `PQmblenBounded` and its width `pg_encoding_dsplen`, a control character
/// counting as one column. Tabs are shown as spaces. A line wider than
/// [`DISPLAY_SIZE`] columns is cut to fit, on the right first and on the left
/// if that is not enough, with `...` where it was cut.
fn report_error_position(msg: &mut Vec<u8>, query: &[u8], loc: i32, encoding: Encoding) {
    // :1223 — 1-based to 0-based; nothing to draw before the start.
    let Ok(loc) = usize::try_from(loc.saturating_sub(1)) else {
        return;
    };
    // :1228 — a writable copy of the query as C sees it: up to its NUL.
    let end = query.iter().position(|&c| c == 0).unwrap_or(query.len());
    let mut wquery = query[..end].to_vec();

    // :1271 — a single-byte encoding needs no width lookups.
    let mb_encoding = encoding.max_length() != 1;

    // :1282-:1344 — qidx[] and scridx[] hold each character's byte offset
    // and starting screen column, filled as far as iend.
    let mut qidx: Vec<usize> = Vec::new();
    let mut scridx: Vec<usize> = Vec::new();
    let mut qoffset = 0;
    let mut scroffset = 0;
    let mut loc_line = 1;
    let mut ibeg = 0;
    let mut iend = None;
    let mut cno = 0;
    while qoffset < wquery.len() {
        let ch = wquery[qoffset];
        qidx.push(qoffset);
        scridx.push(scroffset);

        if ch == b'\t' {
            wquery[qoffset] = b' ';
        } else if ch == b'\r' || ch == b'\n' {
            // :1307 — each \r or \n is a line, except \r\n together.
            if cno < loc {
                if ch == b'\r' || cno == 0 || wquery[qidx[cno - 1]] != b'\r' {
                    loc_line += 1;
                }
                ibeg = cno + 1;
            } else {
                iend = Some(cno);
                break;
            }
        }

        if mb_encoding {
            // :1333 — any non-tab control character is one column wide.
            let w = encoding.dsplen(&wquery[qoffset..]);
            scroffset += usize::try_from(w).ok().filter(|&w| w > 0).unwrap_or(1);
            qoffset += encoding.mblen_bounded(&wquery[qoffset..]);
        } else {
            scroffset += 1;
            qoffset += 1;
        }
        cno += 1;
    }
    // :1347 — no end of line after loc: the line runs to the end.
    let mut iend = iend.unwrap_or_else(|| {
        qidx.push(qoffset);
        scridx.push(scroffset);
        cno
    });

    // :1355 — only if loc is within the query.
    if loc > cno {
        return;
    }
    let mut beg_trunc = false;
    let mut end_trunc = false;
    if scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
        if scridx[ibeg] + DISPLAY_SIZE >= scridx[loc] + MIN_RIGHT_CUT {
            // :1367 — cutting on the right is enough.
            while scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
                iend -= 1;
            }
            end_trunc = true;
        } else {
            // :1376 — cut on the right, short of the cursor, then on the
            // left if still too long.
            while scridx[loc] + MIN_RIGHT_CUT < scridx[iend] {
                iend -= 1;
                end_trunc = true;
            }
            while scridx[iend] - scridx[ibeg] > DISPLAY_SIZE {
                ibeg += 1;
                beg_trunc = true;
            }
        }
    }

    // :1395 — the LINE line, measuring the prefix's own width as it goes.
    let start = msg.len();
    msg.extend_from_slice(format!("LINE {loc_line}: ").as_bytes());
    if beg_trunc {
        msg.extend_from_slice(b"...");
    }
    let mut prefix_width = 0;
    let mut i = start;
    while i < msg.len() {
        let w = encoding.dsplen(&msg[i..]);
        prefix_width += usize::try_from(w).ok().filter(|&w| w > 0).unwrap_or(1);
        i += encoding.mblen_bounded(&msg[i..]).max(1);
    }
    msg.extend_from_slice(&wquery[qidx[ibeg]..qidx[iend]]);
    if end_trunc {
        msg.extend_from_slice(b"...");
    }
    msg.push(b'\n');

    // :1421 — the cursor line.
    let column = prefix_width + scridx[loc] - scridx[ibeg];
    msg.resize(msg.len() + column, b' ');
    msg.extend_from_slice(b"^\n");
}

/// One column of a `RowDescription` — `PGresAttDesc`, `libpq-fe.h:305`, as
/// filled by `getRowDescriptions` (`fe-protocol3.c:519`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDescription {
    pub name: Vec<u8>,
    pub tableid: u32,
    pub columnid: i16,
    pub typid: u32,
    pub typlen: i16,
    pub atttypmod: i32,
    pub format: i16,
}

/// One `PGresult`: what `PQgetResult` hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryResult {
    status: ExecStatus,
    fields: Vec<FieldDescription>,
    rows: Vec<Vec<Option<Vec<u8>>>>,
    command_status: Vec<u8>,
    error: Option<ResultError>,
    /// `paramDescs`: the parameter types a ParameterDescription reported
    /// (`getParamDescriptions`, `fe-protocol3.c:690`).
    params: Vec<u32>,
    /// `binary`: a COPY response's overall format (`getCopyStart`,
    /// `fe-protocol3.c:1719`), or whether a RowDescription's columns are all
    /// binary ([`QueryResult::set_fields`]).
    binary: bool,
}

impl QueryResult {
    /// `PQmakeEmptyPGresult`, `fe-exec.c:160`.
    #[must_use]
    pub fn new(status: ExecStatus) -> Self {
        Self {
            status,
            fields: Vec::new(),
            rows: Vec::new(),
            command_status: Vec::new(),
            error: None,
            params: Vec::new(),
            binary: false,
        }
    }

    #[must_use]
    pub fn with_error(status: ExecStatus, error: ResultError) -> Self {
        Self {
            status,
            fields: Vec::new(),
            rows: Vec::new(),
            command_status: Vec::new(),
            error: Some(error),
            params: Vec::new(),
            binary: false,
        }
    }

    /// The `PGRES_COPY_*` result `getCopyStart` builds (`fe-protocol3.c:1707`):
    /// one column per format code, with nothing but the format filled in —
    /// `attDescs` are zeroed there (`:1732`), so every name is empty here.
    #[must_use]
    pub fn copy(status: ExecStatus, format: &crate::message::CopyFormat) -> Self {
        let mut result = Self::new(status);
        result.binary = format.overall != 0;
        result.fields = format
            .column_formats
            .iter()
            .map(|&format| FieldDescription {
                name: Vec::new(),
                tableid: 0,
                columnid: 0,
                typid: 0,
                typlen: 0,
                atttypmod: 0,
                format,
            })
            .collect();
        result
    }

    /// `PQresultStatus`, `fe-exec.c:3442`.
    #[must_use]
    pub fn status(&self) -> ExecStatus {
        self.status
    }

    /// `PQnfields`, `fe-exec.c:3520`.
    #[must_use]
    pub fn nfields(&self) -> usize {
        self.fields.len()
    }

    /// `PQntuples`, `fe-exec.c:3512`.
    #[must_use]
    pub fn ntuples(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn fields(&self) -> &[FieldDescription] {
        &self.fields
    }

    /// A RowDescription's columns, and with them `binary`: "true only if
    /// ALL columns are binary", and false with no columns
    /// (`getRowDescriptions`, `fe-protocol3.c:571`-`:572`, `:619`-`:620`).
    pub(crate) fn set_fields(&mut self, fields: Vec<FieldDescription>) {
        self.binary = !fields.is_empty() && fields.iter().all(|field| field.format == 1);
        self.fields = fields;
    }

    pub(crate) fn push_row(&mut self, row: Vec<Option<Vec<u8>>>) {
        self.rows.push(row);
    }

    /// `PQftype`, `fe-exec.c:3750` — the column's type OID. Out of range is
    /// `InvalidOid` there and `None` here.
    #[must_use]
    pub fn ftype(&self, column: usize) -> Option<u32> {
        self.fields.get(column).map(|f| f.typid)
    }

    /// `PQnparams`, `fe-exec.c:3946` — how many parameters a described
    /// prepared statement takes.
    #[must_use]
    pub fn nparams(&self) -> usize {
        self.params.len()
    }

    /// `PQparamtype`, `fe-exec.c:3957`.
    #[must_use]
    pub fn paramtype(&self, param: usize) -> Option<u32> {
        self.params.get(param).copied()
    }

    pub(crate) fn set_params(&mut self, params: Vec<u32>) {
        self.params = params;
    }

    /// `PQbinaryTuples`, `fe-exec.c:3528`: whether a COPY result's data is
    /// binary, or every column of a row result is.
    #[must_use]
    pub fn binary_tuples(&self) -> bool {
        self.binary
    }

    /// `PQfformat`, `fe-exec.c:3739` — the column's format code, 0 for text
    /// and 1 for binary.
    #[must_use]
    pub fn fformat(&self, column: usize) -> Option<i16> {
        self.fields.get(column).map(|f| f.format)
    }

    /// `PQfname`, `fe-exec.c:3598`.
    #[must_use]
    pub fn fname(&self, column: usize) -> Option<&[u8]> {
        self.fields.get(column).map(|f| f.name.as_slice())
    }

    /// `PQgetvalue`, `fe-exec.c:3907`. A NULL field is an empty string there
    /// and `None` here; `PQgetisnull` is the only way to tell them apart in C,
    /// so the distinction is kept rather than flattened.
    #[must_use]
    pub fn value(&self, row: usize, column: usize) -> Option<&[u8]> {
        self.rows.get(row)?.get(column)?.as_ref().map(Vec::as_slice)
    }

    /// `PQgetisnull`, `fe-exec.c:3932`.
    #[must_use]
    pub fn is_null(&self, row: usize, column: usize) -> bool {
        self.rows
            .get(row)
            .and_then(|r| r.get(column))
            .is_none_or(Option::is_none)
    }

    /// `PQcmdStatus`, `fe-exec.c:3783` — the CommandComplete tag.
    #[must_use]
    pub fn command_status(&self) -> &[u8] {
        &self.command_status
    }

    pub(crate) fn set_command_status(&mut self, status: Vec<u8>) {
        self.command_status = status;
    }

    /// The broken-down error fields, if this result is an error or notice.
    #[must_use]
    pub fn error(&self) -> Option<&ResultError> {
        self.error.as_ref()
    }

    /// `PQresultErrorMessage`, `fe-exec.c:3458`.
    #[must_use]
    pub fn error_message(&self) -> Vec<u8> {
        match &self.error {
            Some(error) => error.message(
                self.status,
                Verbosity::default(),
                ContextVisibility::default(),
            ),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_error() -> ResultError {
        // The field order a PostgreSQL 18 backend sends for a syntax error.
        ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::SEVERITY_NONLOCALIZED, b"ERROR".to_vec()),
            (diag::SQLSTATE, b"42601".to_vec()),
            (
                diag::MESSAGE_PRIMARY,
                b"syntax error at or near \"selct\"".to_vec(),
            ),
            (diag::STATEMENT_POSITION, b"1".to_vec()),
            (diag::SOURCE_FILE, b"scan.l".to_vec()),
            (diag::SOURCE_LINE, b"1244".to_vec()),
            (diag::SOURCE_FUNCTION, b"scanner_yyerror".to_vec()),
        ])
    }

    /// The default rendering: severity, primary message, and — with no
    /// query kept to draw a cursor over (`res->errQuery` NULL) — the
    /// position as text (`fe-protocol3.c:1102`).
    #[test]
    fn the_default_rendering_is_severity_message_and_position() {
        assert_eq!(
            server_error().message(
                ExecStatus::FatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ),
            b"ERROR:  syntax error at or near \"selct\" at character 1\n".to_vec()
        );
    }

    /// With the query kept, the position is a cursor below the primary line
    /// at every verbosity but terse, which keeps the text
    /// (`fe-protocol3.c:1092`-`:1103`); the cursor comes before DETAIL and
    /// HINT (`:1129`).
    #[test]
    fn a_statement_position_draws_a_cursor_over_the_query_kept() {
        let error = server_error()
            .with_err_query(b"selct 1".to_vec())
            .with_client_encoding(Encoding::Utf8);
        let render = |verbosity| {
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                verbosity,
                ContextVisibility::Errors,
            ))
            .unwrap()
        };
        assert_eq!(
            render(Verbosity::Default),
            "ERROR:  syntax error at or near \"selct\"\nLINE 1: selct 1\n        ^\n"
        );
        assert_eq!(
            render(Verbosity::Verbose),
            "ERROR:  42601: syntax error at or near \"selct\"\nLINE 1: selct 1\n        ^\n\
             LOCATION:  scanner_yyerror, scan.l:1244\n"
        );
        assert_eq!(
            render(Verbosity::Terse),
            "ERROR:  syntax error at or near \"selct\" at character 1\n"
        );
        assert_eq!(render(Verbosity::Sqlstate), "ERROR:  42601\n");
    }

    /// An error raised inside PL/pgSQL: the server sends the position as `p`
    /// with the statement as `q`, and upstream draws the cursor over that
    /// (`fe-protocol3.c:1112`) — `src/test/regress/expected/plpgsql.out:1763`
    /// -`:1768`, where the HINT follows the cursor and QUERY follows the HINT.
    #[test]
    fn an_internal_position_draws_its_cursor_over_the_internal_query() {
        let error = ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::SQLSTATE, b"42883".to_vec()),
            (
                diag::MESSAGE_PRIMARY,
                b"operator does not exist: point + integer".to_vec(),
            ),
            (
                diag::MESSAGE_HINT,
                b"No operator matches the given name and argument types. \
                  You might need to add explicit type casts."
                    .to_vec(),
            ),
            (diag::INTERNAL_POSITION, b"3".to_vec()),
            (diag::INTERNAL_QUERY, b"x + 1".to_vec()),
            (
                diag::CONTEXT,
                b"PL/pgSQL function f1(anyelement) line 3 at RETURN".to_vec(),
            ),
        ]);
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  operator does not exist: point + integer\n\
             LINE 1: x + 1\n          ^\n\
             HINT:  No operator matches the given name and argument types. \
             You might need to add explicit type casts.\n\
             QUERY:  x + 1\n\
             CONTEXT:  PL/pgSQL function f1(anyelement) line 3 at RETURN\n"
        );

        // PQERRORS_TERSE has no cursor, so there the position goes in the
        // text (`fe-protocol3.c:1121`).
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Terse,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  operator does not exist: point + integer at character 3\n"
        );

        // With no internal query there is nothing to draw a cursor over, so
        // the text is emitted whatever the verbosity.
        let without_query = ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::MESSAGE_PRIMARY, b"boom".to_vec()),
            (diag::INTERNAL_POSITION, b"7".to_vec()),
        ]);
        assert_eq!(
            String::from_utf8(without_query.message(
                ExecStatus::FatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  boom at character 7\n"
        );
    }

    /// `reportErrorPosition` alone, as a string.
    fn cursor(query: &str, loc: i32, encoding: Encoding) -> String {
        let mut msg = Vec::new();
        report_error_position(&mut msg, query.as_bytes(), loc, encoding);
        String::from_utf8(msg).unwrap()
    }

    /// The cursor lines of upstream's own regression output: each query,
    /// the `LINE` and caret lines C libpq drew for it, and the position
    /// those two lines put the caret at. Short lines, a line cut on the
    /// right, on the left, and on both sides, the second and third line of
    /// a statement, and a position one past the end of the input.
    #[test]
    fn the_cursor_is_upstreams_regression_output() {
        let cases = [
            // aggregates.out:2669-:2672 — cut on the right only.
            (
                "select rank('fred') within group (order by x) from generate_series(1,5) x;",
                13,
                "LINE 1: select rank('fred') within group (order by x) from generate_...\n\
                 \x20                   ^\n",
            ),
            // aggregates.out:2673-:2677 — the first of two lines, cut on
            // both sides.
            (
                "select rank('adam'::text collate \"C\") within group (order by x collate \"POSIX\")\n  \
                 from (values ('fred'),('jim')) v(x);",
                64,
                "LINE 1: ...adam'::text collate \"C\") within group (order by x collate \"P...\n\
                 \x20                                                            ^\n",
            ),
            // alter_table.out:1767-:1770 — cut on the left only.
            (
                "alter table renameColumn add column y int check (x > 0) not enforced enforced;",
                70,
                "LINE 1: ...Column add column y int check (x > 0) not enforced enforced;\n\
                 \x20                                                             ^\n",
            ),
            // alter_table.out:4033-:4036 — cut on both sides.
            (
                "ALTER TABLE list_parted ATTACH PARTITION fail_part FOR VALUES FROM (1) TO (10);",
                63,
                "LINE 1: ...list_parted ATTACH PARTITION fail_part FOR VALUES FROM (1) T...\n\
                 \x20                                                            ^\n",
            ),
            // create_function_sql.out:301-:306 — the third line.
            (
                "CREATE FUNCTION functest_S_xx(x date) RETURNS boolean\n    LANGUAGE SQL\n    \
                 RETURN x > 1;",
                85,
                "LINE 3:     RETURN x > 1;\n\x20                    ^\n",
            ),
            // boolean.out:299-:303 — the second line.
            (
                "INSERT INTO BOOLTBL2 (f1)\n   VALUES (bool 'XXX');",
                43,
                "LINE 2:    VALUES (bool 'XXX');\n\x20                       ^\n",
            ),
            // psql.out:4953-:4956 — "syntax error at end of input" points
            // one past the last character.
            (
                "SELECT 4 AS ",
                13,
                "LINE 1: SELECT 4 AS \n\x20                   ^\n",
            ),
        ];
        for (query, loc, expected) in cases {
            assert_eq!(
                cursor(query, loc, Encoding::SqlAscii),
                expected,
                "{query:?}"
            );
            assert_eq!(cursor(query, loc, Encoding::Utf8), expected, "{query:?}");
        }
    }

    /// Line ends: `\r\n` is one line break, a lone `\r` or `\n` is one each
    /// (`fe-protocol3.c:1307`); a tab is shown as one space; a position
    /// before the start or two past the end draws nothing.
    #[test]
    fn line_ends_tabs_and_out_of_range_positions() {
        let ascii = Encoding::SqlAscii;
        assert_eq!(
            cursor("select 1,\r\n\tfoo", 13, ascii),
            "LINE 2:  foo\n         ^\n"
        );
        assert_eq!(
            cursor("select 1,\r\rfoo", 12, ascii),
            "LINE 3: foo\n        ^\n"
        );
        assert_eq!(
            cursor("select 1,\n\nfoo", 12, ascii),
            "LINE 3: foo\n        ^\n"
        );
        assert_eq!(
            cursor("a\nb", 2, ascii),
            "LINE 1: a\n         ^\n",
            "a position on the line end itself belongs to the line it ends"
        );
        assert_eq!(cursor("selct 1", 0, ascii), "");
        assert_eq!(cursor("selct 1", 9, ascii), "");
        assert_eq!(
            cursor("selct 1", 8, ascii),
            "LINE 1: selct 1\n               ^\n"
        );
    }

    /// In a multibyte encoding the position counts characters, and the
    /// caret moves by each one's display width: two columns for a wide
    /// character, and one for a combining character, since any width of
    /// zero or less counts as one (`fe-protocol3.c:1333`). A single-byte
    /// encoding counts bytes, one column each.
    #[test]
    fn a_multibyte_query_is_measured_in_characters_and_columns() {
        let query = "select '\u{4e16}\u{754c}e\u{301}', foo";
        // 8 characters, two wide ones, `e`, the accent, `', `: `foo` is the
        // 16th character and starts in column 8 + 4 + 1 + 1 + 3.
        assert_eq!(
            cursor(query, 16, Encoding::Utf8),
            format!("LINE 1: {query}\n{}^\n", " ".repeat(8 + 8 + 4 + 1 + 1 + 3))
        );
        let latin1 = b"select '\xe9', foo";
        let mut msg = Vec::new();
        report_error_position(&mut msg, latin1, 13, Encoding::Latin1);
        let mut expected = b"LINE 1: ".to_vec();
        expected.extend_from_slice(latin1);
        expected.extend_from_slice(b"\n                    ^\n");
        assert_eq!(msg, expected);
    }

    /// `atoi`: what the position field is read with.
    #[test]
    fn atoi_reads_like_c() {
        assert_eq!(atoi(b"42"), 42);
        assert_eq!(atoi(b" 7x"), 7);
        assert_eq!(atoi(b"-3"), -3);
        assert_eq!(atoi(b""), 0);
        assert_eq!(atoi(b"99999999999"), i32::MAX);
    }

    /// `PQERRORS_TERSE` drops DETAIL, HINT, QUERY and CONTEXT;
    /// `PQERRORS_SQLSTATE` prints the SQLSTATE and nothing else;
    /// `PQERRORS_VERBOSE` adds the SQLSTATE and the LOCATION line.
    #[test]
    fn every_verbosity_renders_its_own_fields() {
        let error = ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::SQLSTATE, b"23505".to_vec()),
            (
                diag::MESSAGE_PRIMARY,
                b"duplicate key value violates unique constraint \"t_pkey\"".to_vec(),
            ),
            (
                diag::MESSAGE_DETAIL,
                b"Key (i)=(1) already exists.".to_vec(),
            ),
            (diag::MESSAGE_HINT, b"Try something else.".to_vec()),
            (
                diag::CONTEXT,
                b"SQL statement \"insert into t values (1)\"".to_vec(),
            ),
            (diag::SCHEMA_NAME, b"public".to_vec()),
            (diag::TABLE_NAME, b"t".to_vec()),
            (diag::CONSTRAINT_NAME, b"t_pkey".to_vec()),
            (diag::SOURCE_FILE, b"nbtinsert.c".to_vec()),
            (diag::SOURCE_LINE, b"671".to_vec()),
            (diag::SOURCE_FUNCTION, b"_bt_check_unique".to_vec()),
        ]);

        assert_eq!(
            error.message(
                ExecStatus::FatalError,
                Verbosity::Terse,
                ContextVisibility::Errors
            ),
            b"ERROR:  duplicate key value violates unique constraint \"t_pkey\"\n".to_vec()
        );
        assert_eq!(
            error.message(
                ExecStatus::FatalError,
                Verbosity::Sqlstate,
                ContextVisibility::Errors
            ),
            b"ERROR:  23505\n".to_vec()
        );
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  duplicate key value violates unique constraint \"t_pkey\"\n\
             DETAIL:  Key (i)=(1) already exists.\n\
             HINT:  Try something else.\n\
             CONTEXT:  SQL statement \"insert into t values (1)\"\n"
        );
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Verbose,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  23505: duplicate key value violates unique constraint \"t_pkey\"\n\
             DETAIL:  Key (i)=(1) already exists.\n\
             HINT:  Try something else.\n\
             CONTEXT:  SQL statement \"insert into t values (1)\"\n\
             SCHEMA NAME:  public\n\
             TABLE NAME:  t\n\
             CONSTRAINT NAME:  t_pkey\n\
             LOCATION:  _bt_check_unique, nbtinsert.c:671\n"
        );
    }

    /// `PQSHOW_CONTEXT_ERRORS` shows CONTEXT for an error and not for a
    /// notice; `PQSHOW_CONTEXT_ALWAYS` shows it for both
    /// (`fe-protocol3.c:1141`).
    #[test]
    fn context_follows_the_show_context_setting() {
        let notice = ResultError::new(vec![
            (diag::SEVERITY, b"NOTICE".to_vec()),
            (
                diag::MESSAGE_PRIMARY,
                b"table \"t\" does not exist, skipping".to_vec(),
            ),
            (diag::CONTEXT, b"PL/pgSQL function f() line 1".to_vec()),
        ]);
        assert_eq!(
            notice.message(
                ExecStatus::NonfatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ),
            b"NOTICE:  table \"t\" does not exist, skipping\n".to_vec()
        );
        assert!(
            notice
                .message(
                    ExecStatus::NonfatalError,
                    Verbosity::Default,
                    ContextVisibility::Always
                )
                .ends_with(b"CONTEXT:  PL/pgSQL function f() line 1\n")
        );
        assert_eq!(
            notice.message(
                ExecStatus::NonfatalError,
                Verbosity::Default,
                ContextVisibility::Never
            ),
            b"NOTICE:  table \"t\" does not exist, skipping\n".to_vec()
        );
    }

    /// A result with no error fields at all renders as nothing, and the
    /// accessors agree with `PQntuples` / `PQnfields` on an empty result.
    #[test]
    fn an_empty_result_has_no_rows_fields_or_error() {
        let res = QueryResult::new(ExecStatus::CommandOk);
        assert_eq!(res.ntuples(), 0);
        assert_eq!(res.nfields(), 0);
        assert_eq!(res.error_message(), Vec::<u8>::new());
        assert!(res.error().is_none());
        assert_eq!(res.command_status(), b"");
        assert_eq!(res.status().as_str(), "PGRES_COMMAND_OK");
    }

    /// A NULL field is `None`, an empty string is `Some(b"")` — the
    /// distinction `PQgetisnull` exists for.
    #[test]
    fn a_null_field_is_not_an_empty_string() {
        let mut res = QueryResult::new(ExecStatus::TuplesOk);
        res.set_fields(vec![FieldDescription {
            name: b"x".to_vec(),
            tableid: 0,
            columnid: 0,
            typid: 25,
            typlen: -1,
            atttypmod: -1,
            format: 0,
        }]);
        res.push_row(vec![None]);
        res.push_row(vec![Some(Vec::new())]);
        assert!(res.is_null(0, 0));
        assert!(!res.is_null(1, 0));
        assert_eq!(res.value(0, 0), None);
        assert_eq!(res.value(1, 0), Some(&b""[..]));
        assert_eq!(res.fname(0), Some(&b"x"[..]));
        assert_eq!(res.ntuples(), 2);
    }

    /// `getRowDescriptions`, `fe-protocol3.c:571`-`:572`, `:619`-`:620`:
    /// `PQbinaryTuples` is 1 only for a row result whose every column is
    /// binary.
    #[test]
    fn binary_tuples_is_true_only_when_every_column_is_binary() {
        let column = |format| FieldDescription {
            name: b"x".to_vec(),
            tableid: 0,
            columnid: 0,
            typid: 23,
            typlen: 4,
            atttypmod: -1,
            format,
        };
        let mut res = QueryResult::new(ExecStatus::TuplesOk);
        res.set_fields(vec![column(1), column(1)]);
        assert!(res.binary_tuples());
        res.set_fields(vec![column(1), column(0)]);
        assert!(!res.binary_tuples());
        res.set_fields(Vec::new());
        assert!(!res.binary_tuples());
    }

    /// Every `pgresStatus` spelling, since rpsql prints them.
    #[test]
    fn the_status_names_are_upstreams() {
        assert_eq!(ExecStatus::EmptyQuery.as_str(), "PGRES_EMPTY_QUERY");
        assert_eq!(ExecStatus::TuplesOk.as_str(), "PGRES_TUPLES_OK");
        assert_eq!(ExecStatus::FatalError.as_str(), "PGRES_FATAL_ERROR");
        assert_eq!(ExecStatus::NonfatalError.as_str(), "PGRES_NONFATAL_ERROR");
    }
}
