//! The data directory tree: which entries `initdb` makes, in which order,
//! with which modes.
//!
//! This is `initialize_data_directory` (`initdb.c:3049`) as far as its first
//! call into the server — `create_data_directory` (`:2890`),
//! `create_xlog_or_symlink` (`:2948`), the `subdirs[]` loop (`:3068`) and the
//! top-level `write_version_file(NULL)` (`:3086`).
//!
//! Data / Calculations / Actions: [`FsOp`] is the data, [`layout`] is the
//! whole decision as a pure function of a [`CreatePlan`], and [`apply`] is the
//! one function that touches a disk.
//!
//! ## Why the ops carry final modes
//!
//! C sets a process-wide `umask(pg_mode_mask)` (`initdb.c:3057`) and then
//! passes `pg_dir_create_mode` to every `mkdir`. `umask` is not reachable from
//! the standard library and this crate is `#![deny(unsafe_code)]` with no
//! approved libc dependency, so each op names the mode the entry must end up
//! with and [`apply`] sets it explicitly. The two are the same number:
//! `DataDirPerm::masked_dir_mode` computes C's `mode & ~mask` and
//! `file_perm::tests::the_mask_never_touches_the_create_modes` shows it is the
//! create mode unchanged for both settings.
//!
//! [`apply`] therefore creates each entry *at* the wanted mode (so the process
//! umask can only ever make it stricter, never laxer, in the window before the
//! `chmod`) and then chmods it to exactly that mode, which is what C's
//! `mkdir` under its own umask achieves in one step.

use std::path::{Path, PathBuf};

use crate::help::PG_MAJORVERSION;
use crate::validate::{CreatePlan, DirAction};

/// `subdirs[]` (`initdb.c:231`), verbatim and in order. `pg_wal` itself is not
/// here: `create_xlog_or_symlink` has already made it, or made the symlink
/// that stands in for it, by the time this loop runs.
pub const SUBDIRS: [&str; 23] = [
    "global",
    "pg_wal/archive_status",
    "pg_wal/summaries",
    "pg_commit_ts",
    "pg_dynshmem",
    "pg_notify",
    "pg_serial",
    "pg_snapshots",
    "pg_subtrans",
    "pg_twophase",
    "pg_multixact",
    "pg_multixact/members",
    "pg_multixact/offsets",
    "base",
    "base/1",
    "pg_replslot",
    "pg_tblspc",
    "pg_stat",
    "pg_stat_tmp",
    "pg_xact",
    "pg_logical",
    "pg_logical/snapshots",
    "pg_logical/mappings",
];

/// One filesystem change, with the mode the entry must end up with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsOp {
    /// `mkdir(path, mode)`, or `pg_mkdir_p(path, mode)` when `parents`.
    ///
    /// `pg_mkdir_p` (`src/port/pgmkdirp.c:57`) gives the parents it has to
    /// create `0777 & ~(pg_mode_mask & ~u+wx)`, which for both of initdb's two
    /// settings is the same `pg_dir_create_mode` the target gets — so one mode
    /// covers the whole chain. It is also the "equivalent to mkdir -p except
    /// we don't complain if the target directory already exists" call, which
    /// is why `apply` tolerates an existing directory here and nowhere else.
    CreateDir {
        path: PathBuf,
        mode: u32,
        parents: bool,
    },
    /// `chmod(path, mode)` on a directory that was already there and empty.
    SetMode { path: PathBuf, mode: u32 },
    /// `symlink(target, link)` (`initdb.c:3015`).
    Symlink { target: PathBuf, link: PathBuf },
    /// `fopen(path, "wb")`, write, `fclose` — `write_version_file` at
    /// `initdb.c:1024`.
    WriteFile {
        path: PathBuf,
        mode: u32,
        contents: String,
    },
}

