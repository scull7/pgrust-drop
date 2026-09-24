//! Query cancellation, the blocking calls: `src/interfaces/libpq/fe-cancel.c`.
//!
//! A cancel is a second connection to the postmaster that carries one
//! CancelRequest packet (`pqcomm.h:139`) naming the backend's PID and its
//! cancel key, and then waits for the postmaster to close it. Two APIs send
//! it:
//!
//! - the legacy one, [`Cancel`] — `PQgetCancel` (`fe-cancel.c:368`),
//!   `PQcancel` (`:548`), `PQfreeCancel` (`:502`, which is `Drop`) and
//!   `PQrequestCancel` (`:752`), whose errors are the fixed
//!   `PQcancel() -- …` strings;
//! - the PostgreSQL 17 one, [`CancelConn`] — `PQcancelCreate` (`:68`),
//!   `PQcancelBlocking` (`:190`), `PQcancelStatus` (`:302`),
//!   `PQcancelErrorMessage` (`:325`), `PQcancelReset` (`:337`) and
//!   `PQcancelFinish` (`:353`, which is `Drop`).
//!
//! The non-blocking half of the second — `PQcancelStart`, `PQcancelPoll`,
//! `PQcancelSocket` — needs a readiness wait this crate does not have yet
//! (NAT-520), and is not here.
//!
//! Both are plain data once built, so either can be moved to another thread
//! and fired while the connection it came from is blocked in a query — which
//! is what they are for. The decisions ([`CancelError::message`],
//! `judge_response`, the state checks of `PQcancelStart`) are pure; the
//! socket calls are thin and sit at the edge, generic over the stream so the
//! unit tests below can script one.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::connection::{Connection, Stream};
use crate::message::Frontend;

/// Where the connection being cancelled went: `conn->raddr`, the address
/// `PQgetCancel` and `PQcancelCreate` copy (`fe-cancel.c:406`, `:170`) so the
/// request reaches the same postmaster over the same kind of socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peer {
    /// A TCP peer, already resolved: no second name lookup, as in C.
    Tcp(SocketAddr),
    /// The Unix socket's path.
    Unix(PathBuf),
}

impl Peer {
    /// Action: open a fresh socket to the peer — C's `socket()` then
    /// `connect()` (`fe-cancel.c:577`, `:655`), which `std` does as one call.
    ///
    /// # Errors
    /// The socket could not be opened or the connection was refused.
    pub fn connect(&self) -> io::Result<Stream> {
        match self {
            Peer::Tcp(addr) => Ok(Stream::Tcp(TcpStream::connect(addr)?)),
            Peer::Unix(path) => Ok(Stream::Unix(UnixStream::connect(path)?)),
        }
    }
}

/// The system call `PQcancel` names when it fails (`fe-cancel.c:579`-`:678`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStep {
    /// `connect()` — here also `socket()`, which `std` does not separate.
    Connect,
    /// `send()` of the packet.
    Send,
}

impl CancelStep {
    fn name(self) -> &'static str {
        match self {
            CancelStep::Connect => "connect",
            CancelStep::Send => "send",
        }
    }
}

/// Why `PQcancel` or `PQrequestCancel` returned false.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelError {
    /// The dummy `PGcancel` of a connection that got no BackendKeyData
    /// (`fe-cancel.c:564`).
    NoCancelKey,
    /// A socket call failed with this `errno` (`cancel_errReturn`, `:703`).
    Failed { step: CancelStep, errno: i32 },
    /// A socket call failed without an `errno`: `std` refused it before any
    /// system call (a socket path it cannot pass to `connect()`), or a send
    /// wrote nothing. C has no such failure; `reason` is the `io::Error`
    /// text, printed where C prints `error N`.
    FailedWithoutErrno { step: CancelStep, reason: String },
    /// `PQrequestCancel` on a connection without a socket (`:761`).
    ConnectionNotOpen,
}

