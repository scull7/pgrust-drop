//! `PGconn` and the calls of `fe-connect.c` and `fe-exec.c` that open, use,
//! reset and close one: `PQconnectdb` and its siblings `PQconnectdbParams`,
//! `PQsetdbLogin`, `PQconnectStart`, `PQconnectStartParams` and
//! `PQconnectPoll`; `PQreset`, `PQresetStart` and `PQresetPoll`; `PQstatus`,
//! `PQerrorMessage`, `PQexec` and `PQfinish`; [`exec_with`], the
//! `PQexecStart` / `PQexecFinish` frame the extended-query calls share with
//! `PQexec`; and [`PGconn::send_with`], the `PQsendQueryStart` frame of the
//! calls that send a command without waiting for it.
//!
//! The connection itself is an `rlibpq` [`Connection`]; this module keeps
//! what C reads beside it — the status, `conn->errorMessage` as a C string,
//! the option fields `fillPGconn` and `pqConnectOptions2` fill, the host it
//! reached, the parameters the server reported, the notice hooks and the
//! notifications not yet collected — and turns what a call returned into
//! what C's `PGconn` would then hold.
//!
//! Every connection is made blocking, `rlibpq`'s [`Connection::connect`].
//! `PQconnectStart` and `PQresetStart` therefore return with the attempt
//! already over, and `PQconnectPoll` answers from where it ended; see
//! `docs/divergences.md`.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int};
use std::ptr::null_mut;

use rlibpq::pg_config::DEF_PGPORT_STR;
use rlibpq::{
    AsyncStatus, ConnHost, ConnInfo, Connection, ConnectionError, Env, ExecStatus, Filesystem,
    PipelineStatus, QueryResult, conn_hosts, conninfo_array_parse, parse_conninfo,
    recognized_connection_string,
};

use crate::ctext::CText;
use crate::notice::{NoticeHooks, receive};
use crate::result::PGresult;

/// `ConnStatusType`, `libpq-fe.h:82`, as far as a blocking connection ever
/// leaves it: `CONNECTION_OK` (0) or `CONNECTION_BAD` (1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnStatus {
    Ok,
    Bad,
}

impl ConnStatus {
    const fn code(self) -> c_int {
        match self {
            ConnStatus::Ok => 0,
            ConnStatus::Bad => 1,
        }
    }
}

/// `PostgresPollingStatusType`, `libpq-fe.h:113`, as far as a blocking
/// connection ever reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollingStatus {
    /// `PGRES_POLLING_FAILED` (0).
    Failed,
    /// `PGRES_POLLING_OK` (3).
    Ok,
}

impl PollingStatus {
    const fn code(self) -> c_int {
        match self {
            PollingStatus::Failed => 0,
            PollingStatus::Ok => 3,
        }
    }
}

/// What `fillPGconn` and `pqConnectOptions2` leave in a `PGconn`'s option
/// fields (`conn->pghost`, `conn->dbName`, … and `conn->connhost[]`).
#[derive(Debug)]
pub(crate) struct Options {
    /// Every option, as the `PGconn` field it is copied into holds it —
    /// what `PQconninfo` lays out (`fe-connect.c:7415`).
    pub(crate) info: ConnInfo,
    /// Each option's value as C reads it, in `PQconninfoOptions[]` order.
    texts: Vec<Option<CText>>,
    /// `conn->connhost[]`, or the message `pqConnectOptions2` refused the
    /// host, `hostaddr` and `port` lists with, which leaves
    /// `conn->options_valid` false.
    hosts: Result<Vec<ConnHost>, Vec<u8>>,
}

impl Options {
    /// Calculation: `pqConnectOptions2` (`fe-connect.c:1247`) as far as it
    /// changes what the option fields read, over `info` as `fillPGconn`
    /// copied it: the host list (`:1256`-`:1392`), then a user name when
    /// none was given (`:1400`), the database name defaulting to it
    /// (`:1414`), and the password file defaulting to `~/.pgpass` when there
    /// is no password (`:1426`-`:1441`; the home directory is `HOME`, as in
    /// `rlibpq`'s service-file lookup). What the function validates after
    /// that, `rlibpq`'s [`Connection::connect`] validates.
    pub(crate) fn derive(mut info: ConnInfo, env: &Env) -> Self {
        let hosts = conn_hosts(&info).map_err(|err| with_newline(err.message()));
        if hosts.is_ok() {
            let empty = |info: &ConnInfo, keyword| info.get(keyword).is_none_or(<[u8]>::is_empty);
            if empty(&info, "user")
                && let Some(user) = env.effective_user()
            {
                set(&mut info, b"user", user);
            }
            if empty(&info, "dbname")
                && let Some(user) = info.get("user").map(<[u8]>::to_vec)
            {
                set(&mut info, b"dbname", &user);
            }
            if empty(&info, "password")
                && empty(&info, "passfile")
                && let Some(home) = env.get("HOME").filter(|home| !home.is_empty())
            {
                set(&mut info, b"passfile", &[home, b"/.pgpass"].concat());
            }
        }
        let texts = info
            .iter()
            .map(|option| option.value.map(CText::new))
            .collect();
        Options { info, texts, hosts }
    }

