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
//! `pqGetCopyData3`, `:1907`) — and `fe-misc.c`'s socket I/O: `pqSendSome`
//! (`:971`), which reads while it waits to write, and `pqReadData` (`:615`),
//! over the readiness wait in [`crate::poll`].
//!
//! The decisions are pure and live above the socket: [`startup_parameters`]
//! and [`socket_address`] are functions of the `ConnInfo` alone, and
//! [`PipelineState`] decides what every message means and when it may be
//! parsed, without knowing where it came from — which is what lets the tests
//! below replay a whole authenticated session over a scripted stream.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::auth::{AuthError, AuthStep, Authenticator, ChannelBinding};
use crate::cancel::Peer;
use crate::conninfo::ConnInfo;
use crate::encoding::Encoding;
use crate::error::ConnError;
use crate::escape::{self, EscapeError, EscapedString};
use crate::extended::{self, ArgumentError, Params, Plan, TypedCommand};
use crate::hosts::{ConnHost, HostType, LoadBalance, Prng, TargetServerType, conn_hosts};
use crate::lobj::LoFuncs;
use crate::message::{
    Backend, Frame, Frontend, PROTOCOL_VERSION_3_0, ProtocolError, Target, TransactionStatus,
    next_copy_frame, next_frame,
};
use crate::negotiate::{Build, EncMethod, EncryptionOptions, Negotiation};
use crate::pipeline::{
    Admit, AsyncStatus, CopyStep, Event, Next, PipelineError, PipelineState, PipelineStatus,
    QueryClass, message_id,
};
use crate::poll;
use crate::result::{ExecStatus, FieldDescription, QueryResult, ResultError};
use crate::scram::RAW_NONCE_LEN;
use crate::target::{ServerState, TargetCheck, TargetRejection, check_target};
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
    /// A conninfo value refused before anything is opened: the encryption
    /// options `pqConnectOptions2` validates (`fe-connect.c:1747`-`:1987`),
    /// and the `port` `PQconnectPoll` reads (`:3036`-`:3049`).
    Conninfo(ConnError),
    /// An extended-query argument refused before anything was sent
    /// (`PQsendQueryParams` and its siblings, `fe-exec.c:1509`).
    Argument(ArgumentError),
    /// A call the connection's state refuses before anything is sent — a
    /// blocking call in pipeline mode, a second command outside it, leaving
    /// pipeline mode with results outstanding.
    Pipeline(PipelineError),
    /// A server that accepted the connection but is not what
    /// `target_session_attrs` asked for (`fe-connect.c:4380`-`:4659`).
    Target(TargetRejection),
    /// `PQsetnonblocking` could not leave non-blocking mode: its flush left
    /// output unsent (`fe-exec.c:4001`).
    FlushPending,
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
            ConnectionError::Target(rejection) => rejection.message(),
            // C sets no message for it; `PQerrorMessage` keeps what it had.
            ConnectionError::FlushPending => Vec::new(),
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

/// Which socket a `ConnInfo`'s first host names — `pqConnectOptions2`'s host
/// classification at `fe-connect.c:1319`-`:1352` over the port
/// [`parse_port`] settles, as a pure function.
///
/// # Errors
/// The host list does not match (see [`conn_hosts`]), or the first `port`
/// is not one `PQconnectPoll` would use (see [`parse_port`]).
pub fn socket_address(conninfo: &ConnInfo) -> Result<Address, ConnError> {
    let hosts = conn_hosts(conninfo)?;
    let first = &hosts[0];
    Ok(host_address(first, parse_port(first.port.as_deref())?))
}

/// The socket one host entry names on `port`: `hostaddr` when it was given
/// (`CHT_HOST_ADDRESS`, dialled without a lookup), else the host name or
/// socket directory.
#[must_use]
pub fn host_address(host: &ConnHost, port: u16) -> Address {
    let text = |value: &Option<Vec<u8>>| {
        String::from_utf8_lossy(value.as_deref().unwrap_or_default()).into_owned()
    };
    match host.kind {
        HostType::HostAddress => Address::Tcp {
            host: text(&host.hostaddr),
            port,
        },
        HostType::HostName => Address::Tcp {
            host: text(&host.host),
            port,
        },
        HostType::UnixSocket => Address::Unix(unix_socket_path(&text(&host.host), port)),
    }
}

/// Action: `pg_getaddrinfo_all` (`fe-connect.c:3053`-`:3104`) — every
/// address a host resolves to, in the resolver's order. A socket path is
/// its own single address.
///
/// # Errors
/// The name did not resolve.
pub fn resolve(address: &Address) -> io::Result<Vec<Peer>> {
    match address {
        Address::Tcp { host, port } => Ok((host.as_str(), *port)
            .to_socket_addrs()?
            .map(Peer::Tcp)
            .collect()),
        Address::Unix(path) => Ok(vec![Peer::Unix(path.clone())]),
    }
}

/// `libpq_prng_init`, `fe-connect.c:1169`: sixteen strong random bytes, or
/// when those cannot be had a seed from the time and the pid. C also mixes
/// in the `PGconn` pointer, which has no counterpart here.
fn libpq_prng_init() -> Prng {
    if let Ok(bytes) = strong_random(16)
        && let Ok(bytes) = <[u8; 16]>::try_from(bytes)
    {
        return Prng::strong_seed(bytes);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    Prng::seed(u64::from(std::process::id()) ^ u64::from(now.subsec_micros()) ^ now.as_secs())
}

/// `ERRCODE_CANNOT_CONNECT_NOW`, `57P03`: a server still starting up, or a
/// standby not yet accepting connections.
const ERRCODE_CANNOT_CONNECT_NOW: &[u8] = b"57P03";

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

/// `pg_strong_random`, `src/port/pg_strong_random.c:150` — the arm that reads
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

impl AsFd for Stream {
    fn as_fd(&self) -> BorrowedFd<'_> {
        match self {
            Stream::Tcp(s) => s.as_fd(),
            Stream::Unix(s) => s.as_fd(),
        }
    }
}

/// What a [`Connection`] needs of its socket beyond reading and writing
/// bytes: a way to wait until it can do either.
///
/// A [`Stream`] that [`Connection::connect`] opened is non-blocking, as C's
/// socket is (`pg_set_noblock`, `fe-connect.c:3368`): a read or write that
/// cannot proceed returns `WouldBlock`, and the connection then waits here
/// for the direction it needs, so a blocking call never blocks inside a
/// `read` or `write` — which is what lets [`Connection::flush`] read while
/// it waits to write.
pub trait Socket: Read + Write {
    /// `pqWait(forRead, forWrite)`, `fe-misc.c:1165`: block until the socket
    /// can be read (`for_read`) or written (`for_write`), or has an error or
    /// hang-up for the next read or write to find.
    ///
    /// # Errors
    /// The wait itself failed.
    fn wait(&self, for_read: bool, for_write: bool) -> io::Result<()>;
}

