//! The protocol trace writer: `fe-trace.c` (PostgreSQL 18.6).
//!
//! Every function here is a pure calculation from a message's wire bytes to
//! the bytes `PQtrace` would write for it. [`crate::Connection::trace`] is the
//! thin action that feeds them each message it sends and parses, and hands
//! the result to the caller's sink. Output is bytes, not text, because C's
//! `fprintf("%s")` copies a server's string to the file unmangled.
//!
//! A trace line is taken apart the way C takes it apart: with a cursor over
//! the message and one reader per field type (`pqTraceOutputByte1`,
//! `…Int16`, `…Int32`, `…String`, `…Nchar`), so a message whose contents do
//! not agree with its length word earns the same `mismatched message length`
//! line C prints. Where C would read outside the message on such a message —
//! undefined behaviour — this port reads a NUL byte instead, and never panics.

use std::time::{SystemTime, UNIX_EPOCH};

/// `PQTRACE_*`, `libpq-fe.h:478` and `:480`: the `flags` of
/// `PQsetTraceFlags`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraceFlags(i32);

impl TraceFlags {
    /// No flags: timestamps on, nothing suppressed. What `PQtrace` sets.
    pub const NONE: TraceFlags = TraceFlags(0);
    /// `PQTRACE_SUPPRESS_TIMESTAMPS`, `1<<0`.
    pub const SUPPRESS_TIMESTAMPS: TraceFlags = TraceFlags(1 << 0);
    /// `PQTRACE_REGRESS_MODE`, `1<<1`: hide what changes from run to run
    /// (OIDs, PIDs, cancel keys, error lengths and source locations).
    pub const REGRESS_MODE: TraceFlags = TraceFlags(1 << 1);

    /// The flags from the `int` `PQsetTraceFlags` takes; unknown bits are
    /// kept, as C keeps them.
    #[must_use]
    pub fn from_bits(bits: i32) -> Self {
        TraceFlags(bits)
    }

    /// The `int` C stores in `conn->traceFlags`.
    #[must_use]
    pub fn bits(self) -> i32 {
        self.0
    }

    /// True when every bit of `other` is set here.
    #[must_use]
    pub fn contains(self, other: TraceFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `(traceFlags & PQTRACE_REGRESS_MODE) != 0`.
    #[must_use]
    pub fn regress(self) -> bool {
        self.contains(TraceFlags::REGRESS_MODE)
    }
}

impl std::ops::BitOr for TraceFlags {
    type Output = TraceFlags;

    fn bitor(self, rhs: TraceFlags) -> TraceFlags {
        TraceFlags(self.0 | rhs.0)
    }
}

/// Which side sent a message: the `toServer` argument of
/// `pqTraceOutputMessage` (`fe-trace.c:625`), printed as `F` or `B`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Client to server, `F`.
    Frontend,
    /// Server to client, `B`.
    Backend,
}

impl Origin {
    fn prefix(self) -> &'static [u8] {
        match self {
            Origin::Frontend => b"F",
            Origin::Backend => b"B",
        }
    }
}

/// `conn->current_auth_response`, `libpq-int.h:335`-`:338`: which of the four
/// messages sharing type byte `p` is being sent. The byte alone cannot say,
/// so C records it before sending and `fe-trace.c:723` reads it back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AuthResponse {
    /// `'\0'`: none recorded.
    #[default]
    None,
    /// `AUTH_RESPONSE_GSS`.
    Gss,
    /// `AUTH_RESPONSE_PASSWORD`.
    Password,
    /// `AUTH_RESPONSE_SASL_INITIAL`.
    SaslInitial,
    /// `AUTH_RESPONSE_SASL`.
    Sasl,
}

/// `AUTH_REQ_*`, `protocol.h:74`-`:86`.
const AUTH_REQ_OK: i32 = 0;
const AUTH_REQ_PASSWORD: i32 = 3;
const AUTH_REQ_MD5: i32 = 5;
const AUTH_REQ_GSS: i32 = 7;
const AUTH_REQ_GSS_CONT: i32 = 8;
const AUTH_REQ_SSPI: i32 = 9;
const AUTH_REQ_SASL: i32 = 10;
const AUTH_REQ_SASL_CONT: i32 = 11;
const AUTH_REQ_SASL_FIN: i32 = 12;

/// `PG_PROTOCOL(m,n)`, `pqcomm.h:90`: `((m) << 16) | (n)`, written with `+`
/// since `n` never reaches bit 16.
const fn pg_protocol(major: i32, minor: i32) -> i32 {
    (major << 16) + minor
}

/// `pqcomm.h:137`, `:172`, `:173`.
const CANCEL_REQUEST_CODE: i32 = pg_protocol(1234, 5678);
const NEGOTIATE_SSL_CODE: i32 = pg_protocol(1234, 5679);
const NEGOTIATE_GSS_CODE: i32 = pg_protocol(1234, 5680);

/// `isprint()` in the C locale, which is the locale a libpq program that
/// never calls `setlocale` runs in (and musl's `isprint` in every locale).
fn is_print(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}

