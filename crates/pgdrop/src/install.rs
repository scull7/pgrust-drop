//! `pgdrop install-links DIR`: the plain tool names, as symlinks back here.
//!
//! [`crate::dispatch`] already answers to `argv[0]`, so a symlink named
//! `initdb` in a directory on `PATH` *is* initdb as far as a shell or a ported
//! TAP suite is concerned. This module is the command that makes those links
//! (Linear NAT-416). It has no counterpart in the PostgreSQL tree — nothing
//! upstream installs a multicall binary — so there is no C behaviour to copy
//! and no byte-diff gate to run against it; the tests here are this command's
//! own.
//!
//! Data / Calculations / Actions:
//!
//! - [`LinkOp`] is the data: one link, and what has to happen to it.
//! - [`link_plan`] is the whole decision, a pure function of the executable's
//!   path, the target directory and what a [`LinkProbe`] says is already
//!   there. It is unit-tested against a map of fake entries, with no temporary
//!   files at all.
//! - [`apply`] is the only function here that touches a disk, and [`run`] the
//!   only one that reads the process or writes to a stream.
//!
//! ## Why a name in the way refuses the whole plan
//!
//! [`link_plan`] returns `Err` as soon as one of the three names is taken by
//! something that is not a symlink, before [`apply`] has created anything. A
//! directory that already holds a real `psql` is a directory this command has
//! no business writing into at all, and refusing it whole means the next run —
//! after the operator has looked at that file — starts from the state they
//! inspected rather than from a half-installed one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use usage::Args;

use crate::dispatch::Applet;

/// How pgdrop names itself in its own diagnostics (`bin = "pgdrop"`).
const PROGNAME: &str = "pgdrop";

/// Exit status for a failed `install-links`.
const EXIT_FAILURE: u8 = 1;

/// The names `install-links` creates, in the order it creates them.
pub const APPLETS: [Applet; 3] = Applet::ALL;

/// Options for `pgdrop install-links` (Linear NAT-416).
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct InstallLinks {
    /// Directory to create the initdb, psql and postgres links in
    pub dir: PathBuf,
    /// Replace links that are already there (never a file or a directory)
    #[usage(long)]
    pub force: bool,
}

/// What is already sitting at a link's path.
///
/// `lstat`, not `stat`: a symlink is judged as a symlink, whether or not it
/// still resolves to anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// Nothing is there (`ENOENT`).
    Absent,
    /// A symbolic link — the only kind this command will replace.
    Symlink,
    /// A regular file.
    File,
    /// A directory.
    Directory,
    /// A socket, a fifo, a device …
    Other,
    /// `lstat` failed with something other than `ENOENT`.
    Unreadable {
        /// What `%m` would print for the failing `lstat`.
        reason: String,
    },
}

impl EntryKind {
    /// How the entry is named in the "refusing to replace" message.
    fn noun(&self) -> &'static str {
        match self {
            EntryKind::Absent => "nothing",
            EntryKind::Symlink => "a symbolic link",
            EntryKind::File => "a regular file",
            EntryKind::Directory => "a directory",
            EntryKind::Other | EntryKind::Unreadable { .. } => "an entry of another type",
        }
    }
}

/// The one thing planning asks of the filesystem.
///
/// A trait so [`link_plan`] is a calculation; [`RealFs`] is the only
/// implementor that touches a disk.
pub trait LinkProbe {
    /// `lstat(path)`, as the [`EntryKind`] it reveals.
    fn entry_kind(&self, path: &Path) -> EntryKind;
}

/// The real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl LinkProbe for RealFs {
    fn entry_kind(&self, path: &Path) -> EntryKind {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                let kind = meta.file_type();
                if kind.is_symlink() {
                    EntryKind::Symlink
                } else if kind.is_dir() {
                    EntryKind::Directory
                } else if kind.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => EntryKind::Absent,
            Err(err) => EntryKind::Unreadable {
                reason: strerror(&err),
            },
        }
    }
}

/// One link, and what [`apply`] has to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOp {
    /// Nothing is there: `symlink(target, link)`.
    Create {
        /// The name being installed.
        link: PathBuf,
        /// The absolute path of the running executable.
        target: PathBuf,
    },
    /// A symlink is there and `--force` was given: remove it, then create it.
    Replace {
        /// The name being installed.
        link: PathBuf,
        /// The absolute path of the running executable.
        target: PathBuf,
    },
    /// A symlink is there and `--force` was not given: leave it alone.
    Keep {
        /// The name left as it was.
        link: PathBuf,
    },
}

impl LinkOp {
    /// The path this op names in its note and in its error.
    #[must_use]
    pub fn link(&self) -> &Path {
        match self {
            LinkOp::Create { link, .. } | LinkOp::Replace { link, .. } | LinkOp::Keep { link } => {
                link
            }
        }
    }

