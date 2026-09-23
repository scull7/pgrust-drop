//! The protocol version 3 messages, as data plus their exact wire encoding.
//!
//! Ported from `src/interfaces/libpq/fe-protocol3.c` — `pqParseInput3`
//! (`:71`) for the framing and the dispatch, `getRowDescriptions` (`:519`),
//! `getParamDescriptions` (`:690`), `getAnotherTuple` (`:778`),
//! `pqGetErrorNotice3` (`:899`) and `build_startup_packet` (`:2444`) for the
//! bodies — plus the extended-query messages `fe-exec.c` builds
//! (`PQsendPrepare`, `:1553`; `PQsendQueryGuts`, `:1774`;
//! `PQsendTypedCommand`, `:2606`), with the message type codes of
//! `src/include/libpq/protocol.h`.
//!
//! Decoding is a pure function from bytes to a [`Backend`] and encoding a pure
//! function from a [`Frontend`] to bytes, so both are fuzzable and neither
//! needs a socket.

use crate::auth::AuthRequest;
use crate::result::{FieldDescription, ResultError};

/// `pqcomm.h:90` — `PG_PROTOCOL(3, 0)`, the version this port speaks. 3.2
/// exists (`pqcomm.h:97`) but negotiating it is NAT-391's NegotiateProtocol
/// work, and 3.0 is what every supported server accepts.
pub const PROTOCOL_VERSION_3_0: u32 = 3 << 16; // PG_PROTOCOL(3, 0): minor 0.

/// `fe-protocol3.c:102` — lengths above this are believed only for the
/// message types that can legitimately be long.
pub const LONG_MESSAGE_THRESHOLD: i32 = 30000;

/// `VALID_LONG_MESSAGE_TYPE`, `fe-protocol3.c:38`.
#[must_use]
pub fn valid_long_message_type(id: u8) -> bool {
    matches!(id, b'd' | b'D' | b'E' | b'V' | b'N' | b'A' | b'T' | b't')
}

/// Everything that can go wrong turning bytes into a [`Backend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// `handleSyncLoss`, `fe-protocol3.c:506`.
    LostSynchronization { id: u8, length: i32 },
    /// `fe-protocol3.c:553` and `:792` — "insufficient data in \"%c\" message".
    InsufficientData(u8),
    /// `fe-protocol3.c:471`.
    ContentsDoNotAgree(u8),
    /// `fe-protocol3.c:798`.
    UnexpectedFieldCount,
    /// `fe-protocol3.c:405`.
    DataWithoutRowDescription,
    /// `fe-protocol3.c:447`.
    UnexpectedResponse(u8),
    /// `pqGetNegotiateProtocolVersion3`, `fe-protocol3.c:1475`.
    NegativeUnsupportedParameterCount,
}

impl ProtocolError {
    /// The bytes libpq's error buffer would hold, without the newline
    /// `libpq_append_error` adds.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            ProtocolError::LostSynchronization { id, length } => format!(
                "lost synchronization with server: got message type \"{}\", length {length}",
                *id as char
            )
            .into_bytes(),
            ProtocolError::InsufficientData(id) => {
                format!("insufficient data in \"{}\" message", *id as char).into_bytes()
            }
            ProtocolError::ContentsDoNotAgree(id) => format!(
                "message contents do not agree with length in message type \"{}\"",
                *id as char
            )
            .into_bytes(),
            ProtocolError::UnexpectedFieldCount => {
                b"unexpected field count in \"D\" message".to_vec()
            }
            ProtocolError::DataWithoutRowDescription => {
                b"server sent data (\"D\" message) without prior row description (\"T\" message)"
                    .to_vec()
            }
            ProtocolError::UnexpectedResponse(id) => format!(
                "unexpected response from server; first received character was \"{}\"",
                *id as char
            )
            .into_bytes(),
            ProtocolError::NegativeUnsupportedParameterCount => {
                b"received invalid protocol negotiation message: server reported negative number of unsupported parameters"
                    .to_vec()
            }
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for ProtocolError {}

/// The `pqGetc` / `pqGetInt` / `pqGets` family (`fe-misc.c:79`-`:247`) over
/// one message body: running out of bytes is the message type's own
/// "insufficient data" error.
#[derive(Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    id: u8,
}

impl<'a> Reader<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8], id: u8) -> Self {
        Self { buf, pos: 0, id }
    }

    fn short(&self) -> ProtocolError {
        ProtocolError::InsufficientData(self.id)
    }

    /// `pqGetnchar`, `fe-misc.c:167`.
    ///
    /// # Errors
    /// The message body has fewer than `n` bytes left.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.buf.len() - self.pos < n {
            return Err(self.short());
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    /// `pqGetc`, `fe-misc.c:79`.
    ///
    /// # Errors
    /// The body is exhausted.
    pub fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }

    /// `pqGetInt(&x, 4, conn)`, unsigned.
    ///
    /// # Errors
    /// Fewer than four bytes are left.
    pub fn u32(&mut self) -> Result<u32, ProtocolError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `pqGetInt(&x, 4, conn)` where the caller wants it signed — a field
    /// length of -1 means NULL (`getAnotherTuple`, `fe-protocol3.c:827`).
    ///
    /// # Errors
    /// Fewer than four bytes are left.
    #[allow(clippy::cast_possible_wrap)] // The coercion upstream relies on.
    pub fn i32(&mut self) -> Result<i32, ProtocolError> {
        Ok(self.u32()? as i32)
    }

    /// `pqGetInt(&x, 2, conn)`, which "treats 2-byte integers as unsigned"
    /// (`fe-protocol3.c:598`).
    ///
    /// # Errors
    /// Fewer than two bytes are left.
    pub fn u16(&mut self) -> Result<u16, ProtocolError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// The same two bytes coerced to signed, as `fe-protocol3.c:601` does.
    ///
    /// # Errors
    /// Fewer than two bytes are left.
    #[allow(clippy::cast_possible_wrap)] // fe-protocol3.c:601 does exactly this.
    pub fn i16(&mut self) -> Result<i16, ProtocolError> {
        Ok(self.u16()? as i16)
    }

    /// `pqGets`, `fe-misc.c:138` — up to the NUL, which is consumed.
    ///
    /// # Errors
    /// There is no NUL in what is left of the body.
    pub fn cstring(&mut self) -> Result<&'a [u8], ProtocolError> {
        let rest = &self.buf[self.pos..];
        let end = rest
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| self.short())?;
        self.pos += end + 1;
        Ok(&rest[..end])
    }

    /// Everything not yet read.
    #[must_use]
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }

    /// True when the whole body was consumed — `fe-protocol3.c:458`'s check
    /// that the contents agree with the length.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pos == self.buf.len()
    }
}

