//! `cleanup_directories_atexit` (`initdb.c:762`): what initdb takes back when
//! it fails after it has already made a directory.
//!
//! C registers this with `atexit` (`initdb.c:3436`) and it runs on every exit
//! where `success` is still false. It is why
//!
//! ```text
//! initdb: error: WAL directory location must be an absolute path
//! initdb: removing data directory "/tmp/…/data"
//! ```
//!
//! is two lines and not one: `create_data_directory` has already run by the
//! time `create_xlog_or_symlink` rejects `--waldir`.
//!
//! Data / Calculations / Actions:
//!
//! - [`Progress`] is data — the typed form of `made_new_pgdata`,
//!   `found_existing_pgdata`, `made_new_xlogdir` and `found_existing_xlogdir`
//!   (`initdb.c:188`-`:191`). The creation sequence records one entry per
//!   directory it has actually touched, and nothing else knows about it.
//! - [`plan`] is the calculation: the handler's whole `if` cascade, including
//!   the `-n` / `--no-clean` arm, as a list of [`Step`]s. It reads no
//!   filesystem, so every branch is unit-tested from a value.
//! - [`apply`] is the action: it announces each step and carries it out.
//!
//! One thing `rmtree` (`src/common/rmtree.c:50`) does that [`apply`] does not:
//! log a `pg_log_warning` per entry it could not remove. Only the summary
//! `pg_log_error` is reproduced. Reaching either needs a directory initdb
//! created and can no longer delete, which no stolen case provokes.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::DirRole;
use crate::help::PROGNAME;
use crate::validate::DirAction;

/// What the exit handler knows: which of the two directories the creation
/// sequence reached, and whether it made each one or adopted an empty one.
///
/// `None` is C's "died during startup, do nothing" (`initdb.c:795`): the
/// directory was never touched, so there is nothing to take back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// `made_new_pgdata` / `found_existing_pgdata` (`initdb.c:188`, `:189`).
    pub pgdata: Option<(PathBuf, DirAction)>,
    /// `made_new_xlogdir` / `found_existing_xlogdir` (`:190`, `:191`).
    pub waldir: Option<(PathBuf, DirAction)>,
}

/// One directory's worth of the handler's work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// `rmtree(path, true)`: the directory and everything under it, because
    /// initdb is what created it.
    Remove { role: DirRole, path: PathBuf },
    /// `rmtree(path, false)`: the contents only, because the directory was
    /// already there and empty.
    RemoveContents { role: DirRole, path: PathBuf },
    /// `-n` / `--no-clean` (`initdb.c:797`): say what was left behind.
    Keep { role: DirRole, path: PathBuf },
}

impl Step {
    /// The path this step names.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Step::Remove { path, .. }
            | Step::RemoveContents { path, .. }
            | Step::Keep { path, .. } => path,
        }
    }

    /// The `pg_log_info` line that precedes the work (`initdb.c:771`, `:777`,
    /// `:785`, `:791`, `:800`, `:804`).
    #[must_use]
    pub fn announcement(&self) -> String {
        let quoted = self.path().to_string_lossy();
        match self {
            Step::Remove { role, .. } => format!("removing {} \"{quoted}\"", role.noun()),
            Step::RemoveContents { role, .. } => {
                format!("removing contents of {} \"{quoted}\"", role.noun())
            }
            Step::Keep { role, .. } => {
                format!("{} \"{quoted}\" not removed at user's request", role.noun())
            }
        }
    }

    /// The `pg_log_error` line when the removal fails (`initdb.c:773`, `:780`,
    /// `:787`, `:793`); `None` for a step that removes nothing.
    #[must_use]
    pub fn failure(&self) -> Option<String> {
        match self {
            Step::Remove { role, .. } => Some(format!("failed to remove {}", role.noun())),
            Step::RemoveContents { role, .. } => {
                Some(format!("failed to remove contents of {}", role.noun()))
            }
            Step::Keep { .. } => None,
        }
    }
}

