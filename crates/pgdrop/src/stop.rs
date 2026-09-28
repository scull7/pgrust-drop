//! `pgdrop stop`: tear down a cluster `pgdrop start` made (Linear NAT-409).
//!
//! It is `pg_ctl stop -D` (`src/bin/pg_ctl/pg_ctl.c:1027`, `do_stop`) with
//! the shutdown mode fixed at pg_ctl's default, fast (`SIGINT`, `:79`-`:80`),
//! followed by the cleanup `start` recorded in the data directory
//! ([`crate::start::Record`]). Two differences, both because a test suite's
//! teardown must be safe to run twice (the issue's "`stop` is idempotent"):
//! a data directory with no `postmaster.pid`, or no data directory at all,
//! is a cluster that is already stopped and exits 0, where `do_stop` says
//! "PID file … does not exist" (`:1035`) and `get_pgpid` "directory … does
//! not exist" (`:255`), both exit 1. See `docs/divergences.md`.
//!
//! Data / Calculations / Actions: [`Stop`] is the command line, [`plan`]
//! decides from what is on disk, and [`run`] reads the disk, signals, waits
//! and removes.

use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use usage::Args;

use crate::start::{RUN_DIR_PREFIX, Record};

/// Options for `pgdrop stop`.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Stop {
    /// Data directory of the cluster to stop (default: $PGDATA)
    #[usage(long, value_name = "DIR")]
    pub datadir: Option<PathBuf>,
}

/// `postmaster.pid`, in the data directory.
pub const PIDFILE: &str = "postmaster.pid";

/// How long `start` waits for the server to come up and `stop` for it to
/// go: pg_ctl's `DEFAULT_WAIT` (`pg_ctl.c:69`), which governs both.
pub const WAIT: Duration = Duration::from_mins(1);

/// How often `start` and `stop` look. pg_ctl looks ten times a second
/// (`WAITS_PER_SEC`, `pg_ctl.c:73`); a test suite's setup and teardown are
/// measured in milliseconds, so these look every one.
pub const POLL: Duration = Duration::from_millis(1);

/// How often, in polls, `stop` asks whether the server is still alive
/// (`kill -0`), as `wait_for_postmaster_stop` does every time it looks
/// (`pg_ctl.c:727`). Once per pg_ctl interval: each ask spawns a process.
pub const LIVENESS_EVERY: u32 = 100;

/// What `postmaster.pid`'s first line names (`pidfile.h:37`). A standalone
/// backend writes its PID negated (`miscinit.c:1436`, `CreateLockFile`), which
/// `do_stop` refuses to signal (`pg_ctl.c:1039`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Postmaster(u32),
    SingleUser(u32),
}

/// Pure: the PID `postmaster.pid` names, read as `get_pgpid` reads it
/// (`pg_ctl.c:289`: a leading `%d`).
///
/// # Errors
///
/// An empty file, or one that does not start with a number.
pub fn pidfile_owner(text: &str) -> Result<Owner, StopError> {
    if text.is_empty() {
        return Err(StopError::EmptyPidfile);
    }
    let first = text.lines().next().unwrap_or_default().trim();
    match first.parse::<i64>() {
        Ok(pid) if pid > 0 => u32::try_from(pid)
            .map(Owner::Postmaster)
            .map_err(|_| StopError::BadPidfile),
        Ok(pid) if pid < 0 => u32::try_from(-pid)
            .map(Owner::SingleUser)
            .map_err(|_| StopError::BadPidfile),
        _ => Err(StopError::BadPidfile),
    }
}

/// Everything `stop` will do, decided before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopPlan {
    /// The postmaster to send `SIGINT` to and wait for, if one is running.
    pub signal: Option<u32>,
    /// Directories to remove, in order, once the server is gone.
    pub remove_dirs: Vec<PathBuf>,
    /// The record to remove when the data directory itself stays.
    pub remove_record: Option<PathBuf>,
}

