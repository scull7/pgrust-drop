//! The filesystem helpers the stolen TAP tests use.
//!
//! Ports of `slurp_file` (`src/test/perl/PostgreSQL/Test/Utils.pm:512`) and
//! `check_mode_recursive` (`:601`).
//!
//! Data / Calculations / Actions applies here too: [`Entry`] is what one
//! directory entry looks like to the check, [`mode_violations`] and
//! [`is_ignored`] are the whole verdict as pure functions, and [`walk`] /
//! [`check_mode_recursive`] are the only parts that touch a disk.

use std::fmt;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

/// `initdb` leaves PGDATA at these modes; the group-readable pair is what
/// `initdb --allow-group-access` produces (`Utils.pm:601` callers).
pub const PGDATA_DIR_MODE: u32 = 0o700;
/// See [`PGDATA_DIR_MODE`].
pub const PGDATA_FILE_MODE: u32 = 0o600;
/// `--allow-group-access` directory mode.
pub const GROUP_DIR_MODE: u32 = 0o750;
/// `--allow-group-access` file mode.
pub const GROUP_FILE_MODE: u32 = 0o640;

/// What `stat()` said about one entry under the checked directory.
///
/// `mode` is already `S_IMODE`d — the permission bits alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub kind: EntryKind,
    pub mode: u32,
}

impl Entry {
    /// Build an entry by hand, for unit tests of the check.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, kind: EntryKind, mode: u32) -> Self {
        Self {
            path: path.into(),
            kind,
            mode,
        }
    }
}

/// `S_ISDIR` / `S_ISREG` / neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Dir,
    File,
    /// A socket, fifo or device: upstream dies on these ("unknown file type").
    Other,
}

/// One entry whose mode is not what the caller demanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModeViolation {
    /// The entry exists with the wrong permission bits.
    Mode {
        path: PathBuf,
        kind: EntryKind,
        expected: u32,
        found: u32,
    },
    /// Neither a regular file nor a directory: `die "unknown file type for …"`.
    UnknownFileType(PathBuf),
}

impl fmt::Display for ModeViolation {
    /// Worded as upstream words it, so a failure is greppable against the Perl:
    /// `sprintf("$File::Find::name mode must be %04o\n", $expected_…_mode)`
    /// (`Utils.pm:601`), with the mode actually found appended — upstream
    /// prints only the expectation, which makes a failure needlessly hard to
    /// read.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModeViolation::Mode {
                path,
                expected,
                found,
                ..
            } => write!(
                f,
                "{} mode must be {expected:04o}, is {found:04o}",
                path.display()
            ),
            ModeViolation::UnknownFileType(path) => {
                write!(f, "unknown file type for {}", path.display())
            }
        }
    }
}

/// Pure: is `path` in the ignore list?
///
/// Upstream compares `"$dir/$ignore"` against the full name `File::Find`
/// produced, so an ignore entry is a path relative to the checked directory —
/// the pod's "basename only" is true only for entries directly inside it.
#[must_use]
pub fn is_ignored(dir: &Path, path: &Path, ignore_list: &[&str]) -> bool {
    ignore_list
        .iter()
        .any(|ignore| path == dir.join(ignore).as_path())
}

/// Pure: every entry whose mode is wrong.
///
/// `check_mode_recursive` returns a bool upstream and prints each bad entry to
/// stderr; returning the list instead keeps the calculation pure and lets the
/// caller print all of them at once.
#[must_use]
pub fn mode_violations(entries: &[Entry], dir_mode: u32, file_mode: u32) -> Vec<ModeViolation> {
    entries
        .iter()
        .filter_map(|entry| {
            let expected = match entry.kind {
                EntryKind::Dir => dir_mode,
                EntryKind::File => file_mode,
                EntryKind::Other => {
                    return Some(ModeViolation::UnknownFileType(entry.path.clone()));
                }
            };
            (entry.mode != expected).then(|| ModeViolation::Mode {
                path: entry.path.clone(),
                kind: entry.kind,
                expected,
                found: entry.mode,
            })
        })
        .collect()
}

