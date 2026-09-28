//! `pgdrop start`'s actions (Linear NAT-409): everything [`crate::start`]
//! decides, done.
//!
//! 1. Create the run directory, `0700`, retrying on a name already taken.
//!    With `--port auto`, pick a port: bind `127.0.0.1:0` and take the
//!    port the kernel chose ([`PortPicker`]).
//! 2. [`crate::start::plan`].
//! 3. Mint the data directory with rinitdb, in this process.
//! 4. Spawn `<this binary> postgres <server args>` in a process group of its
//!    own (a terminal's Ctrl-C at the shell that ran `start` does not reach
//!    it), stdin from `/dev/null`, stdout and stderr appended to
//!    `<run>/server.log`.
//! 5. Connect with rlibpq, on the server's own socket, until a connection
//!    reaches `ReadyForQuery`, as long as the server is alive, for at most
//!    pg_ctl's 60 seconds. With `--port auto`, a server that exits while its
//!    port is in use lost it to someone else between step 1 and its bind
//!    (`EADDRINUSE`): pick another and go back to step 4, at most
//!    [`AUTO_PORT_ATTEMPTS`] times in all.
//! 6. Write [`crate::start::Record`] into the data directory for `stop`.
//!    Only now: until the server holds the data directory's lock, a record
//!    written there could overwrite the one of a server already running in it.
//!    Then make it the current cluster ([`crate::current`]); a pointer that
//!    cannot be written is a warning on stderr, and `start` goes on.
//! 7. Print the URI, PID and data directory, or `--json`, or `--env`, and
//!    leave the server running.
//!
//! A failure after step 1 stops the server if it was spawned, then removes
//! what `stop` would have removed: never a data directory that was there
//! before `start` ran ([`crate::start::Origin`]).
//!
//! ## `--foreground`
//!
//! The same steps, attached: the server stays in `start`'s process group,
//! its output goes to `start`'s stderr (stdout carries only step 7), and
//! after step 7 `start` waits for it to exit, removes what `stop` would
//! remove (and the pointer, if it still names this cluster), and exits 0 if
//! the server did. SIGINT, SIGTERM and SIGHUP to
//! `start` are forwarded to the server as SIGINT, a fast shutdown, and
//! SIGQUIT as SIGQUIT, an immediate one ([`shutdown_signal`];
//! `postmaster.c:2052`-`:2062`). That is pg_ctl's
//! `trap_sigint_during_startup` (`pg_ctl.c:851`-`:872`), which forwards
//! SIGINT while pg_ctl waits for the server, kept for the server's whole
//! life. `pgdrop stop` works on a foreground cluster as on any other.
//!
//! No thread forwards: the one that reaps the server is the one that
//! signals it, so a signal never goes to a PID the server no longer holds.
//! While `start` waits for the server to be ready it forwards between
//! connection attempts; once attached it sleeps on its signals, SIGCHLD
//! among them, and on each wake forwards what came, then asks whether the
//! server has exited.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::net::TcpListener;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use signal_hook::consts::{SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

use rlibpq::connection::Connection;
use rlibpq::conninfo::{Env, parse_conninfo};

use crate::current;
use crate::start::{self, Found, Listen, Output, RECORD_FILE, Start, StartError, StartPlan};
use crate::stop::{self, POLL, WAIT};

/// The server's log, in the run directory.
pub const SERVER_LOG: &str = "server.log";

/// Names `start` tries before it gives up on the temporary directory.
const RUN_DIR_ATTEMPTS: u64 = 16;

/// Ports `--port auto` tries, in all, before it gives up.
pub const AUTO_PORT_ATTEMPTS: u32 = 8;

/// Debug builds only, for the tests: a comma-separated list of ports
/// `--port auto` tries before it asks the kernel, so a test can hand it one
/// it holds and see the retry.
#[cfg(debug_assertions)]
pub const TEST_AUTO_PORTS: &str = "PGDROP_TEST_AUTO_PORTS";