    /// The field `keyword` is copied into, NULL when it holds nothing.
    pub(crate) fn text(&self, keyword: &str) -> Option<&CText> {
        ConnInfo::index_of(keyword.as_bytes()).and_then(|index| self.texts[index].as_ref())
    }
}

/// Store `value` under `keyword`, a row of `PQconninfoOptions[]`.
fn set(info: &mut ConnInfo, keyword: &[u8], value: &[u8]) {
    info.set(keyword, value)
        .expect("a keyword of PQconninfoOptions[]");
}

/// `PGconn`, opaque to C (`libpq-fe.h:202`: `typedef struct pg_conn PGconn`).
#[derive(Debug)]
pub struct PGconn {
    /// The open connection; `None` once it is `CONNECTION_BAD`.
    pub(crate) connection: Option<Connection>,
    pub(crate) status: ConnStatus,
    /// `conn->errorMessage`.
    pub(crate) error_message: CText,
    /// How many of the connection's notices have been handed to the notice
    /// receiver already.
    notices_reported: usize,
    /// How many of the connection's notifications have been moved to
    /// [`PGconn::notifies`] already.
    notifies_taken: usize,
    /// `conn->notifyHead` … `notifyTail`: pid, channel and payload of each
    /// notification not yet handed out by `PQnotifies`. They outlive a lost
    /// connection, as C's list does until `pqClosePGconn`.
    pub(crate) notifies: VecDeque<(i32, Vec<u8>, Vec<u8>)>,
    /// `conn->noticeHooks`.
    pub(crate) hooks: NoticeHooks,
    /// `conn->errorReported`: how much of `conn->errorMessage` a result has
    /// carried to the caller already (`pqPrepareAsyncResult`,
    /// `fe-exec.c:870`, `:902`).
    error_reported: usize,
    /// Results the connection had complete when it was lost, which
    /// `PQgetResult` still hands out, as C's `conn->result` outlives
    /// `pqDropConnection`.
    stashed: VecDeque<PGresult>,
    /// The connection was lost while a command was running, and the next
    /// `PQgetResult` owes an error result for it.
    error_result: Option<AfterLoss>,
    /// The option fields; `None` while they are all NULL, as a conninfo
    /// that did not parse leaves them.
    pub(crate) options: Option<Options>,
    /// What `PQhost` and `PQport` read from
    /// `conn->connhost[conn->whichhost]`.
    pub(crate) host: CText,
    pub(crate) port: CText,
    /// `conn->pstatus`: each parameter the server reported and its value.
    /// An entry is replaced only when its value changes, so a pointer
    /// `PQparameterStatus` returned stays valid until then, as in C.
    pub(crate) parameters: Vec<(Vec<u8>, CText)>,
}

impl PGconn {
    /// `pqMakeEmptyPGconn` (`fe-connect.c:4940`) and the option parsing a
    /// connect call starts with: `CONNECTION_BAD` with `conn->errorMessage`
    /// set when the options did not parse, and with no fields filled.
    fn new(options: Result<ConnInfo, Vec<u8>>) -> Self {
        let (options, error_message) = match options {
            Ok(info) => (
                Some(Options::derive(info, &Env::from_process())),
                Vec::new(),
            ),
            Err(message) => (None, message),
        };
        let mut conn = PGconn {
            connection: None,
            status: ConnStatus::Bad,
            error_message: CText::new(&error_message),
            notices_reported: 0,
            notifies_taken: 0,
            notifies: VecDeque::new(),
            hooks: NoticeHooks::DEFAULT,
            error_reported: 0,
            stashed: VecDeque::new(),
            error_result: None,
            options,
            host: CText::default(),
            port: CText::default(),
            parameters: Vec::new(),
        };
        conn.set_host(None);
        if let Some(Options {
            hosts: Err(message),
            ..
        }) = &conn.options
        {
            conn.error_message = CText::new(message);
        }
        conn
    }

    /// Point `PQhost` and `PQport` at `host`: its `host`, else its
    /// `hostaddr`, else `""` (`fe-connect.c:7505`); its port, else
    /// `DEF_PGPORT_STR` (`:7541`). With no host entry at all — C's NULL
    /// `conn->connhost` — `""` and `DEF_PGPORT_STR`.
    fn set_host(&mut self, host: Option<&ConnHost>) {
        let given = |value: &Option<Vec<u8>>| value.clone().filter(|value| !value.is_empty());
        let name = host.and_then(|host| given(&host.host).or_else(|| given(&host.hostaddr)));
        let port = host.and_then(|host| given(&host.port));
        self.host = CText::new(&name.unwrap_or_default());
        self.port = CText::new(&port.unwrap_or_else(|| DEF_PGPORT_STR.as_bytes().to_vec()));
    }

    /// Action: `pqConnectDBStart` and `pqConnectDBComplete`
    /// (`fe-connect.c:2704`, `:2782`), blocking: nothing when the options
    /// are not valid (`:2709`), otherwise the attempt and the `PGconn` it
    /// leaves; the startup's notices are returned, to be delivered.
    ///
    /// After a failed attempt `PQhost` names the last host in the list,
    /// where C names the one the attempt stopped at; see
    /// `docs/divergences.md`.
    fn connect(&mut self) -> Notices {
        let Some(Options {
            info,
            hosts: Ok(hosts),
            ..
        }) = &self.options
        else {
            return Notices::default();
        };
        match Connection::connect(info) {
            Ok(connection) => {
                let host = connection.host().cloned();
                self.connection = Some(connection);
                self.status = ConnStatus::Ok;
                self.error_message = CText::default();
                self.set_host(host.as_ref());
                self.collect()
            }
            Err(err) => {
                let last = hosts.last().cloned();
                self.error_message = CText::new(&with_newline(err.message()));
                self.set_host(last.as_ref());
                Notices::default()
            }
        }
    }