/// What `pqParseInput3` finds at the front of the input buffer
/// (`fe-protocol3.c:80`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Not enough bytes yet; read more and try again.
    Incomplete,
    /// `handleSyncLoss`, `fe-protocol3.c:504`: the length word is impossible,
    /// so the connection is not recoverable.
    SyncLoss { id: u8, length: i32 },
    /// A whole message: `id`, the body, and how many bytes to consume.
    Message {
        id: u8,
        /// Byte range of the body within the input buffer.
        body: std::ops::Range<usize>,
    },
}

/// `pqParseInput3`'s header handling, `fe-protocol3.c:86`-`:113`.
#[must_use]
pub fn next_frame(buf: &[u8]) -> Frame {
    if buf.len() < 5 {
        return Frame::Incomplete;
    }
    let id = buf[0];
    let length = i32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]);

    // fe-protocol3.c:97 — "A length less than 4 is definitely broken."
    if length < 4 {
        return Frame::SyncLoss { id, length };
    }
    // fe-protocol3.c:102 — large lengths are believed for a few types only.
    if length > LONG_MESSAGE_THRESHOLD && !valid_long_message_type(id) {
        return Frame::SyncLoss { id, length };
    }

    // `length >= 4` was checked just above, so this cannot fail.
    let Ok(body_len) = usize::try_from(length - 4) else {
        return Frame::SyncLoss { id, length };
    };
    if buf.len() - 5 < body_len {
        return Frame::Incomplete;
    }
    Frame::Message {
        id,
        body: 5..5 + body_len,
    }
}

/// `PQtransactionStatus`, set by `getReadyForQuery` (`fe-protocol3.c:1763`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Idle,
    InTransaction,
    InError,
    Unknown,
}

/// A message from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// `PqMsg_AuthenticationRequest`, `protocol.h:50`.
    Authentication(AuthRequest),
    /// `PqMsg_BackendKeyData`, with the variable-length cancel key protocol
    /// 3.2 allows (`getBackendKeyData`, `fe-protocol3.c:1572`).
    BackendKeyData { pid: i32, cancel_key: Vec<u8> },
    /// `PqMsg_ParameterStatus` (`getParameterStatus`, `:1541`).
    ParameterStatus { name: Vec<u8>, value: Vec<u8> },
    /// `PqMsg_ReadyForQuery` (`getReadyForQuery`, `:1763`).
    ReadyForQuery(TransactionStatus),
    /// `PqMsg_RowDescription` (`getRowDescriptions`, `:519`).
    RowDescription(Vec<FieldDescription>),
    /// `PqMsg_DataRow` (`getAnotherTuple`, `:778`). `None` is a -1 length.
    DataRow(Vec<Option<Vec<u8>>>),
    /// `PqMsg_CommandComplete` — the command tag.
    CommandComplete(Vec<u8>),
    /// `PqMsg_EmptyQueryResponse`.
    EmptyQueryResponse,
    /// `PqMsg_ErrorResponse` (`pqGetErrorNotice3(conn, true)`).
    ErrorResponse(ResultError),
    /// `PqMsg_NoticeResponse` (`pqGetErrorNotice3(conn, false)`).
    NoticeResponse(ResultError),
    /// `PqMsg_NotificationResponse` (`getNotify`, `:1636`).
    NotificationResponse {
        pid: i32,
        channel: Vec<u8>,
        payload: Vec<u8>,
    },
    /// `PqMsg_NegotiateProtocolVersion` (`pqGetNegotiateProtocolVersion3`,
    /// `:1444`).
    NegotiateProtocolVersion {
        newest: u32,
        unrecognized: Vec<Vec<u8>>,
    },
    /// `PqMsg_ParseComplete`, `protocol.h:38`.
    ParseComplete,
    /// `PqMsg_BindComplete`, `protocol.h:39`.
    BindComplete,
    /// `PqMsg_CloseComplete`, `protocol.h:40`.
    CloseComplete,
    /// `PqMsg_NoData`, `protocol.h:56`: the statement or portal described
    /// returns no rows.
    NoData,
    /// `PqMsg_PortalSuspended`, `protocol.h:57`. libpq never asks for it —
    /// every Execute it sends has a row limit of 0 (`fe-exec.c:1899`) — so
    /// `pqParseInput3` has no case for it and it lands in the "unexpected
    /// response" default (`fe-protocol3.c:446`).
    PortalSuspended,
    /// `PqMsg_ParameterDescription` (`getParamDescriptions`,
    /// `fe-protocol3.c:690`): one type OID per parameter.
    ParameterDescription(Vec<u32>),
    /// The COPY messages and anything else this port does not interpret yet:
    /// parsed as far as their type.
    Other { id: u8, body: Vec<u8> },
}