/// Why `start` failed.
#[derive(Debug)]
pub enum LaunchError {
    Plan(StartError),
    /// `--foreground` was sent a signal before the server was spawned.
    Interrupted,
    Io {
        what: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// rinitdb failed; what it printed.
    Initdb(Vec<u8>),
    /// The server exited before it accepted a connection; its log, `None`
    /// when it went to stderr (`--foreground`).
    ServerExited {
        status: ExitStatus,
        log: Option<String>,
    },
    /// The server did not accept a connection within [`WAIT`]; its log.
    NotReady {
        log: Option<String>,
    },
    /// `--foreground`: the server exited unsuccessfully after it was ready.
    ServerFailed(ExitStatus),
    /// `--port auto`: every port picked was taken before the server bound it.
    NoFreePort {
        attempts: u32,
        last: u16,
    },
}

/// `"; its log:\n…"`, or where it went instead.
fn log_tail(log: Option<&String>) -> String {
    match log {
        Some(log) => format!("; its log:\n{}", log.trim_end()),
        None => "; its log is above".to_owned(),
    }
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::Plan(error) => error.fmt(f),
            LaunchError::Interrupted => {
                write!(f, "interrupted before the server was started")
            }
            LaunchError::Io { what, path, error } => {
                write!(f, "could not {what} \"{}\": {error}", path.display())
            }
            LaunchError::Initdb(output) => write!(
                f,
                "initdb failed:\n{}",
                String::from_utf8_lossy(output).trim_end()
            ),
            LaunchError::ServerExited { status, log } => write!(
                f,
                "the server exited during startup ({status}){}",
                log_tail(log.as_ref())
            ),
            LaunchError::NotReady { log } => write!(
                f,
                "the server did not accept a connection within {} seconds{}",
                WAIT.as_secs(),
                log_tail(log.as_ref())
            ),
            LaunchError::ServerFailed(status) => write!(f, "the server exited ({status})"),
            LaunchError::NoFreePort { attempts, last } => write!(
                f,
                "--port auto found no free port: each of the {attempts} it picked was taken \
                 before the server could listen on it (the last, {last})"
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Action: `pgdrop start`.
pub fn run(flags: &Start, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    let result = launch(flags).and_then(|launched| {
        if let Some(error) = &launched.pointer {
            let _ = writeln!(
                stderr,
                "pgdrop: warning: {error}; a bare pgdrop psql or pgdrop stop will not find this cluster"
            );
        }
        let _ = stdout.write_all(launched.text.as_bytes());
        let _ = stdout.flush();
        launched.attached.map_or(Ok(()), Attached::wait)
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(stderr, "pgdrop: error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A started cluster: what to print, and with `--foreground` the server
/// to wait for.
struct Launched {
    text: String,
    /// Why the pointer could not be written, if it could not.
    pointer: Option<current::PointerError>,
    attached: Option<Attached>,
}

/// `--foreground`'s running server, the signals it forwards, and the plan
/// that made it.
struct Attached {
    plan: StartPlan,
    server: Child,
    signals: Forwarder,
}

impl Attached {
    /// Action: wait for the server to exit, forwarding every signal as its
    /// shutdown, then remove what `stop` would. A `pgdrop stop` may have
    /// removed it first; what is already gone is fine.
    fn wait(mut self) -> Result<(), LaunchError> {
        let status = self
            .signals
            .wait(&mut self.server)
            .map_err(|error| LaunchError::Io {
                what: "wait for",
                path: PathBuf::from(&self.plan.datadir),
                error,
            });
        if let Some(path) = current::location() {
            let _ = current::clear_if_names(&path, Path::new(&self.plan.datadir));
        }
        remove_what_stop_removes(&self.plan, true);
        match status? {
            status if status.success() => Ok(()),
            status => Err(LaunchError::ServerFailed(status)),
        }
    }
}

/// Action: what `stop` would remove for `plan`, from the plan rather than
/// the record on disk (which `stop` may have taken already, or `start` not
/// yet written).
fn remove_what_stop_removes(plan: &StartPlan, with_record: bool) {
    let record = plan.record().render();
    if let Ok(mut cleanup) = stop::plan(Path::new(&plan.datadir), None, Some(&record)) {
        if !with_record {
            cleanup.remove_record = None;
        }
        let _ = stop::remove_all(&cleanup);
    }
}

/// Pure: the signal `--foreground` sends the server for one it received:
/// SIGQUIT is an immediate shutdown, and every other it catches a fast one,
/// what `pgdrop stop` asks for (`postmaster.c:2056`-`:2062`). Not SIGTERM
/// as such, a smart shutdown that waits out every client, nor SIGHUP, a
/// reload: a hung-up terminal or a supervisor's SIGTERM means stop now.
#[must_use]
pub fn shutdown_signal(received: i32) -> &'static str {
    if received == SIGQUIT { "-QUIT" } else { "-INT" }
}

/// The signals `--foreground` catches and forwards.
pub const FORWARDED: [i32; 4] = [SIGINT, SIGTERM, SIGHUP, SIGQUIT];

/// `--foreground`'s signals, caught from before the server exists: the
/// [`FORWARDED`] ones, and SIGCHLD, which wakes [`Forwarder::wait`] when
/// the server exits.
struct Forwarder(Signals);

impl Forwarder {
    /// Action: catch them.
    fn new() -> io::Result<Self> {
        Signals::new(FORWARDED.iter().chain(&[SIGCHLD])).map(Self)
    }

    /// Action: whether a signal to forward came, taking what came; for
    /// before the server exists, when there is no one to forward it to.
    fn interrupted(&mut self) -> bool {
        self.0.pending().any(|received| received != SIGCHLD)
    }

    /// Action: send `server` [`shutdown_signal`] for every signal that came
    /// since the last look, without waiting. `server` is not reaped yet: the
    /// caller reaps it, after.
    fn forward_pending(&mut self, server: &Child) {
        for received in self.0.pending() {
            forward(server, received);
        }
    }

    /// Action: reap `server`, forwarding every signal until it has gone.
    /// A SIGCHLD that comes between `try_wait` and `wait` is pending, so
    /// `wait` returns at once.
    fn wait(&mut self, server: &mut Child) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = server.try_wait()? {
                return Ok(status);
            }
            for received in self.0.wait() {
                forward(server, received);
            }
        }
    }
}

/// Action: the server's shutdown for one signal `start` received; nothing
/// for SIGCHLD. A server that has exited but is not yet reaped still holds
/// its PID, and a signal to it is lost, which is fine.
fn forward(server: &Child, received: i32) {
    if received != SIGCHLD {
        let _ = stop::signal(server.id(), shutdown_signal(received));
    }
}

/// Action: steps 1-6 of the module header; what to print, and the server
/// to wait for with `--foreground`.
fn launch(flags: &Start) -> Result<Launched, LaunchError> {
    // Caught from the start: a Ctrl-C while `start` mints is held until the
    // cleanup can run, rather than ending it with the run directory left.
    let signals = if flags.foreground {
        Some(Forwarder::new().map_err(|error| LaunchError::Io {
            what: "catch signals for",
            path: PathBuf::from("pgdrop start --foreground"),
            error,
        })?)
    } else {
        None
    };
    let cwd = std::env::current_dir().map_err(|error| LaunchError::Io {
        what: "read the current directory",
        path: PathBuf::from("."),
        error,
    })?;
    let mut ports = PortPicker::new();
    let listen = match flags.port.fixed() {
        Some(listen) => listen,
        None => Listen::Tcp(ports.next()?),
    };
    let run_dir = make_run_dir(&std::env::temp_dir())?;
    let found = flags
        .datadir
        .as_ref()
        .map_or(Found::Nothing, |dir| look_at(&cwd.join(dir)));
    let plan = match start::plan(flags, &cwd, &run_dir, found, listen) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = std::fs::remove_dir(&run_dir);
            return Err(LaunchError::Plan(error));
        }
    };
    match bring_up(&plan, signals, &mut ports) {
        // The plan the server runs on: `--port auto` may have moved it.
        Ok((plan, text, server)) => Ok(Launched {
            text,
            pointer: current::location()
                .and_then(|path| current::write(&path, &plan.pointer()).err()),
            attached: server.map(|(server, signals)| Attached {
                plan,
                server,
                signals,
            }),
        }),
        Err(error) => {
            // What `stop` would remove, less the record, which is written last.
            remove_what_stop_removes(&plan, false);
            Err(error)
        }
    }
}

/// Action: what `--datadir` names, before `start` touches it. A symbolic
/// link, even a dangling one, is something that was there.
fn look_at(dir: &Path) -> Found {
    if dir.join("PG_VERSION").is_file() {
        Found::Cluster
    } else if std::fs::symlink_metadata(dir).is_ok() {
        Found::Other
    } else {
        Found::Nothing
    }
}

/// The server `start` brought up: the plan it runs on, what to print, and
/// with `--foreground` the server and its signals.
type BroughtUp = (StartPlan, String, Option<(Child, Forwarder)>);

/// Action: steps 3-6, for a plan whose run directory exists. `signals`
/// (`--foreground`) are forwarded to the server from the moment it exists.
fn bring_up(
    plan: &StartPlan,
    mut signals: Option<Forwarder>,
    ports: &mut PortPicker,
) -> Result<BroughtUp, LaunchError> {
    if let Some(args) = plan.initdb_args() {
        mint(&args)?;
    }
    if let Some(signals) = &mut signals
        && signals.interrupted()
    {
        return Err(LaunchError::Interrupted);
    }
    let log_path = Path::new(&plan.run_dir).join(SERVER_LOG);
    let log = if plan.foreground {
        None
    } else {
        Some(log_path.as_path())
    };
    let mut plan = plan.clone();
    let mut attempts = 1;
    let mut server = loop {
        let mut server = spawn(&plan, log)?;
        match wait_until_ready(&plan, &mut server, signals.as_mut(), log) {
            Ok(()) => break server,
            // Reaped already: it exited.
            Err(LaunchError::ServerExited { .. })
                if plan.auto_port && port_in_use(plan.listen.port()) =>
            {
                if attempts == AUTO_PORT_ATTEMPTS {
                    return Err(LaunchError::NoFreePort {
                        attempts,
                        last: plan.listen.port(),
                    });
                }
                attempts += 1;
                plan = plan.on_port(ports.next()?).map_err(LaunchError::Plan)?;
            }
            Err(error) => {
                let _ = server.kill();
                let _ = server.wait();
                return Err(error);
            }
        }
    };
    let pid = server.id();
    if let Err(error) = leave_behind(&plan) {
        // No server outlives a failed start: the cleanup that follows
        // removes its socket directory and perhaps its data directory.
        let _ = server.kill();
        let _ = server.wait();
        return Err(error);
    }
    let text = match plan.output {
        Output::Json => plan.json(pid),
        Output::Env => plan.env(),
        Output::Text => format!(
            "uri:     {}\npid:     {pid}\ndatadir: {}\n",
            plan.uri(),
            plan.datadir
        ),
    };
    Ok((plan, text, signals.map(|signals| (server, signals))))
}

/// Action: step 6, for a server that is ready: the record for `stop`. The
/// pointer, which may fail without failing `start`, is [`launch`]'s.
fn leave_behind(plan: &StartPlan) -> Result<(), LaunchError> {
    let record_path = Path::new(&plan.datadir).join(RECORD_FILE);
    std::fs::write(&record_path, plan.record().render()).map_err(|error| LaunchError::Io {
        what: "write",
        path: record_path,
        error,
    })
}

/// `--port auto`'s source of ports: the kernel's choice for a bind to
/// `127.0.0.1:0`, after (debug builds only) any [`TEST_AUTO_PORTS`] lists.
struct PortPicker {
    /// Ports to try first, the next last.
    queued: Vec<NonZeroU16>,
}

impl PortPicker {
    fn new() -> Self {
        #[cfg(debug_assertions)]
        let mut queued = std::env::var(TEST_AUTO_PORTS)
            .map(|list| port_list(&list))
            .unwrap_or_default();
        #[cfg(not(debug_assertions))]
        let mut queued = Vec::new();
        queued.reverse();
        Self { queued }
    }

    /// Action: the next port to try. The listener that found it is closed
    /// on return, so the server can bind it; until it does, anyone can.
    fn next(&mut self) -> Result<NonZeroU16, LaunchError> {
        if let Some(port) = self.queued.pop() {
            return Ok(port);
        }
        let io_error = |error| LaunchError::Io {
            what: "find a free port on",
            path: PathBuf::from(start::TCP_HOST),
            error,
        };
        let listener = TcpListener::bind((start::TCP_HOST, 0)).map_err(io_error)?;
        let port = listener.local_addr().map_err(io_error)?.port();
        NonZeroU16::new(port).ok_or_else(|| io_error(io::ErrorKind::AddrNotAvailable.into()))
    }
}

/// Pure: a comma-separated list of ports; what is not one is skipped.
#[must_use]
pub fn port_list(list: &str) -> Vec<NonZeroU16> {
    list.split(',')
        .filter_map(|port| port.trim().parse().ok())
        .collect()
}

/// Action: whether something listens on `127.0.0.1:port`: a bind to it
/// fails `EADDRINUSE`.
fn port_in_use(port: u16) -> bool {
    matches!(
        TcpListener::bind((start::TCP_HOST, port)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse
    )
}

/// Action: a fresh `<tmp>/pgdrop-<pid>-<nonce>`, `0700` (the server's
/// socket is in it). `create_dir`, never `create_dir_all`: a name another
/// `start` got first is `AlreadyExists`, and the next nonce is tried.
fn make_run_dir(tmp: &Path) -> Result<PathBuf, LaunchError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seed = u64::from(now.subsec_nanos()) ^ now.as_secs().rotate_left(32);
    let mut last = None;
    for attempt in 0..RUN_DIR_ATTEMPTS {
        let path = tmp.join(start::run_dir_name(
            std::process::id(),
            seed.wrapping_add(attempt),
        ));
        match create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = Some(path),
            Err(error) => {
                return Err(LaunchError::Io {
                    what: "create directory",
                    path,
                    error,
                });
            }
        }
    }
    Err(LaunchError::Io {
        what: "create directory",
        path: last.unwrap_or_else(|| tmp.to_path_buf()),
        error: io::ErrorKind::AlreadyExists.into(),
    })
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

/// Action: rinitdb, in this process, its output kept for an error.
fn mint(args: &[String]) -> Result<(), LaunchError> {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let status = rinitdb::run(OsStr::new("initdb"), &args, &mut output, &mut errors);
    if status == ExitCode::SUCCESS {
        Ok(())
    } else {
        output.extend(errors);
        Err(LaunchError::Initdb(output))
    }
}

/// Action: `<this binary> postgres <server args>`. With a log file,
/// detached (step 4); without one (`--foreground`), in this process group,
/// its stdout and stderr both this process's stderr.
fn spawn(plan: &StartPlan, log_path: Option<&Path>) -> Result<Child, LaunchError> {
    let io_error = |what, path: &Path| {
        let path = path.to_path_buf();
        move |error| LaunchError::Io { what, path, error }
    };
    let exe = std::env::current_exe().map_err(io_error("find", Path::new("pgdrop")))?;
    let (out, err) = match log_path {
        Some(log_path) => {
            // Appended: a `--port auto` retry keeps the log of the attempt before.
            let log = File::options()
                .create(true)
                .append(true)
                .open(log_path)
                .map_err(io_error("create", log_path))?;
            let log_too = log.try_clone().map_err(io_error("open", log_path))?;
            (Stdio::from(log), Stdio::from(log_too))
        }
        None => (
            stderr_copy().map_err(io_error("duplicate", Path::new("stderr")))?,
            Stdio::inherit(),
        ),
    };
    let mut command = Command::new(&exe);
    command
        .arg("postgres")
        .args(plan.server_args())
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    #[cfg(unix)]
    if log_path.is_some() {
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
    }
    command.spawn().map_err(io_error("run", &exe))
}

/// Action: a copy of this process's stderr, for the server's stdout.
#[cfg(unix)]
fn stderr_copy() -> io::Result<Stdio> {
    use std::os::fd::AsFd;
    Ok(Stdio::from(io::stderr().as_fd().try_clone_to_owned()?))
}

#[cfg(not(unix))]
fn stderr_copy() -> io::Result<Stdio> {
    Ok(Stdio::inherit())
}

/// Action: step 5. A refused connection is the server still starting
/// (no socket yet, or "the database system is starting up"); any failure
/// is retried while the server lives. `signals` (`--foreground`) are
/// forwarded between attempts.
fn wait_until_ready(
    plan: &StartPlan,
    server: &mut Child,
    mut signals: Option<&mut Forwarder>,
    log_path: Option<&Path>,
) -> Result<(), LaunchError> {
    let log = || log_path.map(|path| std::fs::read_to_string(path).unwrap_or_default());
    // Nothing from the environment or the service files: the URI is the
    // whole of it, so a PGSSLMODE or PGOPTIONS cannot keep `start` waiting.
    // The socket's, not the TCP port's: whoever took a `--port auto` port
    // first must not be mistaken for the server.
    let conninfo = parse_conninfo(plan.socket_uri().as_bytes())
        .and_then(|mut info| {
            info.add_defaults(&Env::empty(), &BTreeMap::new())?;
            Ok(info)
        })
        .map_err(|error| LaunchError::Io {
            what: "parse the connection URI",
            path: PathBuf::from(plan.socket_uri()),
            error: io::Error::other(error.to_string()),
        })?;
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(mut connection) = Connection::connect(&conninfo) {
            let _ = connection.terminate();
            return Ok(());
        }
        if let Some(signals) = signals.as_deref_mut() {
            signals.forward_pending(server);
        }
        let status = server.try_wait().map_err(|error| LaunchError::Io {
            what: "wait for",
            path: PathBuf::from(&plan.datadir),
            error,
        })?;
        if let Some(status) = status {
            return Err(LaunchError::ServerExited { status, log: log() });
        }
        if Instant::now() > deadline {
            return Err(LaunchError::NotReady { log: log() });
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `postmaster.c:2056`-`:2062`: SIGINT is a fast shutdown, SIGQUIT an
    /// immediate one; SIGTERM (smart) and SIGHUP (reload) become fast.
    #[test]
    fn every_caught_signal_becomes_a_fast_shutdown_but_sigquit() {
        assert_eq!(shutdown_signal(SIGINT), "-INT");
        assert_eq!(shutdown_signal(SIGTERM), "-INT");
        assert_eq!(shutdown_signal(SIGHUP), "-INT");
        assert_eq!(shutdown_signal(SIGQUIT), "-QUIT");
    }

    #[test]
    fn a_port_list_keeps_the_ports_and_skips_the_rest() {
        let ports: Vec<u16> = port_list("5433, 0,x,65535,65536,")
            .into_iter()
            .map(NonZeroU16::get)
            .collect();
        assert_eq!(ports, [5433, 65535]);
    }

    #[test]
    fn port_auto_gives_up_saying_how_often_it_tried() {
        assert_eq!(
            LaunchError::NoFreePort {
                attempts: 8,
                last: 40000
            }
            .to_string(),
            "--port auto found no free port: each of the 8 it picked was taken \
             before the server could listen on it (the last, 40000)"
        );
    }

    #[test]
    fn a_log_that_went_to_stderr_is_said_to_be_above() {
        assert_eq!(log_tail(None), "; its log is above");
        assert_eq!(
            log_tail(Some(&"FATAL:  no\n\n".to_owned())),
            "; its log:\nFATAL:  no"
        );
    }
}