/// A cursor over one message and the line being written for it: the
/// `message`/`cursor`/`FILE *` triple every `pqTraceOutput*` helper takes.
struct Line<'a> {
    message: &'a [u8],
    /// `int *cursor`. Signed and unbounded because C's is: a negative field
    /// length moves it backwards.
    cursor: i64,
    out: Vec<u8>,
}

impl<'a> Line<'a> {
    fn new(message: &'a [u8], out: Vec<u8>) -> Self {
        Line {
            message,
            cursor: 0,
            out,
        }
    }

    /// The byte at `at`, or NUL outside the message.
    fn byte_at(&self, at: i64) -> u8 {
        usize::try_from(at)
            .ok()
            .and_then(|i| self.message.get(i))
            .copied()
            .unwrap_or(0)
    }

    fn be(&self, at: i64, n: i64) -> u32 {
        (0..n).fold(0u32, |acc, k| (acc << 8) | u32::from(self.byte_at(at + k)))
    }

    fn text(&mut self, text: &str) {
        self.out.extend_from_slice(text.as_bytes());
    }

    /// `pqTraceOutputByte1`, `fe-trace.c:107`.
    fn byte1(&mut self) -> u8 {
        let v = self.byte_at(self.cursor);
        if is_print(v) {
            self.out.push(b' ');
            self.out.push(v);
        } else {
            self.text(&format!(" \\x{v:02x}"));
        }
        self.cursor += 1;
        v
    }

    /// `pqTraceOutputInt16`, `fe-trace.c:126`: unsigned on the wire, so
    /// 0 … 65535.
    fn int16(&mut self) -> i32 {
        let value = i32::try_from(self.be(self.cursor, 2)).unwrap_or(0);
        self.cursor += 2;
        self.text(&format!(" {value}"));
        value
    }

    /// `pqTraceOutputInt32`, `fe-trace.c:145`.
    fn int32(&mut self, suppress: bool) -> i32 {
        let value = self.be(self.cursor, 4).cast_signed();
        self.cursor += 4;
        if suppress {
            self.text(" NNNN");
        } else {
            self.text(&format!(" {value}"));
        }
        value
    }

    /// `pqTraceOutputString`, `fe-trace.c:166`: a NUL-terminated string.
    fn string(&mut self, suppress: bool) {
        let mut end = self.cursor;
        while self.byte_at(end) != 0 {
            end += 1;
        }
        if suppress {
            self.text(" \"SSSS\"");
        } else {
            self.text(" \"");
            for at in self.cursor..end {
                let byte = self.byte_at(at);
                self.out.push(byte);
            }
            self.out.push(b'"');
        }
        self.cursor = end + 1;
    }

    /// `pqTraceOutputNchar`, `fe-trace.c:193`: exactly `len` bytes, the
    /// unprintable ones as `\xNN`.
    ///
    /// Only the bytes inside the message are printed. A length word that
    /// runs past the end is a malformed message on which C reads whatever
    /// memory follows; printing up to 2 GiB of NULs for it instead would turn
    /// one bad message into an unbounded trace.
    fn nchar(&mut self, len: i32, suppress: bool) {
        if suppress {
            self.text(" 'BBBB'");
        } else {
            self.text(" '");
            let size = i64::try_from(self.message.len()).unwrap_or(i64::MAX);
            let end = (self.cursor + i64::from(len)).min(size);
            for at in self.cursor.max(0)..end {
                let byte = self.byte_at(at);
                if is_print(byte) {
                    self.out.push(byte);
                } else {
                    self.text(&format!("\\x{byte:02x}"));
                }
            }
            self.out.push(b'\'');
        }
        self.cursor += i64::from(len);
    }

    /// The `length - *cursor + 1` of the helpers that print "the rest of the
    /// message": the length word does not count the type byte, the cursor does.
    fn rest(&self, length: i32) -> i32 {
        i32::try_from(i64::from(length) - self.cursor + 1).unwrap_or(0)
    }

    /// `int16` count, then that many `int16`s.
    fn int16_list(&mut self) {
        let n = self.int16();
        for _ in 0..n {
            self.int16();
        }
    }

    /// `int16` count, then that many length-prefixed values, -1 being NULL:
    /// the parameters of Bind and FunctionCall, the columns of DataRow.
    fn values(&mut self) {
        let n = self.int16();
        for _ in 0..n {
            let len = self.int32(false);
            if len == -1 {
                continue;
            }
            self.nchar(len, false);
        }
    }

    /// `pqTraceOutputNR`, `fe-trace.c:320`: ErrorResponse and
    /// NoticeResponse. In regress mode the file (`F`), line (`L`) and
    /// routine (`R`) fields are hidden, since they move with server code.
    fn error_fields(&mut self, name: &str, regress: bool) {
        self.text(name);
        self.text("\t");
        loop {
            let field = self.byte1();
            if field == 0 {
                break;
            }
            let suppress = regress && matches!(field, b'L' | b'F' | b'R');
            self.string(suppress);
        }
    }

