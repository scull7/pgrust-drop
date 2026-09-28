//! `pgdrop start`'s actions (Linear NAT-409): everything [`crate::start`]
//! decides, done.
//!
//! 1. Create the run directory, `0700`, retrying on a name already taken.
//! 2. Claim an absent `--datadir` with `create_dir`, then
//!    [`crate::start::plan`].
//! 3. Mint the data directory with rinitdb, in this process.
//! 4. Spawn `<this binary> postgres <server args>` in a process group of its
//!    own (a terminal's Ctrl-C at the shell that ran `start` does not reach
//!    it), stdin from `/dev/null`, stdout and stderr to `<run>/server.log`,
//!    and no other descriptor `start` inherited (Linear NAT-607).
//! 5. Connect with rlibpq until a connection reaches `ReadyForQuery`, as
//!    long as the server is alive, for at most pg_ctl's 60 seconds.
//! 6. Write [`crate::start::Record`] into the data directory for `stop`.
//!    Only now: until the server holds the data directory's lock, a record
//!    written there could overwrite the one of a server already running in it.
//! 7. Print the URI, PID and data directory, or `--json`, and leave the
//!    server running.
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
//! remove, and exits 0 if the server did. SIGINT, SIGTERM and SIGHUP to
//! `start` are forwarded to the server as SIGINT, a fast shutdown, and
//! SIGQUIT as SIGQUIT, an immediate one ([`shutdown_signal`];
//! `postmaster.c:2052`-`:2062`). That is pg_ctl's
//! `trap_sigint_during_startup` (`pg_ctl.c:851`-`:872`), which forwards
//! SIGINT while pg_ctl waits for the server, kept for the server's whole
//! life. `pgdrop stop` works on a foreground cluster as on any other.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
use signal_hook::iterator::Signals;

use rlibpq::connection::Connection;
use rlibpq::conninfo::{Env, parse_conninfo};

use crate::start::{self, Found, RECORD_FILE, Start, StartError, StartPlan};
use crate::stop::{self, POLL, WAIT};

/// The server's log, in the run directory.
pub const SERVER_LOG: &str = "server.log";

/// Names `start` tries before it gives up on the temporary directory.
const RUN_DIR_ATTEMPTS: u64 = 16;

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
        }
    }
}

impl std::error::Error for LaunchError {}