/// `slurp_file(filename [, $offset])` (`Utils.pm:512`): the whole file, from
/// `offset` when one is given.
///
/// Bytes, not a `String`: the server writes its log in the server encoding and
/// a stolen test may slurp a `pg_wal` segment.
///
/// # Errors
/// The `io::Error` from opening, seeking or reading.
pub fn slurp_file(filename: &Path, offset: Option<u64>) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(filename)?;
    if let Some(offset) = offset {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    Ok(contents)
}

#[cfg(unix)]
mod unix {
    use std::collections::HashSet;
    use std::io::Write as _;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    use super::{Entry, EntryKind, ModeViolation, is_ignored, mode_violations};

    /// Action: `stat` every entry under `dir`, `dir` itself included.
    ///
    /// Symlinks are followed, as `File::Find`'s `follow_fast => 1` does, and
    /// the `(device, inode)` pairs already seen are remembered so a symlink
    /// loop ends the walk instead of hanging it — that memo is what
    /// `follow_fast` buys upstream.
    ///
    /// An ignored entry is left out of the result but still descended into:
    /// upstream's `wanted` merely `return`s for it and never sets
    /// `$File::Find::prune`, so `File::Find` walks on into an ignored
    /// directory and every file below it is still checked. Pruning here would
    /// silently stop checking a whole subtree.
    ///
    /// `ENOENT` is allowed and skipped, for the `stat` and for the directory
    /// read alike: "a running server can delete files, such as those in
    /// `pg_stat`" (`Utils.pm:621`), and a directory can vanish between the two
    /// calls just as a file can. Every other failure is returned, where
    /// upstream dies.
    ///
    /// # Errors
    /// Any `io::Error` other than `NotFound` from reading a directory or
    /// stat-ing an entry.
    pub fn walk(dir: &Path, ignore_list: &[&str]) -> std::io::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(path) = queue.pop() {
            // `stat`, not `lstat`: upstream stats through the symlink.
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    warn(&format!("unable to stat {}: {err}", path.display()));
                    continue;
                }
                Err(err) => return Err(err),
            };
            if !seen.insert((metadata.dev(), metadata.ino())) {
                continue;
            }
            let kind = if metadata.is_dir() {
                EntryKind::Dir
            } else if metadata.is_file() {
                EntryKind::File
            } else {
                EntryKind::Other
            };
            // Descend first, so an ignored directory still yields its children.
            if kind == EntryKind::Dir {
                match std::fs::read_dir(&path) {
                    Ok(children) => {
                        for child in children {
                            queue.push(child?.path());
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        warn(&format!("unable to read {}: {err}", path.display()));
                    }
                    Err(err) => return Err(err),
                }
            }
            if !is_ignored(dir, &path, ignore_list) {
                entries.push(Entry::new(path, kind, metadata.mode() & 0o7777));
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// `check_mode_recursive(dir, expected_dir_mode, expected_file_mode,
    /// ignore_list)` (`Utils.pm:601`): every directory under `dir` must be
    /// `dir_mode` and every file `file_mode`.
    ///
    /// Upstream returns a bool; the violations are returned instead so the
    /// caller can print all of them. An empty list is upstream's `1`.
    ///
    /// # Errors
    /// See [`walk`].
    pub fn check_mode_recursive(
        dir: &Path,
        dir_mode: u32,
        file_mode: u32,
        ignore_list: &[&str],
    ) -> std::io::Result<Vec<ModeViolation>> {
        Ok(mode_violations(
            &walk(dir, ignore_list)?,
            dir_mode,
            file_mode,
        ))
    }

    /// `ok(check_mode_recursive(...), "check PGDATA permissions")`.
    ///
    /// # Panics
    /// When the directory cannot be walked, or any mode is wrong.
    pub fn check_mode_recursive_ok(
        dir: &Path,
        dir_mode: u32,
        file_mode: u32,
        ignore_list: &[&str],
    ) {
        let violations = check_mode_recursive(dir, dir_mode, file_mode, ignore_list)
            .unwrap_or_else(|err| panic!("could not walk {}: {err}", dir.display()));
        assert!(
            violations.is_empty(),
            "{}:\n{}",
            dir.display(),
            violations
                .iter()
                .map(|violation| format!("  - {violation}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    /// Upstream `warn`s about a vanished file and carries on; go around
    /// libtest's capture so the note survives a passing run, for the same
    /// reason [`crate::reference::announce_skip`] does.
    fn warn(message: &str) {
        let _ = writeln!(std::io::stderr().lock(), "warning: {message}");
    }
}

#[cfg(unix)]
pub use unix::{check_mode_recursive, check_mode_recursive_ok, walk};

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(path: &str, mode: u32) -> Entry {
        Entry::new(path, EntryKind::Dir, mode)
    }

    fn file(path: &str, mode: u32) -> Entry {
        Entry::new(path, EntryKind::File, mode)
    }

    #[test]
    fn default_pgdata_modes_are_accepted() {
        let entries = [
            dir("/data", PGDATA_DIR_MODE),
            dir("/data/base", PGDATA_DIR_MODE),
            file("/data/PG_VERSION", PGDATA_FILE_MODE),
        ];
        assert!(mode_violations(&entries, PGDATA_DIR_MODE, PGDATA_FILE_MODE).is_empty());
    }

    #[test]
    fn group_access_modes_are_accepted_by_the_group_pair() {
        let entries = [
            dir("/data", GROUP_DIR_MODE),
            file("/data/f", GROUP_FILE_MODE),
        ];
        assert!(mode_violations(&entries, GROUP_DIR_MODE, GROUP_FILE_MODE).is_empty());
        // …and rejected by the default pair, which is the whole point of the
        // assertion upstream makes twice with different expectations.
        assert_eq!(
            mode_violations(&entries, PGDATA_DIR_MODE, PGDATA_FILE_MODE).len(),
            2
        );
    }

    #[test]
    fn a_world_readable_file_is_reported() {
        let entries = [file("/data/postgresql.conf", 0o644)];
        assert_eq!(
            mode_violations(&entries, PGDATA_DIR_MODE, PGDATA_FILE_MODE),
            vec![ModeViolation::Mode {
                path: PathBuf::from("/data/postgresql.conf"),
                kind: EntryKind::File,
                expected: PGDATA_FILE_MODE,
                found: 0o644,
            }]
        );
    }

    #[test]
    fn a_directory_is_judged_by_the_directory_mode() {
        // 0600 on a directory is wrong even though it is the file mode.
        let entries = [dir("/data/base", PGDATA_FILE_MODE)];
        let violations = mode_violations(&entries, PGDATA_DIR_MODE, PGDATA_FILE_MODE);
        assert_eq!(violations.len(), 1);
        assert_eq!(
            violations[0].to_string(),
            "/data/base mode must be 0700, is 0600"
        );
    }

    #[test]
    fn an_unknown_file_type_is_a_violation() {
        let entries = [Entry::new("/data/.s.PGSQL.5432", EntryKind::Other, 0o777)];
        assert_eq!(
            mode_violations(&entries, PGDATA_DIR_MODE, PGDATA_FILE_MODE),
            vec![ModeViolation::UnknownFileType(PathBuf::from(
                "/data/.s.PGSQL.5432"
            ))]
        );
    }

    #[test]
    fn the_ignore_list_is_relative_to_the_checked_directory() {
        let dir = Path::new("/data");
        assert!(is_ignored(dir, Path::new("/data/pg_wal"), &["pg_wal"]));
        assert!(is_ignored(
            dir,
            Path::new("/data/pg_stat/global.stat"),
            &["pg_stat/global.stat"]
        ));
        assert!(!is_ignored(dir, Path::new("/data/base"), &["pg_wal"]));
        // A bare basename deeper in the tree is not ignored, matching the
        // upstream string comparison against "$dir/$ignore".
        assert!(!is_ignored(
            dir,
            Path::new("/data/base/pg_wal"),
            &["pg_wal"]
        ));
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    /// A scratch directory that removes itself, so the walk runs against a real
    /// tree without a temp-file dependency.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "testkit-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create scratch dir");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    #[test]
    fn a_correct_tree_walks_clean_and_a_bad_mode_is_found() {
        let scratch = Scratch::new("modes");
        let data = scratch.0.join("data");
        fs::create_dir(&data).expect("mkdir data");
        fs::create_dir(data.join("base")).expect("mkdir base");
        fs::write(data.join("PG_VERSION"), "18\n").expect("write PG_VERSION");
        chmod(&data.join("PG_VERSION"), PGDATA_FILE_MODE);
        chmod(&data.join("base"), PGDATA_DIR_MODE);
        chmod(&data, PGDATA_DIR_MODE);

        let clean = check_mode_recursive(&data, PGDATA_DIR_MODE, PGDATA_FILE_MODE, &[])
            .expect("walk the tree");
        assert!(clean.is_empty(), "{clean:?}");

        // One file loosened: exactly one violation, naming that file.
        let loose = data.join("postgresql.conf");
        fs::write(&loose, "# empty\n").expect("write conf");
        chmod(&loose, 0o644);
        let violations = check_mode_recursive(&data, PGDATA_DIR_MODE, PGDATA_FILE_MODE, &[])
            .expect("walk the tree");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].to_string().contains("postgresql.conf"));

        // …and ignoring it brings the tree back to clean.
        let ignored = check_mode_recursive(
            &data,
            PGDATA_DIR_MODE,
            PGDATA_FILE_MODE,
            &["postgresql.conf"],
        )
        .expect("walk the tree");
        assert!(ignored.is_empty(), "{ignored:?}");
    }

    #[test]
    fn an_ignored_directory_is_still_descended_into() {
        // File::Find's wanted returns for an ignored name but does not prune,
        // so upstream still reports "$dir/pg_wal/badfile mode must be 0600".
        // Pruning here would quietly stop checking a whole subtree.
        let scratch = Scratch::new("ignore-dir");
        let data = scratch.0.join("data");
        let wal = data.join("pg_wal");
        fs::create_dir_all(&wal).expect("mkdir pg_wal");
        let bad = wal.join("badfile");
        fs::write(&bad, "").expect("write badfile");
        chmod(&bad, 0o644);
        chmod(&wal, PGDATA_DIR_MODE);
        chmod(&data, PGDATA_DIR_MODE);

        let violations =
            check_mode_recursive(&data, PGDATA_DIR_MODE, PGDATA_FILE_MODE, &["pg_wal"])
                .expect("walk the tree");
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0]
                .to_string()
                .ends_with("badfile mode must be 0600, is 0644"),
            "{}",
            violations[0]
        );
        // The ignored directory itself is still left out of the result.
        let walked = walk(&data, &["pg_wal"]).expect("walk the tree");
        assert!(
            walked.iter().all(|entry| entry.path != wal),
            "the ignored directory must not be checked itself: {walked:?}"
        );
        assert!(walked.iter().any(|entry| entry.path == bad));
    }

    #[test]
    fn a_vanished_entry_is_warned_about_not_fatal() {
        // The ENOENT the doc comment cites: a dangling symlink stats as
        // NotFound, and upstream warns and carries on rather than dying.
        let scratch = Scratch::new("enoent");
        let data = scratch.0.join("data");
        fs::create_dir(&data).expect("mkdir data");
        chmod(&data, PGDATA_DIR_MODE);
        std::os::unix::fs::symlink(data.join("gone"), data.join("dangling")).expect("symlink");
        let entries = walk(&data, &[]).expect("a dangling symlink is not an error");
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].path, data);
    }

    #[test]
    fn a_symlink_loop_ends_the_walk() {
        let scratch = Scratch::new("loop");
        let data = scratch.0.join("data");
        fs::create_dir(&data).expect("mkdir data");
        chmod(&data, PGDATA_DIR_MODE);
        std::os::unix::fs::symlink(&data, data.join("self")).expect("symlink");
        // Without the (dev, inode) memo this never returns.
        let entries = walk(&data, &[]).expect("walk the tree");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, EntryKind::Dir);
    }

    #[test]
    fn slurp_file_reads_all_of_it_and_from_an_offset() {
        let scratch = Scratch::new("slurp");
        let path = scratch.0.join("server.log");
        fs::write(&path, b"LOG:  first\nLOG:  second\n").expect("write log");
        assert_eq!(
            slurp_file(&path, None).expect("slurp"),
            b"LOG:  first\nLOG:  second\n"
        );
        assert_eq!(
            slurp_file(&path, Some(12)).expect("slurp from offset"),
            b"LOG:  second\n"
        );
        // Past the end is empty, not an error: a test that slurps after a
        // truncation must see "nothing new", exactly as Perl's seek does.
        assert!(
            slurp_file(&path, Some(9_000))
                .expect("slurp past end")
                .is_empty()
        );
    }

    #[test]
    fn slurp_file_reports_a_missing_file() {
        let err = slurp_file(Path::new("/nonexistent/server.log"), None)
            .expect_err("missing file is an error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