    /// `pqTraceOutput_Authentication`, `fe-trace.c:485`.
    fn authentication(&mut self, length: i32, suppress: bool) {
        let auth_type = self.be(self.cursor, 4).cast_signed();
        self.cursor += 4;
        match auth_type {
            AUTH_REQ_OK => self.text("AuthenticationOk"),
            AUTH_REQ_PASSWORD => self.text("AuthenticationCleartextPassword"),
            AUTH_REQ_MD5 => self.text("AuthenticationMD5Password"),
            AUTH_REQ_GSS => self.text("AuthenticationGSS"),
            AUTH_REQ_GSS_CONT => {
                self.text("AuthenticationGSSContinue\t");
                self.nchar(self.rest(length), suppress);
            }
            AUTH_REQ_SSPI => self.text("AuthenticationSSPI"),
            AUTH_REQ_SASL => {
                self.text("AuthenticationSASL\t");
                while self.byte_at(self.cursor) != 0 {
                    self.string(false);
                }
                self.string(false);
            }
            AUTH_REQ_SASL_CONT => {
                self.text("AuthenticationSASLContinue\t");
                self.nchar(self.rest(length), suppress);
            }
            AUTH_REQ_SASL_FIN => {
                self.text("AuthenticationSASLFinal\t");
                self.nchar(self.rest(length), suppress);
            }
            other => self.text(&format!("Unknown authentication message {other}")),
        }
    }

    /// The `p` messages, told apart by what was recorded before sending
    /// (`fe-trace.c:723`).
    fn auth_response(&mut self, auth: AuthResponse, length: i32, regress: bool) {
        match auth {
            AuthResponse::Gss => {
                self.text("GSSResponse\t");
                self.nchar(self.rest(length), regress);
            }
            AuthResponse::Password => {
                self.text("PasswordMessage\t");
                self.string(false);
            }
            AuthResponse::SaslInitial => {
                self.text("SASLInitialResponse\t");
                self.string(false);
                let initial = self.int32(false);
                if initial != -1 {
                    self.nchar(initial, regress);
                }
            }
            AuthResponse::Sasl => {
                self.text("SASLResponse\t");
                self.nchar(self.rest(length), regress);
            }
            AuthResponse::None => self.text("UnknownAuthenticationResponse"),
        }
    }

    /// `pqTraceOutput_RowDescription`, `fe-trace.c:560`.
    fn row_description(&mut self, regress: bool) {
        self.text("RowDescription\t");
        let n = self.int16();
        for _ in 0..n {
            self.string(false);
            self.int32(regress);
            self.int16();
            self.int32(regress);
            self.int16();
            self.int32(false);
            self.int16();
        }
    }

    /// The body of `pqTraceOutputMessage`'s switch, `fe-trace.c:660`-`:823`.
    #[allow(clippy::too_many_lines)]
    fn body(&mut self, id: u8, length: i32, origin: Origin, auth: AuthResponse, regress: bool) {
        let to_server = origin == Origin::Frontend;
        match id {
            b'1' => self.text("ParseComplete"),
            b'2' => self.text("BindComplete"),
            b'3' => self.text("CloseComplete"),
            b'A' => {
                self.text("NotificationResponse\t");
                self.int32(regress);
                self.string(false);
                self.string(false);
            }
            b'B' => {
                self.text("Bind\t");
                self.string(false);
                self.string(false);
                self.int16_list();
                self.values();
                self.int16_list();
            }
            b'c' => self.text("CopyDone"),
            // Close(F) and CommandComplete(B) share the byte.
            b'C' if to_server => {
                self.text("Close\t");
                self.byte1();
                self.string(false);
            }
            b'C' => {
                self.text("CommandComplete\t");
                self.string(false);
            }
            b'd' => {
                self.text("CopyData\t");
                self.nchar(self.rest(length), regress);
            }
            // Describe(F) and DataRow(B).
            b'D' if to_server => {
                self.text("Describe\t");
                self.byte1();
                self.string(false);
            }
            b'D' => {
                self.text("DataRow\t");
                self.values();
            }
            // Execute(F) and ErrorResponse(B).
            b'E' if to_server => {
                self.text("Execute\t");
                self.string(false);
                self.int32(false);
            }
            b'E' => self.error_fields("ErrorResponse", regress),
            b'f' => {
                self.text("CopyFail\t");
                self.string(false);
            }
            b'p' => self.auth_response(auth, length, regress),
            b'F' => {
                self.text("FunctionCall\t");
                self.int32(regress);
                self.int16_list();
                self.values();
                self.int16();
            }
            b'G' => {
                self.text("CopyInResponse\t");
                self.byte1();
                self.int16_list();
            }
            // Flush(F) and CopyOutResponse(B).
            b'H' if to_server => self.text("Flush"),
            b'H' => {
                self.text("CopyOutResponse\t");
                self.byte1();
                self.int16_list();
            }
            b'I' => self.text("EmptyQueryResponse"),
            b'K' => {
                self.text("BackendKeyData\t");
                self.int32(regress);
                self.nchar(self.rest(length), regress);
            }
            b'n' => self.text("NoData"),
            b'N' => self.error_fields("NoticeResponse", regress),
            b'P' => {
                self.text("Parse\t");
                self.string(false);
                self.string(false);
                let n = self.int16();
                for _ in 0..n {
                    self.int32(regress);
                }
            }
            b'Q' => {
                self.text("Query\t");
                self.string(false);
            }
            b'R' => self.authentication(length, regress),
            b's' => self.text("PortalSuspended"),
            // Sync(F) and ParameterStatus(B).
            b'S' if to_server => self.text("Sync"),
            b'S' => {
                self.text("ParameterStatus\t");
                self.string(false);
                self.string(false);
            }
            b't' => {
                self.text("ParameterDescription\t");
                let n = self.int16();
                for _ in 0..n {
                    self.int32(regress);
                }
            }
            b'T' => self.row_description(regress),
            b'v' => {
                self.text("NegotiateProtocolVersion\t");
                self.int32(false);
                let n = self.int32(false);
                for _ in 0..n {
                    self.string(false);
                }
            }
            b'V' => {
                self.text("FunctionCallResponse\t");
                let len = self.int32(false);
                if len != -1 {
                    self.nchar(len, false);
                }
            }
            b'W' => {
                self.text("CopyBothResponse\t");
                self.byte1();
                while i64::from(length) > self.cursor {
                    self.int16();
                }
            }
            b'X' => self.text("Terminate"),
            b'Z' => {
                self.text("ReadyForQuery\t");
                self.byte1();
            }
            // `%02x` of a `char`: signed on x86-64 and on Apple arm64, the
            // two lanes this ships to, so a byte above 0x7f prints as the
            // sign-extended int (`ffffff80`).
            other => self.text(&format!(
                "Unknown message: {:02x}",
                i32::from(other.cast_signed())
            )),
        }
    }
}