/// Pure: the plan for stopping the cluster in `datadir`, given the contents
/// of its `postmaster.pid` and of `start`'s record, `None` for either that
/// does not exist.
///
/// A run directory is removed only when its name is one `start` gives
/// ([`RUN_DIR_PREFIX`]): a hand-edited record cannot aim `stop` at `/`.
///
/// # Errors
///
/// A `postmaster.pid` that does not name a PID, a single-user server, or a
/// record `start` did not write.
pub fn plan(
    datadir: &Path,
    pidfile: Option<&str>,
    record: Option<&str>,
) -> Result<StopPlan, StopError> {
    let signal = match pidfile.map(pidfile_owner).transpose()? {
        None => None,
        Some(Owner::Postmaster(pid)) => Some(pid),
        Some(Owner::SingleUser(pid)) => return Err(StopError::SingleUser(pid)),
    };
    let record = record
        .map(|text| Record::parse(text).ok_or(StopError::BadRecord))
        .transpose()?;
    let mut remove_dirs = Vec::new();
    let mut remove_record = None;
    if let Some(record) = record {
        if record.removes_datadir {
            remove_dirs.push(datadir.to_path_buf());
        } else {
            remove_record = Some(datadir.join(crate::start::RECORD_FILE));
        }
        let run_dir = PathBuf::from(&record.run_dir);
        let named_by_start = run_dir
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(RUN_DIR_PREFIX));
        if record.removes_run_dir && named_by_start {
            remove_dirs.push(run_dir);
        }
    }
    Ok(StopPlan {
        signal,
        remove_dirs,
        remove_record,
    })
}

/// Pure: the data directory to stop: `--datadir`, else `PGDATA`, as
/// pg_ctl falls back (`pg_ctl.c:2005`), made absolute against `cwd`.
///
/// # Errors
///
/// Neither is set (`pg_ctl.c:2442`).
pub fn datadir(flags: &Stop, pgdata: Option<&OsStr>, cwd: &Path) -> Result<PathBuf, StopError> {
    flags
        .datadir
        .as_deref()
        .or(pgdata.filter(|value| !value.is_empty()).map(Path::new))
        .map(|dir| cwd.join(dir))
        .ok_or(StopError::NoDatadir)
}

/// Why `stop` failed.
#[derive(Debug)]
pub enum StopError {
    /// Neither `--datadir` nor `PGDATA`.
    NoDatadir,
    /// `postmaster.pid` is empty (`pg_ctl.c:293`).
    EmptyPidfile,
    /// `postmaster.pid` does not start with a PID (`pg_ctl.c:296`).
    BadPidfile,
    /// A standalone backend holds the data directory (`pg_ctl.c:1042`).
    SingleUser(u32),
    /// `pgdrop.start` is not what `start` writes.
    BadRecord,
    /// `kill -INT` failed (`pg_ctl.c:1050`).
    Signal { pid: u32, detail: String },
    /// The server did not go within [`WAIT`], or died leaving its
    /// `postmaster.pid` behind (`pg_ctl.c:1067`).
    DoesNotShutDown,
    /// Reading or removing something failed.
    Io {
        what: &'static str,
        path: PathBuf,
        error: io::Error,
    },
}

impl fmt::Display for StopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopError::NoDatadir => write!(
                f,
                "no data directory specified and environment variable PGDATA unset"
            ),
            StopError::EmptyPidfile => write!(f, "the PID file \"{PIDFILE}\" is empty"),
            StopError::BadPidfile => write!(f, "invalid data in PID file \"{PIDFILE}\""),
            StopError::SingleUser(pid) => write!(
                f,
                "cannot stop server; single-user server is running (PID: {pid})"
            ),
            StopError::BadRecord => write!(
                f,
                "\"{}\" was not written by pgdrop start",
                crate::start::RECORD_FILE
            ),
            StopError::Signal { pid, detail } => {
                write!(f, "could not send stop signal (PID: {pid}): {detail}")
            }
            StopError::DoesNotShutDown => write!(f, "server does not shut down"),
            StopError::Io { what, path, error } => {
                write!(f, "could not {what} \"{}\": {error}", path.display())
            }
        }
    }
}