impl FsOp {
    /// The path this op names in its `pg_fatal` message.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            FsOp::CreateDir { path, .. }
            | FsOp::SetMode { path, .. }
            | FsOp::WriteFile { path, .. } => path,
            // initdb.c:3015 reports `subdirloc`, the link, not the target.
            FsOp::Symlink { link, .. } => link,
        }
    }
}

/// The content of a `PG_VERSION` file: `fprintf(version_file, "%s\n",
/// PG_MAJORVERSION)` (`initdb.c:1036`).
#[must_use]
pub fn version_file_contents() -> String {
    format!("{PG_MAJORVERSION}\n")
}

/// Pure: every filesystem change `initialize_data_directory` makes before it
/// starts a backend, in upstream order.
///
/// The order is load-bearing twice over: `pg_wal` must exist (or be a symlink)
/// before the `subdirs[]` loop reaches `pg_wal/archive_status`, and `base`
/// must precede `base/1`, which is why neither is created with parents.
#[must_use]
pub fn layout(plan: &CreatePlan) -> Vec<FsOp> {
    let perm = plan.perm;
    let dir_mode = perm.masked_dir_mode();
    let mut ops = Vec::with_capacity(SUBDIRS.len() + 4);

    // create_data_directory(), initdb.c:2890.
    ops.push(dir_op(&plan.pgdata, plan.pgdata_action, dir_mode));

    // create_xlog_or_symlink(), initdb.c:2948.
    let subdirloc = plan.pgdata.join("pg_wal");
    match &plan.waldir {
        Some((xlog_dir, action)) => {
            ops.push(dir_op(xlog_dir, *action, dir_mode));
            ops.push(FsOp::Symlink {
                target: xlog_dir.clone(),
                link: subdirloc,
            });
        }
        // initdb.c:3021 — "just make the subdirectory normally".
        None => ops.push(FsOp::CreateDir {
            path: subdirloc,
            mode: dir_mode,
            parents: false,
        }),
    }

    // initdb.c:3068 — mkdir(), not pg_mkdir_p(): "the parent directory already
    // exists, so we only need mkdir() not pg_mkdir_p() here, which avoids some
    // failure modes; cf bug #13853".
    ops.extend(SUBDIRS.iter().map(|subdir| FsOp::CreateDir {
        path: plan.pgdata.join(subdir),
        mode: dir_mode,
        parents: false,
    }));

    // initdb.c:3086 — "Top level PG_VERSION is checked by bootstrapper, so
    // make it first". `base/1/PG_VERSION` (`:3097`) is written only after
    // bootstrap and so is not part of this layout.
    ops.push(FsOp::WriteFile {
        path: plan.pgdata.join("PG_VERSION"),
        mode: perm.masked_file_mode(),
        contents: version_file_contents(),
    });

    ops
}

/// The two halves of `create_data_directory`'s and `create_xlog_or_symlink`'s
/// switch: `pg_check_dir` said 0, so make it; or 1, so fix its permissions.
fn dir_op(path: &Path, action: DirAction, mode: u32) -> FsOp {
    match action {
        // initdb.c:2902 and :2973, both pg_mkdir_p.
        DirAction::Create => FsOp::CreateDir {
            path: path.to_path_buf(),
            mode,
            parents: true,
        },
        // initdb.c:2916 and :2988, both chmod.
        DirAction::ReuseEmpty => FsOp::SetMode {
            path: path.to_path_buf(),
            mode,
        },
    }
}