    /// Action: `pqClosePGconn` (`fe-connect.c:5254`): Terminate if the
    /// connection is up, close it, and forget the error and everything the
    /// server said; the options and the notice hooks stay.
    fn close(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            // "Ignore any error" (`:5236`-`:5237`).
            let _ = connection.terminate();
        }
        self.status = ConnStatus::Bad;
        self.error_message = CText::default();
        self.notices_reported = 0;
        self.notifies_taken = 0;
        self.notifies.clear();
        self.error_reported = 0;
        self.stashed.clear();
        self.error_result = None;
        self.parameters.clear();
    }

    /// Bring [`PGconn::parameters`] up to date with what the server has
    /// reported, keeping each entry whose value did not change.
    fn sync_parameters(&mut self) {
        let Some(connection) = &self.connection else {
            return;
        };
        let mut previous = std::mem::take(&mut self.parameters);
        self.parameters = connection
            .parameter_statuses()
            .into_iter()
            .map(|(name, value)| {
                match previous
                    .iter()
                    .position(|(known, text)| known == name && text.bytes() == value)
                {
                    Some(index) => previous.swap_remove(index),
                    None => (name.to_vec(), CText::new(value)),
                }
            })
            .collect();
    }

    /// Bring the `PGconn` up to date with what the connection has parsed
    /// since the last call: the parameters, the notifications, which join
    /// [`PGconn::notifies`], and the notices, returned as `PGresult`s
    /// carrying the hooks, for [`Notices::deliver`].
    pub(crate) fn collect(&mut self) -> Notices {
        self.sync_parameters();
        let Some(connection) = &self.connection else {
            return Notices::default();
        };
        let notifications = &connection.notifications()[self.notifies_taken..];
        self.notifies.extend(notifications.iter().cloned());
        self.notifies_taken += notifications.len();
        let notices = &connection.notices()[self.notices_reported..];
        self.notices_reported += notices.len();
        Notices(
            notices
                .iter()
                .map(|notice| {
                    let notice = QueryResult::with_error(ExecStatus::NonfatalError, notice.clone());
                    PGresult::from_result(&notice, self.hooks)
                })
                .collect(),
        )
    }

    /// `libpq_append_conn_error` and its kin: add `message`, which ends in
    /// its newline, to `conn->errorMessage`.
    pub(crate) fn append_error(&mut self, message: &[u8]) {
        self.error_message = CText::new(&[self.error_message.bytes(), message].concat());
    }

    /// `pqClearConnErrorState`, `libpq-int.h:927`: a new query cycle
    /// starts with no error, none of it reported.
    pub(crate) fn clear_error(&mut self) {
        self.error_message = CText::default();
        self.error_reported = 0;
    }

    /// Action: what a failed read or parse leaves (`pqReadData`,
    /// `fe-misc.c:833`-`:842`; `handleSyncLoss`, `fe-protocol3.c:504`): the
    /// error added to `conn->errorMessage`, the connection dropped and
    /// `CONNECTION_BAD`, and — when a command was running — an error result
    /// owed to the next `PQgetResult`, which `then` says how C comes to.
    ///
    /// Results already complete are kept for `PQgetResult` first.
    pub(crate) fn lose(&mut self, err: &ConnectionError, then: AfterLoss) {
        while let Some(connection) = self.connection.as_mut()
            && matches!(
                connection.async_status(),
                AsyncStatus::Ready | AsyncStatus::ReadyMore
            )
        {
            // A complete result is handed over without a read.
            let Ok(Some(result)) = connection.get_result() else {
                break;
            };
            if result.status() == ExecStatus::FatalError {
                self.append_error(&result.error_message());
            }
            self.stashed
                .push_back(PGresult::from_result(&result, self.hooks));
        }
        self.append_error(&with_newline(err.message()));
        if self
            .connection
            .take()
            .is_some_and(|connection| connection.async_status() != AsyncStatus::Idle)
        {
            self.error_result = Some(then);
        }
        self.status = ConnStatus::Bad;
    }

    /// Action: what a lost connection still owes `PQgetResult`: each result
    /// [`PGconn::lose`] kept, then the error result, once
    /// (`pqPrepareAsyncResult`, `fe-exec.c:857`, with no result from the
    /// server) — the error text no result has carried yet, which is then
    /// reported (`:901`-`:902`).
    pub(crate) fn take_error_result(&mut self) -> Option<PGresult> {
        if let Some(result) = self.stashed.pop_front() {
            if result.is_fatal_error() {
                self.error_reported = self.error_message.bytes().len();
            }
            return Some(result);
        }
        if self.error_result.take()? == AfterLoss::Wait {
            // `pqSocketCheck`, `fe-misc.c:1242`-`:1245`.
            self.append_error(b"invalid socket\n");
        }
        let unreported = self
            .error_message
            .bytes()
            .get(self.error_reported..)
            .unwrap_or_default();
        let result = PGresult::fatal_error(unreported, self.hooks);
        self.error_reported = self.error_message.bytes().len();
        Some(result)
    }

    /// An error result from the server: its message joins
    /// `conn->errorMessage` (`pqGetErrorNotice3`, `fe-protocol3.c:995`),
    /// and returning it reports all of that (`fe-exec.c:870`).
    pub(crate) fn report_error(&mut self, message: &[u8]) {
        self.append_error(message);
        self.error_reported = self.error_message.bytes().len();
    }

    /// Action: `PQsendQueryStart` (`fe-exec.c:1690`) and the call's own
    /// sending, for the calls that return without waiting: 1 when the
    /// command went out, else 0 with the reason added to
    /// `conn->errorMessage`.
    ///
    /// The error is cleared first unless a command is still running
    /// (`:1700`, "the error buffer belongs to that command"); then a
    /// connection that is not up (`:1704`) and one busy with another
    /// command (`:1711`) are refused before `send` checks the call's own
    /// arguments, as C checks them after `PQsendQueryStart`.
    pub(crate) fn send_with(
        &mut self,
        send: impl FnOnce(&mut Connection) -> Result<Result<(), ConnectionError>, Refused>,
    ) -> c_int {
        let running = self
            .connection
            .as_ref()
            .is_some_and(|connection| connection.async_status() != AsyncStatus::Idle);
        if !running {
            self.clear_error();
        }
        let Some(connection) = self.connection.as_mut() else {
            self.append_error(b"no connection to the server\n");
            return 0;
        };
        if running && connection.pipeline_status() == PipelineStatus::Off {
            self.append_error(b"another command is already in progress\n");
            return 0;
        }
        match send(connection) {
            Ok(Ok(())) => 1,
            Ok(Err(err)) => {
                self.append_error(&with_newline(err.message()));
                0
            }
            Err(Refused(message)) => {
                self.append_error(&with_newline(message));
                0
            }
        }
    }
}

