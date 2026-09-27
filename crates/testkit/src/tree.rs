//! The data directory tree diff (Linear NAT-386): two directory trees, each
//! reduced to a manifest — every entry's kind, permission bits, size and a
//! digest of its contents — and compared entry by entry, with an explicit
//! allow-list of the differences the caller expects and has justified.
//!
//! Data / Calculations / Actions: [`Node`] and [`Tree`] are the data;
//! [`differences`] and [`unexplained`] are the whole verdict as pure
//! functions; [`read_tree`] is the only part that touches a disk.
//!
//! Timestamps are not in a [`Node`] at all. No two runs agree on an mtime,
//! and nothing a server reads from a data directory depends on one, so there
//! is nothing to allow: they are simply not compared.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::{DefaultHasher, Hasher as _};
use std::path::{Path, PathBuf};

use crate::files::EntryKind;

/// What the tree diff knows about one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub kind: EntryKind,
    /// The permission bits alone (`S_IMODE`).
    pub mode: u32,
    /// The length in bytes, for a regular file; 0 for anything else.
    pub size: u64,
    /// [`digest`] of the contents, for a regular file; 0 for anything else.
    pub digest: u64,
}

impl Node {
    /// A directory with `mode`.
    #[must_use]
    pub fn dir(mode: u32) -> Self {
        Self {
            kind: EntryKind::Dir,
            mode,
            size: 0,
            digest: 0,
        }
    }

    /// A regular file with `mode` holding `contents`.
    #[must_use]
    pub fn file(mode: u32, contents: &[u8]) -> Self {
        Self {
            kind: EntryKind::File,
            mode,
            size: contents.len() as u64,
            digest: digest(contents),
        }
    }
}

/// A whole tree, keyed by the path relative to its root. The root itself is
/// the empty path, so its mode is compared like any other entry's.
pub type Tree = BTreeMap<PathBuf, Node>;

/// Pure: a 64-bit digest of `contents`.
///
/// std's `DefaultHasher::new()`, whose keys are fixed, so two digests taken in
/// one process are comparable — and that is all a tree diff needs, because it
/// reads both trees in the same test. A digest is never written down or
/// compared across builds. Equal sizes and equal 64-bit digests are what
/// "same contents" means here; the chance of two different files of one size
/// colliding is 2^-64 per pair.
#[must_use]
pub fn digest(contents: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    hasher.write(contents);
    hasher.finish()
}

/// Which property of an entry differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Aspect {
    /// The entry is in one tree only.
    Presence,
    /// Directory in one tree, file in the other. Mode and contents are not
    /// compared across kinds.
    Kind,
    /// The permission bits.
    Mode,
    /// A regular file's size or bytes.
    Content,
}

/// One way the two trees differ at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    pub path: PathBuf,
    pub aspect: Aspect,
    /// The reference tree's entry, if it has one.
    pub theirs: Option<Node>,
    /// The tree under test's entry, if it has one.
    pub ours: Option<Node>,
}

impl Difference {
    /// The entry either side has, the reference's first.
    fn node(&self) -> Option<Node> {
        self.theirs.or(self.ours)
    }
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = if self.path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            self.path.as_path()
        };
        write!(f, "{}: ", path.display())?;
        match (self.aspect, self.theirs, self.ours) {
            (Aspect::Presence, Some(_), None) => write!(f, "only in the reference tree"),
            (Aspect::Presence, None, Some(_)) => write!(f, "only in the tree under test"),
            (Aspect::Kind, Some(theirs), Some(ours)) => {
                write!(
                    f,
                    "kind {:?} in the reference, {:?} here",
                    theirs.kind, ours.kind
                )
            }
            (Aspect::Mode, Some(theirs), Some(ours)) => write!(
                f,
                "mode {:04o} in the reference, {:04o} here",
                theirs.mode, ours.mode
            ),
            (Aspect::Content, Some(theirs), Some(ours)) if theirs.size != ours.size => write!(
                f,
                "{} bytes in the reference, {} here",
                theirs.size, ours.size
            ),
            (Aspect::Content, Some(theirs), Some(_)) => {
                write!(f, "contents differ (both {} bytes)", theirs.size)
            }
            (aspect, theirs, ours) => write!(f, "{aspect:?}: {theirs:?} against {ours:?}"),
        }
    }
}