/// Pure: the entries `layout` leaves under `pgdata`, relative to it and
/// sorted, paired with the mode each must have.
///
/// This is the "tree listing (names + modes)" the acceptance for this port
/// compares against C initdb's, and it is also what makes the op list readable
/// in a test failure.
#[must_use]
pub fn tree_listing(plan: &CreatePlan) -> Vec<(PathBuf, u32)> {
    let mut listing: Vec<(PathBuf, u32)> = layout(plan)
        .into_iter()
        .filter_map(|op| match op {
            FsOp::CreateDir { path, mode, .. }
            | FsOp::SetMode { path, mode }
            | FsOp::WriteFile { path, mode, .. } => {
                let relative = path.strip_prefix(&plan.pgdata).ok()?.to_path_buf();
                Some((relative, mode))
            }
            // A symlink's own mode is not the tree's business: `check_mode_recursive`
            // stats through it (`Utils.pm:601`), reporting the target's mode.
            FsOp::Symlink { .. } => None,
        })
        // `strip_prefix` leaves the data directory itself as "", and the WAL
        // directory (an absolute path elsewhere) does not strip at all.
        .filter(|(relative, _)| !relative.as_os_str().is_empty())
        .collect();
    listing.sort();
    listing
}

/// Pure: the directories `pg_mkdir_p` has to create for `path`, outermost
/// first, given a predicate that says which ones are already there.
///
/// `pg_mkdir_p` (`src/port/pgmkdirp.c:57`) walks the *components* of the path
/// and mkdirs each prefix, so it never touches anything outside `path`. Walking
/// `Path::ancestors` instead is the same set of prefixes with one trap: the
/// last ancestor of a relative path is the empty path, which no `mkdir` may
/// ever be handed, and which `Path::exists` reports as absent. `initdb -D
/// mydata` is a legal relative data directory (`initdb.c:2634` canonicalizes
/// it, it does not require an absolute one), so this is reachable; the empty
/// path is dropped here rather than in the caller.
#[must_use]
pub fn missing_ancestors(path: &Path, exists: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut missing: Vec<PathBuf> = path
        .ancestors()
        .take_while(|dir| !dir.as_os_str().is_empty() && !exists(dir))
        .map(Path::to_path_buf)
        .collect();
    // `ancestors` yields deepest first; mkdir needs the outermost first.
    missing.reverse();
    missing
}

#[cfg(unix)]
mod unix {
    use std::fs::{DirBuilder, OpenOptions, Permissions};
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::Path;

    use super::FsOp;
    use crate::error::InitdbError;
    use crate::validate::strerror;

    /// Action: carry out `ops`, in order.
    ///
    /// Each failure is the `pg_fatal` C reaches at the same point, so the
    /// stderr is identical; C exits on the first one and so does this, by
    /// returning.
    ///
    /// # Errors
    /// The first op that fails, as the [`InitdbError`] naming its upstream
    /// site.
    pub fn apply(ops: &[FsOp]) -> Result<(), InitdbError> {
        for op in ops {
            match op {
                FsOp::CreateDir {
                    path,
                    mode,
                    parents,
                } => create_dir(path, *mode, *parents)?,
                FsOp::SetMode { path, mode } => set_mode(path, *mode)?,
                FsOp::Symlink { target, link } => std::os::unix::fs::symlink(target, link)
                    .map_err(|err| InitdbError::CouldNotCreateSymbolicLink {
                        path: link.display().to_string(),
                        reason: strerror(&err),
                    })?,
                FsOp::WriteFile {
                    path,
                    mode,
                    contents,
                } => write_file(path, *mode, contents)?,
            }
        }
        Ok(())
    }

    /// `mkdir(path, mode)` / `pg_mkdir_p(path, mode)` (`initdb.c:2903`,
    /// `:2974`, `:3022`, `:3079`).
    fn create_dir(path: &Path, mode: u32, parents: bool) -> Result<(), InitdbError> {
        let fail = |dir: &Path, err: &std::io::Error| InitdbError::CouldNotCreateDirectory {
            path: dir.display().to_string(),
            reason: strerror(err),
        };
        if !parents {
            return mkdir(path, mode).map_err(|err| fail(path, &err));
        }
        // Every level pg_mkdir_p creates gets the same `mode` — see the note
        // on `FsOp::CreateDir` — and "on failure, the path arg has been
        // modified to show the particular directory level we had problems
        // with", so the failing level is what gets named.
        for dir in super::missing_ancestors(path, &|dir| dir.exists()) {
            match mkdir(&dir, mode) {
                Ok(()) => {}
                // pgmkdirp.c:129 — "If we got EEXIST because there's already a
                // directory there, don't complain", and leave its mode alone.
                // Only pg_mkdir_p forgives this; the plain `mkdir` above does
                // not, which is how `initdb $existing_datadir` still fails.
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
                Err(err) => return Err(fail(&dir, &err)),
            }
        }
        Ok(())
    }