/// How C comes to owe the error result of a connection lost while a
/// command was running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterLoss {
    /// `pqSaveErrorResult` (`fe-exec.c:809`): a parse failure
    /// (`handleSyncLoss`, `fe-protocol3.c:504`) or a read `PQgetResult`
    /// made itself (`fe-exec.c:2114`-`:2121`).
    ErrorResult,
    /// A read `PQconsumeInput` made: the command is still `PGASYNC_BUSY`,
    /// so the next `PQgetResult` waits on the closed socket, which fails
    /// with "invalid socket" before the error result is made
    /// (`fe-exec.c:2114`-`:2121`, `fe-misc.c:1242`-`:1245`).
    Wait,
}

/// Notices [`PGconn::collect`] took from the connection, not yet handed to
/// the receiver.
///
/// They are delivered once the shim no longer holds the `PGconn`, because a
/// receiver is C code that may call back into the library with it.
#[derive(Debug, Default)]
#[must_use]
pub(crate) struct Notices(Vec<PGresult>);

impl Notices {
    /// Action: hand each notice to the receiver it carries, oldest first, as
    /// `pqGetErrorNotice3` does as it parses one (`fe-protocol3.c:1012`).
    pub(crate) fn deliver(self) {
        for notice in &self.0 {
            receive(notice);
        }
    }
}

/// Calculation: `message` as `libpq_append_conn_error` leaves it in
/// `conn->errorMessage`, ending in the newline it adds (`fe-misc.c:1539`).
/// `rlibpq`'s messages carry that newline for some errors and not others.
pub(crate) fn with_newline(mut message: Vec<u8>) -> Vec<u8> {
    if !message.ends_with(b"\n") {
        message.push(b'\n');
    }
    message
}

/// What one `PQexec` leaves behind.
#[derive(Debug)]
struct ExecOutcome {
    /// What `PQexec` returns; `None` is NULL, "the query was not even sent".
    result: Option<PGresult>,
    /// `conn->errorMessage` afterwards.
    error_message: Vec<u8>,
    /// The connection is gone, so the status is now `CONNECTION_BAD`.
    lost: bool,
}

/// Calculation: `PQexecFinish` (`fe-exec.c:2427`) over what
/// [`Connection::exec`] returned.
///
/// The last result is the one returned (`:2444`-`:2455`), and
/// `conn->errorMessage`, cleared when the query began, has accumulated the
/// message of every error result on the way (`:2433`-`:2436`). A refusal
/// before anything was sent is NULL, the refusal the error message. Any
/// other failure lost the connection: C then returns the `PGRES_FATAL_ERROR`
/// result `pqPrepareAsyncResult` (`fe-exec.c:857`) builds from the error
/// message, and the connection is `CONNECTION_BAD`.
fn exec_outcome(
    outcome: Result<Vec<QueryResult>, ConnectionError>,
    hooks: NoticeHooks,
) -> ExecOutcome {
    match outcome {
        Ok(results) => ExecOutcome {
            error_message: results
                .iter()
                .filter(|result| result.status() == ExecStatus::FatalError)
                .flat_map(QueryResult::error_message)
                .collect(),
            result: results
                .last()
                .map(|result| PGresult::from_result(result, hooks)),
            lost: false,
        },
        Err(err @ (ConnectionError::Pipeline(_) | ConnectionError::Argument(_))) => ExecOutcome {
            result: None,
            error_message: with_newline(err.message()),
            lost: false,
        },
        Err(err) => {
            let message = with_newline(err.message());
            ExecOutcome {
                result: Some(PGresult::fatal_error(&message, hooks)),
                error_message: message,
                lost: true,
            }
        }
    }
}

