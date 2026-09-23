//! The extended-query commands as plans: which messages `PQsendQueryParams`,
//! `PQsendQueryPrepared`, `PQsendPrepare` and `PQsendTypedCommand` put on the
//! wire, and the argument checks they make first.
//!
//! Ported from `src/interfaces/libpq/fe-exec.c` — `PQsendQueryParams`
//! (`:1509`), `PQsendPrepare` (`:1553`), `PQsendQueryPrepared` (`:1650`),
//! `PQsendQueryGuts` (`:1774`) and `PQsendTypedCommand` (`:2606`). Every
//! function here is a calculation from arguments to [`Frontend`] messages;
//! sending them and folding the replies is [`crate::Connection`]'s job.
//!
//! Each plan ends with the command's own Sync, as it is sent outside pipeline
//! mode (`fe-exec.c:1610`, `:1903`, `:2628`); in pipeline mode the Sync is
//! the caller's to send, and [`Plan::without_sync`] is the plan then.

use crate::message::{Frontend, Target};
use crate::pipeline::QueryClass;

/// `PQ_QUERY_PARAM_MAX_LIMIT`, `libpq-fe.h:507`: the most parameters a
/// Parse or Bind can carry, since both count them in two bytes.
pub const PQ_QUERY_PARAM_MAX_LIMIT: usize = 65535;

/// A parameter or result format code: `0` is text, `1` is binary
/// (`paramFormats` / `resultFormat` in `libpq-fe.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Text,
    Binary,
}

impl Format {
    /// The two-byte code on the wire.
    #[must_use]
    pub fn code(self) -> i16 {
        match self {
            Format::Text => 0,
            Format::Binary => 1,
        }
    }
}

/// The parameter arguments of `PQexecParams` and `PQexecPrepared`.
///
/// `values[i]` is parameter `$i+1`; `None` is SQL NULL. `formats` is the C
/// `paramFormats` array, and empty stands for its NULL pointer: every
/// parameter is text and the Bind carries no format codes
/// (`fe-exec.c:1830`). A text value is sent as the bytes given — C sends
/// `strlen(paramValues[i])` bytes (`fe-exec.c:1870`), which for a C string
/// is the same thing; a Rust slice holding a NUL is passed through whole and
/// left for the server to judge.
#[derive(Debug, Clone, Copy, Default)]
pub struct Params<'a> {
    pub values: &'a [Option<&'a [u8]>],
    pub formats: &'a [Format],
    pub result_format: Format,
}

impl<'a> Params<'a> {
    /// Text parameters and a text result: `PQexecParams(conn, command,
    /// n, types, values, NULL, NULL, 0)`.
    #[must_use]
    pub fn text(values: &'a [Option<&'a [u8]>]) -> Self {
        Self {
            values,
            formats: &[],
            result_format: Format::Text,
        }
    }
}

/// An argument `PQsendQueryParams` and its siblings refuse before anything
/// is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentError {
    /// `fe-exec.c:1527`, `:1573`, `:1667`.
    TooManyParameters,
    /// An array argument whose length is neither 0 (C's NULL pointer) nor
    /// the parameter count. C cannot report this — it reads `nParams`
    /// entries whatever the array holds — so this message is this port's
    /// own.
    LengthMismatch {
        array: &'static str,
        len: usize,
        nparams: usize,
    },
}

impl ArgumentError {
    /// The bytes libpq's error buffer would hold, without the newline
    /// `libpq_append_conn_error` adds.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            ArgumentError::TooManyParameters => {
                format!("number of parameters must be between 0 and {PQ_QUERY_PARAM_MAX_LIMIT}")
                    .into_bytes()
            }
            ArgumentError::LengthMismatch {
                array,
                len,
                nparams,
            } => format!("{array} has {len} entries for {nparams} parameters").into_bytes(),
        }
    }
}

impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for ArgumentError {}

/// What a command sends and how its replies are to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub messages: Vec<Frontend>,
    pub class: QueryClass,
}

impl Plan {
    /// The plan as pipeline mode sends it: "Add a Sync, unless in pipeline
    /// mode" (`fe-exec.c:1610`, `:1903`, `:2628`), so the command's own
    /// trailing Sync is dropped and `PQpipelineSync` sends one later.
    #[must_use]
    pub fn without_sync(mut self) -> Self {
        if self.messages.last() == Some(&Frontend::Sync) {
            self.messages.pop();
        }
        self
    }
}

/// `fe-exec.c:1527` — the parameter count must fit the two-byte counts.
fn check_count(nparams: usize) -> Result<(), ArgumentError> {
    if nparams > PQ_QUERY_PARAM_MAX_LIMIT {
        return Err(ArgumentError::TooManyParameters);
    }
    Ok(())
}