impl std::error::Error for StopError {}

/// Action: `pgdrop stop`. Prints nothing on success.
pub fn run(flags: &Stop, stderr: &mut impl Write) -> ExitCode {
    match stop(flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(stderr, "pgdrop: error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn stop(flags: &Stop) -> Result<(), StopError> {
    let cwd = std::env::current_dir().map_err(|error| StopError::Io {
        what: "read the current directory",
        path: PathBuf::from("."),
        error,
    })?;
    let datadir = datadir(flags, std::env::var_os("PGDATA").as_deref(), &cwd)?;
    let pidfile_path = datadir.join(PIDFILE);
    let pidfile = read_if_exists(&pidfile_path)?;
    let record = read_if_exists(&datadir.join(crate::start::RECORD_FILE))?;
    let plan = plan(&datadir, pidfile.as_deref(), record.as_deref())?;
    if let Some(pid) = plan.signal {
        signal(pid, "-INT").map_err(|detail| StopError::Signal { pid, detail })?;
        wait_for_postmaster_stop(&pidfile_path, pid)?;
    }
    remove_all(&plan)
}

/// Action: the plan's removals, in order.
pub(crate) fn remove_all(plan: &StopPlan) -> Result<(), StopError> {
    for dir in &plan.remove_dirs {
        remove(std::fs::remove_dir_all(dir), dir)?;
    }
    if let Some(record) = &plan.remove_record {
        remove(std::fs::remove_file(record), record)?;
    }
    Ok(())
}

/// Action: a file's contents, `None` when it (or its directory) is not there.
pub(crate) fn read_if_exists(path: &Path) -> Result<Option<String>, StopError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StopError::Io {
            what: "read",
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// Something already gone was removed by someone else, which is fine.
fn remove(result: io::Result<()>, path: &Path) -> Result<(), StopError> {
    match result {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(StopError::Io {
            what: "remove",
            path: path.to_path_buf(),
            error,
        }),
        _ => Ok(()),
    }
}

/// Action: `kill(pid, sig)` through the POSIX `kill` utility. pgdrop binds
/// no libc and forbids `unsafe`, and std can send nothing but `SIGKILL`.
/// `Err` carries the utility's complaint.
pub(crate) fn signal(pid: u32, sig: &str) -> Result<(), String> {
    let output = Command::new("kill")
        .arg(sig)
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run kill: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Action: `wait_for_postmaster_stop` (`pg_ctl.c:717`): done when
/// `postmaster.pid` is gone; given up on when the server has died with it
/// still there, or after [`WAIT`].
fn wait_for_postmaster_stop(pidfile: &Path, pid: u32) -> Result<(), StopError> {
    let deadline = Instant::now() + WAIT;
    let mut polls = 0u32;
    loop {
        if !pidfile.exists() {
            return Ok(());
        }
        polls = polls.wrapping_add(1);
        if polls.is_multiple_of(LIVENESS_EVERY) && signal(pid, "-0").is_err() {
            // `:730`-`:735`: look once more, to avoid a race with the exit.
            return if pidfile.exists() {
                Err(StopError::DoesNotShutDown)
            } else {
                Ok(())
            };
        }
        if Instant::now() > deadline {
            return Err(StopError::DoesNotShutDown);
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::start::Record;

    const PIDFILE_TEXT: &str =
        "4242\n/tmp/pgdrop-1-2/data\n1790561889\n5432\n/tmp/pgdrop-1-2\n\n  1234\nready   \n";

    fn record(removes_datadir: bool, removes_run_dir: bool, run_dir: &str) -> String {
        Record {
            removes_datadir,
            removes_run_dir,
            run_dir: run_dir.to_owned(),
        }
        .render()
    }

    #[test]
    fn the_pid_is_the_first_line_and_a_negative_one_is_a_single_user_server() {
        assert_eq!(
            pidfile_owner(PIDFILE_TEXT).unwrap(),
            Owner::Postmaster(4242)
        );
        assert_eq!(pidfile_owner("-77\n").unwrap(), Owner::SingleUser(77));
        assert!(matches!(pidfile_owner(""), Err(StopError::EmptyPidfile)));
        for bad in ["x\n", "0\n", "\n4242\n", "99999999999\n"] {
            assert!(
                matches!(pidfile_owner(bad), Err(StopError::BadPidfile)),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_running_minted_cluster_is_signalled_then_removed_with_its_run_directory() {
        let datadir = Path::new("/tmp/pgdrop-1-2/data");
        let plan = plan(
            datadir,
            Some(PIDFILE_TEXT),
            Some(&record(true, true, "/tmp/pgdrop-1-2")),
        )
        .unwrap();
        assert_eq!(
            plan,
            StopPlan {
                signal: Some(4242),
                remove_dirs: vec![datadir.into(), "/tmp/pgdrop-1-2".into()],
                remove_record: None,
            }
        );
    }

    #[test]
    fn a_kept_or_existing_datadir_stays_and_only_loses_the_record() {
        let datadir = Path::new("/srv/data");
        let existing = plan(datadir, None, Some(&record(false, true, "/tmp/pgdrop-1-2"))).unwrap();
        assert_eq!(existing.signal, None);
        assert_eq!(existing.remove_dirs, [PathBuf::from("/tmp/pgdrop-1-2")]);
        assert_eq!(
            existing.remove_record,
            Some(PathBuf::from("/srv/data/pgdrop.start"))
        );
        let kept = plan(
            datadir,
            None,
            Some(&record(false, false, "/tmp/pgdrop-1-2")),
        )
        .unwrap();
        assert!(kept.remove_dirs.is_empty());
    }

    /// Idempotence: what the first `stop` leaves, the second finds nothing
    /// to do in.
    #[test]
    fn a_stopped_or_vanished_cluster_is_nothing_to_do() {
        assert_eq!(
            plan(Path::new("/gone"), None, None).unwrap(),
            StopPlan {
                signal: None,
                remove_dirs: Vec::new(),
                remove_record: None,
            }
        );
    }

    #[test]
    fn a_server_start_did_not_make_is_still_stopped_but_nothing_is_removed() {
        let plan = plan(Path::new("/srv/data"), Some(PIDFILE_TEXT), None).unwrap();
        assert_eq!(plan.signal, Some(4242));
        assert!(plan.remove_dirs.is_empty());
        assert_eq!(plan.remove_record, None);
    }

    #[test]
    fn a_run_directory_start_did_not_name_is_never_removed() {
        for run_dir in ["/", "/tmp", "/home/me/pgdrop", ""] {
            let plan = plan(Path::new("/d"), None, Some(&record(false, true, run_dir))).unwrap();
            assert!(plan.remove_dirs.is_empty(), "{run_dir:?}");
        }
    }

    #[test]
    fn a_single_user_server_or_a_foreign_record_is_refused() {
        assert!(matches!(
            plan(Path::new("/d"), Some("-9\n"), None),
            Err(StopError::SingleUser(9))
        ));
        assert!(matches!(
            plan(Path::new("/d"), None, Some("remove everything\n")),
            Err(StopError::BadRecord)
        ));
    }

    #[test]
    fn the_datadir_is_the_flag_else_pgdata_made_absolute() {
        let cwd = Path::new("/work");
        let flag = Stop {
            datadir: Some("d".into()),
        };
        assert_eq!(
            datadir(&flag, Some(OsStr::new("/env")), cwd).unwrap(),
            Path::new("/work/d")
        );
        assert_eq!(
            datadir(&Stop::default(), Some(OsStr::new("/env")), cwd).unwrap(),
            Path::new("/env")
        );
        for unset in [None, Some(OsStr::new(""))] {
            assert!(matches!(
                datadir(&Stop::default(), unset, cwd),
                Err(StopError::NoDatadir)
            ));
        }
    }
}