impl Backend {
    /// Decode one message body. `id` is the type byte `pqParseInput3` already
    /// read (`fe-protocol3.c:87`).
    ///
    /// # Errors
    /// The body is too short for the message type, or longer than the type can
    /// account for (`fe-protocol3.c:471`).
    ///
    /// # Panics
    /// Never: the one conversion that could is guarded by the negative-length
    /// test that precedes it (a -1 field length is NULL, `:827`).
    pub fn decode(id: u8, body: &[u8]) -> Result<Self, ProtocolError> {
        let mut r = Reader::new(body, id);
        let message = match id {
            b'R' => Backend::Authentication(AuthRequest::decode(body)?),
            b'K' => {
                let pid = r.i32()?;
                Backend::BackendKeyData {
                    pid,
                    cancel_key: r.rest().to_vec(),
                }
            }
            b'S' => {
                let name = r.cstring()?.to_vec();
                let value = r.cstring()?.to_vec();
                Backend::ParameterStatus { name, value }
            }
            b'Z' => {
                // fe-protocol3.c:1769 — anything else is PQTRANS_UNKNOWN.
                let status = match r.u8()? {
                    b'I' => TransactionStatus::Idle,
                    b'T' => TransactionStatus::InTransaction,
                    b'E' => TransactionStatus::InError,
                    _ => TransactionStatus::Unknown,
                };
                Backend::ReadyForQuery(status)
            }
            b'T' => {
                let nfields = r.u16()? as usize;
                let mut fields = Vec::with_capacity(nfields);
                for _ in 0..nfields {
                    fields.push(FieldDescription {
                        name: r.cstring()?.to_vec(),
                        tableid: r.u32()?,
                        columnid: r.i16()?,
                        typid: r.u32()?,
                        typlen: r.i16()?,
                        atttypmod: r.i32()?,
                        format: r.i16()?,
                    });
                }
                Backend::RowDescription(fields)
            }
            b'D' => {
                let nfields = r.u16()? as usize;
                let mut values = Vec::with_capacity(nfields);
                for _ in 0..nfields {
                    let vlen = r.i32()?;
                    if vlen < 0 {
                        values.push(None);
                    } else {
                        let vlen = usize::try_from(vlen).expect("vlen is not negative here");
                        values.push(Some(r.take(vlen)?.to_vec()));
                    }
                }
                Backend::DataRow(values)
            }
            b'C' => Backend::CommandComplete(r.cstring()?.to_vec()),
            b'I' => Backend::EmptyQueryResponse,
            b'E' | b'N' => {
                let fields = decode_error_fields(&mut r)?;
                if id == b'E' {
                    Backend::ErrorResponse(fields)
                } else {
                    Backend::NoticeResponse(fields)
                }
            }
            b'A' => {
                let pid = r.i32()?;
                Backend::NotificationResponse {
                    pid,
                    channel: r.cstring()?.to_vec(),
                    payload: r.cstring()?.to_vec(),
                }
            }
            b'v' => decode_negotiate_protocol_version(&mut r)?,
            b'1' => Backend::ParseComplete,
            b'2' => Backend::BindComplete,
            b'3' => Backend::CloseComplete,
            b'n' => Backend::NoData,
            b's' => Backend::PortalSuspended,
            b't' => {
                // fe-protocol3.c:703 — a two-byte count, read unsigned, then
                // one four-byte OID per parameter (`:719`). Nothing reserves
                // capacity from the count, so a lying one costs only the
                // bytes that are actually there.
                let nparams = r.u16()?;
                let mut types = Vec::new();
                for _ in 0..nparams {
                    types.push(r.u32()?);
                }
                Backend::ParameterDescription(types)
            }
            _ => Backend::Other {
                id,
                body: body.to_vec(),
            },
        };

        // fe-protocol3.c:458 — the body must be exactly consumed. Three kinds
        // read the whole body by construction and leave this reader at zero:
        // an authentication request (`AuthRequest::decode` has its own reader
        // over the same bytes), BackendKeyData's variable-length cancel key,
        // and a message type this port does not interpret.
        if !r.is_empty()
            && !matches!(
                message,
                Backend::Other { .. } | Backend::BackendKeyData { .. } | Backend::Authentication(_)
            )
        {
            return Err(ProtocolError::ContentsDoNotAgree(id));
        }
        Ok(message)
    }
}

/// `pqGetNegotiateProtocolVersion3`, `fe-protocol3.c:1444`.
///
/// The count is read with `pqGetInt(&num, 4, …)` into an `int` and a negative
/// one is refused at `:1475`. Reading it unsigned instead would turn a hostile
/// or corrupted `FF FF FF FF` into a four-billion-element allocation, so the
/// count is signed here too — and nothing reserves capacity from it: the
/// strings are pushed one at a time and `cstring` stops at the end of the body.
fn decode_negotiate_protocol_version(r: &mut Reader<'_>) -> Result<Backend, ProtocolError> {
    let newest = r.u32()?;
    let count = r.i32()?;
    if count < 0 {
        return Err(ProtocolError::NegativeUnsupportedParameterCount);
    }
    let mut unrecognized = Vec::new();
    for _ in 0..count {
        unrecognized.push(r.cstring()?.to_vec());
    }
    Ok(Backend::NegotiateProtocolVersion {
        newest,
        unrecognized,
    })
}

/// The field loop of `pqGetErrorNotice3`, `fe-protocol3.c:945`: one type byte
/// and one string per field, ending at a `\0` type byte.
fn decode_error_fields(r: &mut Reader<'_>) -> Result<ResultError, ProtocolError> {
    let mut fields = Vec::new();
    loop {
        let code = r.u8()?;
        if code == 0 {
            break;
        }
        fields.push((code, r.cstring()?.to_vec()));
    }
    Ok(ResultError::new(fields))
}

