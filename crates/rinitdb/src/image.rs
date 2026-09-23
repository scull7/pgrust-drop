//! The template cluster image: a C-`initdb`-minted data directory, stripped,
//! packed into one byte string, and expanded again into a new cluster.
//!
//! This is the bridge ADR-0002 builds cluster creation on while pgrust has no
//! `postgres --boot`: C `initdb` runs once, offline, and every later cluster
//! starts as a copy of its catalogs. This module is the format and the two
//! ends of it — [`read_tree`] and [`strip`] on the mint side, [`pack`] in the
//! middle, [`parse`] and [`expand`] on the run side. Where the minted image
//! lives and how it is embedded are separate steps (NAT-381 follow-ups).
//!
//! Data / Calculations / Actions: [`ImagePath`], [`Entry`] and [`Node`] are
//! the data; [`strip`], [`pack`] and [`parse`] are pure; [`read_tree`] and
//! [`expand`] are the only functions that touch a disk.
//!
//! ## The format, version 1
//!
//! Stdlib only, uncompressed, little-endian:
//!
//! ```text
//! magic    8 bytes   "RINITDB" 0x01   (the last byte is the format version)
//! count    u32       number of entries
//! entry *  kind u8   0 = directory, 1 = regular file
//!          len  u16  path length, then that many bytes of UTF-8 path
//!          size u64  file entries only, then that many bytes of contents
//! crc      u32       CRC-32C over every byte before it
//! ```
//!
//! Entries are in canonical order — by path component, so a directory always
//! precedes what is in it — with no duplicates, and every entry's parent is
//! an earlier directory entry (or the root). [`pack`] establishes that order
//! and [`parse`] refuses anything else, so a parsed image can be expanded
//! front to back without looking ahead.
//!
//! The image carries no modes and no owners. Those belong to the cluster
//! being created, not the one that was minted: `-g` decides them
//! (`crate::file_perm::DataDirPerm`), exactly as it decides them for the
//! entries `crate::layout` makes.
//!
//! An uncompressed image of a PostgreSQL 18.6 `--no-locale --encoding=UTF8`
//! cluster is about 24 MB, inside the issue's 50 MB budget. The format
//! version byte is there so a compressed encoding can follow without guessing.

use std::cmp::Ordering;
use std::fmt;

use crate::crc32c::crc32c;

/// `"RINITDB"` followed by the format version.
pub const MAGIC: [u8; 8] = *b"RINITDB\x01";

/// The C `initdb` options a template is minted with, after `-D <dir>`
/// (NAT-381, ADR-0002): C locale and UTF-8, so the image bakes no
/// libc-versioned collation into the default databases, and `--no-sync`
/// because the mint directory is packed and thrown away.
///
/// One thing the recipe does not control: `initdb` always runs
/// `SELECT pg_import_system_collations('pg_catalog')` (`setup_collation`,
/// `initdb.c:1781`), so `pg_collation` in the image holds the mint host's
/// system locales as rows. ADR-0002's run-time stamping has to account for
/// them.
pub const MINT_ARGS: [&str; 7] = [
    "--no-locale",
    "--encoding=UTF8",
    "-U",
    "postgres",
    "-A",
    "trust",
    "--no-sync",
];

/// [`MAGIC`] + the entry count.
const HEADER_LEN: usize = MAGIC.len() + 4;
/// The CRC-32C trailer.
const TRAILER_LEN: usize = 4;

const KIND_DIR: u8 = 0;
const KIND_FILE: u8 = 1;

/// A path inside the image: relative, `/`-separated, and unable to leave the
/// directory it is expanded into.
///
/// Every component is non-empty, neither `.` nor `..`, and free of NUL. The
/// check is in the constructor, so [`expand`] can join an `ImagePath` onto a
/// target directory without re-validating it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImagePath(String);

