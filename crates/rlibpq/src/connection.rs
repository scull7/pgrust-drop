//! The actions: a socket, the startup exchange, and `PQexec` over it.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c` (`pqConnectDBComplete`'s
//! blocking loop, `:2782`, and `PQconnectPoll`'s `CONNECTION_AWAITING_RESPONSE`
//! state, `:3982`) and `fe-exec.c` (`PQexec`, `:2279`, which sends one Query and
//! collects results until ReadyForQuery).
//!
//! The decisions are pure and live above the socket: [`startup_parameters`]
//! and [`socket_address`] are functions of the `ConnInfo` alone, and
//! [`QueryRunner`] turns a stream of [`Backend`] messages into results without
//! knowing where they came from — which is what lets the tests below replay a
//! whole authenticated session over a scripted stream.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::auth::{AuthError, AuthStep, Authenticator, ChannelBinding};
use crate::conninfo::ConnInfo;
use crate::error::ConnError;
use crate::message::{
    Backend, Frame, Frontend, PROTOCOL_VERSION_3_0, ProtocolError, TransactionStatus, next_frame,
};
use crate::pg_config::DEFAULT_PGSOCKET_DIR;
use crate::result::{ExecStatus, FieldDescription, QueryResult, ResultError};
use crate::scram::RAW_NONCE_LEN;

/// Anything that can stop a connection or a query.
#[derive(Debug)]
pub enum ConnectionError {
    /// A socket, DNS or `/dev/urandom` failure.
    Io(io::Error),
    /// The server closed the connection mid-message — `pqReadData`'s
    /// "server closed the connection unexpectedly" (`fe-misc.c:833`).
    ServerClosedConnection,
    /// A message that did not parse.
    Protocol(ProtocolError),
    /// The client refused the authentication request.
    Auth(AuthError),
    /// An ErrorResponse during startup, before any result exists.
    Server(Box<ResultError>),
    /// A message that is well-formed but cannot appear here
    /// (`fe-protocol3.c:447`).
    UnexpectedMessage(u8),
    /// A conninfo value `PQconnectPoll` refuses before it opens anything —
    /// today the `port` alone (`fe-connect.c:3036`-`:3049`).
    Conninfo(ConnError),
}

impl ConnectionError {
    /// The bytes libpq would have left in `conn->errorMessage`.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            ConnectionError::Io(err) => err.to_string().into_bytes(),
            ConnectionError::ServerClosedConnection => {
                b"server closed the connection unexpectedly\n\tThis probably means the server terminated abnormally\n\tbefore or while processing the request."
                    .to_vec()
            }
            ConnectionError::Protocol(err) => err.message(),
            ConnectionError::Auth(err) => err.message(),
            ConnectionError::Server(err) => err.message(
                ExecStatus::FatalError,
                crate::result::Verbosity::default(),
                crate::result::ContextVisibility::default(),
            ),
            ConnectionError::UnexpectedMessage(id) => {
                ProtocolError::UnexpectedResponse(*id).message()
            }
            ConnectionError::Conninfo(err) => err.message(),
        }
    }
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for ConnectionError {}

impl From<io::Error> for ConnectionError {
    fn from(err: io::Error) -> Self {
        ConnectionError::Io(err)
    }
}

impl From<ProtocolError> for ConnectionError {
    fn from(err: ProtocolError) -> Self {
        ConnectionError::Protocol(err)
    }
}

impl From<AuthError> for ConnectionError {
    fn from(err: AuthError) -> Self {
        ConnectionError::Auth(err)
    }
}

impl From<ConnError> for ConnectionError {
    fn from(err: ConnError) -> Self {
        ConnectionError::Conninfo(err)
    }
}

/// Where to connect: `pg_conn_host_type`, `fe-connect.c`'s `CHT_HOST_NAME` and
/// `CHT_UNIX_SOCKET`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    /// `host:port` over TCP.
    Tcp { host: String, port: u16 },
    /// `<sockdir>/.s.PGSQL.<port>`, the `UNIXSOCK_PATH` of `pqcomm.h:44`.
    Unix(PathBuf),
}

/// `is_unixsock_path`, `pqcomm.h:67`: an absolute path or a `@`-prefixed
/// abstract name is a socket directory, not a host name.
#[must_use]
pub fn is_unixsock_path(host: &[u8]) -> bool {
    host.first() == Some(&b'/') || host.first() == Some(&b'@')
}

/// `DEF_PGPORT`, the integer `PQconnectPoll` substitutes for an absent or
/// empty `port` (`fe-connect.c:3038`).
///
/// `pg_config::DEF_PGPORT_STR` is the same number in the spelling
/// `PQconninfoOptions[]` stores; `the_two_spellings_of_def_pgport_agree` pins
/// the pair, so only one of them can drift.
const DEF_PGPORT: u16 = 5432;

/// `isspace` in the "C" locale — the bytes `strtol` skips before a number
/// (`fe-connect.c:8206`) and the ones `pqParseIntParam` skips after one
/// (`:8221`).
fn is_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r')
}

/// The same bytes without their leading whitespace.
fn without_leading_space(bytes: &[u8]) -> &[u8] {
    let spaces = bytes.iter().take_while(|byte| is_c_space(**byte)).count();
    &bytes[spaces..]
}

/// `strtol(value, &end, 10)` and the two checks around it
/// (`fe-connect.c:8208`-`:8225`), as far as `pqParseIntParam` uses them:
/// `None` is every arm that reaches its `error:` label.
fn strtol_int(value: &[u8]) -> Option<i32> {
    let body = without_leading_space(value);
    let (negative, after_sign) = match body.split_first() {
        Some((&b'-', rest)) => (true, rest),
        Some((&b'+', rest)) => (false, rest),
        _ => (false, body),
    };
    let digit_count = after_sign
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let (digits, tail) = after_sign.split_at(digit_count);
    // `value == end` (`:8214`) and `*end != '\0'` (`:8224`): strtol must
    // convert something, and only whitespace may follow what it converted.
    if digits.is_empty() || !without_leading_space(tail).is_empty() {
        return None;
    }
    let magnitude = digits.iter().try_fold(0i64, |acc, &digit| {
        acc.checked_mul(10)?.checked_add(i64::from(digit - b'0'))
    })?;
    // `errno != 0 || numval != (int) numval` (`:8214`): a value too wide for
    // an `int` is an error, not a saturated one.
    i32::try_from(if negative { -magnitude } else { magnitude }).ok()
}