/// A message to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frontend {
    /// The startup packet — no type byte, and the protocol version where one
    /// would be (`build_startup_packet`, `fe-protocol3.c:2444`).
    Startup {
        version: u32,
        parameters: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// `PqMsg_Query`, `protocol.h:26`.
    Query(Vec<u8>),
    /// `PqMsg_PasswordMessage`, sent with `strlen(pwd) + 1`
    /// (`fe-auth.c:858`), so the NUL is part of the message.
    PasswordMessage(Vec<u8>),
    /// `PqMsg_SASLInitialResponse`, `fe-auth.c:663`.
    SaslInitialResponse {
        mechanism: Vec<u8>,
        initial_response: Option<Vec<u8>>,
    },
    /// `PqMsg_SASLResponse`, `fe-auth.c:782`.
    SaslResponse(Vec<u8>),
    /// `PqMsg_Terminate`, `protocol.h:28`.
    Terminate,
    /// `PqMsg_Parse`, `protocol.h:25`, as `PQsendPrepare` builds it
    /// (`fe-exec.c:1584`): statement name, query, then a two-byte count and
    /// one OID per declared parameter type.
    Parse {
        statement: Vec<u8>,
        query: Vec<u8>,
        param_types: Vec<u32>,
    },
    /// `PqMsg_Bind`, `protocol.h:19`, as `PQsendQueryGuts` builds it
    /// (`fe-exec.c:1823`): portal, statement, the parameter format codes, the
    /// parameter values (`None` is SQL NULL, sent as length -1), and the
    /// result format codes.
    Bind {
        portal: Vec<u8>,
        statement: Vec<u8>,
        param_formats: Vec<i16>,
        params: Vec<Option<Vec<u8>>>,
        result_formats: Vec<i16>,
    },
    /// `PqMsg_Describe`, `protocol.h:21` (`PQsendTypedCommand`,
    /// `fe-exec.c:2606`).
    Describe { target: Target, name: Vec<u8> },
    /// `PqMsg_Execute`, `protocol.h:22`: the portal and a row limit, where 0
    /// means all rows (`fe-exec.c:1896`).
    Execute { portal: Vec<u8>, max_rows: i32 },
    /// `PqMsg_Close`, `protocol.h:20` (`PQsendTypedCommand`,
    /// `fe-exec.c:2606`).
    Close { target: Target, name: Vec<u8> },
    /// `PqMsg_Sync`, `protocol.h:27`.
    Sync,
    /// `PqMsg_Flush`, `protocol.h:24`.
    Flush,
}

/// What a Describe or Close names: the `type` byte of `PQsendTypedCommand`
/// (`fe-exec.c:2600`-`:2601`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `'S'`, a prepared statement.
    Statement,
    /// `'P'`, a portal.
    Portal,
}

impl Target {
    /// The byte on the wire.
    #[must_use]
    pub fn byte(self) -> u8 {
        match self {
            Target::Statement => b'S',
            Target::Portal => b'P',
        }
    }
}

impl Frontend {
    /// The bytes on the wire, length word included.
    ///
    /// # Panics
    /// A message is longer than its four-byte length word can say, or a
    /// Parse or Bind carries more than 65535 of something — neither of which
    /// [`crate::Connection`] builds, because it refuses more than
    /// `PQ_QUERY_PARAM_MAX_LIMIT` parameters first (`fe-exec.c:1527`).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frontend::Startup {
                version,
                parameters,
            } => {
                let mut body = version.to_be_bytes().to_vec();
                for (name, value) in parameters {
                    body.extend_from_slice(name);
                    body.push(0);
                    body.extend_from_slice(value);
                    body.push(0);
                }
                // fe-protocol3.c:2504 — the trailing terminator.
                body.push(0);
                let mut out = Vec::with_capacity(body.len() + 4);
                out.extend_from_slice(&length_word(body.len()));
                out.extend_from_slice(&body);
                out
            }
            Frontend::Query(query) => {
                let mut body = query.clone();
                body.push(0);
                packet(b'Q', &body)
            }
            Frontend::PasswordMessage(password) => {
                let mut body = password.clone();
                body.push(0);
                packet(b'p', &body)
            }
            Frontend::SaslInitialResponse {
                mechanism,
                initial_response,
            } => {
                let mut body = mechanism.clone();
                body.push(0);
                // fe-auth.c:667 — with no initial response, nothing follows
                // the mechanism name at all.
                if let Some(response) = initial_response {
                    body.extend_from_slice(
                        &u32::try_from(response.len())
                            .unwrap_or(u32::MAX)
                            .to_be_bytes(),
                    );
                    body.extend_from_slice(response);
                }
                packet(b'p', &body)
            }
            Frontend::SaslResponse(response) => packet(b'p', response),
            Frontend::Terminate => packet(b'X', b""),
            Frontend::Parse {
                statement,
                query,
                param_types,
            } => {
                let mut body = cstring(statement);
                body.extend_from_slice(&cstring(query));
                body.extend_from_slice(&count16(param_types.len()));
                for oid in param_types {
                    body.extend_from_slice(&oid.to_be_bytes());
                }
                packet(b'P', &body)
            }
            Frontend::Bind {
                portal,
                statement,
                param_formats,
                params,
                result_formats,
            } => packet(
                b'B',
                &bind_body(portal, statement, param_formats, params, result_formats),
            ),
            Frontend::Describe { target, name } => {
                let mut body = vec![target.byte()];
                body.extend_from_slice(&cstring(name));
                packet(b'D', &body)
            }
            Frontend::Execute { portal, max_rows } => {
                let mut body = cstring(portal);
                body.extend_from_slice(&max_rows.to_be_bytes());
                packet(b'E', &body)
            }
            Frontend::Close { target, name } => {
                let mut body = vec![target.byte()];
                body.extend_from_slice(&cstring(name));
                packet(b'C', &body)
            }
            Frontend::Sync => packet(b'S', b""),
            Frontend::Flush => packet(b'H', b""),
        }
    }
}