/// An array that is empty (C's NULL) or exactly one entry per parameter.
fn check_array(array: &'static str, len: usize, nparams: usize) -> Result<(), ArgumentError> {
    if len != 0 && len != nparams {
        return Err(ArgumentError::LengthMismatch {
            array,
            len,
            nparams,
        });
    }
    Ok(())
}

/// `PQsendQueryParams`, `fe-exec.c:1509`: Parse into the unnamed statement,
/// then Bind, Describe portal, Execute, Sync.
///
/// `param_types` is the C `paramTypes` array; empty stands for NULL and
/// leaves every type for the server to infer (`fe-exec.c:1804`).
///
/// # Errors
/// More than [`PQ_QUERY_PARAM_MAX_LIMIT`] parameters, or a `param_types` or
/// `formats` whose length is neither 0 nor the parameter count.
pub fn query_params(
    command: &[u8],
    param_types: &[u32],
    params: &Params<'_>,
) -> Result<Plan, ArgumentError> {
    check_count(params.values.len())?;
    check_array("paramTypes", param_types.len(), params.values.len())?;
    query_guts(Some((command, param_types)), b"", params)
}

/// `PQsendQueryPrepared`, `fe-exec.c:1650`: Bind the named statement, then
/// Describe portal, Execute, Sync — no Parse.
///
/// # Errors
/// More than [`PQ_QUERY_PARAM_MAX_LIMIT`] parameters, or a `formats` whose
/// length is neither 0 nor the parameter count.
pub fn query_prepared(statement: &[u8], params: &Params<'_>) -> Result<Plan, ArgumentError> {
    check_count(params.values.len())?;
    query_guts(None, statement, params)
}

/// `PQsendQueryGuts`, `fe-exec.c:1774`.
fn query_guts(
    parse: Option<(&[u8], &[u32])>,
    statement: &[u8],
    params: &Params<'_>,
) -> Result<Plan, ArgumentError> {
    let nparams = params.values.len();
    check_array("paramFormats", params.formats.len(), nparams)?;

    let mut messages = Vec::with_capacity(5);
    if let Some((command, param_types)) = parse {
        // fe-exec.c:1804 — the types only when there are parameters and a
        // types array; otherwise a count of zero.
        let param_types = if nparams > 0 {
            param_types.to_vec()
        } else {
            Vec::new()
        };
        messages.push(Frontend::Parse {
            statement: statement.to_vec(),
            query: command.to_vec(),
            param_types,
        });
    }

    // fe-exec.c:1830 — the format codes only when there are parameters and
    // a formats array.
    let param_formats = if nparams > 0 {
        params.formats.iter().map(|f| f.code()).collect()
    } else {
        Vec::new()
    };
    messages.push(Frontend::Bind {
        portal: Vec::new(),
        statement: statement.to_vec(),
        param_formats,
        params: params
            .values
            .iter()
            .map(|value| value.map(<[u8]>::to_vec))
            .collect(),
        // fe-exec.c:1883 — always exactly one result format code.
        result_formats: vec![params.result_format.code()],
    });
    messages.push(Frontend::Describe {
        target: Target::Portal,
        name: Vec::new(),
    });
    messages.push(Frontend::Execute {
        portal: Vec::new(),
        max_rows: 0,
    });
    messages.push(Frontend::Sync);
    Ok(Plan {
        messages,
        class: QueryClass::Extended,
    })
}

/// `PQsendPrepare`, `fe-exec.c:1553`: one Parse, then Sync.
///
/// # Errors
/// More than [`PQ_QUERY_PARAM_MAX_LIMIT`] parameter types.
pub fn prepare(statement: &[u8], query: &[u8], param_types: &[u32]) -> Result<Plan, ArgumentError> {
    check_count(param_types.len())?;
    Ok(Plan {
        messages: vec![
            Frontend::Parse {
                statement: statement.to_vec(),
                query: query.to_vec(),
                param_types: param_types.to_vec(),
            },
            Frontend::Sync,
        ],
        class: QueryClass::Prepare,
    })
}

/// The two commands `PQsendTypedCommand` sends (`fe-exec.c:2595`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedCommand {
    Describe,
    Close,
}