impl Socket for Stream {
    fn wait(&self, for_read: bool, for_write: bool) -> io::Result<()> {
        poll::socket_check(self.as_fd(), for_read, for_write, None).map(|_| ())
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

/// What [`Connection::flush`] left behind: `pqFlush`'s `0` and `1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// Everything buffered was sent (`0`).
    Done,
    /// A non-blocking connection could not send it all without waiting; the
    /// rest stays buffered for the next flush (`1`, `fe-misc.c:1109`).
    Pending,
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
    /// `conn->connhost[conn->whichhost]`: the host entry the connection was
    /// made to, set with `raddr`. `None` for a stream
    /// [`Connection::start_up`] was handed.
    connhost: Option<ConnHost>,
    /// `conn->nonblocking` (`libpq-int.h:465`): whether a flush may leave
    /// output unsent rather than wait ([`Connection::set_nonblocking`]).
    nonblocking: bool,
    /// `conn->lobjfuncs`: the large-object function OIDs, looked up by the
    /// first `lo_*` call (`lo_initialize`, `fe-lobj.c:843`).
    pub(crate) lobjfuncs: Option<LoFuncs>,
}

impl Connection<Stream> {
    /// `PQconnectdb`: walk the host list the `ConnInfo` names until one
    /// server accepts, send it the startup packet, authenticate, and return
    /// once ReadyForQuery arrives.
    ///
    /// The list is [`conn_hosts`]'s, shuffled when `load_balance_hosts` is
    /// `random` (`fe-connect.c:2085`); each host's addresses are shuffled
    /// the same way (`:3116`). A host is left for the next one when its port
    /// is out of range (`:3044`), its name does not resolve (`:3060`), no
    /// address accepts the socket, the server answers "cannot connect
    /// now" (`:4136`), or it is not what `target_session_attrs` asks for
    /// (`CONNECTION_CHECK_TARGET`, `:4380`); anything else ends the attempt,
    /// as `error_return` does. `prefer-standby` walks the list a second
    /// time, settling for any server, when no standby was found (`:3010`).
    ///
    /// The caller is expected to have run `ConnInfo::add_defaults` already —
    /// `add_defaults(&Env::from_process(), &Filesystem)` is what
    /// `PQconnectdb` does with the environment and the service files.
    ///
    /// # Errors
    /// A `host`, `hostaddr` or `port` list that does not match, an option
    /// this build refuses (`sslmode=require` without TLS, say) or does not
    /// know, a `port` that is not an integer, the nonce could not be drawn,
    /// a server refused the connection or authentication failed, the
    /// connection broke while `target_session_attrs` was being checked — or
    /// every host was left, and then the reason the last one was.
    ///
    /// # Panics
    /// Never: [`conn_hosts`] always names at least one host, and every host
    /// that is left records why.
    pub fn connect(conninfo: &ConnInfo) -> Result<Self, ConnectionError> {
        // `pqConnectOptions2`, in its order: the host list (fe-connect.c:1256),
        // the encryption options (:1747-:1987), target_session_attrs (:1992)
        // and load_balance_hosts (:2067). All of it runs at `PQconnectStart`,
        // before `PQconnectPoll` reads a port or opens a socket.
        let mut hosts = conn_hosts(conninfo)?;
        let options = EncryptionOptions::from_conninfo(conninfo, Build::THIS)?;
        let target = TargetServerType::from_conninfo(conninfo)?;
        let mut prng = match LoadBalance::from_conninfo(conninfo)? {
            LoadBalance::Disable => None,
            LoadBalance::Random => Some(libpq_prng_init()),
        };
        if let Some(prng) = prng.as_mut() {
            prng.shuffle(&mut hosts);
        }

        let mut last_error = None;
        // fe-connect.c:2999-:3015 — out of hosts, `prefer-standby` drops the
        // standby requirement and starts over at the first host
        // (`SERVER_TYPE_PREFER_STANDBY_PASS2`), in the same order.
        let passes: &[bool] = if target == TargetServerType::PreferStandby {
            &[false, true]
        } else {
            &[false]
        };
        for &second_pass in passes {
            for host in &hosts {
                // fe-connect.c:3036 — a port that is not an integer is
                // `error_return`; one out of range moves on (`goto keep_going`).
                let port = match parse_port(host.port.as_deref()) {
                    Ok(port) => port,
                    Err(err @ ConnError::InvalidPortNumber(_)) => {
                        last_error = Some(err.into());
                        continue;
                    }
                    Err(err) => return Err(err.into()),
                };
                let mut peers = match resolve(&host_address(host, port)) {
                    Ok(peers) => peers,
                    Err(err) => {
                        last_error = Some(err.into());
                        continue;
                    }
                };
                if let Some(prng) = prng.as_mut() {
                    prng.shuffle(&mut peers);
                }
                for peer in peers {
                    // fe-connect.c:3285 — the first method is chosen before the
                    // socket is opened, so a combination with none fails without
                    // connecting.
                    let negotiation =
                        Negotiation::start(&options, Build::THIS, matches!(peer, Peer::Unix(_)))?;
                    // Without `USE_SSL` or `ENABLE_GSS` nothing but plaintext is
                    // ever allowed (`fe-connect.c:4721`-`:4741`; pinned by
                    // `negotiate::tests::this_build_only_ever_negotiates_plaintext`),
                    // so there is no SSLRequest to send and no method to fall
                    // back to.
                    debug_assert_eq!(negotiation.current(), Some(EncMethod::Plaintext));
                    let stream = match peer.connect() {
                        Ok(stream) => stream,
                        Err(err) => {
                            // fe-connect.c:3516 — try the next address.
                            last_error = Some(err.into());
                            continue;
                        }
                    };
                    // fe-connect.c:3368 — the socket itself is always
                    // non-blocking; `conn->nonblocking` only decides whether
                    // libpq waits on it.
                    stream.set_nonblocking(true)?;
                    let nonce = strong_random(RAW_NONCE_LEN)?;
                    match Connection::start_up(stream, conninfo, &nonce) {
                        Ok(mut conn) => {
                            // fe-connect.c:3249 — the address it was dialled at.
                            conn.raddr = Some(peer);
                            conn.connhost = Some(host.clone());
                            match conn.check_target(target, second_pass)? {
                                None => return Ok(conn),
                                // :4436 — the next host, not the next address.
                                Some(rejection) => {
                                    last_error = Some(ConnectionError::Target(rejection));
                                    break;
                                }
                            }
                        }
                        // fe-connect.c:4136 — the next host, not the next address.
                        Err(ConnectionError::Server(err))
                            if err.sqlstate() == Some(ERRCODE_CANNOT_CONNECT_NOW) =>
                        {
                            last_error = Some(ConnectionError::Server(err));
                            break;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
        }
        Err(last_error.expect("conn_hosts names at least one host"))
    }

    /// `PQsocket`, `fe-connect.c:7664`: the connection's socket, for a
    /// caller that waits on it itself (with [`poll::socket_poll`], say).
    #[must_use]
    pub fn socket(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl<S: Socket> Connection<S> {
    /// The startup exchange over an already-open stream. `raw_nonce` is what
    /// `pg_strong_random` drew for SCRAM (`fe-auth-scram.c:363`); passing it in
    /// keeps the exchange reproducible for a replayed trace.
    ///
    /// # Errors
    /// The server sent an ErrorResponse, a message that cannot appear during
    /// startup, or an authentication request this build cannot answer.
    pub fn start_up(
        stream: S,
        conninfo: &ConnInfo,
        raw_nonce: &[u8],
    ) -> Result<Self, ConnectionError> {
        let startup = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(conninfo),
        };

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
            connhost: None,
            nonblocking: false,
            lobjfuncs: None,
        };
        // fe-connect.c:3737 — `pqPacketSend` (`:5409`) puts the startup
        // packet in the output buffer and `pqFlush`es it (`:5425`), as every
        // later message is sent.
        conn.outbuf = startup.encode();
        conn.flush()?;

        loop {
            match conn.read_message()? {
                Backend::Authentication(request) => match authenticator.respond(&request)? {
                    AuthStep::Send(message) => conn.send(&message)?,
                    AuthStep::Nothing | AuthStep::Complete => {}
                },
                Backend::ParameterStatus { name, value } => {
                    conn.state.save_parameter_status(&name, &value);
                    conn.parameters.push((name, value));
                }
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

    /// `CONNECTION_CHECK_TARGET`, `fe-connect.c:4380`: hold the server that
    /// just accepted the connection to `target`, asking it
    /// `SHOW transaction_read_only` or `SELECT pg_catalog.pg_is_in_recovery()`
    /// when its startup ParameterStatus did not say. `Some` is why it was
    /// refused; the connection has then been closed politely, a Terminate
    /// whose failure is ignored (`sendTerminateConn`, `:5220`).
    ///
    /// # Errors
    /// The connection broke while the query ran (`error_return`, `:4410`,
    /// `:4564`).
    pub fn check_target(
        &mut self,
        target: TargetServerType,
        second_pass: bool,
    ) -> Result<Option<TargetRejection>, ConnectionError> {
        let mut state = ServerState::from_parameters(|name| self.parameter_status(name));
        loop {
            let rejection = match check_target(target, second_pass, &state) {
                TargetCheck::Accept => return Ok(None),
                TargetCheck::Ask(query) => {
                    let results = self.exec(query.sql())?;
                    match query.answer(results.first(), &mut state) {
                        Ok(()) => continue,
                        Err(rejection) => rejection,
                    }
                }
                TargetCheck::Reject(rejection) => rejection,
            };
            let _ = self.terminate();
            return Ok(Some(rejection));
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
        self.flush()?;
        Ok(())
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
            // fe-exec.c:1484.
            query: Some(query.to_vec()),
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
        self.state.append_command(plan.class, plan.query);
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
                    while self.flush()? == Flush::Pending {
                        self.stream.wait(false, true)?;
                    }
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

    /// `PQflush`, `fe-exec.c:4031`, which is `pqSendSome`
    /// (`fe-misc.c:971`) over the whole buffer: write what is buffered, and
    /// while the socket will not take more, read what the server has sent
    /// (`:1103`) — so a pipeline whose requests and replies both overflow
    /// the socket buffers cannot deadlock, the server blocked writing
    /// replies nobody reads and the client blocked writing requests it does
    /// not read. A blocking connection then waits until the socket can be
    /// read *or* written (`pqWait(true, true)`, `:1115`) and goes on until
    /// all is sent; a non-blocking one returns [`Flush::Pending`] (`:1109`).
    /// What is read is buffered, not parsed.
    ///
    /// # Errors
    /// The socket write failed — the output is then dropped, "no chance
    /// it'll ever be sent" (`:1044`), and input already sent is read first
    /// (`:1047`), a read failure being the error reported — or a read or wait
    /// made while the write was blocked failed.
    pub fn flush(&mut self) -> Result<Flush, ConnectionError> {
        let mut sent = 0;
        let result = loop {
            if sent == self.outbuf.len() {
                break Ok(Flush::Done);
            }
            match self.stream.write(&self.outbuf[sent..]) {
                Ok(0) => break Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                Ok(n) => sent += n,
                // :1029 — EAGAIN is "wait and try again", EINTR "try again".
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    // :1043-:1066
                    self.outbuf.clear();
                    sent = 0;
                    break match self.read_data() {
                        Err(read) => Err(read),
                        Ok(_) => Err(err.into()),
                    };
                }
            }
            if sent == self.outbuf.len() {
                continue;
            }
            // :1103 — not all sent: take in what the server has sent.
            if let Err(err) = self.read_data() {
                break Err(err);
            }
            if self.nonblocking {
                break Ok(Flush::Pending);
            }
            if let Err(err) = self.stream.wait(true, true) {
                break Err(err.into());
            }
        };
        // :1123 — keep what is still unsent.
        self.outbuf.drain(..sent);
        result
    }

    /// `PQsetnonblocking`, `fe-exec.c:3975`: from now on, may a flush leave
    /// output buffered instead of waiting to send it? What is buffered is
    /// flushed first, in the mode being left (`:4000`).
    ///
    /// # Errors
    /// That flush failed, or — leaving non-blocking mode — could not send
    /// everything without waiting ([`ConnectionError::FlushPending`]; C
    /// returns `-1` for either, `:4001`). The mode is then unchanged.
    pub fn set_nonblocking(&mut self, nonblocking: bool) -> Result<(), ConnectionError> {
        if nonblocking == self.nonblocking {
            return Ok(());
        }
        if self.flush()? == Flush::Pending {
            return Err(ConnectionError::FlushPending);
        }
        self.nonblocking = nonblocking;
        Ok(())
    }

    /// `PQisnonblocking`, `fe-exec.c:4014`.
    #[must_use]
    pub fn is_nonblocking(&self) -> bool {
        self.nonblocking
    }

    /// `PQconsumeInput`, `fe-exec.c:2001`: read whatever the server has
    /// already sent, without waiting for more and without parsing it. A
    /// non-blocking connection first flushes what it has buffered (`:2011`),
    /// or it might wait for replies to requests the server never got.
    ///
    /// # Errors
    /// The socket failed, or the server closed the connection.
    pub fn consume_input(&mut self) -> Result<(), ConnectionError> {
        if self.nonblocking {
            self.flush()?;
        }
        self.read_data()?;
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
        self.flush()?;
        Ok(())
    }

    fn send(&mut self, message: &Frontend) -> Result<(), ConnectionError> {
        self.put_message(message);
        self.flush()?;
        Ok(())
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

    /// `pqReadData`, `fe-misc.c:615`: append what one read returns, after
    /// moving what is left unparsed to the front of the buffer (`:659`) —
    /// without that, a long COPY OUT would keep every byte it ever read.
    /// `false` when the socket has nothing yet (`EAGAIN`, `:710`); a read a
    /// signal interrupted is retried (`:705`).
    fn read_data(&mut self) -> Result<bool, ConnectionError> {
        if self.start > 0 {
            self.inbuf.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = [0u8; 8192];
        let n = loop {
            match self.stream.read(&mut chunk) {
                Ok(n) => break n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(err) => return Err(err.into()),
            }
        };
        if n == 0 {
            return Err(ConnectionError::ServerClosedConnection);
        }
        self.inbuf.extend_from_slice(&chunk[..n]);
        Ok(true)
    }

    /// `pqWait(true, false)` then `pqReadData`, until a read brings
    /// something: the blocking read `PQgetResult` makes (`fe-exec.c:2114`).
    fn read_more(&mut self) -> Result<(), ConnectionError> {
        while !self.read_data()? {
            self.stream.wait(true, false)?;
        }
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

    /// `conn->pstatus` (`pqSaveParameterStatus`, `fe-exec.c:1091`): every
    /// parameter the server has reported, each with the last value it
    /// reported, in the order the names were first reported.
    #[must_use]
    pub fn parameter_statuses(&self) -> Vec<(&[u8], &[u8])> {
        let mut statuses: Vec<(&[u8], &[u8])> = Vec::new();
        for (name, value) in &self.parameters {
            match statuses
                .iter_mut()
                .find(|(seen, _)| *seen == name.as_slice())
            {
                Some(status) => status.1 = value,
                None => statuses.push((name, value)),
            }
        }
        statuses
    }

    /// `conn->connhost[conn->whichhost]`, which `PQhost` and `PQport` read
    /// (`fe-connect.c:7505`, `:7541`): the host entry
    /// [`Connection::connect`] reached, as `conn_hosts` listed it. `None`
    /// for a stream [`Connection::start_up`] was handed.
    #[must_use]
    pub fn host(&self) -> Option<&ConnHost> {
        self.connhost.as_ref()
    }

    /// `conn->asyncStatus`, which `PQtransactionStatus` reports as
    /// `PQTRANS_ACTIVE` whenever it is not idle (`fe-connect.c:7583`).
    #[must_use]
    pub fn async_status(&self) -> AsyncStatus {
        self.state.async_status()
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

    /// `PQclientEncoding`, `fe-connect.c:7728`: `conn->client_encoding`,
    /// which `pqSaveParameterStatus` sets from each `client_encoding` the
    /// server reports, falling back to SQL_ASCII for a name it does not know
    /// (`fe-exec.c:1145`-`:1151`). SQL_ASCII before any report
    /// (`pqMakeEmptyPGconn`, `fe-connect.c:4985`).
    #[must_use]
    pub fn client_encoding(&self) -> Encoding {
        self.parameter_status(b"client_encoding")
            .and_then(Encoding::from_name)
            .unwrap_or_default()
    }

    /// `conn->std_strings`, `fe-exec.c:1155`: did the server last report
    /// `standard_conforming_strings` as exactly `on`?
    #[must_use]
    pub fn std_strings(&self) -> bool {
        self.parameter_status(b"standard_conforming_strings") == Some(b"on")
    }

    /// `PQserverVersion`, `fe-connect.c:7628`: `conn->sversion`, which
    /// `pqSaveParameterStatus` computes from `server_version`
    /// (`fe-exec.c:1158`-`:1192`); 0 when unknown.
    #[must_use]
    pub fn server_version(&self) -> i32 {
        self.parameter_status(b"server_version")
            .map_or(0, server_version_number)
    }

    /// `PQescapeStringConn`, `fe-exec.c:4208`, under this connection's
    /// client encoding and `standard_conforming_strings`.
    #[must_use]
    pub fn escape_string_conn(&self, from: &[u8]) -> EscapedString {
        escape::escape_string(from, self.client_encoding(), self.std_strings())
    }

    /// `PQescapeLiteral`, `fe-exec.c:4413`.
    ///
    /// # Errors
    /// The input is not valid in the client encoding.
    pub fn escape_literal(&self, from: &[u8]) -> Result<Vec<u8>, EscapeError> {
        escape::escape_internal(from, self.client_encoding(), false)
    }

    /// `PQescapeIdentifier`, `fe-exec.c:4419`.
    ///
    /// # Errors
    /// The input is not valid in the client encoding.
    pub fn escape_identifier(&self, from: &[u8]) -> Result<Vec<u8>, EscapeError> {
        escape::escape_internal(from, self.client_encoding(), true)
    }

    /// `PQescapeByteaConn`, `fe-exec.c:4590`: hex format for a 9.0 or later
    /// server, the escape format before it.
    #[must_use]
    pub fn escape_bytea_conn(&self, from: &[u8]) -> Vec<u8> {
        escape::escape_bytea(from, self.std_strings(), self.server_version() >= 90000)
    }
}

/// `pqSaveParameterStatus`'s `server_version` arm, `fe-exec.c:1158`: C's
/// `sscanf(value, "%d.%d.%d", …)` and what it makes of one, two or three
/// numbers.
fn server_version_number(value: &[u8]) -> i32 {
    let mut numbers = Vec::with_capacity(3);
    let mut rest = value;
    while numbers.len() < 3 {
        // %d skips leading white space and takes an optional sign.
        let start = rest
            .iter()
            .position(|b| !b.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let body = &rest[start..];
        let sign_len = usize::from(matches!(body.first(), Some(b'+' | b'-')));
        let digits = body[sign_len..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits == 0 {
            break;
        }
        let token = &body[..sign_len + digits];
        let Some(n) = std::str::from_utf8(token)
            .ok()
            .and_then(|t| t.parse::<i32>().ok())
        else {
            break;
        };
        numbers.push(n);
        rest = &body[sign_len + digits..];
        // The literal '.' between conversions.
        match rest.split_first() {
            Some((b'.', tail)) => rest = tail,
            _ => break,
        }
    }
    match numbers[..] {
        // Old style, e.g. 9.6.1.
        [vmaj, vmin, vrev] => (100 * vmaj + vmin) * 100 + vrev,
        // New style, e.g. 10.1; old style without minor version, e.g. 9.6devel.
        [vmaj, vmin] if vmaj >= 10 => 100 * 100 * vmaj + vmin,
        [vmaj, vmin] => (100 * vmaj + vmin) * 100,
        // New style without minor version, e.g. 10devel.
        [vmaj] => 100 * 100 * vmaj,
        _ => 0,
    }
}

impl<S: Socket> Connection<S> {
    /// `PQsetClientEncoding`, `fe-connect.c:7736`: send
    /// `set client_encoding to '<encoding>'` and return its result. The new
    /// encoding takes effect when the server reports it, which it does
    /// before the command completes, so [`Connection::client_encoding`]
    /// reads it as soon as this returns.
    ///
    /// `None` is C's `-1` without a query: an encoding name too long for
    /// upstream's 128-byte buffer (`:7754`), or `auto`, which C resolves from
    /// the locale's `nl_langinfo(CODESET)` (`:7750`) and this crate cannot
    /// reach (see `docs/divergences.md`). `Some` carries the command's
    /// result; C's `0` is [`ExecStatus::CommandOk`].
    ///
    /// # Errors
    /// As [`Connection::exec`].
    pub fn set_client_encoding(
        &mut self,
        encoding: &[u8],
    ) -> Result<Option<QueryResult>, ConnectionError> {
        const QUERY: &[u8] = b"set client_encoding to '%s'";
        // sizeof(qbuf) < sizeof(query) + strlen(encoding): sizeof counts the
        // query's NUL, and the %s it replaces stays in the sum.
        if encoding == b"auto" || 128 < QUERY.len() + 1 + encoding.len() {
            return Ok(None);
        }
        let mut query = b"set client_encoding to '".to_vec();
        query.extend_from_slice(encoding);
        query.push(b'\'');
        Ok(self.exec(&query)?.pop())
    }
}

/// What `PQfn` hands back: the PGresult, COMMAND_OK or FATAL_ERROR, and
/// the function's value (`result_buf` and `*result_len`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnResult {
    pub result: QueryResult,
    /// The FunctionCallResponse's bytes, in network order. `None` for a
    /// NULL result, and when no FunctionCallResponse arrived.
    pub value: Option<Vec<u8>>,
}

impl<S: Socket> Connection<S> {
    /// `PQfn`, `fe-exec.c:2997`: call the function whose OID is `fnid`
    /// through the fast-path interface, every argument and the result in
    /// binary. `None` in `args` is SQL NULL.
    ///
    /// The result is COMMAND_OK when a value came back, or the server's
    /// error as FATAL_ERROR; a ReadyForQuery with neither is libpq's own
    /// "protocol error: no function result" (`fe-protocol3.c:2361`).
    ///
    /// # Errors
    /// In pipeline mode or while another command runs (nothing is sent),
    /// the connection broke, or the server's reply broke the protocol.
    pub fn fn_call(
        &mut self,
        fnid: u32,
        args: &[Option<&[u8]>],
    ) -> Result<FnResult, ConnectionError> {
        self.nfn(fnid, args, None)
    }

    /// `PQnfn`, `fe-exec.c:3016`, and `pqFunctionCall3`,
    /// `fe-protocol3.c:2165`: [`Connection::fn_call`], refusing a value
    /// longer than `buf_size` as "server returned too much data".
    pub(crate) fn nfn(
        &mut self,
        fnid: u32,
        args: &[Option<&[u8]>],
        buf_size: Option<usize>,
    ) -> Result<FnResult, ConnectionError> {
        self.state.begin_fn()?;
        self.put_message(&Frontend::FunctionCall {
            fnid,
            args: args.iter().map(|arg| arg.map(<[u8]>::to_vec)).collect(),
        });
        self.flush()?;

        let mut value = None;
        let mut status = ExecStatus::FatalError;
        let mut error = None;
        loop {
            let (id, body) = match next_frame(&self.inbuf[self.start..]) {
                Frame::Incomplete => {
                    self.read_more()?;
                    continue;
                }
                Frame::SyncLoss { id, length } => {
                    return Err(ProtocolError::LostSynchronization { id, length }.into());
                }
                Frame::Message { id, body } => (id, body),
            };
            // fe-protocol3.c:2301 — checked before the value is consumed.
            if id == b'V'
                && let Some(limit) = buf_size
                && body.len().saturating_sub(4) > limit
            {
                return Err(ProtocolError::TooMuchData.into());
            }
            // fe-protocol3.c:2283 — V or E is the answer, N, A and S may come
            // first, and the final Z must be swallowed before returning.
            match self.parse_frame(id, body)? {
                Backend::FunctionCallResponse(v) => {
                    value = v;
                    status = ExecStatus::CommandOk;
                }
                // pqGetErrorNotice3(conn, true) — the error replaces any
                // result being built.
                Backend::ErrorResponse(fields) => {
                    let fields = fields.with_client_encoding(self.state.client_encoding());
                    error = Some(QueryResult::with_error(ExecStatus::FatalError, fields));
                    status = ExecStatus::FatalError;
                }
                Backend::NotificationResponse {
                    pid,
                    channel,
                    payload,
                } => self.notifications.push((pid, channel, payload)),
                Backend::NoticeResponse(notice) => self
                    .notices
                    .push(notice.with_client_encoding(self.state.client_encoding())),
                Backend::ParameterStatus { name, value } => {
                    self.state.save_parameter_status(&name, &value);
                    self.parameters.push((name, value));
                }
                Backend::ReadyForQuery(xact) => {
                    self.transaction_status = xact;
                    // fe-protocol3.c:2348 — a result already made (the
                    // error) wins; otherwise COMMAND_OK if a value came.
                    let result = match error {
                        Some(result) => result,
                        None if status == ExecStatus::CommandOk => QueryResult::new(status),
                        None => QueryResult::with_error(
                            ExecStatus::FatalError,
                            ResultError::new(vec![(
                                crate::result::diag::MESSAGE_PRIMARY,
                                b"protocol error: no function result".to_vec(),
                            )]),
                        ),
                    };
                    return Ok(FnResult { result, value });
                }
                _ => return Err(ProtocolError::FunctionCallProtocol(id).into()),
            }
        }
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

    /// A script never refuses a read or a write, so nothing waits on it.
    impl Socket for Scripted {
        fn wait(&self, _: bool, _: bool) -> io::Result<()> {
            unreachable!("a scripted stream never returns WouldBlock")
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
        info.add_defaults(
            &Env::empty().with("USER", "alice"),
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
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

    /// `pqConnectOptions2` runs before `PQconnectPoll`, so a refused
    /// encryption option is reported ahead of a bad port, and nothing is
    /// opened — `005_negotiate_encryption.pl`'s `- -> fail` rows, which
    /// show no `connection received` in the server log.
    #[test]
    fn a_refused_encryption_option_stops_a_connection_before_the_port_is_read() {
        let info = conninfo("host=/nonexistent-socket-dir port=99999 sslmode=require");
        let error = Connection::connect(&info).unwrap_err();
        assert_eq!(
            error.message(),
            b"sslmode value \"require\" invalid when SSL support is not compiled in".to_vec()
        );
    }

    /// fe-connect.c:3044 — a port out of range leaves that host for the
    /// next one (`goto keep_going`), and the attempt ends with the last
    /// host's reason; `:3041` — one that is not an integer at all ends the
    /// attempt there (`goto error_return`), whatever hosts are left.
    #[test]
    fn a_port_out_of_range_moves_on_and_one_that_is_not_an_integer_stops() {
        // The first host's reason would be its port; what comes back is the
        // second host's: its socket, /nonexistent-b/.s.PGSQL.5432, is absent.
        let info = conninfo("host=/nonexistent-a,/nonexistent-b port=99999,5432");
        let error = Connection::connect(&info).unwrap_err();
        assert!(
            matches!(&error, ConnectionError::Io(err) if err.kind() == io::ErrorKind::NotFound),
            "{error:?}"
        );

        let info = conninfo("host=/nonexistent-a,/nonexistent-b port=5432,abc");
        let error = Connection::connect(&info).unwrap_err();
        assert_eq!(
            error.message(),
            b"invalid integer value \"abc\" for connection option \"port\"".to_vec()
        );
    }

    /// The host list is the first thing `pqConnectOptions2` settles
    /// (`fe-connect.c:1256`), ahead of the encryption options and
    /// `load_balance_hosts`.
    #[test]
    fn a_mismatched_host_list_is_reported_before_the_other_options() {
        let info = conninfo("host=/a,/b port=1,2,3 sslmode=require load_balance_hosts=x");
        let error = Connection::connect(&info).unwrap_err();
        assert_eq!(
            error.message(),
            b"could not match 3 port numbers to 2 hosts".to_vec()
        );

        let info = conninfo("host=/a,/b load_balance_hosts=x");
        let error = Connection::connect(&info).unwrap_err();
        assert_eq!(
            error.message(),
            b"invalid load_balance_hosts value: \"x\"".to_vec()
        );
    }

    /// `conn->pstatus` keeps one entry per name, the last value reported; a
    /// stream handed to `start_up` has no host entry, and a connection that
    /// has just started up is idle.
    #[test]
    fn parameter_statuses_keep_the_last_value_of_each_name() {
        let mut script = auth_ok();
        script.extend(message(b'S', b"server_version 18.6 "));
        script.extend(message(b'S', b"application_name a "));
        script.extend(message(b'S', b"application_name b "));
        script.extend(ready(b'I'));

        let info = conninfo("user=alice dbname=postgres");
        let conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        assert_eq!(
            conn.parameter_statuses(),
            [
                (&b"server_version"[..], &b"18.6"[..]),
                (&b"application_name"[..], &b"b"[..]),
            ]
        );
        assert_eq!(conn.host(), None);
        assert_eq!(conn.async_status(), AsyncStatus::Idle);
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

    /// A startup that reports `parameters`, then one single-column answer
    /// `value` to whatever query comes next.
    fn target_script(parameters: &[&[u8]], value: &[u8]) -> Vec<u8> {
        let mut script = auth_ok();
        for parameter in parameters {
            script.extend(message(b'S', parameter));
        }
        script.extend(ready(b'I'));
        let mut row_description = 1u16.to_be_bytes().to_vec();
        row_description.extend_from_slice(b"answer\0");
        row_description.extend_from_slice(&[0; 6]);
        row_description.extend_from_slice(&25u32.to_be_bytes());
        row_description.extend_from_slice(&(-1i16).to_be_bytes());
        row_description.extend_from_slice(&(-1i32).to_be_bytes());
        row_description.extend_from_slice(&0i16.to_be_bytes());
        script.extend(message(b'T', &row_description));
        let mut data_row = 1u16.to_be_bytes().to_vec();
        data_row.extend_from_slice(&u32::try_from(value.len()).unwrap().to_be_bytes());
        data_row.extend_from_slice(value);
        script.extend(message(b'D', &data_row));
        script.extend(message(b'C', b"SHOW\0"));
        script.extend(ready(b'I'));
        script
    }

    /// What the client wrote after its startup packet.
    fn after_startup(conn: &Connection<Scripted>, info: &ConnInfo) -> Vec<u8> {
        let startup = Frontend::Startup {
            version: PROTOCOL_VERSION_3_0,
            parameters: startup_parameters(info),
        }
        .encode();
        conn.stream.to_server[startup.len()..].to_vec()
    }

    /// fe-connect.c:4380 — a server that reported `in_hot_standby` and
    /// `default_transaction_read_only` at startup is judged on them, with
    /// nothing sent; a refused one is sent a Terminate (`:4432`).
    #[test]
    fn a_reported_server_state_is_checked_without_a_query() {
        let info = conninfo("user=alice");
        let reported: [&[u8]; 3] = [
            b"server_version\x0018.6\x00",
            b"default_transaction_read_only\0off\0",
            b"in_hot_standby\0on\0",
        ];
        let script = target_script(&reported, b"unused");

        let mut conn =
            Connection::start_up(Scripted::new(script.clone()), &info, &[0; 18]).unwrap();
        assert_eq!(
            conn.check_target(TargetServerType::Standby, false).unwrap(),
            None
        );
        assert!(after_startup(&conn, &info).is_empty());

        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        assert_eq!(
            conn.check_target(TargetServerType::ReadWrite, false)
                .unwrap(),
            Some(TargetRejection::SessionIsReadOnly)
        );
        assert_eq!(after_startup(&conn, &info), Frontend::Terminate.encode());
    }

    /// fe-connect.c:4398, :4457 — a server that did not report its state is
    /// asked, and judged on the answer.
    #[test]
    fn an_unreported_server_state_is_asked_for() {
        let info = conninfo("user=alice");
        let version: [&[u8]; 1] = [b"server_version\x0018.6\x00"];

        let script = target_script(&version, b"on");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        assert_eq!(
            conn.check_target(TargetServerType::ReadWrite, false)
                .unwrap(),
            Some(TargetRejection::SessionIsReadOnly)
        );
        let mut expected = Frontend::Query(b"SHOW transaction_read_only".to_vec()).encode();
        expected.extend(Frontend::Terminate.encode());
        assert_eq!(after_startup(&conn, &info), expected);

        let script = target_script(&version, b"t");
        let mut conn = Connection::start_up(Scripted::new(script), &info, &[0; 18]).unwrap();
        assert_eq!(
            conn.check_target(TargetServerType::Standby, false).unwrap(),
            None
        );
        assert_eq!(
            after_startup(&conn, &info),
            Frontend::Query(b"SELECT pg_catalog.pg_is_in_recovery()".to_vec()).encode()
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
        // The query sent is kept for the cursor (`fe-protocol3.c:966`,
        // `fe-exec.c:1484`), so the position is drawn, not written.
        assert_eq!(
            results[0].error().and_then(ResultError::err_query),
            Some(&b"selct 1"[..])
        );
        assert_eq!(
            String::from_utf8(results[0].error_message()).unwrap(),
            "ERROR:  syntax error at or near \"selct\"\nLINE 1: selct 1\n        ^\n\
             HINT:  Check your spelling.\n"
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
        impl Socket for Dribble {
            fn wait(&self, _: bool, _: bool) -> io::Result<()> {
                unreachable!("a dribble never returns WouldBlock")
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
    /// `PQsendQuery`, in `PQsendQueryInternal` (`:1461`).
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

    #[test]
    fn server_version_is_what_sscanf_makes_of_the_reported_string() {
        assert_eq!(server_version_number(b"18.6"), 180_006);
        assert_eq!(server_version_number(b"10.1"), 100_001);
        assert_eq!(server_version_number(b"9.6.1"), 90_601);
        assert_eq!(server_version_number(b"9.6devel"), 90_600);
        assert_eq!(server_version_number(b"19devel"), 190_000);
        assert_eq!(server_version_number(b"18beta1"), 180_000);
        assert_eq!(server_version_number(b" 18.6 (Debian 18.6-1)"), 180_006);
        assert_eq!(server_version_number(b"devel"), 0);
        assert_eq!(server_version_number(b""), 0);
    }

    /// Before any report the encoding is SQL_ASCII and strings are not
    /// standard-conforming; each ParameterStatus then moves the state, an
    /// unknown encoding name falling back to SQL_ASCII (`fe-exec.c:1150`).
    #[test]
    fn the_escape_settings_follow_parameter_status() {
        let mut conn = replayed(&[]);
        assert_eq!(conn.client_encoding(), Encoding::SqlAscii);
        assert!(!conn.std_strings());
        assert_eq!(conn.server_version(), 0);

        let mut replies = message(b'S', b"client_encoding\0GB18030\0");
        replies.extend(message(b'S', b"standard_conforming_strings\0on\0"));
        replies.extend(message(b'S', b"server_version\x0018.6\0"));
        replies.extend(message(b'C', b"SET\0"));
        replies.extend(ready(b'I'));
        conn = replayed(&replies);
        let result = conn.set_client_encoding(b"GB18030").unwrap().unwrap();
        assert_eq!(result.status(), ExecStatus::CommandOk);
        assert_eq!(conn.client_encoding(), Encoding::Gb18030);
        assert!(conn.std_strings());
        assert_eq!(conn.server_version(), 180_006);
        let query = b"set client_encoding to 'GB18030'\0";
        assert!(
            conn.stream
                .to_server
                .windows(query.len())
                .any(|w| w == query)
        );
        // GB18030 now governs escaping: 0x81 0x5c is one character.
        assert_eq!(conn.escape_literal(b"\x81\\").unwrap(), b"'\x81\\'");
        assert_eq!(conn.escape_bytea_conn(b"\x01"), b"\\x01");

        let mut replies = message(b'S', b"client_encoding\0KLINGON\0");
        replies.extend(message(b'S', b"standard_conforming_strings\0off\0"));
        replies.extend(message(b'C', b"SET\0"));
        replies.extend(ready(b'I'));
        conn = replayed(&replies);
        conn.exec(b"select 1").unwrap();
        assert_eq!(conn.client_encoding(), Encoding::SqlAscii);
        assert!(!conn.std_strings());
        // No server_version, so the escape format, backslashes doubled.
        assert_eq!(conn.escape_bytea_conn(b"\x01"), b"\\\\001");
    }

    /// `auto` and an over-long name are refused before anything is sent
    /// (`fe-connect.c:7750`, `:7754`).
    #[test]
    fn set_client_encoding_sends_nothing_it_cannot_fit_or_resolve() {
        let mut conn = replayed(&[]);
        let sent = conn.stream.to_server.len();
        assert!(conn.set_client_encoding(b"auto").unwrap().is_none());
        // 128 < 28 + 101: one byte too many for qbuf.
        assert!(conn.set_client_encoding(&[b'x'; 101]).unwrap().is_none());
        assert_eq!(conn.stream.to_server.len(), sent);
    }

    /// More than any socket buffer holds, both ways.
    const OVERFLOW: usize = 4 << 20;

    /// A connection over one end of a socket pair whose other end is a
    /// "server" that has sent AuthenticationOk and ReadyForQuery, and that
    /// reads the startup packet and then runs `serve`; the socket is
    /// non-blocking, as `connect` leaves it.
    fn over_a_socket_pair(
        serve: impl FnOnce(UnixStream) + Send + 'static,
    ) -> (Connection, std::thread::JoinHandle<()>) {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(&auth_ok()).unwrap();
        server.write_all(&ready(b'I')).unwrap();
        let stream = Stream::Unix(client);
        stream.set_nonblocking(true).unwrap();
        let conn = Connection::start_up(stream, &conninfo("user=alice"), &[0; 18]).unwrap();
        let server = std::thread::spawn(move || {
            let mut length = [0u8; 4];
            server.read_exact(&mut length).unwrap();
            let mut rest = vec![0u8; u32::from_be_bytes(length) as usize - 4];
            server.read_exact(&mut rest).unwrap();
            serve(server);
        });
        (conn, server)
    }

    /// `pqSendSome`'s reason for being (`fe-misc.c:1076`-`:1102`): a server
    /// that writes everything it has before it reads anything, and a client
    /// with more to send than the socket holds. Were the flush a plain
    /// blocking write, each side would wait on the other for ever; reading
    /// while the write is blocked lets both finish.
    #[test]
    fn a_blocking_flush_reads_while_the_socket_will_not_take_more() {
        let (mut conn, server) = over_a_socket_pair(|mut server| {
            server.write_all(&vec![b'r'; OVERFLOW]).unwrap();
            let mut got = vec![0u8; OVERFLOW];
            server.read_exact(&mut got).unwrap();
            assert!(got.iter().all(|&b| b == b's'));
        });
        conn.outbuf = vec![b's'; OVERFLOW];

        let (done, finished) = std::sync::mpsc::channel();
        let client = std::thread::spawn(move || {
            let flushed = conn.flush().unwrap();
            done.send(()).unwrap();
            (conn, flushed)
        });
        finished
            .recv_timeout(std::time::Duration::from_mins(1))
            .expect("the flush deadlocked");
        let (conn, flushed) = client.join().unwrap();
        server.join().unwrap();
        assert_eq!(flushed, Flush::Done);
        assert!(conn.outbuf.is_empty());
        // What arrived while writing was kept, not parsed and not lost.
        assert!(conn.inbuf[conn.start..].iter().all(|&b| b == b'r'));
        assert!(!conn.inbuf.is_empty());
    }

    /// In non-blocking mode the flush returns `1` instead of waiting
    /// (`fe-misc.c:1109`), keeping the unsent tail; `PQsetnonblocking` will
    /// not leave the mode until that tail is gone (`fe-exec.c:4001`).
    #[test]
    fn a_non_blocking_flush_returns_pending_and_keeps_the_rest() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (mut conn, server) = over_a_socket_pair(move |mut server| {
            released.recv().unwrap();
            // Everything the client sends, then the end of the stream once
            // the client is dropped.
            let mut got = Vec::new();
            server.read_to_end(&mut got).unwrap();
            assert_eq!(got.len(), OVERFLOW);
        });
        assert!(!conn.is_nonblocking());
        conn.set_nonblocking(true).unwrap();
        assert!(conn.is_nonblocking());
        conn.outbuf = vec![b's'; OVERFLOW];

        assert_eq!(conn.flush().unwrap(), Flush::Pending);
        let left = conn.outbuf.len();
        assert!(left > 0 && left < OVERFLOW, "some sent, the rest kept");
        assert!(matches!(
            conn.set_nonblocking(false),
            Err(ConnectionError::FlushPending)
        ));
        assert!(conn.is_nonblocking(), "the mode is unchanged");

        release.send(()).unwrap();
        while conn.flush().unwrap() == Flush::Pending {
            conn.stream.wait(false, true).unwrap();
        }
        conn.set_nonblocking(false).unwrap();
        assert!(!conn.is_nonblocking());
        drop(conn);
        server.join().unwrap();
    }

    /// A write the socket refuses outright drops what is unsent — "no chance
    /// it'll ever be sent" (`fe-misc.c:1044`) — after reading what the
    /// server sent (`:1047`), whose end of stream is then the error. C would
    /// set `write_failed` and report later (`fe-exec.c:2131`); see
    /// `docs/divergences.md`.
    #[test]
    fn a_failed_write_drops_the_output_and_reports_the_closed_connection() {
        let (mut conn, server) = over_a_socket_pair(drop);
        server.join().unwrap();
        conn.outbuf = vec![b's'; OVERFLOW];
        assert!(matches!(
            conn.flush(),
            Err(ConnectionError::ServerClosedConnection)
        ));
        assert!(conn.outbuf.is_empty());
    }

    fn function_result(value: Option<&[u8]>) -> Vec<u8> {
        let mut body = match value {
            Some(v) => i32::try_from(v.len()).unwrap().to_be_bytes().to_vec(),
            None => (-1i32).to_be_bytes().to_vec(),
        };
        body.extend_from_slice(value.unwrap_or_default());
        message(b'V', &body)
    }

    fn fn_conn(reply: Vec<u8>) -> Connection<Scripted> {
        let mut script = auth_ok();
        script.extend(ready(b'I'));
        script.extend(reply);
        Connection::start_up(Scripted::new(script), &conninfo("user=alice"), &[0; 18]).unwrap()
    }

    /// `pqFunctionCall3`, `fe-protocol3.c:2165`: one FunctionCall out; a
    /// notice and a notification before the value are dealt with as they
    /// come, and the ReadyForQuery is swallowed before returning.
    #[test]
    fn a_function_call_returns_its_value_after_the_messages_before_it() {
        let mut reply = message(b'N', b"SNOTICE\0Mhello\0\0");
        let mut notify = 7i32.to_be_bytes().to_vec();
        notify.extend_from_slice(b"chan\0payload\0");
        reply.extend(message(b'A', &notify));
        reply.extend(function_result(Some(&3i32.to_be_bytes())));
        reply.extend(ready(b'T'));
        let mut conn = fn_conn(reply);
        let sent = conn.stream.to_server.len();

        let call = conn
            .fn_call(952, &[Some(&42u32.to_be_bytes()), None])
            .unwrap();
        assert_eq!(call.result.status(), ExecStatus::CommandOk);
        assert_eq!(call.value, Some(3i32.to_be_bytes().to_vec()));
        assert_eq!(conn.notices().len(), 1);
        assert_eq!(
            conn.notifications(),
            [(7, b"chan".to_vec(), b"payload".to_vec())]
        );
        assert_eq!(conn.transaction_status(), TransactionStatus::InTransaction);
        assert_eq!(
            conn.stream.to_server[sent..],
            Frontend::FunctionCall {
                fnid: 952,
                args: vec![Some(42u32.to_be_bytes().to_vec()), None],
            }
            .encode()
        );
    }

    /// `fe-protocol3.c:2320` and `:2348`: an ErrorResponse is the result.
    #[test]
    fn a_refused_function_call_is_a_fatal_error_result() {
        let mut reply = message(b'E', b"SERROR\0C42704\0Mlarge object 42 does not exist\0\0");
        reply.extend(ready(b'E'));
        let mut conn = fn_conn(reply);
        let call = conn.fn_call(952, &[]).unwrap();
        assert_eq!(call.result.status(), ExecStatus::FatalError);
        assert_eq!(call.value, None);
        assert_eq!(
            call.result.error_message(),
            b"ERROR:  large object 42 does not exist\n"
        );
        assert_eq!(conn.transaction_status(), TransactionStatus::InError);
    }

    /// `fe-protocol3.c:2361`: a ReadyForQuery with no value and no error.
    #[test]
    fn a_ready_for_query_without_a_value_is_no_function_result() {
        let mut conn = fn_conn(ready(b'I'));
        let call = conn.fn_call(952, &[]).unwrap();
        assert_eq!(call.result.status(), ExecStatus::FatalError);
        assert_eq!(
            call.result.error_message(),
            b"protocol error: no function result\n"
        );
    }

    /// A NULL result is a -1 length and still COMMAND_OK.
    #[test]
    fn a_null_function_result_is_command_ok_without_a_value() {
        let mut reply = function_result(None);
        reply.extend(ready(b'I'));
        let mut conn = fn_conn(reply);
        let call = conn.fn_call(952, &[]).unwrap();
        assert_eq!(call.result.status(), ExecStatus::CommandOk);
        assert_eq!(call.value, None);
    }

    /// `PQnfn`'s buffer check, `fe-protocol3.c:2306`, and the `default`
    /// case, `:2373`.
    #[test]
    fn too_much_data_and_a_stray_message_break_the_call() {
        let mut reply = function_result(Some(b"abcde"));
        reply.extend(ready(b'I'));
        let mut conn = fn_conn(reply);
        let err = conn.nfn(954, &[], Some(4)).unwrap_err();
        assert_eq!(err.message(), b"server returned too much data");

        let mut reply = message(b'C', b"SELECT 1\0");
        reply.extend(ready(b'I'));
        let mut conn = fn_conn(reply);
        let err = conn.fn_call(952, &[]).unwrap_err();
        assert_eq!(err.message(), b"protocol error: id=0x43");
    }

    /// `PQnfn`'s refusals, `fe-exec.c:3032`-`:3043`: nothing is sent.
    #[test]
    fn a_function_call_is_refused_in_pipeline_mode_and_mid_command() {
        let mut conn = fn_conn(Vec::new());
        let sent = conn.stream.to_server.len();
        conn.enter_pipeline_mode().unwrap();
        assert_eq!(
            conn.fn_call(952, &[]).unwrap_err().message(),
            b"PQfn not allowed in pipeline mode"
        );
        conn.exit_pipeline_mode().unwrap();
        conn.send_query(b"select 1").unwrap();
        let sent_query = conn.stream.to_server.len();
        assert!(sent_query > sent);
        assert_eq!(
            conn.fn_call(952, &[]).unwrap_err().message(),
            b"connection in wrong state"
        );
        assert_eq!(conn.stream.to_server.len(), sent_query);
    }
}
