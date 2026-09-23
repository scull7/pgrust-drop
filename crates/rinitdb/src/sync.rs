//! Syncing the data directory to disk: `--sync-method`, `--sync-only`,
//! `--no-sync-data-files` and `--no-sync`.
//!
//! The option itself is `parse_sync_method`
//! (`src/fe_utils/option_utils.c:90`); the work is `sync_pgdata`
//! (`src/common/file_utils.c:99`) over `walkdir` (`:290`) and `fsync_fname`
//! (`:400`); the two progress messages are `initdb.c:3447` / `:3510` and
//! `:3516`.
//!
//! Data / Calculations / Actions:
//!
//! - [`SyncMethod`] and [`SyncOp`] are data: what was asked for, and the list
//!   of things `sync_pgdata` would do, in upstream order.
//! - [`plan`] is the calculation. It reaches the filesystem only through
//!   [`SyncProbe`], so the whole of `walkdir` — the recursion, the symlink
//!   rules, the `exclude_dir` of `--no-sync-data-files`, and every
//!   `pg_log_error` the walk emits along the way — is unit-tested against a
//!   map of fake directories.
//! - [`apply`] is the only part that opens anything.
//!
//! Warnings are ops rather than side effects of the walk because C interleaves
//! them with the syncing: `walkdir` reports a directory it cannot open and
//! carries on (`file_utils.c:304`), and the position of that line in stderr is
//! part of what the byte-diff gate compares.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::InitdbError;

/// `MINIMUM_VERSION_FOR_PG_WAL` (`file_utils.c:46`) is 100000 and this port
/// only ever syncs its own clusters, so the WAL directory is always `pg_wal`.
const PG_WAL_DIR: &str = "pg_wal";

/// `PG_TBLSPC_DIR` (`src/include/common/relpath.h:41`).
const PG_TBLSPC_DIR: &str = "pg_tblspc";

/// The directory `--no-sync-data-files` excludes (`file_utils.c:190`).
const BASE_DIR: &str = "base";

/// `configure` defines `HAVE_SYNCFS` where `syncfs(2)` exists, which in
/// practice means Linux; `001_initdb.pl:19` reads the same define back out of
/// `pg_config.h` to pick which half of its `--sync-method syncfs` case to run.
///
/// The call itself is not reachable from the standard library — see the
/// `docs/divergences.md` row on `--sync-method=syncfs` — but whether the
/// option is *accepted* is this build-time answer, exactly as upstream.
pub const HAVE_SYNCFS: bool = cfg!(target_os = "linux");

/// `DataDirSyncMethod` (`src/include/common/file_utils.h:27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMethod {
    /// `DATA_DIR_SYNC_METHOD_FSYNC`, the `initdb.c:170` default.
    #[default]
    Fsync,
    /// `DATA_DIR_SYNC_METHOD_SYNCFS`.
    Syncfs,
}

/// `parse_sync_method` (`src/fe_utils/option_utils.c:90`), reached from the
/// `case 19:` arm at `initdb.c:3389`.
///
/// `None` is "the option was not given", which leaves `initdb.c:170`'s default.
///
/// # Errors
/// [`InitdbError::UnsupportedSyncMethod`] (`option_utils.c:99`) or
/// [`InitdbError::UnrecognizedSyncMethod`] (`:106`). Both are `pg_log_error`
/// followed by `exit(1)` at the call site, so neither carries a hint.
pub fn parse_sync_method(arg: Option<&str>) -> Result<SyncMethod, InitdbError> {
    match arg {
        None | Some("fsync") => Ok(SyncMethod::Fsync),
        Some("syncfs") if HAVE_SYNCFS => Ok(SyncMethod::Syncfs),
        Some("syncfs") => Err(InitdbError::UnsupportedSyncMethod { name: "syncfs" }),
        Some(other) => Err(InitdbError::UnrecognizedSyncMethod {
            name: other.to_owned(),
        }),
    }
}