impl ImagePath {
    /// The path, checked.
    ///
    /// # Errors
    /// [`ImageError::BadPath`] when `path` is empty, absolute, or has an
    /// empty, `.`, `..` or NUL-bearing component.
    pub fn new(path: &str) -> Result<Self, ImageError> {
        let bad = || ImageError::BadPath {
            path: path.to_owned(),
        };
        if path.is_empty() || path.len() > usize::from(u16::MAX) {
            return Err(bad());
        }
        for component in path.split('/') {
            if component.is_empty()
                || component == "."
                || component == ".."
                || component.contains('\0')
            {
                return Err(bad());
            }
        }
        Ok(Self(path.to_owned()))
    }

    /// The path as it is stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Its components, in order.
    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// The directory that holds this entry, or `None` for a top-level entry.
    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.0.rsplit_once('/').map(|(parent, _)| parent)
    }
}

impl fmt::Display for ImagePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Canonical order: component by component, so `base` < `base/1` <
/// `base.x`. Plain byte order would put `base.x` between `base` and
/// `base/1`, because `.` sorts before `/`, and a directory's children would
/// no longer follow it directly.
impl Ord for ImagePath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.components().cmp(other.components())
    }
}

impl PartialOrd for ImagePath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What an entry is. `B` holds a file's bytes: borrowed from the image after
/// [`parse`], owned after [`read_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node<B> {
    Dir,
    File(B),
}

/// One directory or regular file in the image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<B> {
    pub path: ImagePath,
    pub node: Node<B>,
}

impl<B> Entry<B> {
    /// A directory entry.
    #[must_use]
    pub fn dir(path: ImagePath) -> Self {
        Self {
            path,
            node: Node::Dir,
        }
    }

    /// A regular-file entry.
    #[must_use]
    pub fn file(path: ImagePath, contents: B) -> Self {
        Self {
            path,
            node: Node::File(contents),
        }
    }
}

/// Why bytes are not an image, or entries cannot become one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageError {
    #[error("not a template image (bad magic)")]
    BadMagic,
    #[error("template image is truncated at byte {at}")]
    Truncated { at: usize },
    #[error("template image checksum mismatch: stored {stored:08x}, computed {computed:08x}")]
    ChecksumMismatch { stored: u32, computed: u32 },
    #[error("template image has {extra} bytes after its last entry")]
    TrailingBytes { extra: usize },
    #[error("template image entry at byte {at} has unknown kind {kind}")]
    BadKind { at: usize, kind: u8 },
    #[error("template image path is not valid UTF-8 at byte {at}")]
    PathNotUtf8 { at: usize },
    #[error("invalid template image path \"{path}\"")]
    BadPath { path: String },
    #[error("template image entry \"{path}\" is out of order or duplicated")]
    OutOfOrder { path: String },
    #[error("template image entry \"{path}\" has no directory entry for its parent")]
    Orphan { path: String },
    #[error("template image has too many entries")]
    TooManyEntries,
}

/// What [`strip`] drops from a minted cluster, and why.
///
/// These are the per-cluster and volatile files NAT-381 names — each is
/// regenerated by a later step of cluster creation, so a copy from the mint
/// host would be wrong or would collide:
///
/// - `postgresql.conf`, `pg_hba.conf`, `pg_ident.conf` are rendered from the
///   vendored samples (`crate::conf`); `postgresql.auto.conf` is written
///   fresh, as `initdb.c` writes it.
/// - `postmaster.opts` records the mint host's server command line.
/// - `global/pg_control` carries the mint's system identifier and timestamps;
///   a fresh one is written (`crate::control`).
/// - `PG_VERSION` at the top level is written by `crate::layout`
///   (`write_version_file(NULL)`, `initdb.c:3087`); the per-database ones
///   under `base/` are not, so they stay.
/// - `pg_stat/pgstat.stat` is the mint server's cumulative statistics as of
///   its shutdown; a server without the file starts from zero, which is what
///   a new cluster should do.
/// - every regular file under `pg_wal/`: the first segment is regenerated to
///   match the fresh `pg_control`. The directories themselves stay.
pub const STRIPPED_FILES: [&str; 8] = [
    "PG_VERSION",
    "postgresql.conf",
    "pg_hba.conf",
    "pg_ident.conf",
    "postgresql.auto.conf",
    "postmaster.opts",
    "global/pg_control",
    "pg_stat/pgstat.stat",
];