    /// Pure: the line [`apply`] prints once the op has been carried out.
    #[must_use]
    pub fn note(&self) -> String {
        match self {
            LinkOp::Create { link, target } => format!(
                "{PROGNAME}: created \"{}\" -> \"{}\"",
                link.display(),
                target.display()
            ),
            LinkOp::Replace { link, target } => format!(
                "{PROGNAME}: replaced \"{}\" -> \"{}\"",
                link.display(),
                target.display()
            ),
            LinkOp::Keep { link } => format!(
                "{PROGNAME}: \"{}\" is already a symbolic link; left alone (use --force to replace it)",
                link.display()
            ),
        }
    }
}

/// Everything `install-links` can fail with.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// `std::env::current_exe()` failed, so there is no target to point at.
    #[error("could not determine the path of this executable: {reason}")]
    NoCurrentExe {
        /// What `%m` would print.
        reason: String,
    },
    /// The target is relative, and a relative link target would be resolved
    /// against the link's own directory rather than the current one.
    #[error("refusing to point the links at the relative path \"{path}\"")]
    RelativeTarget {
        /// The offending target.
        path: String,
    },
    /// Something that is not a symlink is using the name.
    #[error("refusing to replace \"{path}\": it is {noun}, not a symbolic link")]
    Occupied {
        /// The name that is taken.
        path: String,
        /// [`EntryKind::noun`] for what is there.
        noun: &'static str,
    },
    /// `lstat` on the name failed with something other than `ENOENT`.
    #[error("could not access \"{path}\": {reason}")]
    Unreadable {
        /// The name that could not be examined.
        path: String,
        /// What `%m` would print.
        reason: String,
    },
    /// `--force` could not get the old link out of the way.
    #[error("could not remove \"{path}\": {reason}")]
    Remove {
        /// The link that is still there.
        path: String,
        /// What `%m` would print.
        reason: String,
    },
    /// `symlink(2)` failed — most often because `DIR` does not exist.
    #[error("could not create symbolic link \"{path}\": {reason}")]
    Symlink {
        /// The link that was not created.
        path: String,
        /// What `%m` would print.
        reason: String,
    },
    /// A platform with neither `symlink(2)` nor Windows' file symlinks.
    #[error("symbolic links are not supported on this platform")]
    Unsupported,
}

impl InstallError {
    /// The stderr line, `pg_log_error`-shaped, without its newline.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{PROGNAME}: error: {self}")
    }
}

/// Pure: what has to happen to each of `applets`' names in `dir`.
///
/// `exe` is the executable the links point at; it must be absolute, because a
/// relative symlink target is resolved against the directory holding the link.
/// The ops come back in `applets` order.
///
/// # Errors
/// [`InstallError::RelativeTarget`] for a relative `exe`, and
/// [`InstallError::Occupied`] / [`InstallError::Unreadable`] for the first
/// name that is taken by something other than a symlink — the whole plan is
/// refused, so [`apply`] never gets a chance to half-install one.
pub fn link_plan(
    exe: &Path,
    dir: &Path,
    applets: &[Applet],
    force: bool,
    probe: &dyn LinkProbe,
) -> Result<Vec<LinkOp>, InstallError> {
    if !exe.is_absolute() {
        return Err(InstallError::RelativeTarget {
            path: exe.display().to_string(),
        });
    }
    let mut ops = Vec::with_capacity(applets.len());
    for applet in applets {
        let link = dir.join(applet.name());
        let op = match probe.entry_kind(&link) {
            EntryKind::Absent => LinkOp::Create {
                link,
                target: exe.to_path_buf(),
            },
            EntryKind::Symlink if force => LinkOp::Replace {
                link,
                target: exe.to_path_buf(),
            },
            EntryKind::Symlink => LinkOp::Keep { link },
            EntryKind::Unreadable { reason } => {
                return Err(InstallError::Unreadable {
                    path: link.display().to_string(),
                    reason,
                });
            }
            taken => {
                return Err(InstallError::Occupied {
                    path: link.display().to_string(),
                    noun: taken.noun(),
                });
            }
        };
        ops.push(op);
    }
    Ok(ops)
}