    /// One `mkdir` at exactly `mode`, whatever the process umask.
    ///
    /// `.mode()` is masked by that umask, so the directory can only be born
    /// stricter than wanted; the `chmod` then makes it exact. See the module
    /// docs for why the umask itself is out of reach.
    fn mkdir(path: &Path, mode: u32) -> std::io::Result<()> {
        DirBuilder::new().mode(mode).create(path)?;
        std::fs::set_permissions(path, Permissions::from_mode(mode))
    }

    /// `chmod(path, mode)` (`initdb.c:2917`, `:2989`).
    fn set_mode(path: &Path, mode: u32) -> Result<(), InitdbError> {
        std::fs::set_permissions(path, Permissions::from_mode(mode)).map_err(|err| {
            InitdbError::CouldNotChangePermissionsOfDirectory {
                path: path.display().to_string(),
                reason: strerror(&err),
            }
        })
    }

    /// `write_version_file` (`initdb.c:1024`): the open and the write are two
    /// different `pg_fatal`s, so they stay two different errors.
    fn write_file(path: &Path, mode: u32, contents: &str) -> Result<(), InitdbError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(path)
            .map_err(|err| InitdbError::CouldNotOpenFileForWriting {
                path: path.display().to_string(),
                reason: strerror(&err),
            })?;
        // No fsync here: C's write_version_file only fprintf's and fclose's,
        // and initdb's one durability pass is the end-of-run fsync that
        // --no-sync suppresses (`sync_pgdata`, `initdb.c:3512`, guarded by
        // `do_sync` at `:3508`), which is its own issue.
        let written = file
            .write_all(contents.as_bytes())
            .and_then(|()| std::fs::set_permissions(path, Permissions::from_mode(mode)));
        written.map_err(|err| InitdbError::CouldNotWriteFile {
            path: path.display().to_string(),
            reason: strerror(&err),
        })
    }
}

#[cfg(unix)]
pub use unix::apply;