/// Pure: `cleanup_directories_atexit`'s whole cascade, for a run that failed.
///
/// C's order is the data directory then the WAL directory, and it treats "I
/// made it" and "it was there and empty" differently for each — four messages,
/// or two more under `--no-clean`. Nothing here decides *whether* the run
/// failed: the caller only reaches this on the `success == false` path
/// (`initdb.c:764`).
#[must_use]
pub fn plan(progress: &Progress, no_clean: bool) -> Vec<Step> {
    [
        (DirRole::Data, progress.pgdata.as_ref()),
        (DirRole::Wal, progress.waldir.as_ref()),
    ]
    .into_iter()
    .filter_map(|(role, touched)| {
        touched.map(|(path, action)| step(role, path.clone(), *action, no_clean))
    })
    .collect()
}

/// The three arms of the cascade for one directory.
fn step(role: DirRole, path: PathBuf, action: DirAction, no_clean: bool) -> Step {
    match (no_clean, action) {
        (true, _) => Step::Keep { role, path },
        // made_new_*: initdb.c:769 and :783, rmtree(…, true).
        (false, DirAction::Create) => Step::Remove { role, path },
        // found_existing_*: initdb.c:775 and :789, rmtree(…, false).
        (false, DirAction::ReuseEmpty) => Step::RemoveContents { role, path },
    }
}

/// Action: announce each step on `stderr` and carry it out.
///
/// Nothing is returned. The handler runs after initdb has already decided to
/// fail and reported why, so a removal that itself fails adds its own
/// `pg_log_error` line and changes nothing else (`initdb.c:773`).
pub fn apply(steps: &[Step], stderr: &mut impl Write) {
    for step in steps {
        // Writes to a closed stream are not worth a second error message.
        let _ = writeln!(stderr, "{PROGNAME}: {}", step.announcement());
        if remove(step).is_err()
            && let Some(failure) = step.failure()
        {
            let _ = writeln!(stderr, "{PROGNAME}: error: {failure}");
        }
    }
}

/// Action: `rmtree(path, top)` for one step.
fn remove(step: &Step) -> std::io::Result<()> {
    match step {
        Step::Keep { .. } => Ok(()),
        Step::Remove { path, .. } => std::fs::remove_dir_all(path),
        Step::RemoveContents { path, .. } => remove_contents(path),
    }
}