/// What `stat`/`lstat` says an entry is, as far as `walkdir` cares
/// (`PGFileType`, `src/include/common/file_utils.h:18`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// `PGFILETYPE_REG`.
    Regular,
    /// `PGFILETYPE_DIR`.
    Directory,
    /// `PGFILETYPE_LNK`.
    Symlink,
    /// Anything else: sockets, fifos, devices. `walkdir` ignores them.
    Other,
}

/// What one `opendir` + `readdir` loop yielded (`file_utils.c:308`).
///
/// The two failures are different messages at different places, so they are
/// different fields: `opendir` failing is the `Err` of
/// [`SyncProbe::read_dir`], which makes `walkdir` give up on the directory
/// entirely (`file_utils.c:304`); `readdir` failing part-way through is
/// [`DirListing::read_error`], which C reports *after* acting on everything it
/// did manage to read and then still fsyncs the directory (`:337`, `:348`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirListing {
    /// The entry names the loop read, in directory order, without `.` and
    /// `..` (`file_utils.c:312`).
    pub names: Vec<OsString>,
    /// `errno` after the loop, as `%m` would print it: `Some` only when
    /// `readdir` itself failed before the directory ran out.
    pub read_error: Option<String>,
}

/// The three things the walk asks of the filesystem.
///
/// A trait so [`plan`] is a calculation: every case below is exercised from a
/// table of fake entries, with no temporary files at all. [`RealFs`] is the
/// only implementor that touches a disk.
///
/// The `Err` payload is what `%m` would print — `strerror(errno)` and nothing
/// else — because every caller interpolates it into a `pg_log_error`.
pub trait SyncProbe {
    /// `lstat(path)`: the entry itself, symlinks unresolved.
    ///
    /// # Errors
    /// What `%m` would print for the failing `lstat`.
    fn lstat(&self, path: &Path) -> Result<FileKind, String>;

    /// `stat(path)`: the entry a symlink points at.
    ///
    /// # Errors
    /// What `%m` would print for the failing `stat`.
    fn stat(&self, path: &Path) -> Result<FileKind, String>;

    /// `opendir` + the `readdir` loop, as [`DirListing`].
    ///
    /// # Errors
    /// What `%m` would print for a failing `opendir`. A `readdir` that fails
    /// part-way through is not an error here: it comes back as
    /// [`DirListing::read_error`] alongside the names already read.
    fn read_dir(&self, path: &Path) -> Result<DirListing, String>;
}

/// One step of `sync_pgdata`, in the order C performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOp {
    /// `fsync_fname(path, isdir)` (`file_utils.c:400`).
    Fsync { path: PathBuf, isdir: bool },
    /// A `pg_log_error` the walk emits before carrying on (`file_utils.c:123`,
    /// `:304`, `:588`). Fatal sites are not ops: they stop [`apply`].
    Warn(InitdbError),
}