impl CancelError {
    /// Calculation: the bytes C leaves in `errbuf` (or, for
    /// `PQrequestCancel`, in `conn->errorMessage`).
    ///
    /// `PQcancel` must be signal-safe, so it prints `errno` in decimal
    /// instead of calling `strerror` (`fe-cancel.c:713`), and the dummy
    /// object's message has no trailing newline (`:567`).
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            CancelError::NoCancelKey => b"PQcancel() -- no cancellation key received".to_vec(),
            CancelError::Failed { step, errno } => {
                format!("PQcancel() -- {}() failed: error {errno}\n", step.name()).into_bytes()
            }
            CancelError::FailedWithoutErrno { step, reason } => {
                format!("PQcancel() -- {}() failed: {reason}\n", step.name()).into_bytes()
            }
            CancelError::ConnectionNotOpen => {
                b"PQrequestCancel() -- connection is not open\n".to_vec()
            }
        }
    }

    /// Calculation: the error for a failed `step`. An `io::Error` that
    /// carries no `errno` is not reported as `error 0`, which would claim a
    /// system call failed with no error.
    fn failed(step: CancelStep, err: &io::Error) -> Self {
        match err.raw_os_error() {
            Some(errno) => CancelError::Failed { step, errno },
            None => CancelError::FailedWithoutErrno {
                step,
                reason: err.to_string(),
            },
        }
    }
}

impl std::fmt::Display for CancelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for CancelError {}

/// `PGcancel` (`fe-cancel.c:40`): what `PQcancel` needs to cancel a query
/// on one connection, copied out of it so another thread can use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cancel {
    peer: Peer,
    /// The pre-built CancelRequest packet; `None` is the dummy object whose
    /// `cancel_pkt_len` is 0 (`fe-cancel.c:398`).
    packet: Option<Vec<u8>>,
}

impl Cancel {
    /// `PQgetCancel`'s body (`fe-cancel.c:381`-`:457`), given the
    /// connection's peer, PID and cancel key. An empty key makes the dummy
    /// object C returns for a server that sent none: building it succeeds,
    /// using it fails.
    #[must_use]
    pub fn new(peer: Peer, pid: i32, cancel_key: &[u8]) -> Self {
        let packet = (!cancel_key.is_empty()).then(|| {
            Frontend::CancelRequest {
                pid,
                cancel_key: cancel_key.to_vec(),
            }
            .encode()
        });
        Cancel { peer, packet }
    }

    /// The peer the request goes to.
    #[must_use]
    pub fn peer(&self) -> &Peer {
        &self.peer
    }

    /// The CancelRequest packet, or `None` for the dummy object.
    #[must_use]
    pub fn packet(&self) -> Option<&[u8]> {
        self.packet.as_deref()
    }

    /// `PQcancel`, `fe-cancel.c:548`: open a socket to the postmaster, send
    /// the packet, and wait for the postmaster to close the socket.
    ///
    /// Success means the request was delivered, not that anything was
    /// cancelled: the query's own result says that. The object can be used
    /// again for a later query on the same connection.
    ///
    /// # Errors
    /// The dummy object, or the connect or the send failed. A failed or
    /// short final read is ignored, as C ignores it (`:695`).
    pub fn cancel(&self) -> Result<(), CancelError> {
        let Some(packet) = &self.packet else {
            return Err(CancelError::NoCancelKey);
        };
        let mut stream = self
            .peer
            .connect()
            .map_err(|err| CancelError::failed(CancelStep::Connect, &err))?;
        deliver(&mut stream, packet)
    }
}

/// Action: `PQcancel` from `retry4` on (`fe-cancel.c:667`-`:701`) over an
/// open stream — send the packet, then one read that returns at EOF, which
/// is how the postmaster says it has processed the request. Without that
/// wait, a query sent next could be the one the cancel hits.
fn deliver<S: Read + Write>(stream: &mut S, packet: &[u8]) -> Result<(), CancelError> {
    stream
        .write_all(packet)
        .map_err(|err| CancelError::failed(CancelStep::Send, &err))?;
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            // "we ignore other error conditions" (`:695`).
            _ => return Ok(()),
        }
    }
}