/// Non-Unix builds have no `chmod`; the port targets Unix (ADR-0001).
#[cfg(not(unix))]
#[allow(clippy::missing_errors_doc)]
pub fn apply(_ops: &[FsOp]) -> Result<(), crate::error::InitdbError> {
    unimplemented!("rinitdb creates a data directory on Unix only")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LocaleProvider;
    use crate::file_perm::DataDirPerm;

    fn plan(pgdata: &str, allow_group_access: bool) -> CreatePlan {
        CreatePlan {
            pgdata: PathBuf::from(pgdata),
            pgdata_action: DirAction::Create,
            waldir: None,
            perm: DataDirPerm::for_allow_group_access(allow_group_access),
            locale_provider: LocaleProvider::Libc,
            datlocale: None,
            encoding: None,
            username: Some("postgres".to_owned()),
            gucs: Vec::new(),
        }
    }

    #[test]
    fn the_subdirs_table_is_upstreams_in_upstream_order() {
        // A second, independent transcription of initdb.c:231-255. Checking
        // only the length and the ends would let a typo or a dropped row
        // through, and the gate cannot catch that: it compares this port's
        // idea of the tree against C's cluster using this same table.
        // pg_wal is deliberately absent — create_xlog_or_symlink has made it.
        assert_eq!(
            SUBDIRS,
            [
                "global",
                "pg_wal/archive_status",
                "pg_wal/summaries",
                "pg_commit_ts",
                "pg_dynshmem",
                "pg_notify",
                "pg_serial",
                "pg_snapshots",
                "pg_subtrans",
                "pg_twophase",
                "pg_multixact",
                "pg_multixact/members",
                "pg_multixact/offsets",
                "base",
                "base/1",
                "pg_replslot",
                "pg_tblspc",
                "pg_stat",
                "pg_stat_tmp",
                "pg_xact",
                "pg_logical",
                "pg_logical/snapshots",
                "pg_logical/mappings",
            ]
        );
        assert!(!SUBDIRS.contains(&"pg_wal"));
    }

    #[test]
    fn pg_mkdir_p_is_never_handed_the_empty_path() {
        // Path::ancestors ends a relative path with "", which Path::exists
        // reports as absent; mkdir("") is ENOENT. `initdb -D mydata` is a
        // legal command line, so this is a real input, not a curiosity.
        let nothing_exists = |_: &Path| false;
        assert_eq!(
            missing_ancestors(Path::new("mydata"), &nothing_exists),
            [PathBuf::from("mydata")]
        );
        assert_eq!(
            missing_ancestors(Path::new("a/b"), &nothing_exists),
            [PathBuf::from("a"), PathBuf::from("a/b")]
        );
        for path in ["mydata", "a/b", "./data", "/tmp/x/y", ""] {
            assert!(
                missing_ancestors(Path::new(path), &nothing_exists)
                    .iter()
                    .all(|dir| !dir.as_os_str().is_empty()),
                "{path:?} would mkdir the empty path"
            );
        }
    }

    #[test]
    fn only_the_absent_ancestors_are_created_outermost_first() {
        // /tmp exists, /tmp/x and /tmp/x/y do not: pg_mkdir_p makes the two
        // missing levels, parent before child, and leaves /tmp alone.
        let exists = |dir: &Path| dir == Path::new("/tmp") || dir == Path::new("/");
        assert_eq!(
            missing_ancestors(Path::new("/tmp/x/y"), &exists),
            [PathBuf::from("/tmp/x"), PathBuf::from("/tmp/x/y")]
        );
        // Nothing to do when the target is already there.
        assert!(missing_ancestors(Path::new("/tmp"), &exists).is_empty());
    }

    #[test]
    fn a_parent_never_follows_its_child() {
        // mkdir(), not pg_mkdir_p(): base must precede base/1 and pg_wal must
        // already exist when pg_wal/archive_status is made.
        for (index, subdir) in SUBDIRS.iter().enumerate() {
            if let Some((parent, _)) = subdir.rsplit_once('/') {
                let earlier = SUBDIRS[..index].contains(&parent) || parent == "pg_wal";
                assert!(earlier, "{subdir} is created before its parent {parent}");
            }
        }
    }

    #[test]
    fn a_new_data_directory_is_made_then_filled() {
        let ops = layout(&plan("/tmp/data", false));
        assert_eq!(
            ops[0],
            FsOp::CreateDir {
                path: PathBuf::from("/tmp/data"),
                mode: 0o700,
                parents: true,
            }
        );
        // create_xlog_or_symlink() runs before the subdirs loop.
        assert_eq!(
            ops[1],
            FsOp::CreateDir {
                path: PathBuf::from("/tmp/data/pg_wal"),
                mode: 0o700,
                parents: false,
            }
        );
        assert_eq!(
            ops[2],
            FsOp::CreateDir {
                path: PathBuf::from("/tmp/data/global"),
                mode: 0o700,
                parents: false,
            }
        );
        assert_eq!(ops.len(), SUBDIRS.len() + 2 + 1);
    }

    #[test]
    fn an_existing_empty_data_directory_is_chmodded_not_created() {
        let mut create = plan("/tmp/data", false);
        create.pgdata_action = DirAction::ReuseEmpty;
        assert_eq!(
            layout(&create)[0],
            FsOp::SetMode {
                path: PathBuf::from("/tmp/data"),
                mode: 0o700,
            }
        );
    }

    #[test]
    fn the_last_op_is_the_top_level_version_file() {
        let ops = layout(&plan("/tmp/data", false));
        assert_eq!(
            ops.last().unwrap(),
            &FsOp::WriteFile {
                path: PathBuf::from("/tmp/data/PG_VERSION"),
                mode: 0o600,
                contents: "18\n".to_owned(),
            }
        );
        // base/1/PG_VERSION is written after bootstrap, not here.
        assert!(!ops.iter().any(|op| matches!(
            op,
            FsOp::WriteFile { path, .. } if path.ends_with("base/1/PG_VERSION")
        )));
    }

    #[test]
    fn allow_group_access_moves_every_mode_at_once() {
        let ops = layout(&plan("/tmp/data", true));
        for op in &ops {
            match op {
                FsOp::CreateDir { mode, .. } | FsOp::SetMode { mode, .. } => {
                    assert_eq!(*mode, 0o750, "{op:?}");
                }
                FsOp::WriteFile { mode, .. } => assert_eq!(*mode, 0o640, "{op:?}"),
                FsOp::Symlink { .. } => {}
            }
        }
    }

    #[test]
    fn waldir_replaces_the_pg_wal_directory_with_a_symlink() {
        let mut create = plan("/tmp/data", false);
        create.waldir = Some((PathBuf::from("/mnt/wal"), DirAction::Create));
        let ops = layout(&create);
        assert_eq!(
            ops[1],
            FsOp::CreateDir {
                path: PathBuf::from("/mnt/wal"),
                mode: 0o700,
                parents: true,
            }
        );
        assert_eq!(
            ops[2],
            FsOp::Symlink {
                target: PathBuf::from("/mnt/wal"),
                link: PathBuf::from("/tmp/data/pg_wal"),
            }
        );
        // The subdirs loop still makes pg_wal/archive_status, through the link.
        assert!(ops.contains(&FsOp::CreateDir {
            path: PathBuf::from("/tmp/data/pg_wal/archive_status"),
            mode: 0o700,
            parents: false,
        }));
        assert!(!ops.contains(&FsOp::CreateDir {
            path: PathBuf::from("/tmp/data/pg_wal"),
            mode: 0o700,
            parents: false,
        }));
    }

    #[test]
    fn an_existing_empty_waldir_is_chmodded_then_linked() {
        let mut create = plan("/tmp/data", true);
        create.waldir = Some((PathBuf::from("/mnt/wal"), DirAction::ReuseEmpty));
        let ops = layout(&create);
        assert_eq!(
            ops[1],
            FsOp::SetMode {
                path: PathBuf::from("/mnt/wal"),
                mode: 0o750,
            }
        );
        assert!(matches!(ops[2], FsOp::Symlink { .. }));
    }

    #[test]
    fn a_symlink_error_names_the_link_not_the_target() {
        // initdb.c:3015 reports subdirloc.
        let op = FsOp::Symlink {
            target: PathBuf::from("/mnt/wal"),
            link: PathBuf::from("/tmp/data/pg_wal"),
        };
        assert_eq!(op.path(), Path::new("/tmp/data/pg_wal"));
    }

    #[test]
    fn the_tree_listing_is_relative_sorted_and_free_of_the_waldir() {
        let mut create = plan("/tmp/data", false);
        create.waldir = Some((PathBuf::from("/mnt/wal"), DirAction::Create));
        let listing = tree_listing(&create);
        // The data directory itself and the WAL directory are not entries
        // under the tree; everything else is, exactly once.
        assert_eq!(listing.len(), SUBDIRS.len() + 1);
        assert!(listing.iter().all(|(path, _)| path.is_relative()));
        assert!(listing.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(listing[0], (PathBuf::from("PG_VERSION"), 0o600));
        assert!(listing.contains(&(PathBuf::from("base/1"), 0o700)));
    }

    #[test]
    fn the_version_file_holds_the_major_version_and_a_newline() {
        assert_eq!(version_file_contents(), "18\n");
    }
}