/// `pqParseIntParam`, `fe-connect.c:8196`, for one named option.
fn parse_int_param(value: &[u8], option: &'static str) -> Result<i32, ConnError> {
    strtol_int(value).ok_or_else(|| ConnError::InvalidIntegerValue {
        value: value.into(),
        option,
    })
}

/// The port to connect to, from the raw `port` conninfo value.
///
/// `fe-connect.c:3036`-`:3049`, the block that settles `thisport` before
/// `PQconnectPoll` resolves any address — for a Unix socket (`:3079`) exactly
/// as for TCP. An absent or empty value is [`DEF_PGPORT`] (`:3037`); anything
/// else must be an integer `pqParseIntParam` reads in full (`:3041`) that
/// lands in 1..=65535 (`:3044`).
///
/// # Errors
/// [`ConnError::InvalidIntegerValue`] when the value is not an integer at all,
/// [`ConnError::InvalidPortNumber`] when it is one but out of range.
pub fn parse_port(raw: Option<&[u8]>) -> Result<u16, ConnError> {
    let Some(value) = raw.filter(|value| !value.is_empty()) else {
        return Ok(DEF_PGPORT);
    };
    match u16::try_from(parse_int_param(value, "port")?) {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(ConnError::InvalidPortNumber(value.into())),
    }
}

/// Which socket a `ConnInfo` names — `pqConnectOptions2`'s host
/// classification at `fe-connect.c:1315`-`:1352` over the port
/// [`parse_port`] settles, as a pure function.
///
/// # Errors
/// The `port` is not one `PQconnectPoll` would use; see [`parse_port`].
pub fn socket_address(conninfo: &ConnInfo) -> Result<Address, ConnError> {
    let port = parse_port(conninfo.get("port"))?;

    let host = conninfo.get("host").unwrap_or_default();
    if host.is_empty() {
        // fe-connect.c:1339 — the compiled-in socket directory wins when it
        // is not empty; only a build without one falls back to DefaultHost.
        if DEFAULT_PGSOCKET_DIR.is_empty() {
            return Ok(Address::Tcp {
                host: "localhost".to_string(),
                port,
            });
        }
        return Ok(Address::Unix(unix_socket_path(DEFAULT_PGSOCKET_DIR, port)));
    }

    if is_unixsock_path(host) {
        return Ok(Address::Unix(unix_socket_path(
            &String::from_utf8_lossy(host),
            port,
        )));
    }
    Ok(Address::Tcp {
        host: String::from_utf8_lossy(host).into_owned(),
        port,
    })
}

/// `UNIXSOCK_PATH`, `pqcomm.h:44`.
#[must_use]
pub fn unix_socket_path(sockdir: &str, port: u16) -> PathBuf {
    PathBuf::from(format!("{sockdir}/.s.PGSQL.{port}"))
}

/// `build_startup_packet`, `fe-protocol3.c:2444`: the options it sends, in its
/// order, each only when it is set and non-empty.
///
/// The `PQEnvironmentOption` loop at `:2494` (`PGDATESTYLE`, `PGTZ`,
/// `PGGEQO`) is not here: those come from the environment, and this is a
/// function of the `ConnInfo` alone. `conndefaults` has already applied every
/// `PG*` variable that names a conninfo keyword.
#[must_use]
pub fn startup_parameters(conninfo: &ConnInfo) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut parameters = Vec::new();
    let mut add = |name: &str, value: &[u8]| {
        if !value.is_empty() {
            parameters.push((name.as_bytes().to_vec(), value.to_vec()));
        }
    };

    add("user", conninfo.get("user").unwrap_or_default());
    add("database", conninfo.get("dbname").unwrap_or_default());
    add(
        "replication",
        conninfo.get("replication").unwrap_or_default(),
    );
    add("options", conninfo.get("options").unwrap_or_default());
    // fe-protocol3.c:2482 — appname, else fallback_application_name.
    let appname = conninfo.get("application_name").unwrap_or_default();
    if appname.is_empty() {
        add(
            "application_name",
            conninfo
                .get("fallback_application_name")
                .unwrap_or_default(),
        );
    } else {
        add("application_name", appname);
    }
    add(
        "client_encoding",
        conninfo.get("client_encoding").unwrap_or_default(),
    );
    parameters
}

/// `pg_strong_random`, `src/port/pg_strong_random.c:140` — the arm that reads
/// `/dev/urandom` when there is no OpenSSL and no Windows CryptoAPI, which is
/// this build.
///
/// # Errors
/// `/dev/urandom` cannot be opened or read.
pub fn strong_random(len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut file = std::fs::File::open("/dev/urandom")?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// A socket: `PGconn`'s `sock`, which is either kind.
#[derive(Debug)]
pub enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl Stream {
    /// Open the socket a [`Address`] names.
    ///
    /// # Errors
    /// The socket could not be opened: no such path, connection refused, a
    /// host that does not resolve.
    pub fn connect(address: &Address) -> io::Result<Self> {
        match address {
            Address::Tcp { host, port } => {
                Ok(Stream::Tcp(TcpStream::connect((host.as_str(), *port))?))
            }
            Address::Unix(path) => Ok(Stream::Unix(UnixStream::connect(path)?)),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            Stream::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.write(buf),
            Stream::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.flush(),
            Stream::Unix(s) => s.flush(),
        }
    }
}

/// Where a query's messages are accumulating — `pqParseInput3`'s BUSY-state
/// switch (`fe-protocol3.c:203`) as a fold over messages.
#[derive(Debug, Default)]
pub struct QueryRunner {
    results: Vec<QueryResult>,
    current: Option<QueryResult>,
    notices: Vec<ResultError>,
    notifications: Vec<(i32, Vec<u8>, Vec<u8>)>,
    parameters: Vec<(Vec<u8>, Vec<u8>)>,
    transaction_status: Option<TransactionStatus>,
    /// Once an error result is set up, later DataRows are ignored
    /// (`fe-protocol3.c:882`).
    saw_error: bool,
}

/// Whether the caller should keep reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// ReadyForQuery arrived: the command is over.
    Done,
}