/// Action: carry out `ops` in order, writing each one's note to `out`.
///
/// A note is written only once its op has been carried out, so the notes a
/// failing run has already printed are exactly the links it created.
///
/// # Errors
/// The first op that the filesystem refuses, as [`InstallError::Remove`],
/// [`InstallError::Symlink`] or [`InstallError::Unsupported`].
pub fn apply(ops: &[LinkOp], out: &mut impl Write) -> Result<(), InstallError> {
    for op in ops {
        match op {
            LinkOp::Create { link, target } => symlink(target, link)?,
            LinkOp::Replace { link, target } => {
                // Not atomic: there is a window in which the name is missing
                // rather than pointing at the old binary. `install-links` is
                // an installation step, not something a running test suite
                // races with, and a rename dance would trade that window for a
                // stray temporary name in a directory on PATH.
                std::fs::remove_file(link).map_err(|err| InstallError::Remove {
                    path: link.display().to_string(),
                    reason: strerror(&err),
                })?;
                symlink(target, link)?;
            }
            LinkOp::Keep { .. } => {}
        }
        // A closed stream is not worth a second error message.
        let _ = writeln!(out, "{}", op.note());
    }
    Ok(())
}

/// Action: `symlink(2)`, as the one place the platform shows through.
#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<(), InstallError> {
    std::os::unix::fs::symlink(target, link).map_err(|err| InstallError::Symlink {
        path: link.display().to_string(),
        reason: strerror(&err),
    })
}

/// Windows' file symlinks need either developer mode or a privilege; the error
/// says so through `%m` when they are not available.
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> Result<(), InstallError> {
    std::os::windows::fs::symlink_file(target, link).map_err(|err| InstallError::Symlink {
        path: link.display().to_string(),
        reason: strerror(&err),
    })
}

#[cfg(not(any(unix, windows)))]
fn symlink(_target: &Path, _link: &Path) -> Result<(), InstallError> {
    Err(InstallError::Unsupported)
}

/// What `%m` would print: `strerror(errno)` and nothing else.
fn strerror(err: &std::io::Error) -> String {
    rinitdb::validate::strerror(err)
}

/// The whole command: read the process, plan, apply, report.
pub fn run(args: &InstallLinks, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    let plan = current_exe()
        .and_then(|exe| link_plan(&exe, &args.dir, &APPLETS, args.force, &RealFs))
        .and_then(|ops| apply(&ops, stdout));
    match plan {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(stderr, "{}", err.render());
            ExitCode::from(EXIT_FAILURE)
        }
    }
}