/// Action: `connectOptions1` (`fe-connect.c:1073`), which is
/// `parse_connection_string` with `use_defaults` (`:6236`): parse, then fill
/// in the defaults from the environment and the service files; or the error
/// message it leaves.
fn string_options(conninfo: &[u8]) -> Result<ConnInfo, Vec<u8>> {
    let mut info = parse_conninfo(conninfo).map_err(|err| with_newline(err.message()))?;
    info.add_defaults(&Env::from_process(), &Filesystem)
        .map_err(|err| with_newline(err.message()))?;
    Ok(info)
}

/// Action: `conninfo_array_parse` with `use_defaults` (`fe-connect.c:6466`,
/// `:6600`-`:6607`), as `PQconnectStartParams` runs it (`:886`).
fn array_options(
    params: &[(&[u8], Option<&[u8]>)],
    expand_dbname: bool,
) -> Result<ConnInfo, Vec<u8>> {
    let mut info =
        conninfo_array_parse(params, expand_dbname).map_err(|err| with_newline(err.message()))?;
    info.add_defaults(&Env::from_process(), &Filesystem)
        .map_err(|err| with_newline(err.message()))?;
    Ok(info)
}

/// The arguments of `PQsetdbLogin` other than `pgtty`, which it ignores.
#[derive(Debug, Clone, Copy, Default)]
struct SetdbLogin<'a> {
    pghost: Option<&'a [u8]>,
    pgport: Option<&'a [u8]>,
    pgoptions: Option<&'a [u8]>,
    db_name: Option<&'a [u8]>,
    login: Option<&'a [u8]>,
    pwd: Option<&'a [u8]>,
}

impl SetdbLogin<'_> {
    /// `dbName`, when it looks like a connection string
    /// (`fe-connect.c:2250`): the one `connectOptions1` parses. Otherwise
    /// `""` is, for the defaults alone (`:2261`).
    fn conninfo(&self) -> &[u8] {
        self.db_name
            .filter(|db_name| recognized_connection_string(db_name))
            .unwrap_or_default()
    }

    /// Calculation: `PQsetdbLogin` (`fe-connect.c:2231`) past
    /// `connectOptions1`: a plain `dbName`, then every argument that is
    /// neither NULL nor empty, overrides what the defaults — or `dbName` as
    /// a connection string — set (`:2265`-`:2316`).
    fn apply(&self, mut info: ConnInfo) -> ConnInfo {
        let db_name = self
            .db_name
            .filter(|db_name| !recognized_connection_string(db_name));
        for (keyword, value) in [
            (&b"dbname"[..], db_name),
            (b"host", self.pghost),
            (b"port", self.pgport),
            (b"options", self.pgoptions),
            (b"user", self.login),
            (b"password", self.pwd),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                set(&mut info, keyword, value);
            }
        }
        info
    }
}

/// Action: a `PGconn` for `options`, connected when they allow it, handed
/// to C.
fn connect_with(options: Result<ConnInfo, Vec<u8>>) -> *mut PGconn {
    let mut conn = PGconn::new(options);
    let notices = conn.connect();
    let conn = Box::into_raw(Box::new(conn));
    notices.deliver();
    conn
}

/// `PQconnectdb`, `fe-connect.c:820`: connect, blocking, and return the
/// `PGconn` whatever happened; `PQstatus` and `PQerrorMessage` tell.
///
/// A NULL `conninfo` is taken as `""`.
///
/// # Safety
///
/// `conninfo` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectdb(conninfo: *const c_char) -> *mut PGconn {
    // SAFETY: the caller's contract.
    let conninfo = unsafe { c_bytes(conninfo) }.unwrap_or_default();
    connect_with(string_options(conninfo))
}

/// The keyword/value pairs of `PQconnectdbParams`: `keywords` up to its
/// NULL, each with the `values` entry at the same index. A NULL `keywords`
/// is no pairs.
///
/// # Safety
///
/// `keywords` is null or a NULL-terminated array of NUL-terminated
/// strings; `values` has an entry, null or a NUL-terminated string, for
/// every keyword. All of them outlive `'a`.
unsafe fn param_pairs<'a>(
    keywords: *const *const c_char,
    values: *const *const c_char,
) -> Vec<(&'a [u8], Option<&'a [u8]>)> {
    let mut pairs = Vec::new();
    if keywords.is_null() {
        return pairs;
    }
    for index in 0.. {
        // SAFETY: the caller's contract; the walk stops at the NULL.
        let Some(keyword) = (unsafe { c_bytes(*keywords.add(index)) }) else {
            break;
        };
        // SAFETY: the caller's contract.
        let value = unsafe { c_bytes(*values.add(index)) };
        pairs.push((keyword, value));
    }
    pairs
}