/// Action: `pgdrop start`.
pub fn run(flags: &Start, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    let result = launch(flags).and_then(|launched| {
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
    attached: Option<Attached>,
}

/// `--foreground`'s running server and the plan that made it.
struct Attached {
    plan: StartPlan,
    server: Child,
}

impl Attached {
    /// Action: wait for the server to exit (the forwarder turns a signal
    /// into its shutdown), then remove what `stop` would. A `pgdrop stop`
    /// may have removed it first; what is already gone is fine.
    fn wait(mut self) -> Result<(), LaunchError> {
        let status = self.server.wait().map_err(|error| LaunchError::Io {
            what: "wait for",
            path: PathBuf::from(&self.plan.datadir),
            error,
        });
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
pub fn shutdown_signal(received: i32) -> i32 {
    if received == SIGQUIT { SIGQUIT } else { SIGINT }
}

/// The signals `--foreground` catches and forwards.
pub const FORWARDED: [i32; 4] = [SIGINT, SIGTERM, SIGHUP, SIGQUIT];

/// Action: steps 1-6 of the module header; what to print, and the server
/// to wait for with `--foreground`.
fn launch(flags: &Start) -> Result<Launched, LaunchError> {
    // Caught from the start: a Ctrl-C while `start` mints is held until the
    // cleanup can run, rather than ending it with the run directory left.
    let signals = if flags.foreground {
        Some(Signals::new(FORWARDED).map_err(|error| LaunchError::Io {
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
    let run_dir = make_run_dir(&std::env::temp_dir())?;
    let datadir = flags.datadir.as_ref().map(|dir| cwd.join(dir));
    let found = match datadir.as_deref().map(claim).transpose() {
        Ok(found) => found.unwrap_or(Found::Nothing),
        Err(error) => {
            let _ = std::fs::remove_dir(&run_dir);
            return Err(error);
        }
    };
    let plan = match start::plan(flags, &cwd, &run_dir, found) {
        Ok(plan) => plan,
        Err(error) => {
            if let (Some(dir), Found::Nothing) = (&datadir, found) {
                let _ = std::fs::remove_dir(dir);
            }
            let _ = std::fs::remove_dir(&run_dir);
            return Err(LaunchError::Plan(error));
        }
    };
    match bring_up(&plan, signals) {
        Ok((text, server)) => Ok(Launched {
            text,
            attached: server.map(|server| Attached { plan, server }),
        }),
        Err(error) => {
            // What `stop` would remove, less the record, which is written last.
            remove_what_stop_removes(&plan, false);
            Err(error)
        }
    }
}

/// Action: what `--datadir` names, claimed when it is absent: its parents
/// made, then the directory itself with `create_dir`, `0700` as `initdb`
/// makes it (`pg_dir_create_mode`, `src/common/file_perm.c:18`). Two
/// concurrent starts on one absent directory cannot both get
/// [`Found::Nothing`]: the loser's `create_dir` fails `AlreadyExists` and it
/// looks again, so it never takes the winner's directory for one it may
/// remove.
fn claim(dir: &Path) -> Result<Found, LaunchError> {
    let found = look_at(dir);
    if found != Found::Nothing {
        return Ok(found);
    }
    let failed = |error| LaunchError::Io {
        what: "create directory",
        path: dir.to_path_buf(),
        error,
    };
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).map_err(failed)?;
    }
    match create_private_dir(dir) {
        Ok(()) => Ok(Found::Nothing),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(look_at(dir)),
        Err(error) => Err(failed(error)),
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

/// Action: steps 3-6, for a plan whose run directory exists: what to
/// print, and with `--foreground` the server. `signals` (`--foreground`)
/// are forwarded to the server from the moment it exists.
fn bring_up(
    plan: &StartPlan,
    mut signals: Option<Signals>,
) -> Result<(String, Option<Child>), LaunchError> {
    if let Some(args) = plan.initdb_args() {
        mint(&args)?;
    }
    if let Some(signals) = &mut signals
        && signals.pending().next().is_some()
    {
        return Err(LaunchError::Interrupted);
    }
    let log_path = Path::new(&plan.run_dir).join(SERVER_LOG);
    let log = if plan.foreground {
        None
    } else {
        Some(log_path.as_path())
    };
    let mut server = spawn(plan, log)?;
    let pid = server.id();
    if let Some(signals) = signals {
        forward(pid, signals);
    }
    let record_path = Path::new(&plan.datadir).join(RECORD_FILE);
    let ready = wait_until_ready(plan, &mut server, log).and_then(|()| {
        std::fs::write(&record_path, plan.record().render()).map_err(|error| LaunchError::Io {
            what: "write",
            path: record_path,
            error,
        })
    });
    if let Err(error) = ready {
        // No server outlives a failed start: the cleanup that follows
        // removes its socket directory and perhaps its data directory.
        let _ = server.kill();
        let _ = server.wait();
        return Err(error);
    }
    let text = if plan.json {
        plan.json(pid)
    } else {
        format!(
            "uri:     {}\npid:     {pid}\ndatadir: {}\n",
            plan.uri(),
            plan.datadir
        )
    };
    Ok((text, plan.foreground.then_some(server)))
}

/// Action: a thread that sends the server [`shutdown_signal`] for every
/// signal `start` receives. A signal after the server has gone finds no
/// process, which is fine; the thread ends with `start`.
fn forward(pid: u32, mut signals: Signals) {
    let _ = std::thread::Builder::new()
        .name("pgdrop-forward".to_owned())
        .spawn(move || {
            for received in signals.forever() {
                let _ = stop::signal(pid, Some(shutdown_signal(received)));
            }
        });
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
            let log = File::create(log_path).map_err(io_error("create", log_path))?;
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
    #[cfg(unix)]
    close_inherited_fds_on_exec();
    command.spawn().map_err(io_error("run", &exe))
}

/// Action: mark every descriptor above stderr close-on-exec, so the server
/// keeps none that `start` inherited (Linear NAT-607). Whoever runs `start`
/// may have handed it more than stdin, stdout and stderr: on macOS, which
/// has no `pipe2`, std makes a child's pipes with `pipe` and then sets
/// `FD_CLOEXEC`, so a thread that spawns `start` inside another thread's
/// window gives it that thread's pipe. A server holding the write end of
/// someone's stdout pipe keeps their `Command::output` from ever seeing EOF:
/// `two_concurrent_starts_never_collide` hung the apple CI job for two
/// hours. The server is long-lived, so it keeps nothing it was not given on
/// purpose, as fd.c opens every descriptor it manages `O_CLOEXEC`
/// (`src/backend/storage/file/fd.c:1617`). `start` is single-threaded here,
/// so no other thread's descriptor is in flight. `pre_exec` could close
/// them in the child instead, but it would force std off `posix_spawn`.
#[cfg(unix)]
#[allow(unsafe_code)]
fn close_inherited_fds_on_exec() {
    for fd in open_fds() {
        // SAFETY: `fcntl` with `F_GETFD` and `F_SETFD` takes integers,
        // touches no memory of ours and changes only whether `exec` keeps
        // `fd`. A number that is not open is `EBADF`, and is skipped.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Action: the descriptors above stderr this process has open, from the
/// kernel's list of them (`/proc/self/fd` on Linux, glibc and musl alike;
/// `/dev/fd` on macOS). The list is read to the end before it is used, so
/// the directory's own descriptor is closed by then and fails `F_GETFD`.
/// Without the list, every number below the soft `RLIMIT_NOFILE`, at most
/// [`FD_SCAN_CAP`].
#[cfg(unix)]
#[allow(unsafe_code)]
fn open_fds() -> Vec<libc::c_int> {
    let dir = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    if let Ok(entries) = std::fs::read_dir(dir) {
        return entries
            .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
            .filter(|&fd| fd > 2)
            .collect();
    }
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `rlim` is a valid, writable `rlimit` for the call's duration.
    let limit = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut rlim) } == 0 {
        rlim.rlim_cur
    } else {
        FD_SCAN_CAP
    };
    let end = libc::c_int::try_from(limit.min(FD_SCAN_CAP)).unwrap_or(libc::c_int::MAX);
    (3..end).collect()
}

/// The most descriptor numbers [`open_fds`] tries without the kernel's list:
/// a soft `RLIMIT_NOFILE` may be `RLIM_INFINITY`.
#[cfg(unix)]
const FD_SCAN_CAP: libc::rlim_t = 65536;


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
/// is retried while the server lives.
fn wait_until_ready(
    plan: &StartPlan,
    server: &mut Child,
    log_path: Option<&Path>,
) -> Result<(), LaunchError> {
    let log = || log_path.map(|path| std::fs::read_to_string(path).unwrap_or_default());
    // Nothing from the environment or the service files: the URI is the
    // whole of it, so a PGSSLMODE or PGOPTIONS cannot keep `start` waiting.
    let conninfo = parse_conninfo(plan.uri().as_bytes())
        .and_then(|mut info| {
            info.add_defaults(&Env::empty(), &BTreeMap::new())?;
            Ok(info)
        })
        .map_err(|error| LaunchError::Io {
            what: "parse the connection URI",
            path: PathBuf::from(plan.uri()),
            error: io::Error::other(error.to_string()),
        })?;
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(mut connection) = Connection::connect(&conninfo) {
            let _ = connection.terminate();
            return Ok(());
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

    #[test]
    fn only_the_start_that_creates_an_absent_datadir_finds_nothing() {
        let tmp = make_run_dir(&std::env::temp_dir()).unwrap();
        let dir = tmp.join("parent").join("data");
        assert_eq!(claim(&dir).unwrap(), Found::Nothing);
        assert!(dir.is_dir());
        // The loser of a race: the directory is the winner's now.
        assert_eq!(claim(&dir).unwrap(), Found::Other);
        std::fs::write(dir.join("PG_VERSION"), "18\n").unwrap();
        assert_eq!(claim(&dir).unwrap(), Found::Cluster);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    /// `postmaster.c:2056`-`:2062`: SIGINT is a fast shutdown, SIGQUIT an
    /// immediate one; SIGTERM (smart) and SIGHUP (reload) become fast.
    #[test]
    fn every_caught_signal_becomes_a_fast_shutdown_but_sigquit() {
        assert_eq!(shutdown_signal(SIGINT), SIGINT);
        assert_eq!(shutdown_signal(SIGTERM), SIGINT);
        assert_eq!(shutdown_signal(SIGHUP), SIGINT);
        assert_eq!(shutdown_signal(SIGQUIT), SIGQUIT);
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