/// Pure: whether [`strip`] keeps `entry`.
#[must_use]
pub fn keeps<B>(entry: &Entry<B>) -> bool {
    match entry.node {
        Node::Dir => true,
        Node::File(_) => {
            let path = entry.path.as_str();
            !STRIPPED_FILES.contains(&path) && entry.path.components().next() != Some("pg_wal")
        }
    }
}

/// Pure: `entries` without the per-cluster and volatile files
/// ([`STRIPPED_FILES`], `pg_wal/*`).
#[must_use]
pub fn strip<B>(entries: Vec<Entry<B>>) -> Vec<Entry<B>> {
    entries.into_iter().filter(keeps).collect()
}

/// Pure: `entries` in canonical order, as one image.
///
/// The order they arrive in does not matter; the image does not depend on
/// directory iteration order, so two mints of the same tree pack to the same
/// bytes.
///
/// # Errors
/// [`ImageError::OutOfOrder`] for a duplicated path, [`ImageError::Orphan`]
/// for an entry whose parent is not a directory entry, and
/// [`ImageError::TooManyEntries`] past `u32::MAX` entries.
pub fn pack<B: AsRef<[u8]>>(entries: &[Entry<B>]) -> Result<Vec<u8>, ImageError> {
    let mut sorted: Vec<&Entry<B>> = entries.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    check_order(sorted.iter().map(|entry| (&entry.path, &entry.node)))?;

    let count = u32::try_from(sorted.len()).map_err(|_| ImageError::TooManyEntries)?;
    let size = sorted.iter().fold(HEADER_LEN + TRAILER_LEN, |size, entry| {
        size + 3
            + entry.path.as_str().len()
            + match &entry.node {
                Node::Dir => 0,
                Node::File(contents) => 8 + contents.as_ref().len(),
            }
    });
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&count.to_le_bytes());
    for entry in sorted {
        let path = entry.path.as_str().as_bytes();
        // `ImagePath::new` bounds the length; this cannot fail.
        let path_len = u16::try_from(path.len()).map_err(|_| ImageError::BadPath {
            path: entry.path.to_string(),
        })?;
        match &entry.node {
            Node::Dir => {
                out.push(KIND_DIR);
                out.extend_from_slice(&path_len.to_le_bytes());
                out.extend_from_slice(path);
            }
            Node::File(contents) => {
                let contents = contents.as_ref();
                out.push(KIND_FILE);
                out.extend_from_slice(&path_len.to_le_bytes());
                out.extend_from_slice(path);
                out.extend_from_slice(&(contents.len() as u64).to_le_bytes());
                out.extend_from_slice(contents);
            }
        }
    }
    let crc = crc32c(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    Ok(out)
}