/// `sync_pgdata` (`src/common/file_utils.c:99`) as a list of steps.
///
/// `sync_data_files` is `!--no-sync-data-files` (`initdb.c:3396`): false
/// excludes `<pgdata>/base` from the walk and skips `pg_tblspc` entirely.
#[must_use]
pub fn plan(
    pgdata: &Path,
    method: SyncMethod,
    sync_data_files: bool,
    fs: &dyn SyncProbe,
) -> Vec<SyncOp> {
    let pg_wal = pgdata.join(PG_WAL_DIR);
    let pg_tblspc = pgdata.join(PG_TBLSPC_DIR);
    let mut ops = Vec::new();

    // file_utils.c:114 — "If pg_wal is a symlink, we'll need to recurse into
    // it separately, because the first walkdir below will ignore it."
    let xlog_is_symlink = match fs.lstat(&pg_wal) {
        Ok(kind) => kind == FileKind::Symlink,
        Err(reason) => {
            // file_utils.c:123 — reported, and the walk carries on with false.
            ops.push(SyncOp::Warn(InitdbError::CouldNotStatFile {
                path: display(&pg_wal),
                reason,
            }));
            false
        }
    };

    // file_utils.c:128 is a switch on the method, and both arms plan the same
    // `DATA_DIR_SYNC_METHOD_FSYNC` walk here, because `syncfs(2)` is not
    // reachable from the standard library; see the `--sync-method=syncfs` row
    // in docs/divergences.md and `syncfs_plans_the_same_walk_as_fsync`. This
    // match is the exhaustiveness guard that keeps that a decision: a third
    // `DataDirSyncMethod` stops compiling here instead of quietly inheriting
    // the fsync walk.
    match method {
        SyncMethod::Fsync | SyncMethod::Syncfs => {}
    }

    let exclude_dir = (!sync_data_files).then(|| pgdata.join(BASE_DIR));

    // file_utils.c:210 — "The main call ignores symlinks, so in addition to
    // specially processing pg_wal if it's a symlink, pg_tblspc has to be
    // visited separately with process_symlinks = true."
    //
    // The `pre_sync_fname` pass at `:200` is absent: it is a hint compiled in
    // only when `PG_FLUSH_DATA_WORKS` is defined. See docs/divergences.md.
    walkdir(pgdata, exclude_dir.as_deref(), false, fs, &mut ops);
    if xlog_is_symlink {
        walkdir(&pg_wal, None, false, fs, &mut ops);
    }
    if sync_data_files {
        walkdir(&pg_tblspc, None, true, fs, &mut ops);
    }

    ops
}

/// `walkdir` (`src/common/file_utils.c:290`): the action is applied to every
/// regular file and directory below `path`, and to `path` itself last.
fn walkdir(
    path: &Path,
    exclude_dir: Option<&Path>,
    process_symlinks: bool,
    fs: &dyn SyncProbe,
    ops: &mut Vec<SyncOp>,
) {
    // file_utils.c:298.
    if exclude_dir == Some(path) {
        return;
    }

    let listing = match fs.read_dir(path) {
        Ok(listing) => listing,
        Err(reason) => {
            // file_utils.c:304 — reported, and the caller carries on. Note
            // that `path` itself is *not* fsync'd in this case: C returns
            // before the trailing action at `:348`.
            ops.push(SyncOp::Warn(InitdbError::CouldNotOpenDirectory {
                path: display(path),
                reason,
            }));
            return;
        }
    };

    for name in listing.names {
        let subpath = path.join(name);
        match dirent_type(&subpath, process_symlinks, fs, ops) {
            Some(FileKind::Regular) => ops.push(SyncOp::Fsync {
                path: subpath,
                isdir: false,
            }),
            // file_utils.c:279 — "we intentionally don't pass down the
            // process_symlinks flag to recursive calls".
            Some(FileKind::Directory) => walkdir(&subpath, exclude_dir, false, fs, ops),
            // file_utils.c:326 — remaining symlinks, unknown types and the
            // entries `get_dirent_type` already complained about are ignored.
            _ => {}
        }
    }

    // file_utils.c:337 — `if (errno)` after the loop. A readdir that gave up
    // part-way is reported here, after everything it did read has been acted
    // on, and the directory is still fsync'd below.
    if let Some(reason) = listing.read_error {
        ops.push(SyncOp::Warn(InitdbError::CouldNotReadDirectory {
            path: display(path),
            reason,
        }));
    }

    // file_utils.c:343 — "It's important to fsync the destination directory
    // itself as individual file fsyncs don't guarantee that the directory
    // entry for the file is synced."
    ops.push(SyncOp::Fsync {
        path: path.to_path_buf(),
        isdir: true,
    });
}