/// The body of a Bind, as `PQsendQueryGuts` lays it out (`fe-exec.c:1823`-`:1887`).
fn bind_body(
    portal: &[u8],
    statement: &[u8],
    param_formats: &[i16],
    params: &[Option<Vec<u8>>],
    result_formats: &[i16],
) -> Vec<u8> {
    let mut body = cstring(portal);
    body.extend_from_slice(&cstring(statement));
    body.extend_from_slice(&count16(param_formats.len()));
    for format in param_formats {
        body.extend_from_slice(&format.to_be_bytes());
    }
    body.extend_from_slice(&count16(params.len()));
    for param in params {
        match param {
            // fe-exec.c:1878 — a NULL is a length of -1 and no
            // bytes.
            None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            Some(value) => {
                let len =
                    i32::try_from(value.len()).expect("a parameter value fits in its length word");
                body.extend_from_slice(&len.to_be_bytes());
                body.extend_from_slice(value);
            }
        }
    }
    body.extend_from_slice(&count16(result_formats.len()));
    for format in result_formats {
        body.extend_from_slice(&format.to_be_bytes());
    }
    body
}

/// `pqPuts`, `fe-misc.c:154`: the bytes and their NUL.
fn cstring(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.extend_from_slice(bytes);
    out.push(0);
    out
}

/// `pqPutInt(n, 2, conn)` for a count. The callers refuse more than
/// `PQ_QUERY_PARAM_MAX_LIMIT` (`libpq-fe.h:507`) before building a message,
/// so every count that reaches here fits.
fn count16(n: usize) -> [u8; 2] {
    u16::try_from(n)
        .expect("a count is checked against PQ_QUERY_PARAM_MAX_LIMIT before encoding")
        .to_be_bytes()
}

/// `pqPacketSend`, `fe-connect.c:5409`: type byte, length including itself but
/// not the type byte, then the body.
fn packet(id: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 5);
    out.push(id);
    out.extend_from_slice(&length_word(body.len()));
    out.extend_from_slice(body);
    out
}