impl QueryRunner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One message in. `Err` is the connection-fatal case; everything else
    /// becomes a result, a notice or state.
    ///
    /// # Errors
    /// The message cannot appear here at all — a DataRow with no preceding
    /// RowDescription, a field count that disagrees with it, or a message type
    /// the simple-query path does not handle.
    pub fn push(&mut self, message: Backend) -> Result<Flow, ProtocolError> {
        match message {
            Backend::RowDescription(fields) => {
                let mut result = QueryResult::new(ExecStatus::TuplesOk);
                result.set_fields(fields);
                self.finish_current();
                self.current = Some(result);
            }
            Backend::DataRow(values) => {
                if self.saw_error {
                    return Ok(Flow::Continue);
                }
                let Some(result) = self.current.as_mut() else {
                    return Err(ProtocolError::DataWithoutRowDescription);
                };
                // fe-protocol3.c:796 — the field count must match "T".
                if values.len() != result.nfields() {
                    return Err(ProtocolError::UnexpectedFieldCount);
                }
                result.push_row(values);
            }
            Backend::CommandComplete(tag) => {
                let mut result = self
                    .current
                    .take()
                    .unwrap_or_else(|| QueryResult::new(ExecStatus::CommandOk));
                result.set_command_status(tag);
                self.results.push(result);
            }
            Backend::EmptyQueryResponse => {
                self.finish_current();
                self.results.push(QueryResult::new(ExecStatus::EmptyQuery));
            }
            Backend::ErrorResponse(error) => {
                // fe-protocol3.c:915 — an error discards the partial result.
                self.current = None;
                self.saw_error = true;
                self.results
                    .push(QueryResult::with_error(ExecStatus::FatalError, error));
            }
            Backend::NoticeResponse(notice) => self.notices.push(notice),
            Backend::NotificationResponse {
                pid,
                channel,
                payload,
            } => self.notifications.push((pid, channel, payload)),
            Backend::ParameterStatus { name, value } => self.parameters.push((name, value)),
            Backend::ReadyForQuery(status) => {
                self.finish_current();
                self.transaction_status = Some(status);
                return Ok(Flow::Done);
            }
            // Both belong to the startup exchange; naming the byte that
            // actually arrived is the whole point of upstream's message
            // (`fe-protocol3.c:447`).
            Backend::Authentication(_) => return Err(ProtocolError::UnexpectedResponse(b'R')),
            Backend::BackendKeyData { .. } => {
                return Err(ProtocolError::UnexpectedResponse(b'K'));
            }
            Backend::NegotiateProtocolVersion { .. } => {}
            Backend::Other { id, .. } => return Err(ProtocolError::UnexpectedResponse(id)),
        }
        Ok(Flow::Continue)
    }

    /// A RowDescription with no CommandComplete after it (an error arrived
    /// instead) still produced a result; `PQgetResult` returns it.
    fn finish_current(&mut self) {
        if let Some(result) = self.current.take() {
            self.results.push(result);
        }
    }

    #[must_use]
    pub fn results(&self) -> &[QueryResult] {
        &self.results
    }

    #[must_use]
    pub fn into_results(self) -> Vec<QueryResult> {
        self.results
    }

    #[must_use]
    pub fn notices(&self) -> &[ResultError] {
        &self.notices
    }

    #[must_use]
    pub fn notifications(&self) -> &[(i32, Vec<u8>, Vec<u8>)] {
        &self.notifications
    }

    #[must_use]
    pub fn transaction_status(&self) -> Option<TransactionStatus> {
        self.transaction_status
    }
}

/// A live connection: `PGconn`, minus everything the simple query path does
/// not need yet.
#[derive(Debug)]
pub struct Connection<S = Stream> {
    stream: S,
    inbuf: Vec<u8>,
    /// Consumed prefix of `inbuf`, so a read does not shift the buffer per
    /// message (`conn->inStart`).
    start: usize,
    parameters: Vec<(Vec<u8>, Vec<u8>)>,
    backend_pid: i32,
    cancel_key: Vec<u8>,
    transaction_status: TransactionStatus,
    notices: Vec<ResultError>,
}

impl Connection<Stream> {
    /// `PQconnectdb`: open the socket the `ConnInfo` names, send the startup
    /// packet, authenticate, and return once ReadyForQuery arrives.
    ///
    /// The caller is expected to have run `ConnInfo::add_defaults` already —
    /// `conndefaults(&Env::from_process())` is what `PQconnectdb` does with
    /// the environment.
    ///
    /// # Errors
    /// The `port` is not one `PQconnectPoll` would use, the socket could not
    /// be opened, the nonce could not be drawn, the server refused the
    /// connection, or authentication failed.
    pub fn connect(conninfo: &ConnInfo) -> Result<Self, ConnectionError> {
        // fe-connect.c:3036 — `PQconnectPoll` settles the port, and refuses a
        // value that is not one, before it resolves an address or opens a
        // socket. Doing it here keeps that order: nothing is opened for a
        // conninfo C would have rejected.
        let address = socket_address(conninfo)?;
        let stream = Stream::connect(&address)?;
        let nonce = strong_random(RAW_NONCE_LEN)?;
        Connection::start_up(stream, conninfo, &nonce)
    }
}

impl<S: Read + Write> Connection<S> {
    /// The startup exchange over an already-open stream. `raw_nonce` is what
    /// `pg_strong_random` drew for SCRAM (`fe-auth-scram.c:363`); passing it in
    /// keeps the exchange reproducible for a replayed trace.
    ///
    /// # Errors
    /// The server sent an ErrorResponse, a message that cannot appear during
    /// startup, or an authentication request this build cannot answer.
    pub fn start_up(
        mut stream: S,
        conninfo: &ConnInfo,
        raw_nonce: &[u8],
    ) -> Result<Self, ConnectionError> {
        let startup = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(conninfo),
        };
        stream.write_all(&startup.encode())?;
        stream.flush()?;