/// `get_dirent_type(path, de, look_through_symlinks, PG_LOG_ERROR)`
/// (`src/common/file_utils.c:547`).
///
/// `d_type` is not reachable from `std::fs::DirEntry`, so this always takes
/// the `PGFILETYPE_UNKNOWN` path C takes on a filesystem that does not fill it
/// in: `stat` when following symlinks, `lstat` otherwise. The answer is the
/// same either way; only the number of syscalls differs.
fn dirent_type(
    path: &Path,
    look_through_symlinks: bool,
    fs: &dyn SyncProbe,
    ops: &mut Vec<SyncOp>,
) -> Option<FileKind> {
    let probed = if look_through_symlinks {
        fs.stat(path)
    } else {
        fs.lstat(path)
    };
    match probed {
        Ok(kind) => Some(kind),
        Err(reason) => {
            // file_utils.c:588 — PGFILETYPE_ERROR, reported, entry skipped.
            ops.push(SyncOp::Warn(InitdbError::CouldNotStatFile {
                path: display(path),
                reason,
            }));
            None
        }
    }
}

/// `fputs(_("syncing data to disk ... "), stdout)` (`initdb.c:3447`, `:3510`).
/// No newline: `check_ok` finishes the line.
pub const SYNCING_PROGRESS: &str = "syncing data to disk ... ";

/// `check_ok`'s "all seems well" arm (`initdb.c:2127`).
pub const CHECK_OK: &str = "ok\n";

/// The `--no-sync` note (`initdb.c:3516`), with its leading blank line and its
/// trailing newline, exactly as `printf` writes it.
pub const SYNC_SKIPPED_NOTE: &str = "\nSync to disk skipped.\n\
     The data directory might become corrupt if the operating system crashes.\n";

/// Perform `ops`, writing each warning to `stderr` where C writes it.
///
/// # Errors
/// The first fatal site the walk reaches: [`InitdbError::CouldNotFsyncFile`]
/// (`file_utils.c:440`), which is `pg_log_error` + `exit(EXIT_FAILURE)`.
pub fn apply(ops: &[SyncOp], stderr: &mut impl Write) -> Result<(), InitdbError> {
    for op in ops {
        match op {
            SyncOp::Warn(err) => {
                // Writes to a closed stream are not worth a second message.
                let _ = writeln!(stderr, "{}", err.render());
            }
            SyncOp::Fsync { path, isdir } => {
                if let Some(warning) = fsync_fname(path, *isdir)? {
                    let _ = writeln!(stderr, "{}", warning.render());
                }
            }
        }
    }
    Ok(())
}

/// The `errno` values `fsync_fname` (`file_utils.c:426`, `:438`) tests by
/// name. They have no `std::io::ErrorKind` spelling that covers all four, and
/// `libc` is not an approved dependency, so they are the POSIX numbers Linux
/// uses in `<asm-generic/errno-base.h>`.
mod errno {
    pub const EBADF: i32 = 9;
    pub const EACCES: i32 = 13;
    pub const EISDIR: i32 = 21;
    pub const EINVAL: i32 = 22;
}

/// `fsync_fname` (`src/common/file_utils.c:400`).
///
/// `Ok(None)` is C's `return 0`, `Ok(Some(err))` its `return -1` after a
/// non-fatal `pg_log_error`, and `Err` its `exit(EXIT_FAILURE)`.
fn fsync_fname(path: &Path, isdir: bool) -> Result<Option<InitdbError>, InitdbError> {
    // file_utils.c:407 — "Some OSs require directories to be opened read-only
    // whereas other systems don't allow us to fsync files opened read-only;
    // so we need both cases here."
    let mut open = std::fs::OpenOptions::new();
    if isdir {
        open.read(true);
    } else {
        open.read(true).write(true);
    }

    let file = match open.open(path) {
        Ok(file) => file,
        // file_utils.c:424 — unreadable files are silently ignored.
        Err(err) if is(&err, errno::EACCES) || (isdir && is(&err, errno::EISDIR)) => {
            return Ok(None);
        }
        Err(err) => {
            return Ok(Some(InitdbError::CouldNotOpenFile {
                path: display(path),
                reason: crate::strerror::strerror(&err),
            }));
        }
    };

    match file.sync_all() {
        Ok(()) => Ok(None),
        // file_utils.c:435 — "Some OSes don't allow us to fsync directories at
        // all, so we can ignore those errors."
        Err(err) if isdir && (is(&err, errno::EBADF) || is(&err, errno::EINVAL)) => Ok(None),
        Err(err) => Err(InitdbError::CouldNotFsyncFile {
            path: display(path),
            reason: crate::strerror::strerror(&err),
        }),
    }
}