/// Pure: every difference between the reference tree `theirs` and the tree
/// under test `ours`, in path order.
///
/// An entry in both trees is compared for kind first; if the kinds agree its
/// mode is compared, and a regular file's size and digest too, so one path
/// can yield a [`Aspect::Mode`] and a [`Aspect::Content`] difference both.
#[must_use]
pub fn differences(theirs: &Tree, ours: &Tree) -> Vec<Difference> {
    let mut paths: Vec<&PathBuf> = theirs.keys().chain(ours.keys()).collect();
    paths.sort();
    paths.dedup();

    let mut found = Vec::new();
    for path in paths {
        let (their_node, our_node) = (theirs.get(path).copied(), ours.get(path).copied());
        let mut push = |aspect| {
            found.push(Difference {
                path: path.clone(),
                aspect,
                theirs: their_node,
                ours: our_node,
            });
        };
        let (Some(t), Some(o)) = (their_node, our_node) else {
            push(Aspect::Presence);
            continue;
        };
        if t.kind != o.kind {
            push(Aspect::Kind);
            continue;
        }
        if t.mode != o.mode {
            push(Aspect::Mode);
        }
        if t.kind == EntryKind::File && (t.size, t.digest) != (o.size, o.digest) {
            push(Aspect::Content);
        }
    }
    found
}

/// The entries an [`Allowance`] speaks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Covers {
    /// Exactly this relative path.
    Path(&'static str),
    /// Every regular file anywhere below this relative directory — not the
    /// directory itself, and not the directories inside it.
    FilesUnder(&'static str),
}

impl Covers {
    /// Pure: does this cover `difference`'s entry?
    #[must_use]
    pub fn covers(self, difference: &Difference) -> bool {
        match self {
            Covers::Path(path) => difference.path == Path::new(path),
            Covers::FilesUnder(dir) => {
                difference.path.starts_with(dir)
                    && difference.path != Path::new(dir)
                    && difference
                        .node()
                        .is_some_and(|node| node.kind == EntryKind::File)
            }
        }
    }
}

/// One expected difference, and why it is expected.
///
/// An allowance is only as good as its reason, so it carries one; the caller
/// is expected to pair each with a narrower check that runs in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowance {
    pub covers: Covers,
    /// The aspects that may differ there. Anything else still fails.
    pub aspects: &'static [Aspect],
    pub why: &'static str,
}

impl Allowance {
    /// Pure: does this allowance explain `difference`?
    #[must_use]
    pub fn explains(&self, difference: &Difference) -> bool {
        self.aspects.contains(&difference.aspect) && self.covers.covers(difference)
    }
}

/// Pure: the differences no allowance explains. Empty is a pass.
#[must_use]
pub fn unexplained<'a>(found: &'a [Difference], allow: &[Allowance]) -> Vec<&'a Difference> {
    found
        .iter()
        .filter(|difference| !allow.iter().any(|allowance| allowance.explains(difference)))
        .collect()
}

#[cfg(unix)]
mod unix {
    use std::path::Path;

    use super::{Node, Tree};
    use crate::files::{EntryKind, walk};

    /// Action: the [`Tree`] under `root`, `root` itself included as the empty
    /// path.
    ///
    /// It is [`walk`]'s listing, so symlinks are followed as
    /// `check_mode_recursive` follows them, plus each regular file's size and
    /// digest.
    ///
    /// # Errors
    /// Any error from [`walk`], or from reading a file.
    pub fn read_tree(root: &Path) -> std::io::Result<Tree> {
        let mut tree = Tree::new();
        for entry in walk(root, &[])? {
            let relative = entry
                .path
                .strip_prefix(root)
                .map_err(|err| std::io::Error::other(err.to_string()))?
                .to_path_buf();
            let node = match entry.kind {
                EntryKind::File => Node::file(entry.mode, &std::fs::read(&entry.path)?),
                EntryKind::Dir | EntryKind::Other => Node {
                    kind: entry.kind,
                    mode: entry.mode,
                    size: 0,
                    digest: 0,
                },
            };
            tree.insert(relative, node);
        }
        Ok(tree)
    }
}