        let channel_binding = match conninfo.get("channel_binding") {
            Some(b"disable") => ChannelBinding::Disable,
            Some(b"require") => ChannelBinding::Require,
            _ => ChannelBinding::Prefer,
        };
        let mut authenticator = Authenticator::new(
            conninfo.get("user").unwrap_or_default(),
            conninfo.get("password"),
            raw_nonce,
        )
        .with_channel_binding(channel_binding);

        let mut conn = Connection {
            stream,
            inbuf: Vec::new(),
            start: 0,
            parameters: Vec::new(),
            backend_pid: 0,
            cancel_key: Vec::new(),
            transaction_status: TransactionStatus::Unknown,
            notices: Vec::new(),
        };

        loop {
            match conn.read_message()? {
                Backend::Authentication(request) => match authenticator.respond(&request)? {
                    AuthStep::Send(message) => conn.send(&message)?,
                    AuthStep::Nothing | AuthStep::Complete => {}
                },
                Backend::ParameterStatus { name, value } => conn.parameters.push((name, value)),
                Backend::BackendKeyData { pid, cancel_key } => {
                    conn.backend_pid = pid;
                    conn.cancel_key = cancel_key;
                }
                Backend::NoticeResponse(notice) => conn.notices.push(notice),
                Backend::ErrorResponse(error) => {
                    return Err(ConnectionError::Server(Box::new(error)));
                }
                Backend::ReadyForQuery(status) => {
                    conn.transaction_status = status;
                    return Ok(conn);
                }
                // A 3.0 startup can still be answered with this when the
                // server dislikes a `_pq_.` option (fe-protocol3.c:1444).
                Backend::NegotiateProtocolVersion { .. } => {}
                other => return Err(ConnectionError::UnexpectedMessage(message_id(&other))),
            }
        }
    }

    /// `PQexec`, `fe-exec.c:2279`: one Query message, then every result up to
    /// ReadyForQuery.
    ///
    /// # Errors
    /// The connection broke, or the server sent something the simple-query
    /// path cannot make a result of. A *failed query* is not an error here: it
    /// is a `PGRES_FATAL_ERROR` result, exactly as in libpq.
    pub fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, ConnectionError> {
        self.send(&Frontend::Query(query.to_vec()))?;
        let mut runner = QueryRunner::new();
        loop {
            let message = self.read_message()?;
            if runner.push(message)? == Flow::Done {
                break;
            }
        }
        if let Some(status) = runner.transaction_status() {
            self.transaction_status = status;
        }
        for (name, value) in std::mem::take(&mut runner.parameters) {
            self.parameters.push((name, value));
        }
        self.notices.append(&mut runner.notices);
        Ok(runner.into_results())
    }

    /// `PQfinish`'s Terminate, `fe-connect.c:5239`.
    ///
    /// # Errors
    /// The message could not be written to the socket.
    pub fn terminate(&mut self) -> Result<(), ConnectionError> {
        self.send(&Frontend::Terminate)
    }

    fn send(&mut self, message: &Frontend) -> Result<(), ConnectionError> {
        self.stream.write_all(&message.encode())?;
        self.stream.flush()?;
        Ok(())
    }

    /// `pqParseInput3` plus `pqReadData`: return the next whole message,
    /// reading more bytes when the buffer does not hold one yet.
    fn read_message(&mut self) -> Result<Backend, ConnectionError> {
        loop {
            match next_frame(&self.inbuf[self.start..]) {
                Frame::Message { id, body } => {
                    let start = self.start;
                    let message =
                        Backend::decode(id, &self.inbuf[start + body.start..start + body.end])?;
                    self.start += body.end;
                    if self.start == self.inbuf.len() {
                        self.inbuf.clear();
                        self.start = 0;
                    }
                    return Ok(message);
                }
                Frame::SyncLoss { id, length } => {
                    return Err(ProtocolError::LostSynchronization { id, length }.into());
                }
                Frame::Incomplete => {
                    let mut chunk = [0u8; 8192];
                    let n = self.stream.read(&mut chunk)?;
                    if n == 0 {
                        return Err(ConnectionError::ServerClosedConnection);
                    }
                    self.inbuf.extend_from_slice(&chunk[..n]);
                }
            }
        }
    }

    /// `PQparameterStatus`, `fe-connect.c:7593` — the last value the server
    /// reported for a GUC.
    #[must_use]
    pub fn parameter_status(&self, name: &[u8]) -> Option<&[u8]> {
        self.parameters
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    /// `PQbackendPID`, `fe-connect.c:7674`.
    #[must_use]
    pub fn backend_pid(&self) -> i32 {
        self.backend_pid
    }

    /// The cancel key, kept for the cancel request NAT-394 will send.
    #[must_use]
    pub fn cancel_key(&self) -> &[u8] {
        &self.cancel_key
    }

    /// `PQtransactionStatus`, `fe-connect.c:7583`.
    #[must_use]
    pub fn transaction_status(&self) -> TransactionStatus {
        self.transaction_status
    }

    /// The notices collected so far — libpq hands these to the notice
    /// processor as they arrive (`fe-protocol3.c:1011`).
    #[must_use]
    pub fn notices(&self) -> &[ResultError] {
        &self.notices
    }
}

/// The type byte a decoded message came from, for the "unexpected response"
/// error that names it.
fn message_id(message: &Backend) -> u8 {
    match message {
        Backend::Authentication(_) => b'R',
        Backend::BackendKeyData { .. } => b'K',
        Backend::ParameterStatus { .. } => b'S',
        Backend::ReadyForQuery(_) => b'Z',
        Backend::RowDescription(_) => b'T',
        Backend::DataRow(_) => b'D',
        Backend::CommandComplete(_) => b'C',
        Backend::EmptyQueryResponse => b'I',
        Backend::ErrorResponse(_) => b'E',
        Backend::NoticeResponse(_) => b'N',
        Backend::NotificationResponse { .. } => b'A',
        Backend::NegotiateProtocolVersion { .. } => b'v',
        Backend::Other { id, .. } => *id,
    }
}