/// `pqTraceOutputMessage`, `fe-trace.c:625`, minus the timestamp (see
/// [`timestamp_prefix`]): the line for one whole message, type byte and
/// length word included, followed by C's `mismatched message length` line
/// when the fields did not use up exactly the length the message claims.
///
/// `auth` matters only for a `p` message; see [`AuthResponse`].
#[must_use]
pub fn message_line(
    message: &[u8],
    origin: Origin,
    flags: TraceFlags,
    auth: AuthResponse,
) -> Vec<u8> {
    let regress = flags.regress();
    let mut line = Line::new(message, Vec::new());
    let id = line.byte_at(0);
    line.cursor = 1;
    let length = line.be(1, 4).cast_signed();
    line.cursor += 4;

    // fe-trace.c:654 — the length of an error or notice moves with the F, L
    // and R fields, so regress mode hides it.
    if regress && origin == Origin::Backend && matches!(id, b'E' | b'N') {
        line.out.extend_from_slice(origin.prefix());
        line.text("\tNN\t");
    } else {
        line.out.extend_from_slice(origin.prefix());
        line.text(&format!("\t{length}\t"));
    }

    line.body(id, length, origin, auth, regress);
    line.out.push(b'\n');

    // fe-trace.c:830 — the cursor counts the type byte, the length does not.
    if line.cursor - 1 != i64::from(length) {
        let consumed = line.cursor - 1;
        line.text(&format!(
            "mismatched message length: consumed {consumed}, expected {length}\n"
        ));
    }
    line.out
}

/// `pqTraceOutputNoTypeByteMessage`, `fe-trace.c:843`: the startup packet,
/// SSLRequest, GSSENCRequest and CancelRequest, which have a length word but
/// no type byte. Without the timestamp, as [`message_line`].
#[must_use]
pub fn no_type_byte_message_line(message: &[u8], flags: TraceFlags) -> Vec<u8> {
    let regress = flags.regress();
    let mut line = Line::new(message, Vec::new());
    let length = line.be(0, 4).cast_signed();
    line.cursor = 4;
    line.text(&format!("F\t{length}\t"));

    if length < 8 {
        line.text("Unknown message\n");
        return line.out;
    }

    let version = line.be(4, 4).cast_signed();
    if version == CANCEL_REQUEST_CODE && length >= 16 {
        line.text("CancelRequest\t");
        line.int16();
        line.int16();
        line.int32(regress);
        let rest = i32::try_from(i64::from(length) - line.cursor).unwrap_or(0);
        line.nchar(rest, regress);
    } else if version == NEGOTIATE_SSL_CODE {
        line.text("SSLRequest\t");
        line.int16();
        line.int16();
    } else if version == NEGOTIATE_GSS_CODE {
        line.text("GSSENCRequest\t");
        line.int16();
        line.int16();
    } else {
        line.text("StartupMessage\t");
        line.int16();
        line.int16();
        while line.byte_at(line.cursor) != 0 {
            line.string(false);
            line.string(false);
        }
    }
    line.out.push(b'\n');
    line.out
}

/// `pqTraceOutputCharResponse`, `fe-trace.c:918`: the one-byte answer to an
/// SSLRequest or GSSENCRequest. Without the timestamp, as [`message_line`].
#[must_use]
pub fn char_response_line(response_type: &str, response: u8) -> Vec<u8> {
    let mut out = format!("B\t1\t{response_type}\t ").into_bytes();
    out.push(response);
    out.push(b'\n');
    out
}