/// The states a [`CancelConn`] can be in: the part of `ConnStatusType` the
/// blocking path reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStatus {
    /// `CONNECTION_ALLOCATED`: created, or reset, and not yet sent.
    Allocated,
    /// `CONNECTION_OK`: the request was delivered.
    Ok,
    /// `CONNECTION_BAD`: it could not be created or sent.
    Bad,
}

/// `PGcancelConn` (`fe-cancel.c:31`): a cancel request with a status and an
/// error message of its own, for `PQcancelBlocking`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelConn {
    /// `None` when `PQcancelCreate` failed before it copied anything.
    cancel: Option<Cancel>,
    status: CancelStatus,
    /// `conn->errorMessage`: every line ends in `\n`
    /// (`libpq_append_conn_error`, `fe-misc.c:1568`).
    error_message: Vec<u8>,
}

impl CancelConn {
    /// `PQcancelCreate`, `fe-cancel.c:68`, given the connection's peer
    /// (`None` for "no socket") and its PID and cancel key.
    ///
    /// A connection that cannot be cancelled still yields an object — in
    /// `CONNECTION_BAD` (`pqMakeEmptyPGconn`, `fe-connect.c:4979`), with the
    /// reason in its error message.
    #[must_use]
    pub fn new(peer: Option<Peer>, pid: i32, cancel_key: &[u8]) -> Self {
        let failed = |reason: &[u8]| CancelConn {
            cancel: None,
            status: CancelStatus::Bad,
            error_message: reason.to_vec(),
        };
        let Some(peer) = peer else {
            return failed(b"connection not open\n"); // :85
        };
        if cancel_key.is_empty() {
            return failed(b"no cancellation key received\n"); // :92
        }
        CancelConn {
            cancel: Some(Cancel::new(peer, pid, cancel_key)),
            status: CancelStatus::Allocated,
            error_message: Vec::new(),
        }
    }

    /// `PQcancelStatus`, `fe-cancel.c:302`.
    #[must_use]
    pub fn status(&self) -> CancelStatus {
        self.status
    }

    /// `PQcancelErrorMessage`, `fe-cancel.c:325`.
    #[must_use]
    pub fn error_message(&self) -> &[u8] {
        &self.error_message
    }

    /// `PQcancelReset`, `fe-cancel.c:337`: back to `CONNECTION_ALLOCATED`,
    /// with the error message cleared (`pqClosePGconn`,
    /// `fe-connect.c:5282`), so the object can send another request.
    pub fn reset(&mut self) {
        self.status = CancelStatus::Allocated;
        self.error_message.clear();
    }

    /// `PQcancelBlocking`, `fe-cancel.c:190`: send the request and wait
    /// until the postmaster has processed it. `true` when it was delivered.
    pub fn blocking(&mut self) -> bool {
        self.blocking_over(Peer::connect)
    }

    /// `PQcancelStart`'s checks (`fe-cancel.c:206`-`:217`) and then
    /// `pqConnectDBComplete` over whatever `connect` opens.
    fn blocking_over<S: Read + Write>(
        &mut self,
        connect: impl FnOnce(&Peer) -> io::Result<S>,
    ) -> bool {
        match self.start() {
            Ok(()) => {}
            Err(line) => {
                self.error_message.extend_from_slice(line);
                self.status = CancelStatus::Bad;
                return false;
            }
        }
        let Some(cancel) = &self.cancel else {
            // A reset object whose creation failed: `pqConnectDBStart` with
            // `options_valid` false (`fe-connect.c:2709`). No new message.
            self.status = CancelStatus::Bad;
            return false;
        };
        match exchange(cancel, connect) {
            Ok(()) => {
                // `PQcancelPoll`, `fe-cancel.c:291`-`:292`.
                self.status = CancelStatus::Ok;
                self.error_message.clear();
                true
            }
            Err(line) => {
                self.error_message.extend_from_slice(&line);
                self.status = CancelStatus::Bad;
                false
            }
        }
    }

    /// Calculation: `PQcancelStart`'s refusals. `Err(b"")` is the silent one
    /// (an object already bad, `fe-cancel.c:206`).
    fn start(&self) -> Result<(), &'static [u8]> {
        match self.status {
            CancelStatus::Allocated => Ok(()),
            CancelStatus::Bad => Err(b""),
            CancelStatus::Ok => Err(b"cancel request is already being sent on this connection\n"),
        }
    }
}