/// `PQsendTypedCommand`, `fe-exec.c:2606`: one Describe or Close of a
/// statement or a portal, then Sync. It cannot fail: C's one refusal
/// (`:2647`, an unrecognized message type) is unreachable through the typed
/// [`TypedCommand`].
#[must_use]
pub fn typed_command(command: TypedCommand, target: Target, name: &[u8]) -> Plan {
    let name = name.to_vec();
    let (message, class) = match command {
        TypedCommand::Describe => (Frontend::Describe { target, name }, QueryClass::Describe),
        TypedCommand::Close => (Frontend::Close { target, name }, QueryClass::Close),
    };
    Plan {
        messages: vec![message, Frontend::Sync],
        class,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole wire image of a plan.
    fn wire(plan: &Plan) -> Vec<u8> {
        plan.messages.iter().flat_map(Frontend::encode).collect()
    }

    /// `test_simple_pipeline`'s `PQsendQueryParams(conn, "SELECT $1", 1,
    /// {INT4OID}, {"1"}, NULL, NULL, 0)`
    /// (`src/test/modules/libpq_pipeline/libpq_pipeline.c:1615`) is the five
    /// messages of `traces/simple_pipeline.trace` lines 1-5 — Parse 21,
    /// Bind 19, Describe 6, Execute 9, Sync 4 — outside pipeline mode as
    /// inside it, since that test syncs straight after the one query.
    #[test]
    fn query_params_sends_what_simple_pipeline_traced() {
        let plan = query_params(b"SELECT $1", &[23], &Params::text(&[Some(b"1")])).unwrap();
        assert_eq!(plan.class, QueryClass::Extended);
        let lengths: Vec<(u8, usize)> = plan
            .messages
            .iter()
            .map(|m| {
                let bytes = m.encode();
                (bytes[0], bytes.len() - 1)
            })
            .collect();
        assert_eq!(
            lengths,
            [(b'P', 21), (b'B', 19), (b'D', 6), (b'E', 9), (b'S', 4)]
        );
    }

    /// With no parameters, neither the types nor the format codes are sent
    /// even when arrays are given (`fe-exec.c:1804`, `:1830`); without a
    /// types array the Parse says zero types even with parameters.
    #[test]
    fn counts_are_sent_only_when_there_are_parameters_and_an_array() {
        let plan = query_params(b"SELECT 1", &[], &Params::text(&[])).unwrap();
        let Frontend::Parse { param_types, .. } = &plan.messages[0] else {
            panic!("not a Parse");
        };
        assert!(param_types.is_empty());

        let plan = query_params(b"SELECT $1", &[], &Params::text(&[Some(b"x")])).unwrap();
        let Frontend::Parse { param_types, .. } = &plan.messages[0] else {
            panic!("not a Parse");
        };
        assert!(param_types.is_empty(), "NULL paramTypes: a zero count");
        let Frontend::Bind {
            param_formats,
            result_formats,
            ..
        } = &plan.messages[1]
        else {
            panic!("not a Bind");
        };
        assert!(param_formats.is_empty(), "NULL paramFormats: a zero count");
        assert_eq!(result_formats, &[0]);
    }

    /// Binary parameters carry their format codes and a binary result asks
    /// for one, and a NULL parameter is a NULL whatever its format.
    #[test]
    fn formats_and_nulls_reach_the_bind() {
        let values: [Option<&[u8]>; 2] = [Some(&[0, 0, 0, 42]), None];
        let formats = [Format::Binary, Format::Text];
        let params = Params {
            values: &values,
            formats: &formats,
            result_format: Format::Binary,
        };
        let plan = query_params(b"SELECT $1::int4, $2::text", &[23, 25], &params).unwrap();
        assert_eq!(
            plan.messages[1],
            Frontend::Bind {
                portal: Vec::new(),
                statement: Vec::new(),
                param_formats: vec![1, 0],
                params: vec![Some(vec![0, 0, 0, 42]), None],
                result_formats: vec![1],
            }
        );
    }

    /// A text value is sent as the bytes given, a NUL included: C's
    /// `strlen` (`fe-exec.c:1870`) cannot see past a NUL because a C string
    /// cannot hold one, and a Rust slice can. The divergence is recorded in
    /// `docs/divergences.md`.
    #[test]
    fn a_text_parameter_is_sent_whole_even_with_a_nul_in_it() {
        let plan = query_params(b"SELECT $1", &[], &Params::text(&[Some(b"a\0b")])).unwrap();
        let Frontend::Bind { params, .. } = &plan.messages[1] else {
            panic!("not a Bind");
        };
        assert_eq!(params, &[Some(b"a\0b".to_vec())]);
    }

    /// `PQsendQueryPrepared` sends no Parse; the Bind names the statement.
    #[test]
    fn query_prepared_binds_the_named_statement() {
        let plan = query_prepared(b"select_one", &Params::text(&[Some(b"7")])).unwrap();
        assert_eq!(plan.class, QueryClass::Extended);
        assert!(matches!(
            &plan.messages[0],
            Frontend::Bind { statement, portal, .. } if statement == b"select_one" && portal.is_empty()
        ));
        assert_eq!(plan.messages.len(), 4);
        assert_eq!(plan.messages[3], Frontend::Sync);
    }

    /// `PQprepare` outside pipeline mode is Parse then Sync; its Parse is
    /// the one `traces/prepared.trace:1` recorded at length 68.
    #[test]
    fn prepare_is_a_parse_and_a_sync() {
        let plan = prepare(
            b"select_one",
            b"SELECT $1, '42', $1::numeric, interval '1 sec'",
            &[23],
        )
        .unwrap();
        assert_eq!(plan.class, QueryClass::Prepare);
        assert_eq!(plan.messages.len(), 2);
        assert_eq!(plan.messages[0].encode().len() - 1, 68);
        assert_eq!(plan.messages[1], Frontend::Sync);
    }

    /// `traces/prepared.trace` lines 12-13 and 16-17: Describe S and Close S
    /// of `select_one`, each followed by its own Sync.
    #[test]
    fn a_typed_command_is_the_command_and_a_sync() {
        let describe = typed_command(TypedCommand::Describe, Target::Statement, b"select_one");
        assert_eq!(describe.class, QueryClass::Describe);
        let mut expected = b"D\0\0\0\x10Sselect_one\0".to_vec();
        expected.extend_from_slice(b"S\0\0\0\x04");
        assert_eq!(wire(&describe), expected);

        let close = typed_command(TypedCommand::Close, Target::Portal, b"cursor_one");
        assert_eq!(close.class, QueryClass::Close);
        let mut expected = b"C\0\0\0\x10Pcursor_one\0".to_vec();
        expected.extend_from_slice(b"S\0\0\0\x04");
        assert_eq!(wire(&close), expected);
    }

    /// `fe-exec.c:1527` — more than 65535 parameters is refused with
    /// upstream's message, before anything is built.
    #[test]
    fn more_than_the_parameter_limit_is_refused() {
        let values = vec![None; PQ_QUERY_PARAM_MAX_LIMIT + 1];
        let error = query_params(b"SELECT 1", &[], &Params::text(&values)).unwrap_err();
        assert_eq!(error, ArgumentError::TooManyParameters);
        assert_eq!(
            error.message(),
            b"number of parameters must be between 0 and 65535".to_vec()
        );
        assert_eq!(
            query_prepared(b"s", &Params::text(&values)).unwrap_err(),
            ArgumentError::TooManyParameters
        );
        assert_eq!(
            prepare(b"s", b"SELECT 1", &vec![0; PQ_QUERY_PARAM_MAX_LIMIT + 1]).unwrap_err(),
            ArgumentError::TooManyParameters
        );

        // The limit itself is allowed.
        let values = vec![None; PQ_QUERY_PARAM_MAX_LIMIT];
        assert!(query_params(b"SELECT 1", &[], &Params::text(&values)).is_ok());
    }

    /// An array that is neither empty nor one entry per parameter is
    /// refused instead of being read past or truncated.
    #[test]
    fn an_array_of_the_wrong_length_is_refused() {
        let values: [Option<&[u8]>; 2] = [None, None];
        assert_eq!(
            query_params(b"SELECT $1, $2", &[23], &Params::text(&values)).unwrap_err(),
            ArgumentError::LengthMismatch {
                array: "paramTypes",
                len: 1,
                nparams: 2
            }
        );
        let params = Params {
            values: &values,
            formats: &[Format::Text],
            result_format: Format::Text,
        };
        let error = query_prepared(b"s", &params).unwrap_err();
        assert_eq!(
            String::from_utf8(error.message()).unwrap(),
            "paramFormats has 1 entries for 2 parameters"
        );
    }

    /// In pipeline mode each command goes out without its Sync:
    /// `traces/multi_pipelines.trace` lines 1-4 are the four messages of
    /// `query_params` less the Sync, which line 5 is `PQpipelineSync`'s.
    #[test]
    fn a_pipelined_command_sends_no_sync_of_its_own() {
        let plan = query_params(b"SELECT $1", &[23], &Params::text(&[Some(b"1")]))
            .unwrap()
            .without_sync();
        let ids: Vec<u8> = plan.messages.iter().map(|m| m.encode()[0]).collect();
        assert_eq!(ids, b"PBDE");
        assert_eq!(plan.class, QueryClass::Extended);

        let plan = prepare(b"s", b"SELECT 1", &[]).unwrap().without_sync();
        assert_eq!(plan.messages.len(), 1);
        let plan = typed_command(TypedCommand::Close, Target::Statement, b"s").without_sync();
        assert_eq!(plan.messages.len(), 1);
        assert_eq!(
            plan.clone().without_sync(),
            plan,
            "a plan without a Sync is left alone"
        );
    }
}
