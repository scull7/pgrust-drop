//! The actions: a socket, the startup exchange, and the query calls over it.
//!
//! Ported from `src/interfaces/libpq/fe-connect.c` (`pqConnectDBComplete`'s
//! blocking loop, `:2782`, and `PQconnectPoll`'s `CONNECTION_AWAITING_RESPONSE`
//! state, `:3982`) and `fe-exec.c`: the asynchronous calls — `PQsendQuery`
//! (`:1433`) and its extended-query siblings, `PQgetResult` (`:2079`),
//! `PQisBusy` (`:2048`), `PQconsumeInput` (`:2001`), and pipeline mode from
//! `PQenterPipelineMode` (`:3073`) to `PQsendFlushRequest` (`:3402`) — and
//! the blocking ones built on them, `PQexec` (`:2279`) to `PQclosePortal`
//! (`:2556`), which are `PQexecStart`, one send, and `PQexecFinish` — and
//! the COPY data transfer, `PQputCopyData` (`:2712`), `PQputCopyEnd`
//! (`:2766`) and `PQgetCopyData` (`:2833`, with `fe-protocol3.c`'s
//! `pqGetCopyData3`, `:1907`).
//!
//! The decisions are pure and live above the socket: [`startup_parameters`]
//! and [`socket_address`] are functions of the `ConnInfo` alone, and
//! [`PipelineState`] decides what every message means and when it may be
//! parsed, without knowing where it came from — which is what lets the tests
//! below replay a whole authenticated session over a scripted stream.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::auth::{AuthError, AuthStep, Authenticator, ChannelBinding};
use crate::cancel::Peer;
use crate::conninfo::ConnInfo;
use crate::error::ConnError;
use crate::extended::{self, ArgumentError, Params, Plan, TypedCommand};
use crate::message::{
    Backend, Frame, Frontend, PROTOCOL_VERSION_3_0, ProtocolError, Target, TransactionStatus,
    next_copy_frame, next_frame,
};
use crate::pg_config::DEFAULT_PGSOCKET_DIR;
use crate::pipeline::{
    Admit, CopyStep, Event, Next, PipelineError, PipelineState, PipelineStatus, QueryClass,
    message_id,
};
use crate::result::{ExecStatus, FieldDescription, QueryResult, ResultError};
use crate::scram::RAW_NONCE_LEN;
use crate::trace::{self, AuthResponse, Origin, TraceFlags};

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
    /// An extended-query argument refused before anything was sent
    /// (`PQsendQueryParams` and its siblings, `fe-exec.c:1509`).
    Argument(ArgumentError),
    /// A call the connection's state refuses before anything is sent — a
    /// blocking call in pipeline mode, a second command outside it, leaving
    /// pipeline mode with results outstanding.
    Pipeline(PipelineError),
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
            ConnectionError::Argument(err) => err.message(),
            ConnectionError::Pipeline(err) => err.message(),
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

impl From<ArgumentError> for ConnectionError {
    fn from(err: ArgumentError) -> Self {
        ConnectionError::Argument(err)
    }
}

impl From<PipelineError> for ConnectionError {
    fn from(err: PipelineError) -> Self {
        ConnectionError::Pipeline(err)
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

    /// `conn->raddr` for a socket [`Stream::connect`] opened to `address`:
    /// the address it was dialled at, as C copies the address it is about to
    /// `connect()` to (`fe-connect.c:3249`) rather than asking the kernel
    /// afterwards.
    ///
    /// A Unix peer is the path this side dialled. `getpeername` is no
    /// substitute there: it answers with whatever the server bound — another
    /// spelling of the path, through a symlink or relative to the server's
    /// directory — and on Darwin with the whole `sun_path`, NUL padding
    /// included, which `std` keeps as part of the path, so a cancel sent to
    /// it cannot even be connected. A TCP peer is the resolved address the
    /// connect succeeded on, which `std` chooses among the name's addresses
    /// and reports only through `getpeername`; asked straight after the
    /// connect, that is the connect target. `None` when the TCP peer has
    /// already gone (`ENOTCONN`).
    #[must_use]
    pub fn raddr(&self, address: &Address) -> Option<Peer> {
        match (self, address) {
            (_, Address::Unix(path)) => Some(Peer::Unix(path.clone())),
            (Stream::Tcp(s), Address::Tcp { .. }) => s.peer_addr().ok().map(Peer::Tcp),
            (Stream::Unix(_), Address::Tcp { .. }) => None,
        }
    }

    /// Switch the socket between blocking and non-blocking reads.
    ///
    /// # Errors
    /// The socket refused the change.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            Stream::Tcp(s) => s.set_nonblocking(nonblocking),
            Stream::Unix(s) => s.set_nonblocking(nonblocking),
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

/// Where `PQtrace` writes: `conn->Pfdebug` and `conn->traceFlags`.
pub struct Tracer {
    sink: Box<dyn Write + Send>,
    flags: TraceFlags,
}

impl std::fmt::Debug for Tracer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracer")
            .field("flags", &self.flags)
            .finish_non_exhaustive()
    }
}

impl Tracer {
    /// Action: write one record. C ignores what `fprintf` returns, so a
    /// failing sink does not fail the query it is tracing.
    fn write(&mut self, line: &[u8]) {
        let mut record = trace::timestamp_prefix(self.flags, std::time::SystemTime::now());
        record.extend_from_slice(line);
        let _ = self.sink.write_all(&record);
    }
}

/// Calculation: which `p` message a frontend message is, as
/// `conn->current_auth_response` records it before each is sent
/// (`fe-auth.c:674`, `:781`, `:857`).
fn auth_response(message: &Frontend) -> AuthResponse {
    match message {
        Frontend::PasswordMessage(_) => AuthResponse::Password,
        Frontend::SaslInitialResponse { .. } => AuthResponse::SaslInitial,
        Frontend::SaslResponse(_) => AuthResponse::Sasl,
        _ => AuthResponse::None,
    }
}

/// What `PQgetCopyData` returned (`fe-exec.c:2823`), less its `-2`, which is
/// an `Err` here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyRead {
    /// One CopyData message's bytes — a row, for a text or CSV COPY (`> 0`).
    Row(Vec<u8>),
    /// No whole message is buffered yet, and the call was asked not to wait
    /// (`0`, `async` only).
    WouldBlock,
    /// The COPY is over (`-1`): collect its result with
    /// [`Connection::get_result`].
    End,
}

/// `pqPutMsgEnd`, `fe-misc.c:559`: output is pushed once this much is
/// buffered, "the typical size of a pipe buffer on Unix systems".
const PUT_MSG_PUSH_THRESHOLD: usize = 8192;