/// Action: the cancel connection's life in `PQconnectPoll` and
/// `PQcancelPoll` — connect, `PQsendCancelRequest` in `CONNECTION_MADE`
/// (`fe-connect.c:3705`), then read until the postmaster closes the socket.
/// `Err` is the line to append to the error message.
fn exchange<S: Read + Write>(
    cancel: &Cancel,
    connect: impl FnOnce(&Peer) -> io::Result<S>,
) -> Result<(), Vec<u8>> {
    let packet = cancel.packet().unwrap_or_default();
    let mut stream = connect(cancel.peer()).map_err(|err| format!("{err}\n").into_bytes())?;
    stream
        .write_all(packet)
        .and_then(|()| stream.flush())
        .map_err(|err| format!("could not send cancel packet: {err}\n").into_bytes())?;
    let mut buf = [0u8; 256];
    loop {
        let read = stream.read(&mut buf);
        if matches!(&read, Err(err) if err.kind() == io::ErrorKind::Interrupted) {
            continue;
        }
        return judge_response(read);
    }
}

/// Calculation: what `PQcancelPoll` makes of its read in
/// `CONNECTION_AWAITING_RESPONSE` (`fe-cancel.c:247`-`:293`). EOF is the
/// answer it waits for; data is "unexpected"; an error is `pqReadData`'s.
/// `Err` is the line to append to the error message.
fn judge_response(read: io::Result<usize>) -> Result<(), Vec<u8>> {
    match read {
        Ok(0) => Ok(()),
        Ok(_) => Err(b"unexpected response from server\n".to_vec()),
        // `fe-secure.c:233`.
        Err(err) => Err(format!("could not receive data from server: {err}\n").into_bytes()),
    }
}

impl Connection<Stream> {
    /// `PQgetCancel`, `fe-cancel.c:368`: a [`Cancel`] for this connection,
    /// or `None` when it has no peer address to send one to — C's "no
    /// socket" (`:377`). A connection that received
    /// no cancel key yields the dummy object (`:398`).
    #[must_use]
    pub fn get_cancel(&self) -> Option<Cancel> {
        Some(Cancel::new(
            self.peer()?,
            self.backend_pid(),
            self.cancel_key(),
        ))
    }

    /// `PQcancelCreate`, `fe-cancel.c:68`.
    #[must_use]
    pub fn cancel_create(&self) -> CancelConn {
        CancelConn::new(self.peer(), self.backend_pid(), self.cancel_key())
    }