/// A `FieldDescription` for a text column, which the tests below build often.
#[must_use]
pub fn text_field(name: &[u8]) -> FieldDescription {
    FieldDescription {
        name: name.to_vec(),
        tableid: 0,
        columnid: 0,
        typid: 25,
        typlen: -1,
        atttypmod: -1,
        format: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conninfo::{Env, parse_conninfo};
    use crate::pg_config::DEF_PGPORT_STR;
    use crate::result::diag;

    /// A stream that plays back recorded server bytes and records what the
    /// client wrote — the replay harness for a captured trace.
    #[derive(Debug)]
    struct Scripted {
        from_server: Vec<u8>,
        read_pos: usize,
        to_server: Vec<u8>,
    }

    impl Scripted {
        fn new(from_server: Vec<u8>) -> Self {
            Self {
                from_server,
                read_pos: 0,
                to_server: Vec::new(),
            }
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = (self.from_server.len() - self.read_pos).min(buf.len());
            buf[..n].copy_from_slice(&self.from_server[self.read_pos..self.read_pos + n]);
            self.read_pos += n;
            Ok(n)
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.to_server.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn message(id: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![id];
        out.extend_from_slice(&u32::try_from(body.len() + 4).unwrap().to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    fn auth_ok() -> Vec<u8> {
        message(b'R', &0u32.to_be_bytes())
    }

    fn ready(status: u8) -> Vec<u8> {
        message(b'Z', &[status])
    }

    fn conninfo(s: &str) -> ConnInfo {
        let mut info = parse_conninfo(s.as_bytes()).unwrap();
        info.add_defaults(&Env::empty().with("USER", "alice"));
        info
    }

    /// `build_startup_packet`'s selection, `fe-protocol3.c:2474`-`:2491`.
    #[test]
    fn the_startup_parameters_are_the_ones_upstream_sends() {
        let info = conninfo("user=bob dbname=mydb application_name=psql options=-c%20x");
        let parameters = startup_parameters(&info);
        assert_eq!(
            parameters
                .iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .collect::<Vec<_>>(),
            ["user", "database", "options", "application_name"],
            "and in upstream's order"
        );
        assert_eq!(parameters[0].1, b"bob");
        assert_eq!(parameters[1].1, b"mydb");

        // An unset option contributes nothing at all.
        let bare = conninfo("");
        let keys: Vec<_> = startup_parameters(&bare)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(!keys.contains(&b"application_name".to_vec()));
        assert!(!keys.contains(&b"client_encoding".to_vec()));
        assert_eq!(keys.first().map(Vec::as_slice), Some(&b"user"[..]));
    }

    /// `fallback_application_name` is used only when `application_name` is
    /// unset (`fe-protocol3.c:2485`).
    #[test]
    fn the_fallback_application_name_is_the_second_choice() {
        let info = conninfo("fallback_application_name=pgdrop");
        assert!(
            startup_parameters(&info).contains(&(b"application_name".to_vec(), b"pgdrop".to_vec()))
        );
        let info = conninfo("application_name=psql fallback_application_name=pgdrop");
        assert!(
            startup_parameters(&info).contains(&(b"application_name".to_vec(), b"psql".to_vec()))
        );
    }

    /// The three GUCs `build_startup_packet` takes from the environment
    /// (`fe-protocol3.c:2494`: `PGDATESTYLE`, `PGTZ`, `PGGEQO`) are not sent.
    /// `startup_parameters` is a function of the `ConnInfo` alone and cannot
    /// read them — which is the point, and the divergence
    /// `docs/divergences.md` records.
    #[test]
    fn the_environment_driven_gucs_are_not_sent_yet() {
        let info = conninfo("user=alice options=-c%20datestyle%3DISO");
        let keys: Vec<Vec<u8>> = startup_parameters(&info)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        for guc in [&b"DateStyle"[..], b"TimeZone", b"geqo"] {
            assert!(!keys.contains(&guc.to_vec()), "{guc:?} must not be sent");
        }
        // `options` is sent, which is how a session asks for them instead.
        assert!(keys.contains(&b"options".to_vec()));
    }

    /// Host classification: `fe-connect.c:1327` and `:1339`.
    #[test]
    fn the_address_is_the_socket_upstream_would_pick() {
        assert_eq!(
            socket_address(&conninfo("host=example.com port=5433")).unwrap(),
            Address::Tcp {
                host: "example.com".to_string(),
                port: 5433
            }
        );
        assert_eq!(
            socket_address(&conninfo("host=/var/run/postgresql")).unwrap(),
            Address::Unix(PathBuf::from("/var/run/postgresql/.s.PGSQL.5432"))
        );
        // No host at all: the compiled-in socket directory.
        assert_eq!(
            socket_address(&conninfo("")).unwrap(),
            Address::Unix(PathBuf::from("/tmp/.s.PGSQL.5432"))
        );
        assert!(is_unixsock_path(b"/tmp"));
        assert!(is_unixsock_path(b"@abstract"));
        assert!(!is_unixsock_path(b"localhost"));
        assert_eq!(
            unix_socket_path("/tmp", 5432),
            PathBuf::from("/tmp/.s.PGSQL.5432")
        );
    }

    /// `fe-connect.c:3037` — a `port` that is absent or empty is the only way
    /// to reach `DEF_PGPORT`, and it must keep reaching it.
    #[test]
    fn a_port_that_was_never_given_is_the_compiled_in_default() {
        assert_eq!(parse_port(None).unwrap(), 5432);
        assert_eq!(parse_port(Some(b"")).unwrap(), 5432);
        assert_eq!(
            socket_address(&conninfo("host=example.com")).unwrap(),
            Address::Tcp {
                host: "example.com".to_string(),
                port: 5432
            }
        );
    }

    /// The integer form this module uses against the string form
    /// `PQconninfoOptions[]` stores (`fe-connect.c:237`), so the two cannot
    /// drift apart.
    #[test]
    fn the_two_spellings_of_def_pgport_agree() {
        assert_eq!(DEF_PGPORT.to_string(), DEF_PGPORT_STR);
    }

    /// `fe-connect.c:3041` — a `port` `pqParseIntParam` cannot read in full
    /// is that function's message (`:8231`), not a silent fallback.
    #[test]
    fn a_port_that_is_not_an_integer_is_refused_the_way_pq_parse_int_param_refuses_it() {
        for value in [&b"abc"[..], b"54ab", b"5432 5433", b"5432,5433", b"-"] {
            let error = parse_port(Some(value)).unwrap_err();
            let mut expected = b"invalid integer value \"".to_vec();
            expected.extend_from_slice(value);
            expected.extend_from_slice(b"\" for connection option \"port\"");
            assert_eq!(error.message(), expected, "for {value:?}");
        }
    }

    /// `:8214`'s `numval != (int) numval`: too wide for an `int` is
    /// `pqParseIntParam`'s error, not the range one.
    #[test]
    fn a_port_too_wide_for_an_int_is_an_invalid_integer_value_not_a_bad_port() {
        assert_eq!(
            parse_port(Some(b"99999999999")).unwrap_err().message(),
            b"invalid integer value \"99999999999\" for connection option \"port\"".to_vec()
        );
    }

    /// `fe-connect.c:3044` — an integer outside 1..=65535 is `invalid port
    /// number`. `0` is included on purpose: upstream's lower bound is 1, so
    /// `port=0` is refused rather than handed to the resolver.
    #[test]
    fn a_port_outside_upstreams_range_is_an_invalid_port_number() {
        for value in [&b"99999"[..], b"0", b"-1", b"65536", b"2147483647"] {
            let error = parse_port(Some(value)).unwrap_err();
            let mut expected = b"invalid port number: \"".to_vec();
            expected.extend_from_slice(value);
            expected.extend_from_slice(b"\"");
            assert_eq!(error.message(), expected, "for {value:?}");
        }
        // The two ends of the range upstream does accept.
        assert_eq!(parse_port(Some(b"1")).unwrap(), 1);
        assert_eq!(parse_port(Some(b"65535")).unwrap(), 65535);
    }

    /// `strtol` skips leading whitespace (`:8206`) and `:8221` skips trailing
    /// whitespace, so both are a port and neither is an error.
    #[test]
    fn a_port_is_the_number_strtol_reads_out_of_it() {
        assert_eq!(parse_port(Some(b" \t5433\r\n ")).unwrap(), 5433);
        assert_eq!(parse_port(Some(b"+5433")).unwrap(), 5433);
    }

    /// A `port` that is not UTF-8 is quoted back as the bytes C would have
    /// written, the same property `error.rs` pins for the URI tokens.
    #[test]
    fn a_port_that_is_not_utf8_reaches_the_message_unchanged() {
        assert_eq!(
            parse_port(Some(b"\xff\xfe")).unwrap_err().message(),
            b"invalid integer value \"\xff\xfe\" for connection option \"port\"".to_vec()
        );
    }

    /// The pre-flight is where C puts it: `PQconnectPoll` fails on the port
    /// before it resolves an address (`fe-connect.c:3036`), so no socket is
    /// ever opened for a conninfo upstream would have rejected. The host here
    /// does not exist, which is what makes "no socket was opened" visible:
    /// the error is the port's, not the socket's.
    #[test]
    fn an_invalid_port_stops_a_connection_before_any_socket_is_opened() {
        let info = conninfo("host=/nonexistent-socket-dir port=99999");
        let error = Connection::connect(&info).unwrap_err();
        assert_eq!(error.message(), b"invalid port number: \"99999\"".to_vec());
    }

    /// A trust connection and one `select version()`, replayed end to end:
    /// AuthenticationOk, two ParameterStatus, BackendKeyData, ReadyForQuery,
    /// then RowDescription / DataRow / CommandComplete / ReadyForQuery.
    #[test]
    fn a_trust_connection_runs_a_simple_query() {
        let mut script = auth_ok();
        script.extend(message(b'S', b"server_version\x0018.6\x00"));
        script.extend(message(b'S', b"client_encoding\0UTF8\0"));
        let mut keydata = 4242i32.to_be_bytes().to_vec();
        keydata.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);
        script.extend(message(b'K', &keydata));
        script.extend(ready(b'I'));

        let mut row_description = 1u16.to_be_bytes().to_vec();
        row_description.extend_from_slice(b"version\0");
        row_description.extend_from_slice(&0u32.to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        row_description.extend_from_slice(&25u32.to_be_bytes());
        row_description.extend_from_slice(&(-1i16).to_be_bytes());
        row_description.extend_from_slice(&(-1i32).to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        script.extend(message(b'T', &row_description));

        let value = b"PostgreSQL 18.6 on x86_64-pc-linux-gnu";
        let mut data_row = 1u16.to_be_bytes().to_vec();
        data_row.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
        data_row.extend_from_slice(value);
        script.extend(message(b'D', &data_row));
        script.extend(message(b'C', b"SELECT 1\0"));
        script.extend(ready(b'I'));

        let info = conninfo("user=alice dbname=postgres");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        assert_eq!(conn.backend_pid(), 4242);
        assert_eq!(conn.cancel_key(), [0xaa, 0xbb, 0xcc, 0xdd]);
        assert_eq!(conn.parameter_status(b"server_version"), Some(&b"18.6"[..]));
        assert_eq!(conn.transaction_status(), TransactionStatus::Idle);

        let results = conn.exec(b"select version()").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::TuplesOk);
        assert_eq!(results[0].ntuples(), 1);
        assert_eq!(results[0].nfields(), 1);
        assert_eq!(results[0].fname(0), Some(&b"version"[..]));
        assert_eq!(results[0].value(0, 0), Some(&value[..]));
        assert_eq!(results[0].command_status(), b"SELECT 1");

        // And the client wrote a startup packet followed by exactly one Query.
        let written = conn.stream.to_server.clone();
        let startup = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(&info),
        }
        .encode();
        assert!(written.starts_with(&startup));
        assert_eq!(
            &written[startup.len()..],
            &Frontend::Query(b"select version()".to_vec()).encode()[..]
        );
    }

    /// An `AuthenticationMD5Password` is answered with the double hash, and
    /// the bytes on the wire are the PasswordMessage upstream would send.
    #[test]
    fn an_md5_connection_sends_the_hashed_password() {
        let mut script = message(b'R', &{
            let mut body = 5u32.to_be_bytes().to_vec();
            body.extend_from_slice(&[0x01, 0x02, 0x03, 0x04]);
            body
        });
        script.extend(auth_ok());
        script.extend(ready(b'I'));

        let info = conninfo("user=alice password=secret dbname=postgres");
        let conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();

        // The fixed vector, so the bytes on the wire are checked against a
        // value neither `md5_encrypt` nor this test computed (system
        // `md5sum`: md5("secretalice"), then md5 of that hex plus the salt).
        let expected = b"md598a0412b9c31436fc53776e863350083".to_vec();
        let startup_len = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(&info),
        }
        .encode()
        .len();
        assert_eq!(
            &conn.stream.to_server[startup_len..],
            &Frontend::PasswordMessage(expected).encode()[..]
        );
    }

    /// The whole SCRAM-SHA-256 handshake over the wire, using the RFC 7677
    /// vector as the recorded server side.
    #[test]
    fn a_scram_connection_completes_the_exchange() {
        let mut sasl = 10u32.to_be_bytes().to_vec();
        sasl.extend_from_slice(b"SCRAM-SHA-256\0\0");
        let mut script = message(b'R', &sasl);

        let mut challenge = 11u32.to_be_bytes().to_vec();
        challenge.extend_from_slice(
            b"r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,s=W22ZaJ0SNY7soEsUEjb6gQ==,i=4096",
        );
        script.extend(message(b'R', &challenge));

        let mut fin = 12u32.to_be_bytes().to_vec();
        fin.extend_from_slice(b"v=3HO6Qt1M4MKJrmlKaoOqLAI0/0TV0HZe7J9H3MBtSOg=");
        script.extend(message(b'R', &fin));
        script.extend(auth_ok());
        script.extend(ready(b'I'));

        let raw_nonce = crate::base64::decode(b"rOprNGfwEbeRWgbNEkqO").unwrap();
        let info = conninfo("user=user password=pencil dbname=postgres");
        let conn = Connection::start_up(Scripted::new(script), &info, &raw_nonce).unwrap();

        let startup_len = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(&info),
        }
        .encode()
        .len();
        let mut expected = Frontend::SaslInitialResponse {
            mechanism: b"SCRAM-SHA-256".to_vec(),
            initial_response: Some(b"n,,n=,r=rOprNGfwEbeRWgbNEkqO".to_vec()),
        }
        .encode();
        expected.extend(
            Frontend::SaslResponse(
                b"c=biws,r=rOprNGfwEbeRWgbNEkqO%hvYDpWUa2RaTCAfuxFIlj)hNlF$k0,p=qvT2SWdEH5Q06albL+hjSYuUhCG7VndFyzIb7CK4n9k=".to_vec(),
            )
            .encode(),
        );
        assert_eq!(&conn.stream.to_server[startup_len..], &expected[..]);
    }

    /// A server that asks for a password nobody supplied fails with
    /// `PQnoPasswordSupplied`, before anything is sent.
    #[test]
    fn a_password_request_without_a_password_fails() {
        let mut script = message(b'R', &3u32.to_be_bytes());
        script.extend(auth_ok());
        script.extend(ready(b'I'));
        let info = conninfo("user=alice dbname=postgres");
        let err = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap_err();
        assert_eq!(
            String::from_utf8(err.message()).unwrap(),
            "fe_sendauth: no password supplied\n"
        );
    }

    /// An ErrorResponse during startup — a wrong database name — comes back
    /// with every field the server sent.
    #[test]
    fn a_startup_error_carries_its_fields() {
        let mut body = Vec::new();
        for (code, value) in [
            (diag::SEVERITY, &b"FATAL"[..]),
            (diag::SQLSTATE, b"3D000"),
            (diag::MESSAGE_PRIMARY, b"database \"nope\" does not exist"),
        ] {
            body.push(code);
            body.extend_from_slice(value);
            body.push(0);
        }
        body.push(0);

        let info = conninfo("user=alice dbname=nope");
        let err =
            Connection::start_up(Scripted::new(message(b'E', &body)), &info, &[0; 18]).unwrap_err();
        let ConnectionError::Server(error) = &err else {
            panic!("not a server error: {err:?}");
        };
        assert_eq!(error.sqlstate(), Some(&b"3D000"[..]));
        assert_eq!(
            String::from_utf8(err.message()).unwrap(),
            "FATAL:  database \"nope\" does not exist\n"
        );
    }

    /// A query that errors: the result is PGRES_FATAL_ERROR with the fields,
    /// and `PQresultErrorMessage` renders them the way libpq does.
    #[test]
    fn a_failing_query_produces_a_fatal_error_result() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));

        let mut body = Vec::new();
        for (code, value) in [
            (diag::SEVERITY, &b"ERROR"[..]),
            (diag::SQLSTATE, b"42601"),
            (diag::MESSAGE_PRIMARY, b"syntax error at or near \"selct\""),
            (diag::STATEMENT_POSITION, b"1"),
            (diag::MESSAGE_HINT, b"Check your spelling."),
        ] {
            body.push(code);
            body.extend_from_slice(value);
            body.push(0);
        }
        body.push(0);
        script.extend(message(b'E', &body));
        script.extend(ready(b'E'));

        let info = conninfo("user=alice");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let results = conn.exec(b"selct 1").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::FatalError);
        assert_eq!(
            results[0]
                .error()
                .and_then(crate::result::ResultError::sqlstate),
            Some(&b"42601"[..])
        );
        assert_eq!(
            String::from_utf8(results[0].error_message()).unwrap(),
            "ERROR:  syntax error at or near \"selct\" at character 1\nHINT:  Check your spelling.\n"
        );
        assert_eq!(conn.transaction_status(), TransactionStatus::InError);
    }

    /// Notices arrive out of band: they are not results, and they do not stop
    /// the command (`fe-protocol3.c:158`).
    #[test]
    fn a_notice_is_collected_and_the_command_still_succeeds() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        let mut notice = Vec::new();
        for (code, value) in [
            (diag::SEVERITY, &b"NOTICE"[..]),
            (diag::SQLSTATE, b"00000"),
            (
                diag::MESSAGE_PRIMARY,
                b"table \"t\" does not exist, skipping",
            ),
        ] {
            notice.push(code);
            notice.extend_from_slice(value);
            notice.push(0);
        }
        notice.push(0);
        script.extend(message(b'N', &notice));
        script.extend(message(b'C', b"DROP TABLE\0"));
        script.extend(ready(b'I'));

        let info = conninfo("user=alice");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let results = conn.exec(b"drop table if exists t").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CommandOk);
        assert_eq!(results[0].command_status(), b"DROP TABLE");
        assert_eq!(conn.notices().len(), 1);
        assert_eq!(
            conn.notices()[0].field(diag::MESSAGE_PRIMARY),
            Some(&b"table \"t\" does not exist, skipping"[..])
        );
    }

    /// Two statements in one simple-query string produce two results, and an
    /// empty query string produces PGRES_EMPTY_QUERY.
    #[test]
    fn a_multi_statement_query_produces_one_result_per_statement() {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::CommandComplete(b"CREATE TABLE".to_vec()))
            .unwrap();
        runner
            .push(Backend::CommandComplete(b"INSERT 0 1".to_vec()))
            .unwrap();
        assert_eq!(
            runner.push(Backend::ReadyForQuery(TransactionStatus::Idle)),
            Ok(Flow::Done)
        );
        let results = runner.into_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].command_status(), b"CREATE TABLE");
        assert_eq!(results[1].command_status(), b"INSERT 0 1");

        let mut runner = QueryRunner::new();
        runner.push(Backend::EmptyQueryResponse).unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        assert_eq!(runner.results()[0].status(), ExecStatus::EmptyQuery);
    }

    /// The two malformed-stream cases `pqParseInput3` names for "D".
    #[test]
    fn a_data_row_out_of_place_is_refused() {
        let mut runner = QueryRunner::new();
        assert_eq!(
            runner.push(Backend::DataRow(vec![None])),
            Err(ProtocolError::DataWithoutRowDescription)
        );

        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(vec![text_field(b"a")]))
            .unwrap();
        assert_eq!(
            runner.push(Backend::DataRow(vec![None, None])),
            Err(ProtocolError::UnexpectedFieldCount)
        );
    }

    /// After an error result, later DataRows are ignored rather than
    /// misfiled (`fe-protocol3.c:882`).
    #[test]
    fn data_rows_after_an_error_are_ignored() {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(vec![text_field(b"a")]))
            .unwrap();
        runner
            .push(Backend::ErrorResponse(ResultError::new(vec![(
                diag::MESSAGE_PRIMARY,
                b"boom".to_vec(),
            )])))
            .unwrap();
        assert_eq!(
            runner.push(Backend::DataRow(vec![Some(b"x".to_vec())])),
            Ok(Flow::Continue)
        );
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::InError))
            .unwrap();
        let results = runner.into_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::FatalError);
    }

    /// A server that hangs up mid-message is reported, not hung on.
    #[test]
    fn a_truncated_stream_reports_the_closed_connection() {
        let info = conninfo("user=alice");
        let err =
            Connection::start_up(Scripted::new(b"R\0\0\0".to_vec()), &info, &[0; 18]).unwrap_err();
        assert!(matches!(err, ConnectionError::ServerClosedConnection));
        assert!(
            String::from_utf8(err.message())
                .unwrap()
                .starts_with("server closed the connection unexpectedly")
        );
    }

    /// A length word that cannot be right ends the connection with
    /// `handleSyncLoss`'s message (`fe-protocol3.c:506`).
    #[test]
    fn a_broken_length_word_is_lost_synchronization() {
        let info = conninfo("user=alice");
        let err = Connection::start_up(Scripted::new(b"Z\0\0\0\x01".to_vec()), &info, &[0; 18])
            .unwrap_err();
        assert_eq!(
            String::from_utf8(err.message()).unwrap(),
            "lost synchronization with server: got message type \"Z\", length 1"
        );
    }

    /// Messages split across reads are reassembled: the framing is over the
    /// buffer, not over one `read` (`pqParseInput3` is called after every
    /// `pqReadData`).
    #[test]
    fn a_message_split_across_reads_is_reassembled() {
        #[derive(Debug)]
        struct Dribble {
            bytes: Vec<u8>,
            pos: usize,
        }
        impl Read for Dribble {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.bytes.len() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.bytes[self.pos];
                self.pos += 1;
                Ok(1)
            }
        }
        impl Write for Dribble {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut script = auth_ok();
        script.extend(message(b'S', b"server_version\x0018.6\x00"));
        script.extend(ready(b'I'));
        let info = conninfo("user=alice");
        let conn = Connection::start_up(
            Dribble {
                bytes: script,
                pos: 0,
            },
            &info,
            &[0; 18],
        )
        .unwrap();
        assert_eq!(conn.parameter_status(b"server_version"), Some(&b"18.6"[..]));
    }

    /// `pg_strong_random` must give the SCRAM nonce its full length; a nonce
    /// that is short or constant would be a downgrade.
    #[test]
    fn the_nonce_comes_from_the_operating_system() {
        let first = strong_random(RAW_NONCE_LEN).unwrap();
        let second = strong_random(RAW_NONCE_LEN).unwrap();
        assert_eq!(first.len(), RAW_NONCE_LEN);
        assert_eq!(second.len(), RAW_NONCE_LEN);
        assert_ne!(first, second, "two draws must differ");
        assert_ne!(first, vec![0u8; RAW_NONCE_LEN]);
    }
}