/// Pure: the entries `image` holds, borrowing each file's bytes from it.
///
/// Checks the magic, the checksum, every length, every path and the
/// canonical order; an image that passes can be expanded front to back.
///
/// # Errors
/// The first [`ImageError`] found.
pub fn parse(image: &[u8]) -> Result<Vec<Entry<&[u8]>>, ImageError> {
    if image.len() < MAGIC.len() || image[..MAGIC.len()] != MAGIC {
        return Err(ImageError::BadMagic);
    }
    if image.len() < HEADER_LEN + TRAILER_LEN {
        return Err(ImageError::Truncated { at: image.len() });
    }
    let Some((body, trailer)) = image.split_last_chunk::<TRAILER_LEN>() else {
        return Err(ImageError::Truncated { at: image.len() });
    };
    let stored = u32::from_le_bytes(*trailer);
    let computed = crc32c(body);
    if stored != computed {
        return Err(ImageError::ChecksumMismatch { stored, computed });
    }

    let mut cursor = Cursor {
        bytes: body,
        at: MAGIC.len(),
    };
    let count = u32::from_le_bytes(cursor.array()?);
    let mut entries = Vec::new();
    for _ in 0..count {
        let start = cursor.at;
        let [kind] = cursor.array()?;
        let path_len = usize::from(u16::from_le_bytes(cursor.array()?));
        let path_at = cursor.at;
        let path = std::str::from_utf8(cursor.take(path_len)?)
            .map_err(|_| ImageError::PathNotUtf8 { at: path_at })?;
        let path = ImagePath::new(path)?;
        let node = match kind {
            KIND_DIR => Node::Dir,
            KIND_FILE => {
                let size = u64::from_le_bytes(cursor.array()?);
                let size = usize::try_from(size).map_err(|_| ImageError::Truncated {
                    at: cursor.bytes.len(),
                })?;
                Node::File(cursor.take(size)?)
            }
            kind => return Err(ImageError::BadKind { at: start, kind }),
        };
        entries.push(Entry { path, node });
    }
    if cursor.at != body.len() {
        return Err(ImageError::TrailingBytes {
            extra: body.len() - cursor.at,
        });
    }
    check_order(entries.iter().map(|entry| (&entry.path, &entry.node)))?;
    Ok(entries)
}

/// Pure: entries are strictly increasing in canonical order and every
/// parent is an earlier directory entry.
///
/// In canonical order a directory's descendants follow it contiguously, so
/// the open directories form a stack: an entry's parent, if it is anywhere,
/// is on top once every directory that is not an ancestor has been popped.
fn check_order<'a, B: 'a>(
    entries: impl Iterator<Item = (&'a ImagePath, &'a Node<B>)>,
) -> Result<(), ImageError> {
    let mut previous: Option<&ImagePath> = None;
    let mut open_dirs: Vec<&ImagePath> = Vec::new();
    for (path, node) in entries {
        if previous.is_some_and(|previous| previous >= path) {
            return Err(ImageError::OutOfOrder {
                path: path.to_string(),
            });
        }
        previous = Some(path);
        let parent = path.parent();
        while let Some(top) = open_dirs.last() {
            if parent.is_some_and(|parent| is_same_or_ancestor(top.as_str(), parent)) {
                break;
            }
            open_dirs.pop();
        }
        if let Some(parent) = parent
            && open_dirs.last().map(|top| top.as_str()) != Some(parent)
        {
            return Err(ImageError::Orphan {
                path: path.to_string(),
            });
        }
        if matches!(node, Node::Dir) {
            open_dirs.push(path);
        }
    }
    Ok(())
}

/// Whether `dir` is `path` or one of its ancestors, component-wise.
fn is_same_or_ancestor(dir: &str, path: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// A bounds-checked reader over the image body.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], ImageError> {
        let end = self
            .at
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(ImageError::Truncated {
                at: self.bytes.len(),
            })?;
        let taken = &self.bytes[self.at..end];
        self.at = end;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ImageError> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }
}

/// What [`expand`] wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expanded {
    /// Directories created (not counting ones that were already there).
    pub dirs: usize,
    /// Regular files written.
    pub files: usize,
    /// Bytes of file contents written.
    pub bytes: u64,
}

#[cfg(unix)]
mod unix {
    use std::fs::{DirBuilder, OpenOptions, Permissions};
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    use super::{Entry, Expanded, ImagePath, Node};
    use crate::error::InitdbError;
    use crate::file_perm::DataDirPerm;
    use crate::strerror::strerror;

    /// Action: every directory and regular file under `root`, as entries
    /// (the mint side).
    ///
    /// # Errors
    /// Any I/O error, or `InvalidData` for an entry that is neither a
    /// directory nor a regular file (a symlink, a socket …) or whose name is
    /// not UTF-8: a freshly minted cluster has none, so one is a sign the
    /// wrong directory was read.
    pub fn read_tree(root: &Path) -> std::io::Result<Vec<Entry<Vec<u8>>>> {
        let mut entries = Vec::new();
        walk(root, "", &mut entries)?;
        Ok(entries)
    }