/// `PQconnectdbParams`, `fe-connect.c:765`: [`PQconnectdb`] with the
/// options as keyword/value arrays, the first `dbname` expanded as a
/// connection string when `expand_dbname` is nonzero.
///
/// # Safety
///
/// `keywords` is null or a NULL-terminated array of NUL-terminated
/// strings, and `values` has an entry, null or a NUL-terminated string, for
/// every keyword.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectdbParams(
    keywords: *const *const c_char,
    values: *const *const c_char,
    expand_dbname: c_int,
) -> *mut PGconn {
    // SAFETY: the caller's contract.
    let pairs = unsafe { param_pairs(keywords, values) };
    connect_with(array_options(&pairs, expand_dbname != 0))
}

/// `PQsetdbLogin`, `fe-connect.c:2231` (and `PQsetdb`, the macro over it at
/// `libpq-fe.h:339`): connect with the defaults, `dbName` taken as a
/// connection string when it looks like one, and each other argument that
/// is neither NULL nor empty overriding them. `pgtty` is ignored.
///
/// # Safety
///
/// Each argument is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQsetdbLogin(
    pghost: *const c_char,
    pgport: *const c_char,
    pgoptions: *const c_char,
    _pgtty: *const c_char,
    db_name: *const c_char,
    login: *const c_char,
    pwd: *const c_char,
) -> *mut PGconn {
    // SAFETY: the caller's contract, for every argument.
    let args = unsafe {
        SetdbLogin {
            pghost: c_bytes(pghost),
            pgport: c_bytes(pgport),
            pgoptions: c_bytes(pgoptions),
            db_name: c_bytes(db_name),
            login: c_bytes(login),
            pwd: c_bytes(pwd),
        }
    };
    connect_with(string_options(args.conninfo()).map(|info| args.apply(info)))
}

/// `PQconnectStart`, `fe-connect.c:948`. The connection is made blocking,
/// so the attempt is over when this returns; [`PQconnectPoll`] then
/// reports how it ended (`docs/divergences.md`).
///
/// # Safety
///
/// `conninfo` is null or a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectStart(conninfo: *const c_char) -> *mut PGconn {
    // SAFETY: the caller's contract.
    unsafe { PQconnectdb(conninfo) }
}

/// `PQconnectStartParams`, `fe-connect.c:867`: [`PQconnectdbParams`],
/// blocking, as [`PQconnectStart`] is.
///
/// # Safety
///
/// As for [`PQconnectdbParams`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectStartParams(
    keywords: *const *const c_char,
    values: *const *const c_char,
    expand_dbname: c_int,
) -> *mut PGconn {
    // SAFETY: the caller's contract.
    unsafe { PQconnectdbParams(keywords, values, expand_dbname) }
}

/// `PQconnectPoll`, `fe-connect.c:2908`, for a connection that is no
/// longer being made: `PGRES_POLLING_OK` for `CONNECTION_OK` (`:2929`),
/// `PGRES_POLLING_FAILED` for `CONNECTION_BAD` (`:2926`) and for NULL
/// (`:2915`).
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconnectPoll(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    match unsafe { conn.as_ref() } {
        Some(conn) if conn.status == ConnStatus::Ok => PollingStatus::Ok,
        _ => PollingStatus::Failed,
    }
    .code()
}

/// `PQreset`, `fe-connect.c:5315`: close the connection and make it again
/// with the same options; nothing for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQreset(conn: *mut PGconn) {
    // SAFETY: the caller's contract.
    if let Some(conn) = unsafe { conn.as_mut() } {
        conn.close();
        conn.connect().deliver();
    }
}

/// `PQresetStart`, `fe-connect.c:5348`: [`PQreset`], blocking as
/// [`PQconnectStart`] is; 1 when the connection is up again, 0 when it is
/// not or `conn` is NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQresetStart(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return 0;
    };
    conn.close();
    let notices = conn.connect();
    let up = conn.status == ConnStatus::Ok;
    notices.deliver();
    c_int::from(up)
}

/// `PQresetPoll`, `fe-connect.c:5367`: [`PQconnectPoll`].
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQresetPoll(conn: *mut PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { PQconnectPoll(conn) }
}

/// `PQfinish`, `fe-connect.c:5301`: send Terminate if the connection is up
/// (`sendTerminateConn`, `:5233`), close it and free the `PGconn`; nothing
/// for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library, not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfinish(conn: *mut PGconn) {
    if conn.is_null() {
        return;
    }
    // SAFETY: every `PGconn` this crate hands out is `Box::into_raw`.
    unsafe { Box::from_raw(conn) }.close();
}

/// `PQstatus`, `fe-connect.c:7575`: `CONNECTION_BAD` for NULL.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQstatus(conn: *const PGconn) -> c_int {
    // SAFETY: the caller's contract.
    unsafe { conn.as_ref() }
        .map_or(ConnStatus::Bad, |conn| conn.status)
        .code()
}

/// `PQerrorMessage`, `fe-connect.c:7638`: the most recent error, `""` when
/// there is none.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQerrorMessage(conn: *const PGconn) -> *mut c_char {
    // SAFETY: the caller's contract.
    match unsafe { conn.as_ref() } {
        Some(conn) => conn.error_message.as_ptr(),
        None => c"connection pointer is NULL\n".as_ptr().cast_mut(),
    }
}

/// Why `PQsendQueryParams` and its siblings sent nothing: an argument they
/// check once the connection is known to be idle, the message without the
/// newline `libpq_append_conn_error` adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) Vec<u8>);

impl Refused {
    /// `fe-exec.c:1524`, `:1570`: "command string is a null pointer".
    pub(crate) fn null_command() -> Self {
        Refused(b"command string is a null pointer".to_vec())
    }