#[cfg(unix)]
pub use unix::read_tree;

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(entries: &[(&str, Node)]) -> Tree {
        entries
            .iter()
            .map(|(path, node)| (PathBuf::from(path), *node))
            .collect()
    }

    fn aspects(found: &[Difference]) -> Vec<(String, Aspect)> {
        found
            .iter()
            .map(|d| (d.path.display().to_string(), d.aspect))
            .collect()
    }

    #[test]
    fn identical_trees_have_no_differences() {
        let one = tree(&[
            ("", Node::dir(0o700)),
            ("global", Node::dir(0o700)),
            ("PG_VERSION", Node::file(0o600, b"18\n")),
        ]);
        assert_eq!(differences(&one, &one.clone()), []);
    }

    #[test]
    fn every_aspect_is_reported_where_it_differs() {
        let theirs = tree(&[
            ("", Node::dir(0o700)),
            ("a", Node::file(0o600, b"same")),
            ("b", Node::file(0o600, b"four")),
            ("c", Node::file(0o600, b"x")),
            ("d", Node::dir(0o700)),
            ("only-theirs", Node::file(0o600, b"")),
        ]);
        let ours = tree(&[
            ("", Node::dir(0o750)),
            ("a", Node::file(0o600, b"same")),
            ("b", Node::file(0o640, b"FOUR")),
            ("c", Node::file(0o600, b"xx")),
            ("d", Node::file(0o640, b"")),
            ("only-ours", Node::dir(0o700)),
        ]);
        assert_eq!(
            aspects(&differences(&theirs, &ours)),
            [
                (String::new(), Aspect::Mode),
                ("b".to_owned(), Aspect::Mode),
                ("b".to_owned(), Aspect::Content),
                ("c".to_owned(), Aspect::Content),
                // A kind mismatch is reported alone: mode and contents are
                // not compared across kinds.
                ("d".to_owned(), Aspect::Kind),
                ("only-ours".to_owned(), Aspect::Presence),
                ("only-theirs".to_owned(), Aspect::Presence),
            ]
        );
    }

    #[test]
    fn a_difference_says_which_side_has_what() {
        let theirs = tree(&[("", Node::dir(0o700)), ("f", Node::file(0o600, b"ab"))]);
        let ours = tree(&[("", Node::dir(0o750)), ("f", Node::file(0o600, b"abc"))]);
        let rendered: Vec<String> = differences(&theirs, &ours)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            rendered,
            [
                ".: mode 0700 in the reference, 0750 here",
                "f: 2 bytes in the reference, 3 here",
            ]
        );
        let gone = differences(&theirs, &tree(&[("", Node::dir(0o700))]));
        assert_eq!(gone[0].to_string(), "f: only in the reference tree");
    }

    #[test]
    fn an_allowance_explains_only_its_aspects_at_its_entries() {
        let theirs = tree(&[
            ("pg_wal", Node::dir(0o700)),
            ("pg_wal/archive_status", Node::dir(0o700)),
            ("pg_wal/000000010000000000000001", Node::file(0o600, b"c")),
            ("global/pg_control", Node::file(0o600, b"c")),
        ]);
        let ours = tree(&[
            ("pg_wal", Node::dir(0o750)),
            ("pg_wal/000000010000000000000002", Node::file(0o600, b"r")),
            ("global/pg_control", Node::file(0o640, b"r")),
        ]);
        let allow = [
            Allowance {
                covers: Covers::FilesUnder("pg_wal"),
                aspects: &[Aspect::Presence, Aspect::Content],
                why: "test",
            },
            Allowance {
                covers: Covers::Path("global/pg_control"),
                aspects: &[Aspect::Content],
                why: "test",
            },
        ];
        let found = differences(&theirs, &ours);
        let left: Vec<(String, Aspect)> = unexplained(&found, &allow)
            .into_iter()
            .map(|d| (d.path.display().to_string(), d.aspect))
            .collect();
        assert_eq!(
            left,
            [
                // Content is allowed at pg_control, its mode is not.
                ("global/pg_control".to_owned(), Aspect::Mode),
                // FilesUnder covers neither the directory itself …
                ("pg_wal".to_owned(), Aspect::Mode),
                // … nor a directory below it.
                ("pg_wal/archive_status".to_owned(), Aspect::Presence),
            ]
        );
    }

    #[test]
    fn a_path_allowance_is_the_path_not_a_prefix() {
        let theirs = tree(&[("postgresql.conf", Node::file(0o600, b"a"))]);
        let ours = tree(&[("postgresql.conf.bak", Node::file(0o600, b"a"))]);
        let allow = [Allowance {
            covers: Covers::Path("postgresql.conf"),
            aspects: &[Aspect::Content],
            why: "test",
        }];
        let found = differences(&theirs, &ours);
        assert_eq!(unexplained(&found, &allow).len(), 2);
    }

    #[test]
    fn the_digest_tells_same_sized_contents_apart() {
        assert_eq!(digest(b"abcd"), digest(b"abcd"));
        assert_ne!(digest(b"abcd"), digest(b"abce"));
    }

    #[cfg(unix)]
    #[test]
    fn read_tree_lists_the_root_and_every_entry_below_it() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!("testkit-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("global")).unwrap();
        std::fs::write(root.join("global/pg_control"), b"control").unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(
            root.join("global/pg_control"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        let tree = read_tree(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            tree.keys().collect::<Vec<_>>(),
            [
                Path::new(""),
                Path::new("global"),
                Path::new("global/pg_control")
            ]
        );
        assert_eq!(tree[Path::new("")].mode, 0o700);
        assert_eq!(
            tree[Path::new("global/pg_control")],
            Node::file(0o600, b"control")
        );
    }
}