    fn walk(dir: &Path, prefix: &str, entries: &mut Vec<Entry<Vec<u8>>>) -> std::io::Result<()> {
        let invalid = |what: String| std::io::Error::new(std::io::ErrorKind::InvalidData, what);
        for dirent in std::fs::read_dir(dir)? {
            let dirent = dirent?;
            let name = dirent.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| invalid(format!("non-UTF-8 name in {}", dir.display())))?;
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let path = ImagePath::new(&relative).map_err(|err| invalid(err.to_string()))?;
            let kind = dirent.file_type()?;
            if kind.is_dir() {
                entries.push(Entry::dir(path));
                walk(&dirent.path(), &relative, entries)?;
            } else if kind.is_file() {
                entries.push(Entry::file(path, std::fs::read(dirent.path())?));
            } else {
                return Err(invalid(format!(
                    "\"{}\" is neither a directory nor a regular file",
                    dirent.path().display()
                )));
            }
        }
        Ok(())
    }

    /// Action: write `entries` (from [`super::parse`]) under `target`, which
    /// must exist.
    ///
    /// Directories get `perm`'s directory mode and files its file mode, the
    /// modes `crate::layout` gives its own entries. A directory that is
    /// already there — `crate::layout` makes `global`, `base`, `pg_wal` and
    /// the rest before the image is expanded — is left as it is, the way
    /// `pg_mkdir_p` forgives `EEXIST` (`src/port/pgmkdirp.c:124`); `pg_wal`
    /// may be `--waldir`'s symlink, and a symlink to a directory counts. A
    /// file that is already there is an error: nothing before the expansion
    /// writes one the image also holds.
    ///
    /// # Errors
    /// The first failure, as the `InitdbError` for a failed `mkdir`, `open`
    /// or `write`.
    pub fn expand(
        entries: &[Entry<&[u8]>],
        target: &Path,
        perm: DataDirPerm,
    ) -> Result<Expanded, InitdbError> {
        let mut expanded = Expanded::default();
        for entry in entries {
            let path = join(target, &entry.path);
            match entry.node {
                Node::Dir => {
                    if create_dir(&path, perm.masked_dir_mode())? {
                        expanded.dirs += 1;
                    }
                }
                Node::File(contents) => {
                    write_file(&path, perm.masked_file_mode(), contents)?;
                    expanded.files += 1;
                    expanded.bytes += contents.len() as u64;
                }
            }
        }
        Ok(expanded)
    }

    /// `target` joined with every component of `path`. `ImagePath`'s
    /// invariant is what keeps the result under `target`.
    fn join(target: &Path, path: &ImagePath) -> PathBuf {
        let mut joined = target.to_path_buf();
        joined.extend(path.components());
        joined
    }

    /// `mkdir(path, mode)`, forgiving a directory that is already there.
    /// Returns whether it created one.
    fn create_dir(path: &Path, mode: u32) -> Result<bool, InitdbError> {
        let fail = |err: &std::io::Error| InitdbError::CouldNotCreateDirectory {
            path: path.display().to_string(),
            reason: strerror(err),
        };
        match DirBuilder::new().mode(mode).create(path) {
            Ok(()) => {
                std::fs::set_permissions(path, Permissions::from_mode(mode))
                    .map_err(|err| fail(&err))?;
                Ok(true)
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
                Ok(false)
            }
            Err(err) => Err(fail(&err)),
        }
    }

    /// Create `path` (it must not exist) at `mode` and write `contents`.
    fn write_file(path: &Path, mode: u32, contents: &[u8]) -> Result<(), InitdbError> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .map_err(|err| InitdbError::CouldNotOpenFileForWriting {
                path: path.display().to_string(),
                reason: strerror(&err),
            })?;
        file.write_all(contents)
            .and_then(|()| std::fs::set_permissions(path, Permissions::from_mode(mode)))
            .map_err(|err| InitdbError::CouldNotWriteFile {
                path: path.display().to_string(),
                reason: strerror(&err),
            })
    }
}

