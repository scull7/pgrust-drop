//! `PGresult` as data: the status, the row description, the rows, and the
//! broken-down error fields.
//!
//! Ported from `src/interfaces/libpq/fe-exec.c` (the `PGresult` struct and its
//! accessors) and `fe-protocol3.c` (`pqGetErrorNotice3`, `:899`, which fills
//! the error fields, and `pqBuildErrorMessage3`, `:1031`, which renders them).
//! Everything here is a calculation over bytes; nothing touches a socket.

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
/// the server sent them (`pqSaveMessageField`, `fe-exec.c:1066`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultError {
    fields: Vec<(u8, Vec<u8>)>,
}

impl ResultError {
    #[must_use]
    pub fn new(fields: Vec<(u8, Vec<u8>)>) -> Self {
        Self { fields }
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
    /// `PQresultErrorMessage` returns, trailing newline included.
    ///
    /// The syntax-cursor display (`reportErrorPosition`, `fe-protocol3.c:1202`)
    /// is not ported: this result never carries the query text, which is the
    /// case upstream renders as " at character %s" (`:1102`). See
    /// `docs/divergences.md`.
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
        // fe-protocol3.c:1089 — a statement position always renders as text
        // here, because this result never carries the query it would point at
        // (`res->errQuery`); that is the divergence `docs/divergences.md`
        // records.
        if let Some(val) = self.field(diag::STATEMENT_POSITION) {
            msg.extend_from_slice(b" at character ");
            msg.extend_from_slice(val);
        } else if let Some(val) = self.field(diag::INTERNAL_POSITION) {
            // fe-protocol3.c:1112 — an *internal* position has its query right
            // here in `PG_DIAG_INTERNAL_QUERY`, so upstream draws a cursor
            // over it and emits no " at character" text at all. The cursor is
            // not ported; suppressing the text is, because otherwise the
            // primary line differs from C libpq's for every error inside a
            // PL/pgSQL EXECUTE. The query itself still reaches the reader on
            // the "QUERY:  " line below.
            if verbosity == Verbosity::Terse || self.field(diag::INTERNAL_QUERY).is_none() {
                msg.extend_from_slice(b" at character ");
                msg.extend_from_slice(val);
            }
        }
        msg.push(b'\n');

        if verbosity != Verbosity::Terse {
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
        }
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

    pub(crate) fn set_fields(&mut self, fields: Vec<FieldDescription>) {
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

    /// The default rendering: severity, primary message, the position as text.
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

    /// An error raised inside PL/pgSQL's EXECUTE: the server sends the
    /// position as `p` together with the statement as `q`, and upstream then
    /// puts the position on a cursor display rather than in the primary line
    /// (`fe-protocol3.c:1112`). The cursor is not ported, so the primary line
    /// is all this must match — and it must match, or every such error reads
    /// differently here than through C libpq.
    #[test]
    fn an_internal_position_stays_out_of_the_primary_line() {
        let error = ResultError::new(vec![
            (diag::SEVERITY, b"ERROR".to_vec()),
            (diag::SQLSTATE, b"42601".to_vec()),
            (
                diag::MESSAGE_PRIMARY,
                b"syntax error at or near \"selct\"".to_vec(),
            ),
            (diag::INTERNAL_POSITION, b"1".to_vec()),
            (diag::INTERNAL_QUERY, b"selct 1".to_vec()),
        ]);
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Default,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  syntax error at or near \"selct\"\nQUERY:  selct 1\n"
        );

        // PQERRORS_TERSE has no QUERY line to carry the statement, so there
        // the position does go in the text (`fe-protocol3.c:1121`).
        assert_eq!(
            String::from_utf8(error.message(
                ExecStatus::FatalError,
                Verbosity::Terse,
                ContextVisibility::Errors
            ))
            .unwrap(),
            "ERROR:  syntax error at or near \"selct\" at character 1\n"
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

    /// Every `pgresStatus` spelling, since rpsql prints them.
    #[test]
    fn the_status_names_are_upstreams() {
        assert_eq!(ExecStatus::EmptyQuery.as_str(), "PGRES_EMPTY_QUERY");
        assert_eq!(ExecStatus::TuplesOk.as_str(), "PGRES_TUPLES_OK");
        assert_eq!(ExecStatus::FatalError.as_str(), "PGRES_FATAL_ERROR");
        assert_eq!(ExecStatus::NonfatalError.as_str(), "PGRES_NONFATAL_ERROR");
    }
}