/// Action: where this process's executable lives.
fn current_exe() -> Result<PathBuf, InstallError> {
    std::env::current_exe().map_err(|err| InstallError::NoCurrentExe {
        reason: strerror(&err),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    /// A map of paths to what is there; anything absent is [`EntryKind::Absent`].
    struct FakeFs(BTreeMap<PathBuf, EntryKind>);

    impl FakeFs {
        fn new(entries: &[(&str, EntryKind)]) -> Self {
            FakeFs(
                entries
                    .iter()
                    .map(|(path, kind)| (PathBuf::from(path), kind.clone()))
                    .collect(),
            )
        }

        fn empty() -> Self {
            FakeFs(BTreeMap::new())
        }
    }

    impl LinkProbe for FakeFs {
        fn entry_kind(&self, path: &Path) -> EntryKind {
            self.0.get(path).cloned().unwrap_or(EntryKind::Absent)
        }
    }

    const EXE: &str = "/opt/pgdrop/bin/pgdrop";

    fn plan(fs: &FakeFs, force: bool) -> Result<Vec<LinkOp>, InstallError> {
        link_plan(Path::new(EXE), Path::new("/tmp/bin"), &APPLETS, force, fs)
    }

    fn create(name: &str) -> LinkOp {
        LinkOp::Create {
            link: PathBuf::from(format!("/tmp/bin/{name}")),
            target: PathBuf::from(EXE),
        }
    }

    #[test]
    fn an_empty_directory_gets_one_link_per_applet_in_order() {
        let ops = plan(&FakeFs::empty(), false).expect("plan");
        assert_eq!(
            ops,
            vec![create("initdb"), create("psql"), create("postgres")]
        );
    }

    #[test]
    fn the_links_point_at_the_running_executable() {
        for op in plan(&FakeFs::empty(), false).expect("plan") {
            match op {
                LinkOp::Create { target, .. } => assert_eq!(target, Path::new(EXE)),
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_relative_executable_is_refused_before_anything_is_planned() {
        let err = link_plan(
            Path::new("target/debug/pgdrop"),
            Path::new("/tmp/bin"),
            &APPLETS,
            false,
            &FakeFs::empty(),
        )
        .expect_err("relative target");
        assert_eq!(
            err,
            InstallError::RelativeTarget {
                path: "target/debug/pgdrop".to_owned(),
            }
        );
    }

    #[test]
    fn an_existing_link_is_kept_unless_force_is_given() {
        let fs = FakeFs::new(&[("/tmp/bin/psql", EntryKind::Symlink)]);
        let ops = plan(&fs, false).expect("plan");
        assert_eq!(
            ops,
            vec![
                create("initdb"),
                LinkOp::Keep {
                    link: PathBuf::from("/tmp/bin/psql"),
                },
                create("postgres"),
            ]
        );
    }

    #[test]
    fn force_replaces_an_existing_link_and_only_a_link() {
        let fs = FakeFs::new(&[("/tmp/bin/psql", EntryKind::Symlink)]);
        let ops = plan(&fs, true).expect("plan");
        assert_eq!(
            ops,
            vec![
                create("initdb"),
                LinkOp::Replace {
                    link: PathBuf::from("/tmp/bin/psql"),
                    target: PathBuf::from(EXE),
                },
                create("postgres"),
            ]
        );
    }

    #[test]
    fn a_file_in_the_way_refuses_the_whole_plan_with_or_without_force() {
        for (kind, noun) in [
            (EntryKind::File, "a regular file"),
            (EntryKind::Directory, "a directory"),
            (EntryKind::Other, "an entry of another type"),
        ] {
            let fs = FakeFs::new(&[("/tmp/bin/psql", kind.clone())]);
            for force in [false, true] {
                assert_eq!(
                    plan(&fs, force).expect_err("occupied"),
                    InstallError::Occupied {
                        path: "/tmp/bin/psql".to_owned(),
                        noun,
                    },
                    "{kind:?} force={force}"
                );
            }
        }
    }

    #[test]
    fn a_name_that_cannot_be_examined_is_its_own_error() {
        let fs = FakeFs::new(&[(
            "/tmp/bin/initdb",
            EntryKind::Unreadable {
                reason: "Permission denied".to_owned(),
            },
        )]);
        assert_eq!(
            plan(&fs, true).expect_err("unreadable"),
            InstallError::Unreadable {
                path: "/tmp/bin/initdb".to_owned(),
                reason: "Permission denied".to_owned(),
            }
        );
    }

    #[test]
    fn every_error_renders_as_one_prefixed_line() {
        let errors = [
            InstallError::NoCurrentExe {
                reason: "No such file or directory".to_owned(),
            },
            InstallError::RelativeTarget {
                path: "./pgdrop".to_owned(),
            },
            InstallError::Occupied {
                path: "/tmp/bin/psql".to_owned(),
                noun: "a regular file",
            },
            InstallError::Unreadable {
                path: "/tmp/bin/psql".to_owned(),
                reason: "Permission denied".to_owned(),
            },
            InstallError::Remove {
                path: "/tmp/bin/psql".to_owned(),
                reason: "Permission denied".to_owned(),
            },
            InstallError::Symlink {
                path: "/tmp/bin/psql".to_owned(),
                reason: "No such file or directory".to_owned(),
            },
            InstallError::Unsupported,
        ];
        for err in errors {
            let line = err.render();
            assert!(
                line.starts_with("pgdrop: error: "),
                "{line}: not a pg_log_error line"
            );
            assert!(!line.ends_with('\n'), "{line}: the caller adds the newline");
        }
        assert_eq!(
            InstallError::Occupied {
                path: "/tmp/bin/psql".to_owned(),
                noun: "a regular file",
            }
            .render(),
            "pgdrop: error: refusing to replace \"/tmp/bin/psql\": it is a regular file, \
             not a symbolic link"
        );
    }

    #[test]
    fn each_note_names_the_link_and_says_what_happened() {
        let created = create("initdb").note();
        assert_eq!(
            created,
            format!("pgdrop: created \"/tmp/bin/initdb\" -> \"{EXE}\"")
        );
        let replaced = LinkOp::Replace {
            link: PathBuf::from("/tmp/bin/psql"),
            target: PathBuf::from(EXE),
        }
        .note();
        assert_eq!(
            replaced,
            format!("pgdrop: replaced \"/tmp/bin/psql\" -> \"{EXE}\"")
        );
        let kept = LinkOp::Keep {
            link: PathBuf::from("/tmp/bin/postgres"),
        }
        .note();
        assert!(kept.contains("/tmp/bin/postgres"), "{kept}");
        assert!(kept.contains("--force"), "{kept}");
    }

    #[test]
    fn the_op_knows_which_link_it_is_about() {
        assert_eq!(create("initdb").link(), Path::new("/tmp/bin/initdb"));
        assert_eq!(
            LinkOp::Keep {
                link: PathBuf::from("/tmp/bin/psql"),
            }
            .link(),
            Path::new("/tmp/bin/psql")
        );
    }

    #[test]
    fn every_applet_the_dispatcher_answers_to_gets_a_link_with_its_name() {
        assert_eq!(APPLETS, Applet::ALL);
        let ops = plan(&FakeFs::empty(), false).expect("plan");
        let names: Vec<String> = ops
            .iter()
            .map(|op| {
                op.link()
                    .file_name()
                    .expect("a link has a file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let expected: Vec<String> = Applet::ALL.iter().map(|a| a.name().to_owned()).collect();
        assert_eq!(names, expected);
    }
}