    /// `PQrequestCancel`, `fe-cancel.c:752`: `PQgetCancel` and `PQcancel` in
    /// one call. It needs only `&self`, so it can run between
    /// [`Connection::send_query`] and [`Connection::get_result`].
    ///
    /// # Errors
    /// The connection has no socket, or `PQcancel` failed.
    pub fn request_cancel(&self) -> Result<(), CancelError> {
        self.get_cancel()
            .ok_or(CancelError::ConnectionNotOpen)?
            .cancel()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream that records what is written and answers reads from a
    /// script: `Ok(bytes)` returns them (empty is EOF), `Err` fails.
    struct Scripted {
        written: Vec<u8>,
        replies: Vec<io::Result<Vec<u8>>>,
    }

    impl Scripted {
        fn replying(replies: Vec<io::Result<Vec<u8>>>) -> Self {
            Scripted {
                written: Vec::new(),
                replies,
            }
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let reply = self.replies.remove(0)?;
            buf[..reply.len()].copy_from_slice(&reply);
            Ok(reply.len())
        }
    }

    impl Write for Scripted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn peer() -> Peer {
        Peer::Unix(PathBuf::from("/nonexistent/rlibpq-cancel/.s.PGSQL.5432"))
    }

    const KEY: [u8; 4] = [1, 2, 3, 4];

    fn packet() -> Vec<u8> {
        Frontend::CancelRequest {
            pid: 99,
            cancel_key: KEY.to_vec(),
        }
        .encode()
    }

    #[test]
    fn get_cancel_builds_the_packet_and_an_empty_key_the_dummy() {
        let cancel = Cancel::new(peer(), 99, &KEY);
        assert_eq!(cancel.packet(), Some(packet().as_slice()));
        assert_eq!(cancel.peer(), &peer());

        let dummy = Cancel::new(peer(), 99, b"");
        assert_eq!(dummy.packet(), None);
        assert_eq!(dummy.cancel(), Err(CancelError::NoCancelKey));
    }

    /// `PQgetCancel` copies `conn->raddr` (`fe-cancel.c:406`), which C fixed
    /// when it connected (`fe-connect.c:3249`), and checks only that the
    /// socket is open (`:377`). A TCP peer that has since reset the socket —
    /// after which `getpeername` answers `ENOTCONN` — does not change that:
    /// the request still goes to the postmaster's address.
    #[test]
    fn get_cancel_survives_a_peer_that_reset_the_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (connected, wait_for_client) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            // AuthenticationOk, BackendKeyData (pid 99), ReadyForQuery.
            socket
                .write_all(b"R\0\0\0\x08\0\0\0\0K\0\0\0\x0c\0\0\0\x63\x01\x02\x03\x04Z\0\0\0\x05I")
                .unwrap();
            wait_for_client.recv().unwrap();
            // The startup packet is still unread, so closing sends RST.
            drop(socket);
        });

        let info = crate::conninfo::parse_conninfo(
            format!("host=127.0.0.1 port={} user=u", addr.port()).as_bytes(),
        )
        .unwrap();
        let mut conn = Connection::connect(&info).unwrap();
        connected.send(()).unwrap();
        server.join().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while conn.consume_input().is_ok() {
            assert!(std::time::Instant::now() < deadline, "no reset arrived");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let cancel = conn.get_cancel().expect("raddr outlives the peer");
        assert_eq!(cancel.peer(), &Peer::Tcp(addr));
        assert_eq!(cancel.packet(), Some(packet().as_slice()));
        // Nobody listens there now: the connect fails, the socket was open.
        assert!(matches!(
            conn.request_cancel(),
            Err(CancelError::Failed {
                step: CancelStep::Connect,
                ..
            })
        ));
    }

    /// The fixed strings of `fe-cancel.c`, byte for byte — including the
    /// dummy object's, which alone has no newline (`:567`).
    #[test]
    fn pqcancel_messages_are_upstreams() {
        assert_eq!(
            CancelError::NoCancelKey.message(),
            b"PQcancel() -- no cancellation key received"
        );
        assert_eq!(
            CancelError::Failed {
                step: CancelStep::Connect,
                errno: 111
            }
            .message(),
            b"PQcancel() -- connect() failed: error 111\n"
        );
        assert_eq!(
            CancelError::Failed {
                step: CancelStep::Send,
                errno: 0
            }
            .message(),
            b"PQcancel() -- send() failed: error 0\n"
        );
        assert_eq!(
            CancelError::ConnectionNotOpen.message(),
            b"PQrequestCancel() -- connection is not open\n"
        );
    }

    /// `PQcancel` against a socket path nobody listens on: `connect()` fails
    /// with `ENOENT`, which is 2 on every target this crate ships to.
    #[test]
    fn pqcancel_reports_a_failed_connect_by_errno() {
        let cancel = Cancel::new(peer(), 99, &KEY);
        assert_eq!(
            cancel.cancel(),
            Err(CancelError::Failed {
                step: CancelStep::Connect,
                errno: 2
            })
        );
    }

    /// An `io::Error` with no `errno` — here the one `std` returns for a
    /// socket path holding a NUL, before any `connect()` — is reported with
    /// its text in C's `PQcancel() -- connect() failed: ` line, not as
    /// `error 0`. The divergence `docs/divergences.md` records.
    #[test]
    fn pqcancel_reports_a_failure_without_an_errno_by_its_text() {
        let cancel = Cancel::new(Peer::Unix(PathBuf::from("/tmp/nul\0padded")), 99, &KEY);
        let err = cancel.cancel().unwrap_err();
        let reason = UnixStream::connect("/tmp/nul\0padded")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            CancelError::FailedWithoutErrno {
                step: CancelStep::Connect,
                reason: reason.clone()
            }
        );
        assert_eq!(
            err.message(),
            format!("PQcancel() -- connect() failed: {reason}\n").into_bytes()
        );
        assert!(!err.message().ends_with(b"error 0\n"));

        let write_zero =
            CancelError::failed(CancelStep::Send, &io::Error::from(io::ErrorKind::WriteZero));
        assert_eq!(
            write_zero.message(),
            format!(
                "PQcancel() -- send() failed: {}\n",
                io::Error::from(io::ErrorKind::WriteZero)
            )
            .into_bytes()
        );
    }

    /// `conn->raddr` for a Unix socket is the path libpq dialled
    /// (`fe-connect.c:3249` copies the address before `connect()`), not
    /// what `getpeername` says afterwards. Here the two differ: the server
    /// binds under `real/` and the client dials through the symlink `link/`,
    /// so `getpeername` would answer `real/…`. On Darwin it answers the
    /// whole NUL-padded `sun_path`, which a cancel cannot connect to at all.
    #[test]
    fn a_unix_peer_is_the_path_dialled_not_the_one_the_server_bound() {
        let dir = std::env::temp_dir().join(format!("rlibpq-raddr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(real.join(".s.PGSQL.5432")).unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            // AuthenticationOk, BackendKeyData (pid 99), ReadyForQuery.
            socket
                .write_all(b"R\0\0\0\x08\0\0\0\0K\0\0\0\x0c\0\0\0\x63\x01\x02\x03\x04Z\0\0\0\x05I")
                .unwrap();
            socket
        });

        let info = crate::conninfo::parse_conninfo(
            format!("host={} port=5432 user=u", link.display()).as_bytes(),
        )
        .unwrap();
        let conn = Connection::connect(&info).unwrap();
        let socket = server.join().unwrap();

        let dialled = link.join(".s.PGSQL.5432");
        assert_eq!(conn.peer(), Some(Peer::Unix(dialled.clone())));
        assert_eq!(conn.get_cancel().unwrap().peer(), &Peer::Unix(dialled));
        drop((conn, socket));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `retry4` … `retry5`: the packet goes out whole, then one read, whose
    /// errors are ignored after an interrupted one is retried.
    #[test]
    fn pqcancel_sends_the_packet_and_waits_for_one_read() {
        for last in [
            Ok(Vec::new()),
            Ok(b"x".to_vec()),
            Err(io::Error::from(io::ErrorKind::ConnectionReset)),
        ] {
            let mut stream =
                Scripted::replying(vec![Err(io::Error::from(io::ErrorKind::Interrupted)), last]);
            assert_eq!(deliver(&mut stream, &packet()), Ok(()));
            assert_eq!(stream.written, packet());
            assert!(stream.replies.is_empty(), "the interrupted read is retried");
        }
    }

    #[test]
    fn pqcancelcreate_without_a_socket_or_a_key_is_bad_with_a_reason() {
        let mut no_socket = CancelConn::new(None, 99, &KEY);
        assert_eq!(no_socket.status(), CancelStatus::Bad);
        assert_eq!(no_socket.error_message(), b"connection not open\n");
        // `PQcancelStart` on a bad object: 0, and nothing appended (:206).
        assert!(!no_socket.blocking());
        assert_eq!(no_socket.error_message(), b"connection not open\n");

        let no_key = CancelConn::new(Some(peer()), 99, b"");
        assert_eq!(no_key.status(), CancelStatus::Bad);
        assert_eq!(no_key.error_message(), b"no cancellation key received\n");
    }

    /// A reset object whose creation failed has nothing to connect to:
    /// `pqConnectDBStart`'s `options_valid` check, bad without a message.
    #[test]
    fn pqcancelreset_of_a_failed_create_still_cannot_send() {
        let mut conn = CancelConn::new(None, 99, &KEY);
        conn.reset();
        assert_eq!(conn.status(), CancelStatus::Allocated);
        assert!(conn.error_message().is_empty());
        assert!(!conn.blocking());
        assert_eq!(conn.status(), CancelStatus::Bad);
        assert!(conn.error_message().is_empty());
    }

    #[test]
    fn pqcancelblocking_sends_the_packet_and_succeeds_at_eof() {
        let mut conn = CancelConn::new(Some(peer()), 99, &KEY);
        assert_eq!(conn.status(), CancelStatus::Allocated);
        let mut sent = Vec::new();
        let ok = conn.blocking_over(|to| {
            assert_eq!(to, &peer(), "the request goes to the original peer");
            Ok(Recorder(&mut sent))
        });
        assert!(ok);
        assert_eq!(sent, packet());
        assert_eq!(conn.status(), CancelStatus::Ok);
        assert!(conn.error_message().is_empty());
    }

    /// The packet the exchange writes is the one `PQsendCancelRequest`
    /// builds (`fe-cancel.c:472`), and nothing else: no startup packet, and
    /// no Terminate at the end (`sendTerminateConn`, `fe-connect.c:5226`).
    #[test]
    fn the_exchange_writes_exactly_the_cancel_request() {
        let cancel = Cancel::new(peer(), 99, &KEY);
        let mut written = Vec::new();
        let result = exchange(&cancel, |_| Ok(Recorder(&mut written)));
        assert_eq!(result, Ok(()));
        assert_eq!(written, packet());
    }

    /// Writes into a borrowed buffer and reads EOF.
    struct Recorder<'a>(&'a mut Vec<u8>);

    impl Read for Recorder<'_> {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for Recorder<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn pqcancelpoll_judges_its_read() {
        assert_eq!(judge_response(Ok(0)), Ok(()));
        assert_eq!(
            judge_response(Ok(1)),
            Err(b"unexpected response from server\n".to_vec())
        );
        let err = judge_response(Err(io::Error::other("boom"))).unwrap_err();
        assert_eq!(err, b"could not receive data from server: boom\n");
    }

    #[test]
    fn data_instead_of_eof_makes_the_request_bad() {
        let mut conn = CancelConn::new(Some(peer()), 99, &KEY);
        assert!(!conn.blocking_over(|_| Ok(Scripted::replying(vec![Ok(b"?".to_vec())]))));
        assert_eq!(conn.status(), CancelStatus::Bad);
        assert_eq!(conn.error_message(), b"unexpected response from server\n");
    }

    /// `PQcancelStart` on an object that already sent (`fe-cancel.c:209`),
    /// then `PQcancelReset` making it usable again (`:337`).
    #[test]
    fn a_sent_request_must_be_reset_before_it_is_sent_again() {
        let mut conn = CancelConn::new(Some(peer()), 99, &KEY);
        assert!(conn.blocking_over(|_| Ok(Scripted::replying(vec![Ok(Vec::new())]))));

        assert!(!conn.blocking_over(|_| -> io::Result<Scripted> {
            unreachable!("nothing is opened for a refused start")
        }));
        assert_eq!(conn.status(), CancelStatus::Bad);
        assert_eq!(
            conn.error_message(),
            b"cancel request is already being sent on this connection\n"
        );

        conn.reset();
        assert_eq!(conn.status(), CancelStatus::Allocated);
        assert!(conn.error_message().is_empty());
        assert!(conn.blocking_over(|_| Ok(Scripted::replying(vec![Ok(Vec::new())]))));
        assert_eq!(conn.status(), CancelStatus::Ok);
    }

    /// The divergence `docs/divergences.md` records: a failed connect is
    /// reported with the `std::io::Error` text, one line, not
    /// `PQconnectPoll`'s `connection to server … failed:` wording.
    #[test]
    fn a_failed_connect_is_one_line_of_io_error_text() {
        let mut conn = CancelConn::new(Some(peer()), 99, &KEY);
        assert!(!conn.blocking());
        assert_eq!(conn.status(), CancelStatus::Bad);
        let expected = format!(
            "{}\n",
            io::Error::from_raw_os_error(2) // ENOENT
        );
        assert_eq!(conn.error_message(), expected.as_bytes());
    }
}