    /// `fe-exec.c:1565`, `:1664`: "statement name is a null pointer".
    pub(crate) fn null_statement() -> Self {
        Refused(b"statement name is a null pointer".to_vec())
    }
}

/// Action: the shape every blocking `PQexec*` shares — `PQexecStart`,
/// `PQsendQueryStart`, the call's own work, then `PQexecFinish` — with
/// `send` checking the call's arguments and running it on the open
/// connection.
///
/// NULL for a NULL `conn` (`PQexecStart`, `fe-exec.c:2365`), for a
/// connection that is not up (`PQsendQueryStart`, `:1704`-`:1708`) and for
/// an argument `send` refuses; otherwise the last result.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library.
pub(crate) unsafe fn exec_with(
    conn: *mut PGconn,
    send: impl FnOnce(&mut Connection) -> Result<Result<Vec<QueryResult>, ConnectionError>, Refused>,
) -> *mut PGresult {
    // SAFETY: the caller's contract.
    let Some(conn) = (unsafe { conn.as_mut() }) else {
        return null_mut();
    };
    // `PQexecStart`, `fe-exec.c:2374`: a new query cycle clears the error,
    // and the results left over — a lost connection's error result among
    // them — are discarded (`:2386`).
    conn.clear_error();
    conn.stashed.clear();
    conn.error_result = None;
    let Some(connection) = conn.connection.as_mut() else {
        conn.error_message = CText::new(b"no connection to the server\n");
        return null_mut();
    };
    let outcome = match send(connection) {
        Ok(outcome) => exec_outcome(outcome, conn.hooks),
        Err(Refused(message)) => {
            conn.error_message = CText::new(&with_newline(message));
            return null_mut();
        }
    };
    let notices = conn.collect();
    conn.error_message = CText::new(&outcome.error_message);
    conn.error_reported = outcome.error_message.len();
    if outcome.lost {
        conn.connection = None;
        conn.status = ConnStatus::Bad;
    }
    notices.deliver();
    outcome
        .result
        .map_or(null_mut(), |result| Box::into_raw(Box::new(result)))
}

/// The bytes of a C string argument, or `None` for NULL.
///
/// # Safety
///
/// `text` is null or a NUL-terminated string that outlives `'a`.
pub(crate) unsafe fn c_bytes<'a>(text: *const c_char) -> Option<&'a [u8]> {
    // SAFETY: the caller's contract.
    (!text.is_null()).then(|| unsafe { CStr::from_ptr(text) }.to_bytes())
}