#[cfg(unix)]
pub use unix::{expand, read_tree};

#[cfg(test)]
mod tests {
    use super::*;

    fn path(p: &str) -> ImagePath {
        ImagePath::new(p).unwrap()
    }

    fn sample() -> Vec<Entry<Vec<u8>>> {
        vec![
            Entry::file(path("global/1262"), vec![1, 2, 3]),
            Entry::dir(path("base")),
            Entry::file(path("base/1/PG_VERSION"), b"18\n".to_vec()),
            Entry::dir(path("global")),
            Entry::dir(path("base/1")),
            Entry::file(path("base/1/1259"), vec![0; 8192]),
            Entry::file(path("empty"), Vec::new()),
        ]
    }

    #[test]
    fn image_paths_cannot_leave_the_target() {
        for bad in [
            "", "/etc", "a//b", "a/", "./a", "a/.", "..", "a/../b", "a\0b",
        ] {
            assert_eq!(
                ImagePath::new(bad),
                Err(ImageError::BadPath {
                    path: bad.to_owned()
                }),
                "{bad:?}"
            );
        }
        for good in [
            "a",
            "base/1/1259",
            "pg_wal/archive_status",
            ".hidden",
            "a..b",
        ] {
            assert_eq!(ImagePath::new(good).unwrap().as_str(), good);
        }
    }

    #[test]
    fn canonical_order_puts_a_directory_right_before_its_contents() {
        let mut paths = [path("base.x"), path("base/1"), path("base"), path("a")];
        paths.sort();
        let sorted: Vec<&str> = paths.iter().map(ImagePath::as_str).collect();
        assert_eq!(sorted, ["a", "base", "base/1", "base.x"]);
    }

    #[test]
    fn pack_then_parse_is_the_identity_in_canonical_order() {
        let entries = sample();
        let image = pack(&entries).unwrap();
        let parsed = parse(&image).unwrap();
        let mut expected: Vec<Entry<&[u8]>> = entries
            .iter()
            .map(|entry| Entry {
                path: entry.path.clone(),
                node: match &entry.node {
                    Node::Dir => Node::Dir,
                    Node::File(contents) => Node::File(contents.as_slice()),
                },
            })
            .collect();
        expected.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(parsed, expected);
    }

    #[test]
    fn pack_does_not_depend_on_input_order() {
        let entries = sample();
        let mut reversed = sample();
        reversed.reverse();
        assert_eq!(pack(&entries).unwrap(), pack(&reversed).unwrap());
    }

    #[test]
    fn the_layout_is_the_documented_one() {
        let image = pack(&[
            Entry::dir(path("d")),
            Entry::file(path("d/f"), b"xy".as_slice()),
        ])
        .unwrap();
        let mut expected = b"RINITDB\x01".to_vec();
        expected.extend_from_slice(&2u32.to_le_bytes());
        expected.extend_from_slice(&[0, 1, 0, b'd']);
        expected.extend_from_slice(&[1, 3, 0, b'd', b'/', b'f']);
        expected.extend_from_slice(&2u64.to_le_bytes());
        expected.extend_from_slice(b"xy");
        let crc = crc32c(&expected);
        expected.extend_from_slice(&crc.to_le_bytes());
        assert_eq!(image, expected);
    }