/// Action: `rmtree(path, false)` — everything in the directory, not the
/// directory itself.
///
/// A symbolic link is unlinked rather than followed: `DirEntry::file_type` is
/// the `lstat` answer, so a `pg_wal` that stands for a `--waldir` elsewhere
/// loses only the link, exactly as `rmtree` leaves it.
fn remove_contents(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(pgdata: Option<DirAction>, waldir: Option<DirAction>) -> Progress {
        Progress {
            pgdata: pgdata.map(|action| (PathBuf::from("/tmp/data"), action)),
            waldir: waldir.map(|action| (PathBuf::from("/mnt/wal"), action)),
        }
    }

    fn lines(steps: &[Step]) -> Vec<String> {
        steps.iter().map(Step::announcement).collect()
    }

    #[test]
    fn a_run_that_died_before_the_first_mkdir_cleans_up_nothing() {
        // initdb.c:795 — "otherwise died during startup, do nothing!". This is
        // every pre-flight failure, and it is why `existing_data_directory`
        // prints no cleanup line where the two --waldir cases do.
        assert_eq!(plan(&Progress::default(), false), Vec::new());
        assert_eq!(plan(&Progress::default(), true), Vec::new());
    }

    #[test]
    fn a_data_directory_initdb_made_is_removed_whole() {
        let steps = plan(&progress(Some(DirAction::Create), None), false);
        assert_eq!(
            steps,
            [Step::Remove {
                role: DirRole::Data,
                path: PathBuf::from("/tmp/data"),
            }]
        );
        assert_eq!(lines(&steps), ["removing data directory \"/tmp/data\""]);
        assert_eq!(
            steps[0].failure().as_deref(),
            Some("failed to remove data directory")
        );
    }

    #[test]
    fn a_data_directory_that_was_already_there_keeps_its_directory() {
        // initdb.c:775 — rmtree(pg_data, false): the contents, not the mount
        // point somebody handed us.
        let steps = plan(&progress(Some(DirAction::ReuseEmpty), None), false);
        assert_eq!(
            steps,
            [Step::RemoveContents {
                role: DirRole::Data,
                path: PathBuf::from("/tmp/data"),
            }]
        );
        assert_eq!(
            lines(&steps),
            ["removing contents of data directory \"/tmp/data\""]
        );
        assert_eq!(
            steps[0].failure().as_deref(),
            Some("failed to remove contents of data directory")
        );
    }

    #[test]
    fn the_wal_directory_is_reported_after_the_data_directory() {
        let steps = plan(
            &progress(Some(DirAction::Create), Some(DirAction::ReuseEmpty)),
            false,
        );
        assert_eq!(
            lines(&steps),
            [
                "removing data directory \"/tmp/data\"",
                "removing contents of WAL directory \"/mnt/wal\"",
            ]
        );
        assert_eq!(
            steps[1].failure().as_deref(),
            Some("failed to remove contents of WAL directory")
        );
    }

    #[test]
    fn a_wal_directory_initdb_made_is_removed_whole() {
        let steps = plan(&progress(None, Some(DirAction::Create)), false);
        assert_eq!(lines(&steps), ["removing WAL directory \"/mnt/wal\""]);
        assert_eq!(
            steps[0].failure().as_deref(),
            Some("failed to remove WAL directory")
        );
    }

    #[test]
    fn no_clean_keeps_both_directories_and_says_so() {
        // initdb.c:797 — the else arm, which does not distinguish made from
        // found: either way the directory is still there afterwards.
        for pgdata in [DirAction::Create, DirAction::ReuseEmpty] {
            for waldir in [DirAction::Create, DirAction::ReuseEmpty] {
                let steps = plan(&progress(Some(pgdata), Some(waldir)), true);
                assert_eq!(
                    lines(&steps),
                    [
                        "data directory \"/tmp/data\" not removed at user's request",
                        "WAL directory \"/mnt/wal\" not removed at user's request",
                    ],
                    "{pgdata:?}/{waldir:?}"
                );
                assert!(steps.iter().all(|step| step.failure().is_none()));
            }
        }
    }

    #[test]
    fn every_step_names_the_directory_it_is_about() {
        let steps = plan(
            &progress(Some(DirAction::Create), Some(DirAction::Create)),
            false,
        );
        assert_eq!(steps[0].path(), Path::new("/tmp/data"));
        assert_eq!(steps[1].path(), Path::new("/mnt/wal"));
    }

    #[test]
    fn the_announcement_carries_the_pg_log_info_prefix_and_no_level() {
        // pg_log_info prints "<progname>: " and no level word, unlike
        // pg_log_error's "<progname>: error: " (src/common/logging.c:279).
        let mut out = Vec::new();
        apply(
            &[Step::Keep {
                role: DirRole::Data,
                path: PathBuf::from("/tmp/data"),
            }],
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "initdb: data directory \"/tmp/data\" not removed at user's request\n"
        );
    }

    #[test]
    fn a_removal_that_cannot_happen_reports_its_own_failure_line() {
        // The directory is not there at all, so remove_dir_all fails: C's
        // `if (!rmtree(...)) pg_log_error("failed to remove data directory")`.
        let mut out = Vec::new();
        apply(
            &[Step::Remove {
                role: DirRole::Data,
                path: PathBuf::from("/tmp/pgdrop-no-such-directory-ever"),
            }],
            &mut out,
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "initdb: removing data directory \"/tmp/pgdrop-no-such-directory-ever\"\n\
             initdb: error: failed to remove data directory\n"
        );
    }
}