/// The `"%s\t"` timestamp C puts in front of every trace line unless
/// `PQTRACE_SUPPRESS_TIMESTAMPS` is set, or nothing when it is.
#[must_use]
pub fn timestamp_prefix(flags: TraceFlags, now: SystemTime) -> Vec<u8> {
    if flags.contains(TraceFlags::SUPPRESS_TIMESTAMPS) {
        return Vec::new();
    }
    let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let mut out = format_timestamp(secs, since.subsec_micros()).into_bytes();
    out.push(b'\t');
    out
}

/// `pqTraceFormatTimestamp`, `fe-trace.c:80`: `%Y-%m-%d %H:%M:%S` then
/// `.%06u` microseconds — in UTC, where C uses `localtime_r`. The standard
/// library has no time zone database and this crate no libc; see
/// `docs/divergences.md`.
#[must_use]
pub fn format_timestamp(unix_secs: i64, micros: u32) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{micros:06}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// Calculation: the proleptic Gregorian date `days` after 1970-01-01
/// (Howard Hinnant's `civil_from_days`, public domain).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Frontend, Target};

    const REGRESS: TraceFlags = TraceFlags(3);

    /// The vendored upstream trace, `crates/rlibpq/tests/traces/<name>`.
    fn upstream(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/traces")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Line `n` (1-based) of an upstream trace, newline included.
    fn trace_line(name: &str, n: usize) -> Vec<u8> {
        let text = upstream(name);
        let mut line = text
            .split(|b| *b == b'\n')
            .nth(n - 1)
            .unwrap_or_else(|| panic!("{name} has a line {n}"))
            .to_vec();
        line.push(b'\n');
        line
    }

    /// A backend message: type byte, length word, body.
    fn backend(id: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![id];
        out.extend_from_slice(&(u32::try_from(body.len()).unwrap() + 4).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn front(message: &Frontend) -> Vec<u8> {
        message_line(
            &message.encode(),
            Origin::Frontend,
            REGRESS,
            AuthResponse::None,
        )
    }

    fn back(id: u8, body: &[u8]) -> Vec<u8> {
        message_line(
            &backend(id, body),
            Origin::Backend,
            REGRESS,
            AuthResponse::None,
        )
    }

    fn error_body(fields: &[(u8, &str)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (code, value) in fields {
            body.push(*code);
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        body.push(0);
        body
    }

    fn field(name: &str, typid: u32, typlen: i16) -> Vec<u8> {
        let mut out = name.as_bytes().to_vec();
        out.push(0);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0i16.to_be_bytes());
        out.extend_from_slice(&typid.to_be_bytes());
        out.extend_from_slice(&typlen.to_be_bytes());
        out.extend_from_slice(&(-1i32).to_be_bytes());
        out.extend_from_slice(&0i16.to_be_bytes());
        out
    }

    #[test]
    fn each_trace_is_the_file_postgresql_18_6_ships() {
        // tests/traces/README.md: the digests of the files at REL_18_6.
        let expected = [
            (
                "disallowed_in_pipeline.trace",
                "b779cd6aeaddf5e83964028496abf2050094d38060e19ad06e591fc76724a226",
            ),
            (
                "multi_pipelines.trace",
                "88fa742d1dba202ff915302ac493747988531916c321c9b60a4a7c334f947e81",
            ),
            (
                "nosync.trace",
                "793b7ffbb2200d57a6c640d652b0ab41c71d117046f3a60b1b2a89336b7c86a9",
            ),
            (
                "pipeline_abort.trace",
                "c3dab26ab7469fd6fbbc966ec60e7bb460427af73ea40d64fb2119f797284cc7",
            ),
            (
                "pipeline_idle.trace",
                "59cce7f0cd25151f3caca63e868651da6dd7790c173d65c16b08e5e80786a95c",
            ),
            (
                "prepared.trace",
                "8c1749dfab4a2be0d491028502410e0da35b875847a724ff3a9f8ccf7192b2cc",
            ),
            (
                "simple_pipeline.trace",
                "b359dd63118b8ea65f3351988745d9bfe150d9cb717c35817d655e1f6a2b1822",
            ),
            (
                "singlerow.trace",
                "9eb9b67fc7840e7396088bb86c917d0d4e5c297233cee706ce09c9b8b4a79ffd",
            ),
            (
                "transaction.trace",
                "3144e788a01bddb45efe355eda6dc11031a40421a203b78085396849e891b069",
            ),
        ];
        for (name, digest) in expected {
            let hex =
                crate::sha256::sha256(&upstream(name))
                    .iter()
                    .fold(String::new(), |mut hex, b| {
                        use std::fmt::Write as _;
                        let _ = write!(hex, "{b:02x}");
                        hex
                    });
            assert_eq!(hex, digest, "{name}");
        }
    }

    #[test]
    fn a_parse_hides_its_parameter_types_in_regress_mode() {
        // prepared.trace:1.
        let parse = Frontend::Parse {
            statement: b"select_one".to_vec(),
            query: b"SELECT $1, '42', $1::numeric, interval '1 sec'".to_vec(),
            param_types: vec![23],
        };
        assert_eq!(front(&parse), trace_line("prepared.trace", 1));
    }

    #[test]
    fn describe_close_sync_and_terminate_are_traced_as_c_does() {
        // prepared.trace:2, :3, :8, :26 and :42.
        let describe = Frontend::Describe {
            target: Target::Statement,
            name: b"select_one".to_vec(),
        };
        assert_eq!(front(&describe), trace_line("prepared.trace", 2));
        assert_eq!(front(&Frontend::Sync), trace_line("prepared.trace", 3));
        let close = Frontend::Close {
            target: Target::Statement,
            name: b"select_one".to_vec(),
        };
        assert_eq!(front(&close), trace_line("prepared.trace", 8));
        let portal = Frontend::Describe {
            target: Target::Portal,
            name: b"cursor_one".to_vec(),
        };
        assert_eq!(front(&portal), trace_line("prepared.trace", 26));
        assert_eq!(
            front(&Frontend::Terminate),
            trace_line("prepared.trace", 42)
        );
    }

    #[test]
    fn bind_execute_and_query_are_traced_as_c_does() {
        // transaction.trace:1, :8, :10 and :15.
        let query = Frontend::Query(
            b"DROP TABLE IF EXISTS pq_pipeline_tst;CREATE TABLE pq_pipeline_tst (id int)".to_vec(),
        );
        assert_eq!(front(&query), trace_line("transaction.trace", 1));
        let bind = Frontend::Bind {
            portal: Vec::new(),
            statement: Vec::new(),
            param_formats: Vec::new(),
            params: Vec::new(),
            result_formats: vec![0],
        };
        assert_eq!(front(&bind), trace_line("transaction.trace", 8));
        let execute = Frontend::Execute {
            portal: Vec::new(),
            max_rows: 0,
        };
        assert_eq!(front(&execute), trace_line("transaction.trace", 10));
        let rollback = Frontend::Bind {
            portal: Vec::new(),
            statement: b"rollback".to_vec(),
            param_formats: Vec::new(),
            params: Vec::new(),
            result_formats: vec![1],
        };
        assert_eq!(front(&rollback), trace_line("transaction.trace", 15));
    }

    #[test]
    fn bind_parameters_are_printed_with_their_lengths() {
        // simple_pipeline.trace:2 — one text parameter "1", no format codes.
        let bind = Frontend::Bind {
            portal: Vec::new(),
            statement: Vec::new(),
            param_formats: Vec::new(),
            params: vec![Some(b"1".to_vec())],
            result_formats: vec![0],
        };
        assert_eq!(front(&bind), trace_line("simple_pipeline.trace", 2));
    }

    #[test]
    fn a_flush_request_is_traced_as_flush() {
        // pipeline_idle.trace:5.
        assert_eq!(
            front(&Frontend::Flush),
            trace_line("pipeline_idle.trace", 5)
        );
    }

    #[test]
    fn the_completion_messages_have_no_contents() {
        // prepared.trace:4, :10; transaction.trace:39, :40.
        assert_eq!(back(b'1', b""), trace_line("prepared.trace", 4));
        assert_eq!(back(b'3', b""), trace_line("prepared.trace", 10));
        assert_eq!(back(b'2', b""), trace_line("transaction.trace", 39));
        assert_eq!(back(b'n', b""), trace_line("transaction.trace", 40));
    }

    #[test]
    fn a_parameter_description_hides_its_oids_in_regress_mode() {
        // prepared.trace:5.
        let mut body = 1i16.to_be_bytes().to_vec();
        body.extend_from_slice(&23u32.to_be_bytes());
        assert_eq!(back(b't', &body), trace_line("prepared.trace", 5));
    }

    #[test]
    fn a_row_description_hides_its_oids_in_regress_mode() {
        // prepared.trace:6 — four columns; `typlen` 65535 is the int16 -1
        // printed unsigned, as pqTraceOutputInt16 does.
        let mut body = 4i16.to_be_bytes().to_vec();
        body.extend(field("?column?", 23, 4));
        body.extend(field("?column?", 25, -1));
        body.extend(field("numeric", 1700, -1));
        body.extend(field("interval", 1186, 16));
        assert_eq!(back(b'T', &body), trace_line("prepared.trace", 6));
    }

    #[test]
    fn ready_for_query_prints_the_transaction_status() {
        // prepared.trace:7 and :37; transaction.trace:5.
        assert_eq!(back(b'Z', b"I"), trace_line("prepared.trace", 7));
        assert_eq!(back(b'Z', b"I"), trace_line("transaction.trace", 5));
        assert_eq!(back(b'Z', b"E"), trace_line("prepared.trace", 37));
    }

    #[test]
    fn an_error_hides_its_length_and_its_source_location_in_regress_mode() {
        // prepared.trace:14.
        let body = error_body(&[
            (b'S', "ERROR"),
            (b'V', "ERROR"),
            (b'C', "26000"),
            (b'M', "prepared statement \"select_one\" does not exist"),
            (b'F', "prepare.c"),
            (b'L', "451"),
            (b'R', "FetchPreparedStatement"),
        ]);
        assert_eq!(back(b'E', &body), trace_line("prepared.trace", 14));
    }

    #[test]
    fn a_notice_is_traced_like_an_error() {
        // transaction.trace:2.
        let body = error_body(&[
            (b'S', "NOTICE"),
            (b'V', "NOTICE"),
            (b'C', "00000"),
            (b'M', "table \"pq_pipeline_tst\" does not exist, skipping"),
            (b'F', "tablecmds.c"),
            (b'L', "1520"),
            (b'R', "DropErrorMsgNonExistent"),
        ]);
        assert_eq!(back(b'N', &body), trace_line("transaction.trace", 2));
    }

    #[test]
    fn command_complete_and_data_row_are_the_backend_meanings_of_c_and_d() {
        // transaction.trace:3, :57 and :58.
        assert_eq!(
            back(b'C', b"DROP TABLE\0"),
            trace_line("transaction.trace", 3)
        );
        let mut row = 1i16.to_be_bytes().to_vec();
        row.extend_from_slice(&1i32.to_be_bytes());
        row.push(b'3');
        assert_eq!(back(b'D', &row), trace_line("transaction.trace", 58));
        let mut desc = 1i16.to_be_bytes().to_vec();
        let mut id = field("id", 23, 4);
        // A column of a table: a nonzero attribute number, which regress
        // mode does not hide (only the table and type OIDs are NNNN).
        id[3 + 4..3 + 6].copy_from_slice(&1i16.to_be_bytes());
        desc.extend(id);
        assert_eq!(back(b'T', &desc), trace_line("transaction.trace", 57));
    }

    #[test]
    fn a_null_value_is_a_length_of_minus_one_and_nothing_else() {
        let mut row = 2i16.to_be_bytes().to_vec();
        row.extend_from_slice(&(-1i32).to_be_bytes());
        row.extend_from_slice(&2i32.to_be_bytes());
        row.extend_from_slice(b"a\n");
        assert_eq!(back(b'D', &row), b"B\t16\tDataRow\t 2 -1 2 'a\\x0a'\n");
    }

    #[test]
    fn without_regress_mode_every_value_is_shown() {
        let mut body = 1i16.to_be_bytes().to_vec();
        body.extend_from_slice(&23u32.to_be_bytes());
        let line = message_line(
            &backend(b't', &body),
            Origin::Backend,
            TraceFlags::SUPPRESS_TIMESTAMPS,
            AuthResponse::None,
        );
        assert_eq!(line, b"B\t10\tParameterDescription\t 1 23\n");
        let error = error_body(&[(b'S', "ERROR"), (b'L', "1")]);
        let line = message_line(
            &backend(b'E', &error),
            Origin::Backend,
            TraceFlags::NONE,
            AuthResponse::None,
        );
        assert_eq!(line, b"B\t15\tErrorResponse\t S \"ERROR\" L \"1\" \\x00\n");
    }

    #[test]
    fn a_frontend_error_byte_is_an_execute_and_its_length_is_shown() {
        // Regress mode hides the length only of a *backend* E or N.
        let execute = Frontend::Execute {
            portal: b"p".to_vec(),
            max_rows: 5,
        };
        assert_eq!(front(&execute), b"F\t10\tExecute\t \"p\" 5\n");
    }

    #[test]
    fn a_message_that_disagrees_with_its_length_says_so() {
        // A ReadyForQuery claiming two bytes of status: fe-trace.c:830.
        let mut message = backend(b'Z', b"II");
        let line = message_line(&message, Origin::Backend, REGRESS, AuthResponse::None);
        assert_eq!(
            line,
            b"B\t6\tReadyForQuery\t I\nmismatched message length: consumed 5, expected 6\n"
        );
        // A truncated one reads NULs, never out of bounds.
        message.truncate(5);
        let line = message_line(&message, Origin::Backend, REGRESS, AuthResponse::None);
        assert_eq!(
            line,
            b"B\t6\tReadyForQuery\t \\x00\nmismatched message length: consumed 5, expected 6\n"
        );
    }

    #[test]
    fn an_unknown_type_byte_is_printed_in_hex_as_a_signed_char() {
        assert_eq!(back(b'q', b""), b"B\t4\tUnknown message: 71\n".to_vec());
        assert_eq!(
            back(0x80, b""),
            b"B\t4\tUnknown message: ffffff80\n".to_vec()
        );
    }

    #[test]
    fn the_p_messages_are_told_apart_by_the_recorded_auth_response() {
        let password = Frontend::PasswordMessage(b"secret".to_vec()).encode();
        let line = message_line(&password, Origin::Frontend, REGRESS, AuthResponse::Password);
        assert_eq!(line, b"F\t11\tPasswordMessage\t \"secret\"\n");

        let initial = Frontend::SaslInitialResponse {
            mechanism: b"SCRAM-SHA-256".to_vec(),
            initial_response: Some(b"n,,n=,r=abc".to_vec()),
        }
        .encode();
        let line = message_line(
            &initial,
            Origin::Frontend,
            REGRESS,
            AuthResponse::SaslInitial,
        );
        assert_eq!(
            line,
            b"F\t33\tSASLInitialResponse\t \"SCRAM-SHA-256\" 11 'BBBB'\n"
        );

        let response = Frontend::SaslResponse(b"c=biws".to_vec()).encode();
        let line = message_line(
            &response,
            Origin::Frontend,
            TraceFlags::NONE,
            AuthResponse::Sasl,
        );
        assert_eq!(line, b"F\t10\tSASLResponse\t 'c=biws'\n");

        let line = message_line(&response, Origin::Frontend, REGRESS, AuthResponse::None);
        assert_eq!(
            line,
            b"F\t10\tUnknownAuthenticationResponse\nmismatched message length: consumed 4, expected 10\n"
        );
    }

    #[test]
    fn authentication_requests_are_named_by_their_type() {
        let ok = back(b'R', &0i32.to_be_bytes());
        assert_eq!(ok, b"B\t8\tAuthenticationOk\n");
        let mut sasl = 10i32.to_be_bytes().to_vec();
        sasl.extend_from_slice(b"SCRAM-SHA-256-PLUS\0SCRAM-SHA-256\0\0");
        assert_eq!(
            back(b'R', &sasl),
            b"B\t42\tAuthenticationSASL\t \"SCRAM-SHA-256-PLUS\" \"SCRAM-SHA-256\" \"\"\n"
        );
        let mut cont = 11i32.to_be_bytes().to_vec();
        cont.extend_from_slice(b"r=abc");
        assert_eq!(
            back(b'R', &cont),
            b"B\t13\tAuthenticationSASLContinue\t 'BBBB'\n"
        );
        let mut md5 = 5i32.to_be_bytes().to_vec();
        md5.extend_from_slice(b"salt");
        assert_eq!(
            back(b'R', &md5),
            b"B\t12\tAuthenticationMD5Password\nmismatched message length: consumed 8, expected 12\n"
        );
    }

    #[test]
    fn backend_key_data_hides_the_pid_and_key_in_regress_mode() {
        let mut body = 4242i32.to_be_bytes().to_vec();
        body.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(back(b'K', &body), b"B\t12\tBackendKeyData\t NNNN 'BBBB'\n");
        let line = message_line(
            &backend(b'K', &body),
            Origin::Backend,
            TraceFlags::NONE,
            AuthResponse::None,
        );
        assert_eq!(
            line,
            b"B\t12\tBackendKeyData\t 4242 '\\x01\\x02\\x03\\x04'\n"
        );
    }

    #[test]
    fn the_startup_packet_lists_its_parameters() {
        let startup = Frontend::Startup {
            version: 3 << 16,
            parameters: vec![(b"user".to_vec(), b"u".to_vec())],
        }
        .encode();
        assert_eq!(
            no_type_byte_message_line(&startup, REGRESS),
            b"F\t16\tStartupMessage\t 3 0 \"user\" \"u\"\n"
        );
    }

    #[test]
    fn the_requests_without_a_type_byte_are_recognised_by_their_code() {
        let mut ssl = 8i32.to_be_bytes().to_vec();
        ssl.extend_from_slice(&NEGOTIATE_SSL_CODE.to_be_bytes());
        assert_eq!(
            no_type_byte_message_line(&ssl, REGRESS),
            b"F\t8\tSSLRequest\t 1234 5679\n"
        );
        let mut cancel = 16i32.to_be_bytes().to_vec();
        cancel.extend_from_slice(&CANCEL_REQUEST_CODE.to_be_bytes());
        cancel.extend_from_slice(&77i32.to_be_bytes());
        cancel.extend_from_slice(&[9, 9, 9, 9]);
        assert_eq!(
            no_type_byte_message_line(&cancel, REGRESS),
            b"F\t16\tCancelRequest\t 1234 5678 NNNN 'BBBB'\n"
        );
        assert_eq!(
            no_type_byte_message_line(&4i32.to_be_bytes(), REGRESS),
            b"F\t4\tUnknown message\n"
        );
    }

    #[test]
    fn a_one_byte_response_is_its_own_line() {
        assert_eq!(
            char_response_line("SSLResponse", b'N'),
            b"B\t1\tSSLResponse\t N\n"
        );
    }

    #[test]
    fn suppressed_timestamps_are_no_prefix_at_all() {
        let now = UNIX_EPOCH + std::time::Duration::from_micros(1_758_600_000_123_456);
        assert!(timestamp_prefix(REGRESS, now).is_empty());
        assert_eq!(
            timestamp_prefix(TraceFlags::REGRESS_MODE, now),
            b"2025-09-23 04:00:00.123456\t"
        );
    }

    #[test]
    fn a_timestamp_is_rendered_in_utc() {
        assert_eq!(format_timestamp(0, 0), "1970-01-01 00:00:00.000000");
        assert_eq!(
            format_timestamp(951_782_400, 7),
            "2000-02-29 00:00:00.000007"
        );
        assert_eq!(format_timestamp(-1, 999_999), "1969-12-31 23:59:59.999999");
    }

    #[test]
    fn flags_combine_as_the_c_bits_do() {
        let both = TraceFlags::SUPPRESS_TIMESTAMPS | TraceFlags::REGRESS_MODE;
        assert_eq!(both.bits(), 3);
        assert!(both.regress());
        assert!(!TraceFlags::SUPPRESS_TIMESTAMPS.regress());
        assert_eq!(TraceFlags::from_bits(3), both);
    }
}