    #[test]
    fn pack_refuses_duplicates_and_orphans() {
        let dup = [Entry::dir(path("a")), Entry::dir(path("a"))];
        assert_eq!(
            pack::<&[u8]>(&dup),
            Err(ImageError::OutOfOrder {
                path: "a".to_owned()
            })
        );
        let orphan = [Entry::file(path("a/b"), b"".as_slice())];
        assert_eq!(
            pack(&orphan),
            Err(ImageError::Orphan {
                path: "a/b".to_owned()
            })
        );
        // A file cannot be a parent.
        let file_parent = [
            Entry::file(path("a"), b"".as_slice()),
            Entry::file(path("a/b"), b"".as_slice()),
        ];
        assert_eq!(
            pack(&file_parent),
            Err(ImageError::Orphan {
                path: "a/b".to_owned()
            })
        );
        // A sibling's subtree does not stand in for a missing parent.
        let cousin = [
            Entry::dir(path("a")),
            Entry::dir(path("a/b")),
            Entry::file(path("a/c/d"), b"".as_slice()),
        ];
        assert_eq!(
            pack(&cousin),
            Err(ImageError::Orphan {
                path: "a/c/d".to_owned()
            })
        );
    }

    #[test]
    fn parse_refuses_a_damaged_image() {
        let image = pack(&sample()).unwrap();

        assert_eq!(parse(b""), Err(ImageError::BadMagic));
        let mut magic = image.clone();
        magic[7] = 2;
        assert_eq!(parse(&magic), Err(ImageError::BadMagic));

        let mut flipped = image.clone();
        flipped[HEADER_LEN + 5] ^= 0xFF;
        assert!(matches!(
            parse(&flipped),
            Err(ImageError::ChecksumMismatch { .. })
        ));

        assert!(matches!(
            parse(&image[..image.len() - 1]),
            Err(ImageError::ChecksumMismatch { .. })
        ));
    }

    /// Re-seal `body` with a valid checksum, to reach the checks behind it.
    fn sealed(body: &[u8]) -> Vec<u8> {
        let mut image = body.to_vec();
        let crc = crc32c(&image);
        image.extend_from_slice(&crc.to_le_bytes());
        image
    }

    #[test]
    fn parse_checks_the_structure_under_a_valid_checksum() {
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&1u32.to_le_bytes());

        // Count says one entry, none follows.
        assert_eq!(
            parse(&sealed(&header)),
            Err(ImageError::Truncated { at: HEADER_LEN })
        );

        let mut bad_kind = header.clone();
        bad_kind.extend_from_slice(&[7, 1, 0, b'a']);
        assert_eq!(
            parse(&sealed(&bad_kind)),
            Err(ImageError::BadKind {
                at: HEADER_LEN,
                kind: 7
            })
        );

        let mut long_file = header.clone();
        long_file.extend_from_slice(&[1, 1, 0, b'a']);
        long_file.extend_from_slice(&100u64.to_le_bytes());
        assert!(matches!(
            parse(&sealed(&long_file)),
            Err(ImageError::Truncated { .. })
        ));

        let mut escape = header.clone();
        escape.extend_from_slice(&[0, 2, 0, b'.', b'.']);
        assert_eq!(
            parse(&sealed(&escape)),
            Err(ImageError::BadPath {
                path: "..".to_owned()
            })
        );

        let mut not_utf8 = header.clone();
        not_utf8.extend_from_slice(&[0, 1, 0, 0xFF]);
        assert_eq!(
            parse(&sealed(&not_utf8)),
            Err(ImageError::PathNotUtf8 { at: HEADER_LEN + 3 })
        );

        let mut trailing = header.clone();
        trailing.extend_from_slice(&[0, 1, 0, b'a', 9, 9]);
        assert_eq!(
            parse(&sealed(&trailing)),
            Err(ImageError::TrailingBytes { extra: 2 })
        );