/// The four-byte length word: the body plus the word itself, as
/// `pqPutMsgEnd` writes it (`fe-misc.c:542`). A message longer than a `u32`
/// cannot be sent at all, and nothing here builds one.
fn length_word(body_len: usize) -> [u8; 4] {
    u32::try_from(body_len + 4)
        .expect("a frontend message fits in its length word")
        .to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::diag;

    /// The startup packet of `psql "dbname=postgres user=alice"`, byte for
    /// byte: length, `PG_PROTOCOL(3,0)`, the pairs, the terminator.
    #[test]
    fn the_startup_packet_is_upstreams_layout() {
        let message = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: vec![
                (b"user".to_vec(), b"alice".to_vec()),
                (b"database".to_vec(), b"postgres".to_vec()),
            ],
        };
        let encoded = message.encode();
        assert_eq!(
            &encoded[0..4],
            &u32::try_from(encoded.len()).unwrap().to_be_bytes()
        );
        assert_eq!(&encoded[4..8], &[0, 3, 0, 0], "PG_PROTOCOL(3,0)");
        assert_eq!(&encoded[8..], b"user\0alice\0database\0postgres\0\0");
        assert_eq!(PROTOCOL_VERSION_3_0, 196_608);
    }

    /// `PqMsg_Query`: 'Q', the length, the NUL-terminated query.
    #[test]
    fn a_query_message_is_q_length_and_a_c_string() {
        assert_eq!(
            Frontend::Query(b"select 1".to_vec()).encode(),
            b"Q\0\0\0\rselect 1\0".to_vec()
        );
        assert_eq!(Frontend::Terminate.encode(), b"X\0\0\0\x04".to_vec());
    }

    /// `pg_SASL_init`'s SASLInitialResponse, `fe-auth.c:663`: mechanism name,
    /// then a four-byte length and the response — and nothing at all when
    /// there is no initial response.
    #[test]
    fn a_sasl_initial_response_carries_its_length() {
        let with = Frontend::SaslInitialResponse {
            mechanism: b"SCRAM-SHA-256".to_vec(),
            initial_response: Some(b"n,,n=,r=abc".to_vec()),
        }
        .encode();
        assert_eq!(with[0], b'p');
        assert_eq!(&with[5..19], b"SCRAM-SHA-256\0");
        assert_eq!(&with[19..23], &11u32.to_be_bytes());
        assert_eq!(&with[23..], b"n,,n=,r=abc");
        assert_eq!(
            u32::from_be_bytes([with[1], with[2], with[3], with[4]]) as usize,
            with.len() - 1
        );

        let without = Frontend::SaslInitialResponse {
            mechanism: b"SCRAM-SHA-256".to_vec(),
            initial_response: None,
        }
        .encode();
        assert_eq!(without, b"p\0\0\0\x12SCRAM-SHA-256\0".to_vec());
    }

    /// The length word of a frontend message, as a trace prints it: the
    /// message minus its type byte.
    fn traced_length(encoded: &[u8]) -> u32 {
        let length = u32::from_be_bytes([encoded[1], encoded[2], encoded[3], encoded[4]]);
        assert_eq!(
            length as usize,
            encoded.len() - 1,
            "the length word is honest"
        );
        length
    }

    /// The five messages `PQsendQueryParams` sends, with the lengths the
    /// upstream trace `src/test/modules/libpq_pipeline/traces/simple_pipeline.trace`
    /// (lines 1-5) recorded for `SELECT $1` with one `int4` parameter `'1'`.
    /// The lengths come from C libpq, not from this encoder, so they are an
    /// oracle for the layout.
    #[test]
    fn the_extended_query_messages_have_the_lengths_upstream_traced() {
        let parse = Frontend::Parse {
            statement: Vec::new(),
            query: b"SELECT $1".to_vec(),
            param_types: vec![23],
        }
        .encode();
        assert_eq!(traced_length(&parse), 21);
        assert_eq!(parse, b"P\0\0\0\x15\0SELECT $1\0\0\x01\0\0\0\x17".to_vec());

        let bind = Frontend::Bind {
            portal: Vec::new(),
            statement: Vec::new(),
            param_formats: Vec::new(),
            params: vec![Some(b"1".to_vec())],
            result_formats: vec![0],
        }
        .encode();
        assert_eq!(traced_length(&bind), 19);
        assert_eq!(
            bind,
            b"B\0\0\0\x13\0\0\0\0\0\x01\0\0\0\x011\0\x01\0\0".to_vec()
        );

        let describe = Frontend::Describe {
            target: Target::Portal,
            name: Vec::new(),
        }
        .encode();
        assert_eq!(traced_length(&describe), 6);
        assert_eq!(describe, b"D\0\0\0\x06P\0".to_vec());

        let execute = Frontend::Execute {
            portal: Vec::new(),
            max_rows: 0,
        }
        .encode();
        assert_eq!(traced_length(&execute), 9);
        assert_eq!(execute, b"E\0\0\0\x09\0\0\0\0\0".to_vec());

        assert_eq!(Frontend::Sync.encode(), b"S\0\0\0\x04".to_vec());
        assert_eq!(Frontend::Flush.encode(), b"H\0\0\0\x04".to_vec());
    }

    /// `PQsendPrepare`'s Parse and `PQsendTypedCommand`'s Describe and Close,
    /// against `traces/prepared.trace` lines 1, 2 and 8 (lengths 68, 16, 16).
    #[test]
    fn a_named_parse_describe_and_close_have_the_lengths_upstream_traced() {
        let parse = Frontend::Parse {
            statement: b"select_one".to_vec(),
            query: b"SELECT $1, '42', $1::numeric, interval '1 sec'".to_vec(),
            param_types: vec![23],
        }
        .encode();
        assert_eq!(traced_length(&parse), 68);
        assert_eq!(&parse[5..16], b"select_one\0");

        for target in [Target::Statement, Target::Portal] {
            let describe = Frontend::Describe {
                target,
                name: b"select_one".to_vec(),
            }
            .encode();
            assert_eq!(traced_length(&describe), 16);
            assert_eq!(describe[0], b'D');
            assert_eq!(describe[5], target.byte());
            let close = Frontend::Close {
                target,
                name: b"select_one".to_vec(),
            }
            .encode();
            assert_eq!(traced_length(&close), 16);
            assert_eq!(close[0], b'C');
            assert_eq!(&close[6..], b"select_one\0");
        }
        assert_eq!(Target::Statement.byte(), b'S');
        assert_eq!(Target::Portal.byte(), b'P');
    }

    /// A Bind of a named statement with no parameters and a binary result
    /// (`traces/transaction.trace:15`, length 22), and one whose only
    /// parameter is NULL: a length of -1 and no bytes (`fe-exec.c:1878`).
    #[test]
    fn a_bind_carries_formats_and_nulls_as_upstream_sends_them() {
        let bind = Frontend::Bind {
            portal: Vec::new(),
            statement: b"rollback".to_vec(),
            param_formats: Vec::new(),
            params: Vec::new(),
            result_formats: vec![1],
        }
        .encode();
        assert_eq!(traced_length(&bind), 22);
        assert_eq!(&bind[bind.len() - 6..], b"\0\0\0\x01\0\x01");

        let null = Frontend::Bind {
            portal: Vec::new(),
            statement: Vec::new(),
            param_formats: vec![1],
            params: vec![None],
            result_formats: vec![0],
        }
        .encode();
        assert_eq!(
            &null[5..],
            b"\0\0\0\x01\0\x01\0\x01\xff\xff\xff\xff\0\x01\0\0"
        );
    }

    /// The small extended-protocol replies, and ParameterDescription against
    /// `traces/prepared.trace:5` (`B 10 ParameterDescription 1 NNNN`: a
    /// two-byte count and one OID).
    #[test]
    fn the_extended_query_replies_decode() {
        assert_eq!(Backend::decode(b'1', b"").unwrap(), Backend::ParseComplete);
        assert_eq!(Backend::decode(b'2', b"").unwrap(), Backend::BindComplete);
        assert_eq!(Backend::decode(b'3', b"").unwrap(), Backend::CloseComplete);
        assert_eq!(Backend::decode(b'n', b"").unwrap(), Backend::NoData);
        assert_eq!(
            Backend::decode(b's', b"").unwrap(),
            Backend::PortalSuspended
        );

        let mut body = 1u16.to_be_bytes().to_vec();
        body.extend_from_slice(&23u32.to_be_bytes());
        assert_eq!(body.len() + 4, 10, "the traced length");
        assert_eq!(
            Backend::decode(b't', &body).unwrap(),
            Backend::ParameterDescription(vec![23])
        );
        assert_eq!(
            Backend::decode(b't', &0u16.to_be_bytes()).unwrap(),
            Backend::ParameterDescription(Vec::new())
        );

        // fe-protocol3.c:722 — too few OIDs for the count is "insufficient
        // data", and a body for a message with none is a disagreement.
        assert_eq!(
            Backend::decode(b't', &2u16.to_be_bytes()),
            Err(ProtocolError::InsufficientData(b't'))
        );
        assert_eq!(
            Backend::decode(b'1', b"x"),
            Err(ProtocolError::ContentsDoNotAgree(b'1'))
        );
    }

    /// The framing rule of `pqParseInput3`, `fe-protocol3.c:97`-`:113`.
    #[test]
    fn the_framing_rule_is_upstreams() {
        assert_eq!(next_frame(b""), Frame::Incomplete);
        assert_eq!(next_frame(b"Z\0\0\0"), Frame::Incomplete, "header split");
        assert_eq!(
            next_frame(b"Z\0\0\0\x05"),
            Frame::Incomplete,
            "body missing"
        );
        assert_eq!(
            next_frame(b"Z\0\0\0\x05I"),
            Frame::Message {
                id: b'Z',
                body: 5..6
            }
        );
        // A length below 4 is broken whatever the type.
        assert_eq!(
            next_frame(b"Z\0\0\0\x03"),
            Frame::SyncLoss {
                id: b'Z',
                length: 3
            }
        );
        assert_eq!(
            next_frame(&[b'Z', 0xff, 0xff, 0xff, 0xff]),
            Frame::SyncLoss {
                id: b'Z',
                length: -1
            }
        );
        // Above 30000 only the long types are believed.
        let long = [b'Z', 0x00, 0x00, 0x80, 0x00];
        assert_eq!(
            next_frame(&long),
            Frame::SyncLoss {
                id: b'Z',
                length: 32768
            }
        );
        let long_data_row = [b'D', 0x00, 0x00, 0x80, 0x00];
        assert_eq!(next_frame(&long_data_row), Frame::Incomplete);
        for id in [b'd', b'D', b'E', b'V', b'N', b'A', b'T', b't'] {
            assert!(valid_long_message_type(id), "type {}", id as char);
        }
        for id in [b'Z', b'C', b'S', b'K', b'R', b'I'] {
            assert!(!valid_long_message_type(id), "type {}", id as char);
        }
    }

    /// A message exactly on the 30000 boundary is *not* long, so the
    /// threshold is `>` and not `>=` as `fe-protocol3.c:102` has it.
    #[test]
    fn the_long_message_threshold_is_exclusive() {
        let mut header = vec![b'Z'];
        header.extend_from_slice(&30000u32.to_be_bytes());
        header.resize(5 + 30000 - 4, 0);
        assert!(matches!(next_frame(&header), Frame::Message { .. }));

        let mut over = vec![b'Z'];
        over.extend_from_slice(&30001u32.to_be_bytes());
        assert_eq!(
            next_frame(&over),
            Frame::SyncLoss {
                id: b'Z',
                length: 30001
            }
        );
    }

    /// RowDescription and DataRow, the two messages a `select` is made of.
    #[test]
    fn a_row_description_and_a_data_row_decode() {
        let mut body = 1u16.to_be_bytes().to_vec();
        body.extend_from_slice(b"version\0");
        body.extend_from_slice(&0u32.to_be_bytes()); // tableid
        body.extend_from_slice(&0i16.to_be_bytes()); // columnid
        body.extend_from_slice(&25u32.to_be_bytes()); // typid: text
        body.extend_from_slice(&(-1i16).to_be_bytes()); // typlen
        body.extend_from_slice(&(-1i32).to_be_bytes()); // atttypmod
        body.extend_from_slice(&0i16.to_be_bytes()); // format

        let Backend::RowDescription(fields) = Backend::decode(b'T', &body).unwrap() else {
            panic!("not a RowDescription");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, b"version");
        assert_eq!(fields[0].typid, 25);
        assert_eq!(fields[0].typlen, -1, "signed, fe-protocol3.c:602");
        assert_eq!(fields[0].atttypmod, -1);

        let mut row = 2u16.to_be_bytes().to_vec();
        row.extend_from_slice(&3u32.to_be_bytes());
        row.extend_from_slice(b"abc");
        row.extend_from_slice(&(-1i32).to_be_bytes()); // NULL
        assert_eq!(
            Backend::decode(b'D', &row).unwrap(),
            Backend::DataRow(vec![Some(b"abc".to_vec()), None])
        );

        // A zero-length value is not NULL (fe-protocol3.c:837).
        let mut empty = 1u16.to_be_bytes().to_vec();
        empty.extend_from_slice(&0u32.to_be_bytes());
        assert_eq!(
            Backend::decode(b'D', &empty).unwrap(),
            Backend::DataRow(vec![Some(Vec::new())])
        );
    }

    /// ErrorResponse: type-byte-plus-string fields until the `\0` terminator
    /// (`fe-protocol3.c:945`).
    #[test]
    fn an_error_response_decodes_every_field() {
        let mut body = Vec::new();
        body.push(diag::SEVERITY);
        body.extend_from_slice(b"ERROR\0");
        body.push(diag::SQLSTATE);
        body.extend_from_slice(b"42601\0");
        body.push(diag::MESSAGE_PRIMARY);
        body.extend_from_slice(b"syntax error at or near \"selct\"\0");
        body.push(diag::STATEMENT_POSITION);
        body.extend_from_slice(b"1\0");
        body.push(0);

        let Backend::ErrorResponse(error) = Backend::decode(b'E', &body).unwrap() else {
            panic!("not an ErrorResponse");
        };
        assert_eq!(error.sqlstate(), Some(&b"42601"[..]));
        assert_eq!(
            error.field(diag::MESSAGE_PRIMARY),
            Some(&b"syntax error at or near \"selct\""[..])
        );
        assert_eq!(error.field(diag::STATEMENT_POSITION), Some(&b"1"[..]));
        assert_eq!(error.field(diag::MESSAGE_HINT), None);
        assert_eq!(error.fields().len(), 4);

        // The same body as a NoticeResponse is a notice, not an error.
        assert!(matches!(
            Backend::decode(b'N', &body).unwrap(),
            Backend::NoticeResponse(_)
        ));
    }

    /// The small fixed-shape messages.
    #[test]
    fn the_session_messages_decode() {
        assert_eq!(
            Backend::decode(b'Z', b"I").unwrap(),
            Backend::ReadyForQuery(TransactionStatus::Idle)
        );
        assert_eq!(
            Backend::decode(b'Z', b"T").unwrap(),
            Backend::ReadyForQuery(TransactionStatus::InTransaction)
        );
        assert_eq!(
            Backend::decode(b'Z', b"E").unwrap(),
            Backend::ReadyForQuery(TransactionStatus::InError)
        );
        assert_eq!(
            Backend::decode(b'Z', b"?").unwrap(),
            Backend::ReadyForQuery(TransactionStatus::Unknown)
        );
        assert_eq!(
            Backend::decode(b'S', b"client_encoding\0UTF8\0").unwrap(),
            Backend::ParameterStatus {
                name: b"client_encoding".to_vec(),
                value: b"UTF8".to_vec(),
            }
        );
        assert_eq!(
            Backend::decode(b'C', b"SELECT 1\0").unwrap(),
            Backend::CommandComplete(b"SELECT 1".to_vec())
        );
        assert_eq!(
            Backend::decode(b'I', b"").unwrap(),
            Backend::EmptyQueryResponse
        );

        let mut keydata = 12345i32.to_be_bytes().to_vec();
        keydata.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(
            Backend::decode(b'K', &keydata).unwrap(),
            Backend::BackendKeyData {
                pid: 12345,
                cancel_key: vec![1, 2, 3, 4],
            }
        );

        let mut notify = 99i32.to_be_bytes().to_vec();
        notify.extend_from_slice(b"chan\0payload\0");
        assert_eq!(
            Backend::decode(b'A', &notify).unwrap(),
            Backend::NotificationResponse {
                pid: 99,
                channel: b"chan".to_vec(),
                payload: b"payload".to_vec(),
            }
        );
    }

    /// A message this port does not interpret yet (CopyDone here) keeps its
    /// bytes rather than failing to decode; refusing it is the caller's decision
    /// (`fe-protocol3.c:447`).
    #[test]
    fn an_unhandled_message_type_keeps_its_body() {
        assert_eq!(
            Backend::decode(b'c', b"").unwrap(),
            Backend::Other {
                id: b'c',
                body: Vec::new()
            }
        );
        assert_eq!(
            ProtocolError::UnexpectedResponse(b'c').message(),
            b"unexpected response from server; first received character was \"c\"".to_vec()
        );
    }

    /// Short and over-long bodies are errors with upstream's wording, not
    /// silent truncation.
    #[test]
    fn a_body_that_disagrees_with_its_length_is_an_error() {
        assert_eq!(
            Backend::decode(b'Z', b""),
            Err(ProtocolError::InsufficientData(b'Z'))
        );
        assert_eq!(
            Backend::decode(b'Z', b"II"),
            Err(ProtocolError::ContentsDoNotAgree(b'Z'))
        );
        assert_eq!(
            Backend::decode(b'C', b"SELECT 1"),
            Err(ProtocolError::InsufficientData(b'C')),
            "unterminated command tag"
        );
        let mut truncated_row = 1u16.to_be_bytes().to_vec();
        truncated_row.extend_from_slice(&5u32.to_be_bytes());
        truncated_row.extend_from_slice(b"abc");
        assert_eq!(
            Backend::decode(b'D', &truncated_row),
            Err(ProtocolError::InsufficientData(b'D'))
        );
        assert_eq!(
            ProtocolError::InsufficientData(b'D').message(),
            b"insufficient data in \"D\" message".to_vec()
        );
        assert_eq!(
            ProtocolError::LostSynchronization {
                id: b'Z',
                length: 3
            }
            .message(),
            b"lost synchronization with server: got message type \"Z\", length 3".to_vec()
        );
        assert_eq!(
            ProtocolError::UnexpectedFieldCount.message(),
            b"unexpected field count in \"D\" message".to_vec()
        );
        assert_eq!(
            ProtocolError::DataWithoutRowDescription.message(),
            b"server sent data (\"D\" message) without prior row description (\"T\" message)"
                .to_vec()
        );
    }

    /// A NegotiateProtocolVersion whose count is negative is refused with
    /// upstream's message (`fe-protocol3.c:1475`) instead of being believed.
    ///
    /// Read as unsigned, `FF FF FF FF` is 4294967295, and reserving that many
    /// `Vec`s asks for 103 GB and aborts the process — a server, a proxy or a
    /// corrupted stream could kill the client where C libpq reports a
    /// connection error. The count is never used as a capacity either, so a
    /// large *positive* count costs only the bytes that are actually there.
    #[test]
    fn a_negative_unsupported_parameter_count_is_refused() {
        let mut body = PROTOCOL_VERSION_3_0.to_be_bytes().to_vec();
        body.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(
            Backend::decode(b'v', &body),
            Err(ProtocolError::NegativeUnsupportedParameterCount)
        );
        assert_eq!(
            ProtocolError::NegativeUnsupportedParameterCount.message(),
            b"received invalid protocol negotiation message: server reported negative number of unsupported parameters".to_vec()
        );

        // A large positive count is bounded by the body: it runs out of
        // strings and says so, without allocating for the count.
        let mut body = PROTOCOL_VERSION_3_0.to_be_bytes().to_vec();
        body.extend_from_slice(&0x7fff_ffffu32.to_be_bytes());
        body.extend_from_slice(b"_pq_.one\0");
        assert_eq!(
            Backend::decode(b'v', &body),
            Err(ProtocolError::InsufficientData(b'v'))
        );

        // And the whole frame the reviewer named parses as far as this error.
        let frame = [
            b'v', 0x00, 0x00, 0x00, 0x0c, 0x00, 0x03, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
        ];
        let Frame::Message { id, body } = next_frame(&frame) else {
            panic!("not a whole message");
        };
        assert_eq!(
            Backend::decode(id, &frame[body]),
            Err(ProtocolError::NegativeUnsupportedParameterCount)
        );
    }

    /// NegotiateProtocolVersion, which a 3.0 startup can still provoke from a
    /// server that dislikes a startup parameter (`fe-protocol3.c:1444`).
    #[test]
    fn negotiate_protocol_version_decodes() {
        let mut body = PROTOCOL_VERSION_3_0.to_be_bytes().to_vec();
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(b"_pq_.some_option\0");
        assert_eq!(
            Backend::decode(b'v', &body).unwrap(),
            Backend::NegotiateProtocolVersion {
                newest: PROTOCOL_VERSION_3_0,
                unrecognized: vec![b"_pq_.some_option".to_vec()],
            }
        );
    }
}
