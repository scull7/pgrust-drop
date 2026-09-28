//! `pgdrop start`'s actions (Linear NAT-409): everything [`crate::start`]
//! decides, done.
//!
//! 1. Create the run directory, `0700`, retrying on a name already taken.
//! 2. [`crate::start::plan`].
//! 3. Mint the data directory with rinitdb, in this process.
//! 4. Spawn `<this binary> postgres <server args>` in a process group of its
//!    own (a terminal's Ctrl-C at the shell that ran `start` does not reach
//!    it), stdin from `/dev/null`, stdout and stderr to `<run>/server.log`.
//! 5. Connect with rlibpq until a connection reaches `ReadyForQuery`, as
//!    long as the server is alive, for at most pg_ctl's 60 seconds.
//! 6. Write [`crate::start::Record`] into the data directory for `stop`.
//!    Only now: until the server holds the data directory's lock, a record
//!    written there could overwrite the one of a server already running in it.
//! 7. Print the URI, PID and data directory, or `--json`, and leave the
//!    server running.
//!
//! A failure after step 1 removes what `stop` would have removed.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rlibpq::connection::Connection;
use rlibpq::conninfo::{Env, parse_conninfo};

use crate::start::{self, RECORD_FILE, Start, StartError, StartPlan};
use crate::stop::{self, POLL, WAIT};

/// The server's log, in the run directory.
pub const SERVER_LOG: &str = "server.log";

/// Names `start` tries before it gives up on the temporary directory.
const RUN_DIR_ATTEMPTS: u64 = 16;

/// Why `start` failed.
#[derive(Debug)]
pub enum LaunchError {
    Plan(StartError),
    /// `--foreground` is the next slice of NAT-409.
    Foreground,
    Io {
        what: &'static str,
        path: PathBuf,
        error: io::Error,
    },
    /// rinitdb failed; what it printed.
    Initdb(Vec<u8>),
    /// The server exited before it accepted a connection; its log.
    ServerExited {
        status: ExitStatus,
        log: String,
    },
    /// The server did not accept a connection within [`WAIT`]; its log.
    NotReady {
        log: String,
    },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::Plan(error) => error.fmt(f),
            LaunchError::Foreground => {
                write!(f, "--foreground is not implemented yet (Linear NAT-409)")
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
                "the server exited during startup ({status}); its log:\n{}",
                log.trim_end()
            ),
            LaunchError::NotReady { log } => write!(
                f,
                "the server did not accept a connection within {} seconds; its log:\n{}",
                WAIT.as_secs(),
                log.trim_end()
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

/// Action: `pgdrop start`.
pub fn run(flags: &Start, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    match launch(flags) {
        Ok(text) => {
            let _ = stdout.write_all(text.as_bytes());
            let _ = stdout.flush();
            ExitCode::SUCCESS
        }
        Err(error) => {
            let _ = writeln!(stderr, "pgdrop: error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Action: steps 1-6 of the module header; what to print.
fn launch(flags: &Start) -> Result<String, LaunchError> {
    if flags.foreground {
        return Err(LaunchError::Foreground);
    }
    let cwd = std::env::current_dir().map_err(|error| LaunchError::Io {
        what: "read the current directory",
        path: PathBuf::from("."),
        error,
    })?;
    let run_dir = make_run_dir(&std::env::temp_dir())?;
    let datadir_holds_cluster = flags
        .datadir
        .as_ref()
        .is_some_and(|dir| cwd.join(dir).join("PG_VERSION").is_file());
    let plan = match start::plan(flags, &cwd, &run_dir, datadir_holds_cluster) {
        Ok(plan) => plan,
        Err(error) => {
            let _ = std::fs::remove_dir(&run_dir);
            return Err(LaunchError::Plan(error));
        }
    };
    bring_up(&plan).inspect_err(|_| {
        // What `stop` would remove, less the record, which is written last.
        let record = plan.record().render();
        if let Ok(mut cleanup) = stop::plan(Path::new(&plan.datadir), None, Some(&record)) {
            cleanup.remove_record = None;
            let _ = stop::remove_all(&cleanup);
        }
    })
}

/// Action: steps 3-6, for a plan whose run directory exists.
fn bring_up(plan: &StartPlan) -> Result<String, LaunchError> {
    if let Some(args) = plan.initdb_args() {
        mint(&args)?;
    }
    let log_path = Path::new(&plan.run_dir).join(SERVER_LOG);
    let mut server = spawn(plan, &log_path)?;
    let pid = server.id();
    if let Err(error) = wait_until_ready(plan, &mut server, &log_path) {
        if matches!(error, LaunchError::NotReady { .. }) {
            let _ = server.kill();
            let _ = server.wait();
        }
        return Err(error);
    }
    let record_path = Path::new(&plan.datadir).join(RECORD_FILE);
    std::fs::write(&record_path, plan.record().render()).map_err(|error| LaunchError::Io {
        what: "write",
        path: record_path,
        error,
    })?;
    Ok(if plan.json {
        plan.json(pid)
    } else {
        format!(
            "uri:     {}\npid:     {pid}\ndatadir: {}\n",
            plan.uri(),
            plan.datadir
        )
    })
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

/// Action: `<this binary> postgres <server args>`, detached; see step 4.
fn spawn(plan: &StartPlan, log_path: &Path) -> Result<Child, LaunchError> {
    let io_error = |what, path: &Path| {
        let path = path.to_path_buf();
        move |error| LaunchError::Io { what, path, error }
    };
    let exe = std::env::current_exe().map_err(io_error("find", Path::new("pgdrop")))?;
    let log = File::create(log_path).map_err(io_error("create", log_path))?;
    let log_too = log.try_clone().map_err(io_error("open", log_path))?;
    let mut command = Command::new(&exe);
    command
        .arg("postgres")
        .args(plan.server_args())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_too);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().map_err(io_error("run", &exe))
}

/// Action: step 5. A refused connection is the server still starting
/// (no socket yet, or "the database system is starting up"); any failure
/// is retried while the server lives.
fn wait_until_ready(
    plan: &StartPlan,
    server: &mut Child,
    log_path: &Path,
) -> Result<(), LaunchError> {
    let log = || std::fs::read_to_string(log_path).unwrap_or_default();
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
            path: log_path.to_path_buf(),
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