/// A live connection: `PGconn`, minus everything the query paths do not
/// need yet.
#[derive(Debug)]
pub struct Connection<S = Stream> {
    stream: S,
    inbuf: Vec<u8>,
    /// Consumed prefix of `inbuf`, so a read does not shift the buffer per
    /// message (`conn->inStart`).
    start: usize,
    /// `conn->outBuffer`: messages put but not yet flushed. In pipeline mode
    /// they wait here until a flush (`pqPipelineFlush`, `fe-exec.c:4047`).
    outbuf: Vec<u8>,
    /// `asyncStatus`, `pipelineStatus`, the command queue and the result
    /// being built.
    state: PipelineState,
    parameters: Vec<(Vec<u8>, Vec<u8>)>,
    backend_pid: i32,
    cancel_key: Vec<u8>,
    transaction_status: TransactionStatus,
    notices: Vec<ResultError>,
    /// `conn->notifyHead` … `notifyTail`: pid, channel, payload.
    notifications: Vec<(i32, Vec<u8>, Vec<u8>)>,
    trace: Option<Tracer>,
    /// `conn->raddr`: where the socket was connected, copied once when it
    /// was opened (`fe-connect.c:3249`), so nothing the peer does later
    /// changes it. `None` for a stream [`Connection::start_up`] was handed.
    raddr: Option<Peer>,
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
        let raddr = stream.raddr(&address);
        let nonce = strong_random(RAW_NONCE_LEN)?;
        let mut conn = Connection::start_up(stream, conninfo, &nonce)?;
        conn.raddr = raddr;
        Ok(conn)
    }

    /// `PQconsumeInput`, `fe-exec.c:2001`: read whatever the server has
    /// already sent, without waiting for more and without parsing it.
    ///
    /// # Errors
    /// The socket failed, or the server closed the connection.
    pub fn consume_input(&mut self) -> Result<(), ConnectionError> {
        self.stream.set_nonblocking(true)?;
        let read = self.read_more();
        self.stream.set_nonblocking(false)?;
        match read {
            Err(ConnectionError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => Ok(()),
            other => other,
        }
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
            outbuf: Vec::new(),
            state: PipelineState::new(),
            parameters: Vec::new(),
            backend_pid: 0,
            cancel_key: Vec::new(),
            transaction_status: TransactionStatus::Unknown,
            notices: Vec::new(),
            notifications: Vec::new(),
            trace: None,
            raddr: None,
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
    /// Every result is returned; `PQexec` itself returns the last of them
    /// (`PQexecFinish`, `fe-exec.c:2427`). Results a previous asynchronous
    /// command left uncollected are discarded first, as `PQexecStart` does
    /// (`fe-exec.c:2386`).
    ///
    /// # Errors
    /// In pipeline mode (`fe-exec.c:2376`, nothing is sent), or the
    /// connection broke, or the server sent something the query path cannot
    /// make a result of. A *failed query* is not an error here: it is a
    /// `PGRES_FATAL_ERROR` result, exactly as in libpq.
    pub fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_start()?;
        self.send_query(query)?;
        self.exec_finish()
    }

    /// `PQexecParams`, `fe-exec.c:2293`: `command` through the unnamed
    /// statement with out-of-line parameters.
    ///
    /// Every result is returned, as [`Connection::exec`] does.
    /// `param_types` is `paramTypes`, empty for NULL.
    ///
    /// # Errors
    /// In pipeline mode, an argument `PQsendQueryParams` refuses (nothing is
    /// sent), or the connection broke. A failed command is a
    /// `PGRES_FATAL_ERROR` result.
    pub fn exec_params(
        &mut self,
        command: &[u8],
        param_types: &[u32],
        params: &Params<'_>,
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_start()?;
        self.send_query_params(command, param_types, params)?;
        self.exec_finish()
    }

    /// `PQprepare`, `fe-exec.c:2323`: Parse `query` as the statement
    /// `statement`; the result is COMMAND_OK or the server's error.
    ///
    /// # Errors
    /// In pipeline mode, more than 65535 parameter types (nothing is sent),
    /// or the connection broke.
    pub fn prepare(
        &mut self,
        statement: &[u8],
        query: &[u8],
        param_types: &[u32],
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_start()?;
        self.send_prepare(statement, query, param_types)?;
        self.exec_finish()
    }

    /// `PQexecPrepared`, `fe-exec.c:2340`: run a prepared statement.
    ///
    /// # Errors
    /// In pipeline mode, an argument `PQsendQueryPrepared` refuses (nothing
    /// is sent), or the connection broke.
    pub fn exec_prepared(
        &mut self,
        statement: &[u8],
        params: &Params<'_>,
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_start()?;
        self.send_query_prepared(statement, params)?;
        self.exec_finish()
    }

    /// `PQdescribePrepared`, `fe-exec.c:2472`: a COMMAND_OK result whose
    /// `nparams`/`paramtype` and `nfields`/`ftype` describe the statement.
    ///
    /// # Errors
    /// In pipeline mode, or the connection broke.
    pub fn describe_prepared(
        &mut self,
        statement: &[u8],
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_typed(TypedCommand::Describe, Target::Statement, statement)
    }

    /// `PQdescribePortal`, `fe-exec.c:2491`.
    ///
    /// # Errors
    /// In pipeline mode, or the connection broke.
    pub fn describe_portal(&mut self, portal: &[u8]) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_typed(TypedCommand::Describe, Target::Portal, portal)
    }

    /// `PQclosePrepared`, `fe-exec.c:2538`. Closing a statement that does not
    /// exist is not an error.
    ///
    /// # Errors
    /// In pipeline mode, or the connection broke.
    pub fn close_prepared(
        &mut self,
        statement: &[u8],
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_typed(TypedCommand::Close, Target::Statement, statement)
    }

    /// `PQclosePortal`, `fe-exec.c:2556`.
    ///
    /// # Errors
    /// In pipeline mode, or the connection broke.
    pub fn close_portal(&mut self, portal: &[u8]) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_typed(TypedCommand::Close, Target::Portal, portal)
    }

    fn exec_typed(
        &mut self,
        command: TypedCommand,
        target: Target,
        name: &[u8],
    ) -> Result<Vec<QueryResult>, ConnectionError> {
        self.exec_start()?;
        self.send_typed(command, target, name)?;
        self.exec_finish()
    }

    /// `PQexecStart`, `fe-exec.c:2361`: refused in pipeline mode; otherwise
    /// "silently discard any prior query result that application didn't
    /// eat" (`:2386`).
    ///
    /// A COPY left running is ended the way `PQexecStart` ends it
    /// (`:2391`-`:2412`): COPY IN with a CopyFail, COPY OUT by dropping the
    /// rest of its data; COPY BOTH is refused.
    fn exec_start(&mut self) -> Result<(), ConnectionError> {
        self.state.begin_exec()?;
        while let Some(result) = self.get_result()? {
            match result.status() {
                ExecStatus::CopyIn => {
                    self.put_copy_end(Some(b"COPY terminated by new PQexec"))?;
                }
                ExecStatus::CopyOut => self.state.abandon_copy_out(),
                ExecStatus::CopyBoth => {
                    return Err(PipelineError::ExecDuringCopyBoth.into());
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `PQexecFinish`, `fe-exec.c:2427`, keeping every result rather than
    /// the last. It stops at a COPY result (`:2448`): the data transfer is
    /// the caller's, through [`Connection::put_copy_data`] or
    /// [`Connection::get_copy_data`].
    fn exec_finish(&mut self) -> Result<Vec<QueryResult>, ConnectionError> {
        let mut results = Vec::new();
        while let Some(result) = self.get_result()? {
            let copy = matches!(
                result.status(),
                ExecStatus::CopyIn | ExecStatus::CopyOut | ExecStatus::CopyBoth
            );
            results.push(result);
            if copy {
                break;
            }
        }
        Ok(results)
    }

    /// `PQputCopyData`, `fe-exec.c:2712`: send `data` as one CopyData
    /// message during COPY IN or COPY BOTH. Nothing is sent for no data.
    ///
    /// The message is buffered, and the buffer pushed once it holds 8 kB,
    /// as `pqPutMsgEnd` pushes it (`fe-misc.c:559`); [`Connection::flush`]
    /// or [`Connection::put_copy_end`] sends the rest. The push is the
    /// whole buffer, C's TCP case: over a Unix socket C holds the last
    /// partial 8 kB back (`:577`) for tidier pipe writes, which changes when
    /// bytes leave but not which bytes do.
    ///
    /// # Errors
    /// No COPY is taking data (`fe-exec.c:2719`), or the write failed.
    pub fn put_copy_data(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
        self.state.begin_put_copy()?;
        // fe-exec.c:2731 — deal with notices and notifications already read,
        // so a long COPY does not pile them up.
        self.parse_input()?;
        if !data.is_empty() {
            self.put_message(&Frontend::CopyData(data.to_vec()));
            if self.outbuf.len() >= PUT_MSG_PUSH_THRESHOLD {
                self.flush()?;
            }
        }
        Ok(())
    }

    /// `PQputCopyEnd`, `fe-exec.c:2766`: end COPY IN with a CopyDone, or
    /// fail it with a CopyFail carrying `error`; a COPY started by an
    /// extended-query command also gets its Sync (`:2801`). Everything
    /// buffered is flushed. The COPY command's result then comes from
    /// [`Connection::get_result`].
    ///
    /// # Errors
    /// No COPY is taking data (`fe-exec.c:2773`), or the write failed.
    pub fn put_copy_end(&mut self, error: Option<&[u8]>) -> Result<(), ConnectionError> {
        self.state.begin_put_copy()?;
        let end = match error {
            Some(message) => Frontend::CopyFail(message.to_vec()),
            None => Frontend::CopyDone,
        };
        self.put_message(&end);
        if self.state.put_copy_end()? {
            self.put_message(&Frontend::Sync);
        }
        self.flush()
    }

    /// `PQgetCopyData`, `fe-exec.c:2833`, and `pqGetCopyData3`,
    /// `fe-protocol3.c:1907`: the next CopyData during COPY OUT or COPY
    /// BOTH. Notices, notifications and ParameterStatus messages in between
    /// are dealt with as they come (`getCopyDataMessage`, `:1795`); an empty
    /// CopyData is skipped (`:1955`). With `nonblocking`, nothing is read
    /// from the socket — as with `async`, the caller reads with
    /// [`Connection::consume_input`].
    ///
    /// # Errors
    /// No COPY is sending data (`fe-exec.c:2841`), the stream lost
    /// synchronization, or the read failed — `PQgetCopyData`'s `-2`.
    pub fn get_copy_data(&mut self, nonblocking: bool) -> Result<CopyRead, ConnectionError> {
        self.state.begin_get_copy()?;
        loop {
            let (id, body) = match next_copy_frame(&self.inbuf[self.start..]) {
                // fe-protocol3.c:1926 — "Need to load more data".
                Frame::Incomplete => {
                    if nonblocking {
                        return Ok(CopyRead::WouldBlock);
                    }
                    self.read_more()?;
                    continue;
                }
                Frame::SyncLoss { id, length } => {
                    return Err(ProtocolError::LostSynchronization { id, length }.into());
                }
                Frame::Message { id, body } => (id, body),
            };
            match self.state.copy_message(id) {
                CopyStep::End => return Ok(CopyRead::End),
                CopyStep::Data => {
                    let Backend::CopyData(data) = self.parse_frame(id, body)? else {
                        unreachable!("a 'd' message decodes to CopyData");
                    };
                    if !data.is_empty() {
                        return Ok(CopyRead::Row(data));
                    }
                }
                CopyStep::Async => {
                    let message = self.parse_frame(id, body)?;
                    let event = self.state.apply(message)?;
                    self.take_event(event);
                }
            }
        }
    }

    /// `PQsendQuery`, `fe-exec.c:1433`: send one Query message and return;
    /// the results come from [`Connection::get_result`].
    ///
    /// # Errors
    /// In pipeline mode (`fe-exec.c:1459`), another command is still running,
    /// or the message could not be written.
    pub fn send_query(&mut self, query: &[u8]) -> Result<(), ConnectionError> {
        self.state.begin_send(QueryClass::Simple)?;
        self.dispatch(Plan {
            messages: vec![Frontend::Query(query.to_vec())],
            class: QueryClass::Simple,
        })
    }

    /// `PQsendQueryParams`, `fe-exec.c:1509`.
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, an argument is
    /// refused, or the messages could not be written.
    pub fn send_query_params(
        &mut self,
        command: &[u8],
        param_types: &[u32],
        params: &Params<'_>,
    ) -> Result<(), ConnectionError> {
        self.state.begin_send(QueryClass::Extended)?;
        let plan = extended::query_params(command, param_types, params)?;
        self.dispatch(plan)
    }

    /// `PQsendPrepare`, `fe-exec.c:1553`.
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, more than 65535
    /// parameter types, or the messages could not be written.
    pub fn send_prepare(
        &mut self,
        statement: &[u8],
        query: &[u8],
        param_types: &[u32],
    ) -> Result<(), ConnectionError> {
        self.state.begin_send(QueryClass::Prepare)?;
        let plan = extended::prepare(statement, query, param_types)?;
        self.dispatch(plan)
    }

    /// `PQsendQueryPrepared`, `fe-exec.c:1650`.
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, an argument is
    /// refused, or the messages could not be written.
    pub fn send_query_prepared(
        &mut self,
        statement: &[u8],
        params: &Params<'_>,
    ) -> Result<(), ConnectionError> {
        self.state.begin_send(QueryClass::Extended)?;
        let plan = extended::query_prepared(statement, params)?;
        self.dispatch(plan)
    }

    /// `PQsendDescribePrepared`, `fe-exec.c:2508`.
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, or the messages
    /// could not be written.
    pub fn send_describe_prepared(&mut self, statement: &[u8]) -> Result<(), ConnectionError> {
        self.send_typed(TypedCommand::Describe, Target::Statement, statement)
    }

    /// `PQsendDescribePortal`, `fe-exec.c:2521`.
    ///
    /// # Errors
    /// As [`Connection::send_describe_prepared`].
    pub fn send_describe_portal(&mut self, portal: &[u8]) -> Result<(), ConnectionError> {
        self.send_typed(TypedCommand::Describe, Target::Portal, portal)
    }

    /// `PQsendClosePrepared`, `fe-exec.c:2573`.
    ///
    /// # Errors
    /// As [`Connection::send_describe_prepared`].
    pub fn send_close_prepared(&mut self, statement: &[u8]) -> Result<(), ConnectionError> {
        self.send_typed(TypedCommand::Close, Target::Statement, statement)
    }

    /// `PQsendClosePortal`, `fe-exec.c:2586`.
    ///
    /// # Errors
    /// As [`Connection::send_describe_prepared`].
    pub fn send_close_portal(&mut self, portal: &[u8]) -> Result<(), ConnectionError> {
        self.send_typed(TypedCommand::Close, Target::Portal, portal)
    }

    /// `PQsendTypedCommand`, `fe-exec.c:2606`.
    fn send_typed(
        &mut self,
        command: TypedCommand,
        target: Target,
        name: &[u8],
    ) -> Result<(), ConnectionError> {
        let plan = extended::typed_command(command, target, name);
        self.state.begin_send(plan.class)?;
        self.dispatch(plan)
    }

    /// The common tail of every `PQsend*`: put the plan's messages (less
    /// its Sync in pipeline mode), give them a push if `pqPipelineFlush`
    /// would (`fe-exec.c:4047`), and queue the command
    /// (`pqAppendCmdQueueEntry`, `:1356`).
    fn dispatch(&mut self, plan: Plan) -> Result<(), ConnectionError> {
        let plan = if self.state.sends_own_sync() {
            plan
        } else {
            plan.without_sync()
        };
        for message in &plan.messages {
            self.put_message(message);
        }
        if self.state.flushes_now(self.outbuf.len()) {
            self.flush()?;
        }
        self.state.append(plan.class);
        Ok(())
    }

    /// `PQenterPipelineMode`, `fe-exec.c:3073`. Nothing is sent.
    ///
    /// # Errors
    /// A command is still running outside pipeline mode.
    pub fn enter_pipeline_mode(&mut self) -> Result<(), ConnectionError> {
        Ok(self.state.enter_pipeline_mode()?)
    }

    /// `PQexitPipelineMode`, `fe-exec.c:3104`: back to one command at a
    /// time, flushing whatever is still buffered. Leaving a mode that is not
    /// on succeeds.
    ///
    /// # Errors
    /// Results are still to be collected, or a command is still running, or
    /// the flush failed.
    pub fn exit_pipeline_mode(&mut self) -> Result<(), ConnectionError> {
        if self.state.exit_pipeline_mode()? {
            self.flush()?;
        }
        Ok(())
    }

    /// `PQpipelineStatus`.
    #[must_use]
    pub fn pipeline_status(&self) -> PipelineStatus {
        self.state.pipeline_status()
    }

    /// `PQpipelineSync`, `fe-exec.c:3303`: end the pipeline with a Sync and
    /// flush everything queued.
    ///
    /// # Errors
    /// Not in pipeline mode, or the flush failed.
    pub fn pipeline_sync(&mut self) -> Result<(), ConnectionError> {
        self.pipeline_sync_internal(true)
    }

    /// `PQsendPipelineSync`, `fe-exec.c:3313`: the Sync without the flush,
    /// unless the buffer is past the threshold.
    ///
    /// # Errors
    /// Not in pipeline mode, or a flush past the threshold failed.
    pub fn send_pipeline_sync(&mut self) -> Result<(), ConnectionError> {
        self.pipeline_sync_internal(false)
    }

    /// `pqPipelineSyncInternal`, `fe-exec.c:3325`.
    fn pipeline_sync_internal(&mut self, immediate_flush: bool) -> Result<(), ConnectionError> {
        self.state.begin_pipeline_sync()?;
        self.put_message(&Frontend::Sync);
        if immediate_flush || self.state.flushes_now(self.outbuf.len()) {
            self.flush()?;
        }
        self.state.append(QueryClass::Sync);
        Ok(())
    }

    /// `PQsendFlushRequest`, `fe-exec.c:3402`: ask the server to send what
    /// it has, without ending the pipeline. No command is queued.
    ///
    /// # Errors
    /// Another command is running outside pipeline mode, or a flush failed.
    pub fn send_flush_request(&mut self) -> Result<(), ConnectionError> {
        self.state.begin_flush_request()?;
        self.put_message(&Frontend::Flush);
        if self.state.flushes_now(self.outbuf.len()) {
            self.flush()?;
        }
        Ok(())
    }

    /// `PQsetSingleRowMode`, `fe-exec.c:1965`: `true` when the mode was
    /// set, which is only right after a command is sent.
    pub fn set_single_row_mode(&mut self) -> bool {
        self.state.set_single_row_mode()
    }

    /// `PQsetChunkedRowsMode`, `fe-exec.c:1982`.
    pub fn set_chunked_rows_mode(&mut self, chunk_size: usize) -> bool {
        self.state.set_chunked_rows_mode(chunk_size)
    }

    /// `PQgetResult`, `fe-exec.c:2079`: the next result, or `None` for the
    /// NULL that ends a command — blocking until the server has said enough.
    ///
    /// # Errors
    /// The connection broke, or the server sent something no result can be
    /// made of.
    pub fn get_result(&mut self) -> Result<Option<QueryResult>, ConnectionError> {
        self.parse_input()?;
        loop {
            match self.state.next_result() {
                Next::Result(result) => return Ok(Some(result)),
                Next::Null => return Ok(None),
                // fe-exec.c:2094 — send what is unsent, else we may be
                // waiting for a reply to a command the server never got;
                // then wait for more and parse it.
                Next::Block => {
                    self.flush()?;
                    self.read_more()?;
                    self.parse_input()?;
                }
            }
        }
    }

    /// `PQisBusy`, `fe-exec.c:2048`: would [`Connection::get_result`] block?
    /// Parses what has already been read, and reads nothing.
    ///
    /// # Errors
    /// What has been read does not parse.
    pub fn is_busy(&mut self) -> Result<bool, ConnectionError> {
        self.parse_input()?;
        Ok(self.state.is_busy())
    }

    /// `PQflush`, `fe-exec.c:4031`: write everything buffered.
    ///
    /// # Errors
    /// The socket write failed.
    pub fn flush(&mut self) -> Result<(), ConnectionError> {
        if !self.outbuf.is_empty() {
            self.stream.write_all(&self.outbuf)?;
            self.outbuf.clear();
        }
        self.stream.flush()?;
        Ok(())
    }

    /// `pqPutMsgStart` … `pqPutMsgEnd`: encode one message into the output
    /// buffer, tracing it as `pqPutMsgEnd` does (`fe-misc.c:546`).
    fn put_message(&mut self, message: &Frontend) {
        let encoded = message.encode();
        self.trace_sent(message, &encoded);
        self.outbuf.extend_from_slice(&encoded);
    }

    /// `pqParseInput3`, `fe-protocol3.c:71`: parse every whole message in
    /// the buffer that the state admits, stopping at the first it does not.
    /// Reads nothing.
    fn parse_input(&mut self) -> Result<(), ConnectionError> {
        loop {
            let (id, body) = match next_frame(&self.inbuf[self.start..]) {
                Frame::Incomplete => return Ok(()),
                Frame::SyncLoss { id, length } => {
                    return Err(ProtocolError::LostSynchronization { id, length }.into());
                }
                Frame::Message { id, body } => (id, body),
            };
            if self.state.admit(id) == Admit::Wait {
                return Ok(());
            }
            let message = self.parse_frame(id, body)?;
            let event = self.state.apply(message)?;
            self.take_event(event);
        }
    }

    /// The connection-level side of a parsed message.
    fn take_event(&mut self, event: Option<Event>) {
        match event {
            None => {}
            Some(Event::Notice(notice)) => self.notices.push(notice),
            Some(Event::Notification {
                pid,
                channel,
                payload,
            }) => self.notifications.push((pid, channel, payload)),
            Some(Event::ParameterStatus { name, value }) => {
                self.parameters.push((name, value));
            }
            Some(Event::ReadyForQuery(status)) => self.transaction_status = status,
        }
    }

    /// `PQfinish`'s Terminate, `fe-connect.c:5239`, flushed with whatever is
    /// still buffered.
    ///
    /// # Errors
    /// The message could not be written to the socket.
    pub fn terminate(&mut self) -> Result<(), ConnectionError> {
        self.put_message(&Frontend::Terminate);
        self.flush()
    }

    fn send(&mut self, message: &Frontend) -> Result<(), ConnectionError> {
        self.put_message(message);
        self.flush()
    }

    /// Decode the whole message at the head of the buffer, trace it, and
    /// consume it — `pqParseDone` traces a message once it has been parsed,
    /// and only then (`fe-misc.c:448`).
    fn parse_frame(
        &mut self,
        id: u8,
        body: std::ops::Range<usize>,
    ) -> Result<Backend, ConnectionError> {
        let start = self.start;
        let message = Backend::decode(id, &self.inbuf[start + body.start..start + body.end])?;
        if let Some(tracer) = &mut self.trace {
            let line = trace::message_line(
                &self.inbuf[start..start + body.end],
                Origin::Backend,
                tracer.flags,
                AuthResponse::None,
            );
            tracer.write(&line);
        }
        self.start += body.end;
        if self.start == self.inbuf.len() {
            self.inbuf.clear();
            self.start = 0;
        }
        Ok(message)
    }

    /// `pqReadData`, blocking: append what one read returns, after moving
    /// what is left unparsed to the front of the buffer (`fe-misc.c:659`) —
    /// without that, a long COPY OUT would keep every byte it ever read.
    fn read_more(&mut self) -> Result<(), ConnectionError> {
        if self.start > 0 {
            self.inbuf.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = [0u8; 8192];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(ConnectionError::ServerClosedConnection);
        }
        self.inbuf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    /// The startup exchange's reader: the next whole message, reading more
    /// bytes when the buffer does not hold one yet.
    fn read_message(&mut self) -> Result<Backend, ConnectionError> {
        loop {
            match next_frame(&self.inbuf[self.start..]) {
                Frame::Message { id, body } => return self.parse_frame(id, body),
                Frame::SyncLoss { id, length } => {
                    return Err(ProtocolError::LostSynchronization { id, length }.into());
                }
                Frame::Incomplete => self.read_more()?,
            }
        }
    }

    /// `PQtrace`, `fe-trace.c:35`: from now on, write every message sent and
    /// parsed to `sink`, with the flags reset. A sink still installed from an
    /// earlier `trace` is flushed and then dropped (closing it, if it is a
    /// `File`), where `PQtrace` only forgets the old `FILE *` (`fe-trace.c:53`).
    /// To keep the old sink, call [`Connection::untrace`] first; it hands the
    /// sink back.
    ///
    /// Tracing starts on a connected `Connection`, so the startup exchange is
    /// never traced — as with `PQconnectdb` followed by `PQtrace`.
    pub fn trace(&mut self, sink: Box<dyn Write + Send>) {
        self.untrace();
        self.trace = Some(Tracer {
            sink,
            flags: TraceFlags::NONE,
        });
    }

    /// `PQuntrace`, `fe-trace.c:49`: flush the sink and stop tracing. The sink
    /// is handed back, since C's `FILE *` was never libpq's to close
    /// (`fe-connect.c:5114`).
    pub fn untrace(&mut self) -> Option<Box<dyn Write + Send>> {
        let mut tracer = self.trace.take()?;
        let _ = tracer.sink.flush();
        Some(tracer.sink)
    }

    /// `PQsetTraceFlags`, `fe-trace.c:64`: does nothing when not tracing.
    pub fn set_trace_flags(&mut self, flags: TraceFlags) {
        if let Some(tracer) = &mut self.trace {
            tracer.flags = flags;
        }
    }

    /// `pqPutMsgEnd`'s trace, `fe-misc.c:546`: each message is traced as it
    /// is completed, before it is written.
    fn trace_sent(&mut self, message: &Frontend, encoded: &[u8]) {
        let Some(tracer) = &mut self.trace else {
            return;
        };
        let line = if matches!(message, Frontend::Startup { .. }) {
            trace::no_type_byte_message_line(encoded, tracer.flags)
        } else {
            trace::message_line(
                encoded,
                Origin::Frontend,
                tracer.flags,
                auth_response(message),
            )
        };
        tracer.write(&line);
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

    /// The cancel key BackendKeyData carried (`conn->be_cancel_key`), which
    /// [`Connection::get_cancel`] and [`Connection::cancel_create`] copy.
    /// Empty when the server sent none.
    #[must_use]
    pub fn cancel_key(&self) -> &[u8] {
        &self.cancel_key
    }

    /// `conn->raddr`, the address a cancel request must reach
    /// (`fe-cancel.c:170`, `:406`): recorded once by
    /// [`Connection::connect`], so a peer that has since reset the socket
    /// does not lose it. `None` only when the stream was not opened there,
    /// or a TCP peer was gone before it could be recorded.
    #[must_use]
    pub fn peer(&self) -> Option<Peer> {
        self.raddr.clone()
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

    /// The notifications collected so far, oldest first — what `PQnotifies`
    /// hands out one at a time: pid, channel, payload.
    #[must_use]
    pub fn notifications(&self) -> &[(i32, Vec<u8>, Vec<u8>)] {
        &self.notifications
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
    ///
    /// A server answers only what it has been sent, so the bytes are
    /// released in turns: the recording is cut after each ReadyForQuery, and
    /// each write from the client releases the next turn. Replaying it all
    /// at once would put a query's replies in the buffer before the query,
    /// where `pqParseInput3` treats them as arriving while idle.
    #[derive(Debug)]
    struct Scripted {
        turns: std::collections::VecDeque<Vec<u8>>,
        from_server: Vec<u8>,
        read_pos: usize,
        to_server: Vec<u8>,
    }

    impl Scripted {
        fn new(recording: impl AsRef<[u8]>) -> Self {
            let recording = recording.as_ref();
            let mut turns = std::collections::VecDeque::new();
            let mut turn = Vec::new();
            let mut rest = recording;
            while rest.len() >= 5 {
                let length = u32::from_be_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
                // A broken or cut-off message is replayed as it is, whole.
                if length < 4 || 1 + length > rest.len() {
                    break;
                }
                let end = 1 + length;
                turn.extend_from_slice(&rest[..end]);
                if rest[0] == b'Z' {
                    turns.push_back(std::mem::take(&mut turn));
                }
                rest = &rest[end..];
            }
            turn.extend_from_slice(rest);
            if !turn.is_empty() {
                turns.push_back(turn);
            }
            Self {
                turns,
                from_server: Vec::new(),
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
            if let Some(turn) = self.turns.pop_front() {
                self.from_server.extend(turn);
            }
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

    /// `PQexecParams` over the wire: the client writes Parse, Bind,
    /// Describe, Execute and Sync in one go, and the replies of
    /// `traces/simple_pipeline.trace` lines 6-11 come back as one TUPLES_OK
    /// result.
    #[test]
    fn exec_params_sends_the_extended_query_and_reads_its_result() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend(message(b'1', b""));
        script.extend(message(b'2', b""));
        let mut row_description = 1u16.to_be_bytes().to_vec();
        row_description.extend_from_slice(b"?column?\0");
        row_description.extend_from_slice(&0u32.to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        row_description.extend_from_slice(&23u32.to_be_bytes());
        row_description.extend_from_slice(&4i16.to_be_bytes());
        row_description.extend_from_slice(&(-1i32).to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        script.extend(message(b'T', &row_description));
        let mut data_row = 1u16.to_be_bytes().to_vec();
        data_row.extend_from_slice(&1u32.to_be_bytes());
        data_row.push(b'1');
        script.extend(message(b'D', &data_row));
        script.extend(message(b'C', b"SELECT 1\0"));
        script.extend(ready(b'I'));

        let info = conninfo("user=alice");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let written_before = conn.stream.to_server.len();
        let results = conn
            .exec_params(b"SELECT $1", &[23], &Params::text(&[Some(b"1")]))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::TuplesOk);
        assert_eq!(results[0].ftype(0), Some(23));
        assert_eq!(results[0].value(0, 0), Some(&b"1"[..]));

        let plan =
            extended::query_params(b"SELECT $1", &[23], &Params::text(&[Some(b"1")])).unwrap();
        let expected: Vec<u8> = plan.messages.iter().flat_map(Frontend::encode).collect();
        assert_eq!(&conn.stream.to_server[written_before..], &expected[..]);
    }

    /// An argument C refuses is refused before a byte is written, with C's
    /// message (`fe-exec.c:1527`).
    #[test]
    fn a_refused_argument_sends_nothing() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        let info = conninfo("user=alice");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let written_before = conn.stream.to_server.len();
        let values = vec![None; extended::PQ_QUERY_PARAM_MAX_LIMIT + 1];
        let error = conn
            .exec_params(b"SELECT 1", &[], &Params::text(&values))
            .unwrap_err();
        assert!(matches!(
            error,
            ConnectionError::Argument(ArgumentError::TooManyParameters)
        ));
        assert_eq!(
            error.message(),
            b"number of parameters must be between 0 and 65535".to_vec()
        );
        assert_eq!(conn.stream.to_server.len(), written_before);
    }

    /// A server that hangs up mid-message is reported, not hung on.
    #[test]
    fn a_truncated_stream_reports_the_closed_connection() {
        let info = conninfo("user=alice");
        let err = Connection::start_up(Scripted::new(b"R\0\0\0"), &info, &[0; 18]).unwrap_err();
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
        let err = Connection::start_up(Scripted::new(b"Z\0\0\0\x01"), &info, &[0; 18]).unwrap_err();
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

    /// A sink the test can read back while the connection still owns it.
    #[derive(Clone, Default)]
    struct SharedSink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// `PQtrace` + `PQsetTraceFlags`: every message sent is traced before
    /// it is written and every message parsed after it is read, in the order
    /// libpq writes them (`fe-misc.c:546`, `:448`); `PQuntrace` stops it.
    #[test]
    fn a_traced_exchange_writes_one_line_per_message_sent_and_parsed() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend(message(b'3', b""));
        script.extend(ready(b'I'));
        script.extend(message(b'C', b"BEGIN\0"));
        script.extend(ready(b'T'));

        let info = conninfo("user=alice dbname=postgres");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let sink = SharedSink::default();
        conn.trace(Box::new(sink.clone()));
        conn.set_trace_flags(TraceFlags::SUPPRESS_TIMESTAMPS | TraceFlags::REGRESS_MODE);

        conn.close_prepared(b"select_one").unwrap();
        let expected: &[u8] = b"F\t16\tClose\t S \"select_one\"\n\
              F\t4\tSync\n\
              B\t4\tCloseComplete\n\
              B\t5\tReadyForQuery\t I\n";
        assert_eq!(sink.0.lock().unwrap().as_slice(), expected);

        assert!(conn.untrace().is_some());
        conn.exec(b"BEGIN").unwrap();
        conn.set_trace_flags(TraceFlags::REGRESS_MODE);
        assert!(
            conn.untrace().is_none(),
            "untraced, and flags are not a trace"
        );
        assert_eq!(
            sink.0.lock().unwrap().as_slice(),
            expected,
            "nothing after PQuntrace"
        );
    }

    /// A connection that is not traced writes nothing and keeps no flags:
    /// `PQsetTraceFlags` before `PQtrace` does nothing (`fe-trace.c:68`),
    /// and `PQtrace` resets the flags to 0 (`fe-trace.c:44`).
    #[test]
    fn trace_flags_need_a_trace_and_a_new_trace_resets_them() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend(message(b'Z', b"I"));
        let info = conninfo("user=alice dbname=postgres");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        conn.set_trace_flags(TraceFlags::SUPPRESS_TIMESTAMPS);
        let sink = SharedSink::default();
        conn.trace(Box::new(sink.clone()));
        conn.terminate().unwrap();
        let written = sink.0.lock().unwrap().clone();
        // Timestamps on: `YYYY-MM-DD HH:MM:SS.uuuuuu\t` then the record.
        assert_eq!(written.len(), 27 + b"F\t4\tTerminate\n".len());
        assert_eq!(written[26], b'\t');
        assert!(written.ends_with(b"F\t4\tTerminate\n"));
    }

    #[test]
    fn each_p_message_records_which_auth_response_it_is() {
        assert_eq!(
            auth_response(&Frontend::PasswordMessage(b"x".to_vec())),
            AuthResponse::Password
        );
        assert_eq!(
            auth_response(&Frontend::SaslInitialResponse {
                mechanism: b"SCRAM-SHA-256".to_vec(),
                initial_response: None,
            }),
            AuthResponse::SaslInitial
        );
        assert_eq!(
            auth_response(&Frontend::SaslResponse(Vec::new())),
            AuthResponse::Sasl
        );
        assert_eq!(auth_response(&Frontend::Sync), AuthResponse::None);
    }

    /// `test_simple_pipeline` (`libpq_pipeline.c:1593`) replayed without a
    /// server: the replies of `traces/simple_pipeline.trace` lines 6-11 as
    /// recorded bytes, and the whole trace this connection writes compared
    /// with the file, byte for byte. The live gate is
    /// `tests/t_001_libpq_pipeline.rs`; this one needs no PostgreSQL.
    #[test]
    fn simple_pipeline_trace_replayed_without_a_server() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend(message(b'1', b""));
        script.extend(message(b'2', b""));
        let mut row_description = 1u16.to_be_bytes().to_vec();
        row_description.extend_from_slice(b"?column?\0");
        row_description.extend_from_slice(&0u32.to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        row_description.extend_from_slice(&23u32.to_be_bytes());
        row_description.extend_from_slice(&4i16.to_be_bytes());
        row_description.extend_from_slice(&(-1i32).to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        script.extend(message(b'T', &row_description));
        let mut data_row = 1u16.to_be_bytes().to_vec();
        data_row.extend_from_slice(&1u32.to_be_bytes());
        data_row.push(b'1');
        script.extend(message(b'D', &data_row));
        script.extend(message(b'C', b"SELECT 1\0"));
        script.extend(ready(b'I'));

        let info = conninfo("user=alice dbname=postgres");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        let sink = SharedSink::default();
        conn.trace(Box::new(sink.clone()));
        conn.set_trace_flags(TraceFlags::SUPPRESS_TIMESTAMPS | TraceFlags::REGRESS_MODE);

        conn.enter_pipeline_mode().unwrap();
        let written = conn.stream.to_server.len();
        conn.send_query_params(b"SELECT $1", &[23], &Params::text(&[Some(b"1")]))
            .unwrap();
        assert_eq!(
            conn.stream.to_server.len(),
            written,
            "a pipelined command waits in the buffer"
        );
        assert!(matches!(
            conn.exit_pipeline_mode(),
            Err(ConnectionError::Pipeline(PipelineError::Busy))
        ));
        conn.pipeline_sync().unwrap();
        let result = conn.get_result().unwrap().expect("a result");
        assert_eq!(result.status(), ExecStatus::TuplesOk);
        assert!(conn.get_result().unwrap().is_none());
        assert!(conn.exit_pipeline_mode().is_err());
        let sync = conn.get_result().unwrap().expect("the sync");
        assert_eq!(sync.status(), ExecStatus::PipelineSync);
        assert!(conn.get_result().unwrap().is_none());
        assert_eq!(conn.pipeline_status(), PipelineStatus::On);
        conn.exit_pipeline_mode().unwrap();
        assert_eq!(conn.pipeline_status(), PipelineStatus::Off);
        conn.terminate().unwrap();

        let expected = include_bytes!("../tests/traces/simple_pipeline.trace");
        assert_eq!(
            String::from_utf8_lossy(&sink.0.lock().unwrap()),
            String::from_utf8_lossy(expected)
        );
    }

    /// The blocking calls are refused in pipeline mode before anything is
    /// sent or read, with upstream's message (`fe-exec.c:2378`), and so is
    /// `PQsendQuery` (`:1461`).
    #[test]
    fn a_blocking_call_in_pipeline_mode_sends_nothing() {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        let info = conninfo("user=alice");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        conn.enter_pipeline_mode().unwrap();
        let written = conn.stream.to_server.len();
        let error = conn.exec(b"SELECT 1").unwrap_err();
        assert_eq!(
            error.message(),
            b"synchronous command execution functions are not allowed in pipeline mode".to_vec()
        );
        assert!(conn.describe_prepared(b"s").is_err());
        let error = conn.send_query(b"SELECT 1").unwrap_err();
        assert_eq!(
            error.message(),
            b"PQsendQuery not allowed in pipeline mode".to_vec()
        );
        assert_eq!(conn.stream.to_server.len(), written);
        assert!(!conn.is_busy().unwrap());
    }

    /// A connected, idle session over `script`, whose first turn is the
    /// startup exchange.
    fn replayed(after_startup: &[u8]) -> Connection<Scripted> {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend_from_slice(after_startup);
        Connection::start_up(
            Scripted::new(script),
            &conninfo("user=alice dbname=postgres"),
            &[0; 18],
        )
        .unwrap()
    }

    /// `CopyOutResponse` with `n` text columns, as the server sends it.
    fn copy_response(id: u8, columns: u16) -> Vec<u8> {
        let mut body = vec![0];
        body.extend_from_slice(&columns.to_be_bytes());
        for _ in 0..columns {
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        message(id, &body)
    }

    /// A NoticeResponse carrying only a primary message.
    fn notice(text: &[u8]) -> Vec<u8> {
        let mut body = vec![b'M'];
        body.extend_from_slice(text);
        body.extend_from_slice(b"\0\0");
        message(b'N', &body)
    }

    /// COPY OUT end to end: `PQexec` stops at the COPY result, each
    /// `PQgetCopyData` returns one CopyData, a notice in between is taken
    /// and an empty CopyData skipped (`fe-protocol3.c:1955`), CopyDone is -1,
    /// and `PQgetResult` then reads the command's own result — with every
    /// message traced once, as it is parsed.
    #[test]
    fn a_copy_out_hands_over_each_copy_data_then_the_result() {
        let mut replies = copy_response(b'H', 2);
        replies.extend(message(b'd', b"1\tone\n"));
        replies.extend(notice(b"midway"));
        replies.extend(message(b'd', b""));
        replies.extend(message(b'd', b"2\t\\N\n"));
        replies.extend(message(b'c', b""));
        replies.extend(message(b'C', b"COPY 2\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        let sink = SharedSink::default();
        conn.trace(Box::new(sink.clone()));
        conn.set_trace_flags(TraceFlags::SUPPRESS_TIMESTAMPS);

        let results = conn.exec(b"COPY t TO STDOUT").unwrap();
        assert_eq!(results.len(), 1, "PQexecFinish stops at the COPY result");
        assert_eq!(results[0].status(), ExecStatus::CopyOut);
        assert_eq!(results[0].nfields(), 2);

        assert_eq!(
            conn.get_copy_data(false).unwrap(),
            CopyRead::Row(b"1\tone\n".to_vec())
        );
        assert_eq!(
            conn.get_copy_data(false).unwrap(),
            CopyRead::Row(b"2\t\\N\n".to_vec())
        );
        assert_eq!(conn.notices().len(), 1, "the notice was taken on the way");
        assert_eq!(conn.get_copy_data(false).unwrap(), CopyRead::End);
        assert!(
            matches!(
                conn.get_copy_data(false),
                Err(ConnectionError::Pipeline(PipelineError::NoCopyInProgress))
            ),
            "the COPY is over"
        );

        let done = conn.get_result().unwrap().expect("the COPY's result");
        assert_eq!(done.status(), ExecStatus::CommandOk);
        assert_eq!(done.command_status(), b"COPY 2");
        assert!(conn.get_result().unwrap().is_none());

        // fe-trace.c prints a printable byte as itself, a backslash
        // included, and the rest as \xNN (`pqTraceOutputNchar`).
        let expected: &[u8] = b"F\t21\tQuery\t \"COPY t TO STDOUT\"\n\
              B\t11\tCopyOutResponse\t \\x00 2 0 0\n\
              B\t10\tCopyData\t '1\\x09one\\x0a'\n\
              B\t13\tNoticeResponse\t M \"midway\" \\x00\n\
              B\t4\tCopyData\t ''\n\
              B\t9\tCopyData\t '2\\x09\\N\\x0a'\n\
              B\t4\tCopyDone\n\
              B\t11\tCommandComplete\t \"COPY 2\"\n\
              B\t5\tReadyForQuery\t I\n";
        assert_eq!(
            String::from_utf8_lossy(sink.0.lock().unwrap().as_slice()),
            String::from_utf8_lossy(expected)
        );
    }

    /// With `nonblocking`, `PQgetCopyData` reads nothing and says 0 until a
    /// whole message is buffered (`fe-protocol3.c:1929`).
    #[test]
    fn a_nonblocking_get_copy_data_reads_nothing() {
        let mut replies = copy_response(b'H', 1);
        replies.extend(message(b'd', b"x\n"));
        replies.extend(message(b'c', b""));
        replies.extend(message(b'C', b"COPY 1\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        conn.send_query(b"COPY t TO STDOUT").unwrap();
        // Only the first read's worth: cut the buffer after the response.
        let copy_out = copy_response(b'H', 1).len();
        conn.read_more().unwrap();
        let buffered = conn.inbuf.split_off(copy_out);
        let result = conn.get_result().unwrap().expect("the COPY result");
        assert_eq!(result.status(), ExecStatus::CopyOut);
        assert_eq!(conn.get_copy_data(true).unwrap(), CopyRead::WouldBlock);
        conn.inbuf.extend(buffered);
        assert_eq!(
            conn.get_copy_data(true).unwrap(),
            CopyRead::Row(b"x\n".to_vec())
        );
        assert_eq!(conn.get_copy_data(true).unwrap(), CopyRead::End);
    }

    /// COPY IN end to end: data goes out as CopyData, `PQputCopyEnd` sends
    /// CopyDone — and, for a simple Query, no Sync (`fe-exec.c:2801`).
    #[test]
    fn a_copy_in_sends_copy_data_then_copy_done() {
        let mut replies = copy_response(b'G', 1);
        replies.extend(message(b'C', b"COPY 2\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        let results = conn.exec(b"COPY t FROM STDIN").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::CopyIn);
        assert!(matches!(
            conn.get_copy_data(false),
            Err(ConnectionError::Pipeline(PipelineError::NoCopyInProgress))
        ));
        let before = conn.stream.to_server.len();

        conn.put_copy_data(b"1\n").unwrap();
        conn.put_copy_data(b"").unwrap();
        conn.put_copy_data(b"2\n").unwrap();
        assert_eq!(
            conn.stream.to_server.len(),
            before,
            "under 8 kB, nothing is pushed yet"
        );
        conn.put_copy_end(None).unwrap();
        let mut sent = Frontend::CopyData(b"1\n".to_vec()).encode();
        sent.extend(Frontend::CopyData(b"2\n".to_vec()).encode());
        sent.extend(Frontend::CopyDone.encode());
        assert_eq!(&conn.stream.to_server[before..], &sent[..]);

        let done = conn.get_result().unwrap().expect("the COPY's result");
        assert_eq!(done.command_status(), b"COPY 2");
        assert!(conn.get_result().unwrap().is_none());
        assert!(matches!(
            conn.put_copy_data(b"3\n"),
            Err(ConnectionError::Pipeline(PipelineError::NoCopyInProgress))
        ));
    }

    /// `pqPutMsgEnd` pushes the buffer once it holds 8 kB (`fe-misc.c:559`).
    #[test]
    fn copy_data_is_pushed_at_eight_kilobytes() {
        let mut replies = copy_response(b'G', 1);
        replies.extend(message(b'C', b"COPY 1\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        conn.exec(b"COPY t FROM STDIN").unwrap();
        let before = conn.stream.to_server.len();
        let row = vec![b'x'; 8192 - 5];
        conn.put_copy_data(&row).unwrap();
        assert_eq!(conn.stream.to_server.len() - before, 8192);
        assert!(conn.outbuf.is_empty());
    }

    /// A COPY started through the extended protocol needs its Sync after
    /// the CopyDone (`fe-exec.c:2801`).
    #[test]
    fn an_extended_query_copy_in_ends_with_a_sync() {
        let mut replies = message(b'1', b"");
        replies.extend(message(b'2', b""));
        replies.extend(copy_response(b'G', 1));
        replies.extend(message(b'C', b"COPY 0\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        let results = conn
            .exec_params(b"COPY t FROM STDIN", &[], &Params::default())
            .unwrap();
        assert_eq!(results[0].status(), ExecStatus::CopyIn);
        let before = conn.stream.to_server.len();
        conn.put_copy_end(Some(b"stop")).unwrap();
        let mut sent = Frontend::CopyFail(b"stop".to_vec()).encode();
        sent.extend(Frontend::Sync.encode());
        assert_eq!(&conn.stream.to_server[before..], &sent[..]);
        assert_eq!(
            conn.get_result().unwrap().unwrap().command_status(),
            b"COPY 0"
        );
    }

    /// `PQexecStart`, `fe-exec.c:2391`: a new `PQexec` fails a COPY IN that
    /// was left open, swallows the error that earns, and runs.
    #[test]
    fn a_new_exec_fails_a_copy_in_left_open() {
        let mut replies = copy_response(b'G', 1);
        replies.extend(message(
            b'E',
            b"SERROR\0C57014\0MCOPY from stdin failed: COPY terminated by new PQexec\0\0",
        ));
        replies.extend(ready(b'I'));
        replies.extend(message(b'I', b""));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        conn.exec(b"COPY t FROM STDIN").unwrap();
        let before = conn.stream.to_server.len();
        let results = conn.exec(b"").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::EmptyQuery);
        let mut sent = Frontend::CopyFail(b"COPY terminated by new PQexec".to_vec()).encode();
        sent.extend(Frontend::Query(Vec::new()).encode());
        assert_eq!(&conn.stream.to_server[before..], &sent[..]);
    }

    /// `PQexecStart`, `fe-exec.c:2399`: a COPY OUT left open is drained by
    /// the next `PQexec`, its data dropped.
    #[test]
    fn a_new_exec_drops_what_is_left_of_a_copy_out() {
        let mut replies = copy_response(b'H', 1);
        replies.extend(message(b'd', b"1\n"));
        replies.extend(message(b'd', b"2\n"));
        replies.extend(message(b'c', b""));
        replies.extend(message(b'C', b"COPY 2\0"));
        replies.extend(ready(b'I'));
        replies.extend(message(b'I', b""));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        conn.exec(b"COPY t TO STDOUT").unwrap();
        assert_eq!(
            conn.get_copy_data(false).unwrap(),
            CopyRead::Row(b"1\n".to_vec())
        );
        let results = conn.exec(b"").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status(), ExecStatus::EmptyQuery);
    }

    /// `fe-exec.c:2411`: COPY BOTH is not something `PQexec` ends.
    #[test]
    fn a_new_exec_refuses_a_copy_both() {
        let mut conn = replayed(&copy_response(b'W', 0));
        let results = conn.exec(b"START_REPLICATION").unwrap();
        assert_eq!(results[0].status(), ExecStatus::CopyBoth);
        let error = conn.exec(b"SELECT 1").unwrap_err();
        assert_eq!(error.message(), b"PQexec not allowed during COPY BOTH");
    }

    /// `pqReadData` moves the unparsed tail to the front before it reads
    /// (`fe-misc.c:659`), so the buffer holds at most one read and a partial
    /// message, however long the COPY.
    #[test]
    fn the_input_buffer_does_not_keep_what_was_parsed() {
        let mut replies = copy_response(b'H', 1);
        let row = message(b'd', &[b'r'; 99]);
        for _ in 0..1000 {
            replies.extend_from_slice(&row);
        }
        replies.extend(message(b'c', b""));
        replies.extend(message(b'C', b"COPY 1000\0"));
        replies.extend(ready(b'I'));
        let mut conn = replayed(&replies);
        conn.exec(b"COPY t TO STDOUT").unwrap();
        let mut rows = 0;
        while let CopyRead::Row(data) = conn.get_copy_data(false).unwrap() {
            assert_eq!(data.len(), 99);
            assert!(conn.inbuf.len() < 8192 + row.len());
            rows += 1;
        }
        assert_eq!(rows, 1000);
    }
}