fn is(err: &std::io::Error, code: i32) -> bool {
    err.raw_os_error() == Some(code)
}

/// A path as it goes into a message. See the `canonicalize_path` row in
/// `docs/divergences.md`: paths are reported as they were typed.
fn display(path: &Path) -> String {
    path.display().to_string()
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl SyncProbe for RealFs {
    fn lstat(&self, path: &Path) -> Result<FileKind, String> {
        std::fs::symlink_metadata(path)
            .map(|meta| kind_of(&meta))
            .map_err(|err| crate::strerror::strerror(&err))
    }

    fn stat(&self, path: &Path) -> Result<FileKind, String> {
        std::fs::metadata(path)
            .map(|meta| kind_of(&meta))
            .map_err(|err| crate::strerror::strerror(&err))
    }

    fn read_dir(&self, path: &Path) -> Result<DirListing, String> {
        // Only the `opendir` failure is an Err: file_utils.c:302.
        let entries = std::fs::read_dir(path).map_err(|err| crate::strerror::strerror(&err))?;
        let mut listing = DirListing::default();
        for entry in entries {
            // file_utils.c:308 — `while (errno = 0, (de = readdir(dir)) !=
            // NULL)`. A failure here ends the loop with the names already
            // read intact; `:337` then reports it. Std folds `readdir`'s
            // errno into the item, so the loop stops at the first bad item,
            // exactly where C's would.
            match entry {
                // `read_dir` already skips "." and ".." (file_utils.c:312).
                Ok(entry) => listing.names.push(entry.file_name()),
                Err(err) => {
                    listing.read_error = Some(crate::strerror::strerror(&err));
                    break;
                }
            }
        }
        Ok(listing)
    }
}

fn kind_of(meta: &std::fs::Metadata) -> FileKind {
    let kind = meta.file_type();
    if kind.is_file() {
        FileKind::Regular
    } else if kind.is_dir() {
        FileKind::Directory
    } else if kind.is_symlink() {
        FileKind::Symlink
    } else {
        FileKind::Other
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    /// A filesystem as a map from path to kind; directories additionally list
    /// their entries in the order `readdir` would hand them over.
    #[derive(Default)]
    struct FakeFs {
        kinds: BTreeMap<PathBuf, FileKind>,
        entries: BTreeMap<PathBuf, Vec<OsString>>,
        /// Where a symlink points, for `stat`.
        targets: BTreeMap<PathBuf, FileKind>,
        /// Directories whose `readdir` gives up after handing over the names
        /// in `entries`, with the `%m` text it gives up with.
        read_errors: BTreeMap<PathBuf, String>,
    }

    impl FakeFs {
        fn dir(mut self, path: &str, names: &[&str]) -> Self {
            self.kinds.insert(PathBuf::from(path), FileKind::Directory);
            self.entries.insert(
                PathBuf::from(path),
                names.iter().map(OsString::from).collect(),
            );
            self
        }

        fn file(mut self, path: &str) -> Self {
            self.kinds.insert(PathBuf::from(path), FileKind::Regular);
            self
        }

        /// A directory whose `readdir` fails after `names` (`file_utils.c:308`).
        fn unreadable_after(mut self, path: &str, names: &[&str], reason: &str) -> Self {
            self = self.dir(path, names);
            self.read_errors
                .insert(PathBuf::from(path), reason.to_owned());
            self
        }

        /// A symlink whose target `stat` resolves to `target`.
        fn link(mut self, path: &str, target: FileKind) -> Self {
            self.kinds.insert(PathBuf::from(path), FileKind::Symlink);
            self.targets.insert(PathBuf::from(path), target);
            self
        }
    }

    const ENOENT: &str = "No such file or directory";

    impl SyncProbe for FakeFs {
        fn lstat(&self, path: &Path) -> Result<FileKind, String> {
            self.kinds
                .get(path)
                .copied()
                .ok_or_else(|| ENOENT.to_owned())
        }

        fn stat(&self, path: &Path) -> Result<FileKind, String> {
            match self.kinds.get(path) {
                Some(FileKind::Symlink) => self
                    .targets
                    .get(path)
                    .copied()
                    .ok_or_else(|| ENOENT.to_owned()),
                Some(kind) => Ok(*kind),
                None => Err(ENOENT.to_owned()),
            }
        }

        fn read_dir(&self, path: &Path) -> Result<DirListing, String> {
            match self.entries.get(path) {
                Some(names) => Ok(DirListing {
                    names: names.clone(),
                    read_error: self.read_errors.get(path).cloned(),
                }),
                None if self.kinds.contains_key(path) => Err("Not a directory".to_owned()),
                None => Err(ENOENT.to_owned()),
            }
        }
    }

    /// A cluster shaped like the one `initdb` leaves behind, cut down to the
    /// directories `sync_pgdata` treats specially.
    fn cluster() -> FakeFs {
        FakeFs::default()
            .dir(
                "/d",
                &["PG_VERSION", "base", "global", "pg_wal", "pg_tblspc"],
            )
            .file("/d/PG_VERSION")
            .dir("/d/base", &["1"])
            .dir("/d/base/1", &["PG_VERSION"])
            .file("/d/base/1/PG_VERSION")
            .dir("/d/global", &["pg_control"])
            .file("/d/global/pg_control")
            .dir("/d/pg_wal", &["000000010000000000000001"])
            .file("/d/pg_wal/000000010000000000000001")
            .dir("/d/pg_tblspc", &[])
    }

    fn fsyncs(ops: &[SyncOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                SyncOp::Fsync { path, .. } => Some(path.display().to_string()),
                SyncOp::Warn(_) => None,
            })
            .collect()
    }

    fn warnings(ops: &[SyncOp]) -> Vec<String> {
        ops.iter()
            .filter_map(|op| match op {
                SyncOp::Warn(err) => Some(err.render()),
                SyncOp::Fsync { .. } => None,
            })
            .collect()
    }

    #[test]
    fn the_default_sync_method_is_fsync() {
        // initdb.c:170.
        assert_eq!(parse_sync_method(None).unwrap(), SyncMethod::Fsync);
        assert_eq!(parse_sync_method(Some("fsync")).unwrap(), SyncMethod::Fsync);
    }

    #[test]
    fn an_unrecognized_sync_method_is_rejected_with_its_own_message() {
        // option_utils.c:106 — no quotes around the value, unlike :99.
        let err = parse_sync_method(Some("fdatasync")).unwrap_err();
        assert_eq!(
            err.render(),
            "initdb: error: unrecognized sync method: fdatasync"
        );
    }

    #[test]
    fn syncfs_is_accepted_exactly_where_the_build_has_it() {
        // option_utils.c:96 and the #else at :98; 001_initdb.pl:19 picks the
        // same two branches from the same define.
        let parsed = parse_sync_method(Some("syncfs"));
        if HAVE_SYNCFS {
            assert_eq!(parsed.unwrap(), SyncMethod::Syncfs);
        } else {
            assert_eq!(
                parsed.unwrap_err().render(),
                "initdb: error: this build does not support sync method \"syncfs\""
            );
        }
    }

    #[test]
    fn a_directory_is_fsynced_after_everything_in_it() {
        // file_utils.c:348.
        let fs = cluster();
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);
        assert_eq!(warnings(&ops), Vec::<String>::new());
        assert_eq!(
            fsyncs(&ops),
            vec![
                "/d/PG_VERSION",
                "/d/base/1/PG_VERSION",
                "/d/base/1",
                "/d/base",
                "/d/global/pg_control",
                "/d/global",
                "/d/pg_wal/000000010000000000000001",
                "/d/pg_wal",
                "/d/pg_tblspc",
                "/d",
                // file_utils.c:221 — pg_tblspc is visited a second time with
                // process_symlinks = true.
                "/d/pg_tblspc",
            ]
        );
        assert!(matches!(
            ops.last(),
            Some(SyncOp::Fsync { isdir: true, .. })
        ));
    }

    #[test]
    fn no_sync_data_files_excludes_base_and_skips_pg_tblspc() {
        // file_utils.c:190 (exclude_dir) and :220 (the pg_tblspc guard).
        let fs = cluster();
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, false, &fs);
        assert_eq!(warnings(&ops), Vec::<String>::new());
        assert_eq!(
            fsyncs(&ops),
            vec![
                "/d/PG_VERSION",
                "/d/global/pg_control",
                "/d/global",
                "/d/pg_wal/000000010000000000000001",
                "/d/pg_wal",
                "/d/pg_tblspc",
                "/d",
            ]
        );
    }

    #[test]
    fn a_symlinked_pg_wal_is_walked_separately() {
        // file_utils.c:114: the first walkdir ignores the symlink, so the
        // contents only get synced by the second call at :219.
        // `cluster()` already gives /d/pg_wal its one segment; `link` only
        // changes what `lstat` says the entry itself is.
        let fs = cluster().link("/d/pg_wal", FileKind::Directory);
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);
        let synced = fsyncs(&ops);
        assert_eq!(
            synced,
            vec![
                "/d/PG_VERSION",
                "/d/base/1/PG_VERSION",
                "/d/base/1",
                "/d/base",
                "/d/global/pg_control",
                "/d/global",
                "/d/pg_tblspc",
                "/d",
                "/d/pg_wal/000000010000000000000001",
                "/d/pg_wal",
                "/d/pg_tblspc",
            ]
        );
    }

    #[test]
    fn a_tablespace_symlink_is_followed_only_under_pg_tblspc() {
        // file_utils.c:221 passes process_symlinks = true for pg_tblspc and
        // :324 refuses to pass it down, so `16384` is walked but the symlink
        // inside the tablespace is not.
        let fs = cluster()
            .dir("/d/pg_tblspc", &["16384"])
            .dir("/d/pg_tblspc/16384", &["PG_18_202506121", "elsewhere"])
            .link("/d/pg_tblspc/16384", FileKind::Directory)
            .dir("/d/pg_tblspc/16384/PG_18_202506121", &[])
            .link("/d/pg_tblspc/16384/elsewhere", FileKind::Directory);
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);
        assert_eq!(warnings(&ops), Vec::<String>::new());
        let synced = fsyncs(&ops);
        assert!(
            synced.contains(&"/d/pg_tblspc/16384/PG_18_202506121".to_owned()),
            "{synced:?}"
        );
        assert!(
            !synced.contains(&"/d/pg_tblspc/16384/elsewhere".to_owned()),
            "{synced:?}"
        );
    }

    #[test]
    fn a_missing_pg_wal_is_reported_and_the_walk_carries_on() {
        // file_utils.c:123: the lstat failure is not fatal.
        let fs = FakeFs::default()
            .dir("/d", &["PG_VERSION"])
            .file("/d/PG_VERSION");
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);
        assert_eq!(
            warnings(&ops),
            vec![
                "initdb: error: could not stat file \"/d/pg_wal\": No such file or directory",
                // :304, from the second visit to a pg_tblspc that is not there.
                "initdb: error: could not open directory \"/d/pg_tblspc\": \
                 No such file or directory",
            ]
        );
        assert_eq!(fsyncs(&ops), vec!["/d/PG_VERSION", "/d"]);
    }

    #[test]
    fn an_unreadable_directory_is_reported_and_not_fsynced() {
        // file_utils.c:304 returns before the trailing action at :348.
        let mut fs = cluster();
        fs.entries.remove(Path::new("/d/global"));
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);
        assert_eq!(
            warnings(&ops),
            vec!["initdb: error: could not open directory \"/d/global\": Not a directory"]
        );
        assert!(!fsyncs(&ops).contains(&"/d/global".to_owned()));
        // The walk still finishes the rest of the tree.
        assert!(fsyncs(&ops).contains(&"/d".to_owned()));
    }

    #[test]
    fn a_readdir_that_gives_up_part_way_is_not_an_unopenable_directory() {
        // file_utils.c:302 and :337 are two different messages at two
        // different places. opendir failing means the directory is skipped
        // whole (tested above); readdir failing part-way means C acts on the
        // names it did read, reports `could not read directory`, and *still*
        // fsyncs the directory at :348.
        let fs = cluster().unreadable_after("/d/global", &["pg_control"], "Stale file handle");
        let ops = plan(Path::new("/d"), SyncMethod::Fsync, true, &fs);

        assert_eq!(
            warnings(&ops),
            vec!["initdb: error: could not read directory \"/d/global\": Stale file handle"]
        );
        // The entry it read is synced, the warning lands after it, and the
        // directory itself is still synced.
        let synced = fsyncs(&ops);
        assert!(
            synced.contains(&"/d/global/pg_control".to_owned()),
            "{synced:?}"
        );
        assert!(synced.contains(&"/d/global".to_owned()), "{synced:?}");

        let global = ops
            .iter()
            .position(|op| matches!(op, SyncOp::Warn(InitdbError::CouldNotReadDirectory { .. })))
            .expect("the readdir warning");
        let entry = ops
            .iter()
            .position(|op| matches!(op, SyncOp::Fsync { path, .. } if path == Path::new("/d/global/pg_control")))
            .expect("the entry it read");
        let dir = ops
            .iter()
            .position(|op| matches!(op, SyncOp::Fsync { path, isdir: true } if path == Path::new("/d/global")))
            .expect("the directory itself");
        assert!(entry < global && global < dir, "{ops:#?}");
    }

    /// The pin for the `--sync-method=syncfs` row in `docs/divergences.md`.
    #[test]
    fn syncfs_plans_the_same_walk_as_fsync() {
        let fs = cluster();
        for sync_data_files in [true, false] {
            assert_eq!(
                plan(Path::new("/d"), SyncMethod::Syncfs, sync_data_files, &fs),
                plan(Path::new("/d"), SyncMethod::Fsync, sync_data_files, &fs),
                "sync_data_files = {sync_data_files}"
            );
        }
    }

    #[test]
    fn the_progress_and_skip_messages_are_upstreams_bytes() {
        // initdb.c:3447 / :3510, :2127 and :3516.
        assert_eq!(SYNCING_PROGRESS, "syncing data to disk ... ");
        assert_eq!(CHECK_OK, "ok\n");
        assert_eq!(
            SYNC_SKIPPED_NOTE,
            "\nSync to disk skipped.\nThe data directory might become corrupt \
             if the operating system crashes.\n"
        );
    }

    #[test]
    fn a_warning_op_reaches_stderr_where_the_walk_put_it() {
        let ops = vec![
            SyncOp::Warn(InitdbError::CouldNotStatFile {
                path: "/d/pg_wal".to_owned(),
                reason: "No such file or directory".to_owned(),
            }),
            SyncOp::Warn(InitdbError::CouldNotOpenDirectory {
                path: "/d/pg_tblspc".to_owned(),
                reason: "No such file or directory".to_owned(),
            }),
        ];
        let mut stderr = Vec::new();
        apply(&ops, &mut stderr).expect("no fatal op");
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "initdb: error: could not stat file \"/d/pg_wal\": No such file or directory\n\
             initdb: error: could not open directory \"/d/pg_tblspc\": \
             No such file or directory\n"
        );
    }
}