/// `PQexec`, `fe-exec.c:2279`: send `query` and wait for every result;
/// return the last, or NULL when nothing was sent.
///
/// # Safety
///
/// `conn` is null or a live `PGconn` from this library; `query` is null or
/// a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQexec(conn: *mut PGconn, query: *const c_char) -> *mut PGresult {
    // SAFETY: the caller's contract.
    unsafe {
        exec_with(conn, |connection| {
            // The argument check of `PQsendQueryInternal`,
            // `fe-exec.c:1453`-`:1457`.
            let query = c_bytes(query).ok_or_else(Refused::null_command)?;
            Ok(connection.exec(query))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rlibpq::{PipelineError, ResultError};

    fn error_result(message: &[u8]) -> QueryResult {
        QueryResult::with_error(
            ExecStatus::FatalError,
            ResultError::new(vec![(b'S', b"ERROR".to_vec()), (b'M', message.to_vec())]),
        )
    }

    #[test]
    fn with_newline_ends_the_message_in_exactly_one_newline() {
        assert_eq!(with_newline(b"boom".to_vec()), b"boom\n");
        assert_eq!(with_newline(b"boom\n".to_vec()), b"boom\n");
    }

    #[test]
    fn exec_returns_the_last_result_and_every_error_message() {
        let outcome = exec_outcome(
            Ok(vec![
                QueryResult::new(ExecStatus::TuplesOk),
                error_result(b"division by zero"),
            ]),
            NoticeHooks::NONE,
        );
        assert!(!outcome.lost);
        assert_eq!(outcome.error_message, b"ERROR:  division by zero\n");
        let result = outcome.result.expect("the last result");
        // SAFETY: `result` is a live `PGresult`.
        assert_eq!(
            unsafe { crate::result::PQresultStatus(&raw const result) },
            7
        );

        let fine = exec_outcome(
            Ok(vec![QueryResult::new(ExecStatus::CommandOk)]),
            NoticeHooks::NONE,
        );
        assert_eq!(fine.error_message, b"");
    }

    #[test]
    fn a_refusal_sends_nothing_and_a_broken_connection_is_lost() {
        let refused = exec_outcome(
            Err(PipelineError::ExecDuringCopyBoth.into()),
            NoticeHooks::NONE,
        );
        assert!(refused.result.is_none());
        assert!(!refused.lost);
        assert!(refused.error_message.ends_with(b"\n"));

        let lost = exec_outcome(
            Err(ConnectionError::ServerClosedConnection),
            NoticeHooks::NONE,
        );
        assert!(lost.lost);
        assert_eq!(
            lost.error_message,
            with_newline(ConnectionError::ServerClosedConnection.message())
        );
        assert!(lost.result.is_some());
    }

    /// Options that did not parse leave every field NULL, `PQhost` `""` and
    /// `PQport` `DEF_PGPORT_STR`, and `conn->errorMessage` the reason.
    #[test]
    fn options_that_do_not_parse_leave_a_bad_conn_with_no_fields() {
        let mut conn = PGconn::new(Err(b"no\n".to_vec()));
        conn.connect().deliver();
        assert_eq!(conn.status.code(), 1);
        assert_eq!(conn.error_message.bytes(), b"no\n");
        assert!(conn.options.is_none());
        assert_eq!(conn.host.bytes(), b"");
        assert_eq!(conn.port.bytes(), b"5432");
        assert_eq!(ConnStatus::Ok.code(), 0);
        assert_eq!(PollingStatus::Ok.code(), 3);
        assert_eq!(PollingStatus::Failed.code(), 0);
    }

    fn info(conninfo: &str) -> ConnInfo {
        parse_conninfo(conninfo.as_bytes()).expect("parses")
    }

    /// `pqConnectOptions2`, `fe-connect.c:1400`-`:1441`: the database
    /// defaults to the user, the user to the effective user, and the
    /// password file to `~/.pgpass` when no password was given.
    #[test]
    fn derive_fills_the_user_the_database_and_the_password_file() {
        let env = Env::empty().with("USER", "me").with("HOME", "/h");
        let options = Options::derive(info("host=a"), &env);
        assert_eq!(options.info.get("user"), Some(b"me".as_slice()));
        assert_eq!(options.info.get("dbname"), Some(b"me".as_slice()));
        assert_eq!(options.info.get("passfile"), Some(b"/h/.pgpass".as_slice()));
        assert_eq!(
            options.text("dbname").map(CText::bytes),
            Some(b"me".as_slice())
        );
        assert!(options.text("hostaddr").is_none());

        let given = Options::derive(info("user=u dbname=d password=p"), &env);
        assert_eq!(given.info.get("user"), Some(b"u".as_slice()));
        assert_eq!(given.info.get("dbname"), Some(b"d".as_slice()));
        assert_eq!(given.info.get("passfile"), None);
    }

    /// `fe-connect.c:1309`: a host list the `hostaddr` list does not match
    /// leaves the options invalid, with the message, and nothing derived.
    #[test]
    fn a_host_list_that_does_not_match_leaves_the_options_invalid() {
        let options = Options::derive(
            info("host=a,b hostaddr=127.0.0.1"),
            &Env::empty().with("USER", "me"),
        );
        assert_eq!(options.info.get("dbname"), None);
        let conn = PGconn::new(Ok(options.info.clone()));
        assert_eq!(conn.status, ConnStatus::Bad);
        assert!(
            String::from_utf8_lossy(conn.error_message.bytes())
                .starts_with("could not match 2 host names to 1 hostaddr values"),
            "{:?}",
            conn.error_message
        );
    }

    /// `PQhost`: the host, else the `hostaddr`; `PQport`: the port, else
    /// `DEF_PGPORT_STR` (`fe-connect.c:7505`, `:7541`).
    #[test]
    fn set_host_reads_the_host_entry_as_pqhost_and_pqport_do() {
        let mut conn = PGconn::new(Err(Vec::new()));
        let entry = |host: &str, hostaddr: &str, port: &str| ConnHost {
            kind: rlibpq::HostType::HostName,
            host: Some(host.as_bytes().to_vec()),
            hostaddr: Some(hostaddr.as_bytes().to_vec()),
            port: Some(port.as_bytes().to_vec()),
        };
        conn.set_host(Some(&entry("h", "127.0.0.1", "1")));
        assert_eq!(
            (conn.host.bytes(), conn.port.bytes()),
            (&b"h"[..], &b"1"[..])
        );
        conn.set_host(Some(&entry("", "127.0.0.1", "")));
        assert_eq!(
            (conn.host.bytes(), conn.port.bytes()),
            (&b"127.0.0.1"[..], &b"5432"[..])
        );
    }

    /// `PQsetdbLogin`, `fe-connect.c:2250`-`:2316`: a `dbName` that looks
    /// like a connection string is parsed; otherwise it is the database,
    /// and each non-empty argument overrides.
    #[test]
    fn setdb_login_overrides_what_the_defaults_set() {
        let plain = SetdbLogin {
            pghost: Some(b"h"),
            pgport: Some(b""),
            db_name: Some(b"d"),
            login: Some(b"u"),
            ..SetdbLogin::default()
        };
        assert_eq!(plain.conninfo(), b"");
        let applied = plain.apply(info("port=7 user=x"));
        assert_eq!(applied.get("host"), Some(b"h".as_slice()));
        assert_eq!(applied.get("port"), Some(b"7".as_slice()));
        assert_eq!(applied.get("dbname"), Some(b"d".as_slice()));
        assert_eq!(applied.get("user"), Some(b"u".as_slice()));

        let string = SetdbLogin {
            pghost: Some(b"h"),
            db_name: Some(b"host=x dbname=y"),
            ..SetdbLogin::default()
        };
        assert_eq!(string.conninfo(), b"host=x dbname=y");
        let applied = string.apply(info("host=x dbname=y"));
        assert_eq!(applied.get("host"), Some(b"h".as_slice()));
        assert_eq!(applied.get("dbname"), Some(b"y".as_slice()));
    }
}