        let mut unsorted = MAGIC.to_vec();
        unsorted.extend_from_slice(&2u32.to_le_bytes());
        unsorted.extend_from_slice(&[0, 1, 0, b'b', 0, 1, 0, b'a']);
        assert_eq!(
            parse(&sealed(&unsorted)),
            Err(ImageError::OutOfOrder {
                path: "a".to_owned()
            })
        );
    }

    #[test]
    fn strip_drops_the_per_cluster_and_volatile_files() {
        let names = [
            "PG_VERSION",
            "base/1/PG_VERSION",
            "postgresql.conf",
            "pg_hba.conf",
            "pg_ident.conf",
            "postgresql.auto.conf",
            "postmaster.opts",
            "global/pg_control",
            "global/pg_filenode.map",
            "global/1262",
            "pg_stat/pgstat.stat",
            "pg_wal/000000010000000000000001",
            "pg_wal/archive_status/x.done",
            "pg_walx",
            "pg_xact/0000",
        ];
        let mut entries: Vec<Entry<Vec<u8>>> = names
            .iter()
            .map(|name| Entry::file(path(name), Vec::new()))
            .collect();
        entries.push(Entry::dir(path("pg_wal")));
        entries.push(Entry::dir(path("pg_wal/archive_status")));
        let kept: Vec<String> = strip(entries)
            .into_iter()
            .map(|entry| entry.path.to_string())
            .collect();
        assert_eq!(
            kept,
            [
                "base/1/PG_VERSION",
                "global/pg_filenode.map",
                "global/1262",
                "pg_walx",
                "pg_xact/0000",
                "pg_wal",
                "pg_wal/archive_status",
            ]
        );
    }

    #[cfg(unix)]
    mod actions {
        use std::os::unix::fs::PermissionsExt as _;
        use std::path::PathBuf;

        use super::*;
        use crate::file_perm::DataDirPerm;

        fn scratch(name: &str) -> PathBuf {
            let dir =
                std::env::temp_dir().join(format!("rinitdb-image-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        fn mode(path: &std::path::Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
        }

        #[test]
        fn expand_then_read_tree_round_trips_with_initdbs_modes() {
            for (group, dir_mode, file_mode) in [(false, 0o700, 0o600), (true, 0o750, 0o640)] {
                let target = scratch(&format!("round-trip-{group}"));
                let image = pack(&sample()).unwrap();
                let entries = parse(&image).unwrap();
                let expanded = expand(
                    &entries,
                    &target,
                    DataDirPerm::for_allow_group_access(group),
                )
                .unwrap();
                assert_eq!(
                    expanded,
                    Expanded {
                        dirs: 3,
                        files: 4,
                        bytes: 3 + 3 + 8192
                    }
                );
                assert_eq!(mode(&target.join("base/1")), dir_mode);
                assert_eq!(mode(&target.join("base/1/1259")), file_mode);

                let mut back = read_tree(&target).unwrap();
                back.sort_by(|a, b| a.path.cmp(&b.path));
                assert_eq!(pack(&back).unwrap(), image);
                std::fs::remove_dir_all(&target).unwrap();
            }
        }

        #[test]
        fn expand_keeps_directories_layout_already_made() {
            let target = scratch("existing-dirs");
            std::fs::create_dir(target.join("base")).unwrap();
            std::fs::set_permissions(target.join("base"), std::fs::Permissions::from_mode(0o711))
                .unwrap();
            let image = pack(&sample()).unwrap();
            let expanded = expand(&parse(&image).unwrap(), &target, DataDirPerm::OWNER).unwrap();
            assert_eq!(expanded.dirs, 2);
            assert_eq!(mode(&target.join("base")), 0o711);
            std::fs::remove_dir_all(&target).unwrap();
        }

        #[test]
        fn expand_refuses_to_overwrite_a_file() {
            let target = scratch("existing-file");
            std::fs::write(target.join("empty"), b"mine").unwrap();
            let image = pack(&sample()).unwrap();
            let err = expand(&parse(&image).unwrap(), &target, DataDirPerm::OWNER).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "could not open file \"{}\" for writing: File exists",
                    target.join("empty").display()
                )
            );
            assert_eq!(std::fs::read(target.join("empty")).unwrap(), b"mine");
            std::fs::remove_dir_all(&target).unwrap();
        }

        #[test]
        fn read_tree_refuses_a_symlink() {
            let target = scratch("symlink");
            std::os::unix::fs::symlink("elsewhere", target.join("link")).unwrap();
            let err = read_tree(&target).unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            std::fs::remove_dir_all(&target).unwrap();
        }
    }
}
