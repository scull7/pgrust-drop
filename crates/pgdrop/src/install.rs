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

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use usage::Args;

use crate::dispatch::Applet;

/// How pgdrop names itself in its own diagnostics (`bin = "pgdrop"`).
const PROGNAME: &str = "pgdrop";

/// Exit status for a failed `install-links`.
const EXIT_FAILURE: u8 = 1;

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
    ///
    /// An [`OsString`], not a `String`: it names a path the operator gave us
    /// and may want to copy back, and `Path::display()` would replace the
    /// bytes of a name that is not UTF-8 with `U+FFFD`. [`write_os_line`]
    /// puts the bytes back on the stream unchanged.
    #[must_use]
    pub fn note(&self) -> OsString {
        let mut note = OsString::from(format!("{PROGNAME}: "));
        match self {
            LinkOp::Create { link, target } => {
                note.push("created ");
                quoted(&mut note, link);
                note.push(" -> ");
                quoted(&mut note, target);
            }
            LinkOp::Replace { link, target } => {
                note.push("replaced ");
                quoted(&mut note, link);
                note.push(" -> ");
                quoted(&mut note, target);
            }
            LinkOp::Keep { link } => {
                quoted(&mut note, link);
                note.push(" is already a symbolic link; left alone (use --force to replace it)");
            }
        }
        note
    }
}

/// Everything `install-links` can fail with.
///
/// The paths are [`PathBuf`]s and the message is built as an [`OsString`] by
/// [`InstallError::message`], because "an error naming the path" has to name
/// the path the operator typed: a `String` field filled from
/// `Path::display()` would hand back `U+FFFD` where a name is not UTF-8, and
/// they could not copy it back. `Display` is that same message with the
/// substitutions `std::fmt` cannot avoid, so there is still one home for the
/// wording; `message` is the authoritative one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// `std::env::current_exe()` failed, so there is no target to point at.
    NoCurrentExe {
        /// What `%m` would print.
        reason: String,
    },
    /// The target is relative, and a relative link target would be resolved
    /// against the link's own directory rather than the current one.
    RelativeTarget {
        /// The offending target.
        path: PathBuf,
    },
    /// Something that is not a symlink is using the name.
    Occupied {
        /// The name that is taken.
        path: PathBuf,
        /// [`EntryKind::noun`] for what is there.
        noun: &'static str,
    },
    /// `lstat` on the name failed with something other than `ENOENT`.
    Unreadable {
        /// The name that could not be examined.
        path: PathBuf,
        /// What `%m` would print.
        reason: String,
    },
    /// `--force` could not get the old link out of the way.
    Remove {
        /// The link that is still there.
        path: PathBuf,
        /// What `%m` would print.
        reason: String,
    },
    /// `symlink(2)` failed — most often because `DIR` does not exist.
    Symlink {
        /// The link that was not created.
        path: PathBuf,
        /// What `%m` would print.
        reason: String,
    },
    /// A platform with neither `symlink(2)` nor Windows' file symlinks.
    Unsupported,
}

impl InstallError {
    /// Pure: the message itself, with every path's bytes intact.
    #[must_use]
    pub fn message(&self) -> OsString {
        match self {
            InstallError::NoCurrentExe { reason } => OsString::from(format!(
                "could not determine the path of this executable: {reason}"
            )),
            InstallError::RelativeTarget { path } => {
                let mut line = OsString::from("refusing to point the links at the relative path ");
                quoted(&mut line, path);
                line
            }
            InstallError::Occupied { path, noun } => {
                let mut line = OsString::from("refusing to replace ");
                quoted(&mut line, path);
                line.push(format!(": it is {noun}, not a symbolic link"));
                line
            }
            InstallError::Unreadable { path, reason } => about(path, "could not access ", reason),
            InstallError::Remove { path, reason } => about(path, "could not remove ", reason),
            InstallError::Symlink { path, reason } => {
                about(path, "could not create symbolic link ", reason)
            }
            InstallError::Unsupported => {
                OsString::from("symbolic links are not supported on this platform")
            }
        }
    }

    /// Pure: the stderr line, `pg_log_error`-shaped, without its newline.
    #[must_use]
    pub fn render(&self) -> OsString {
        let mut line = OsString::from(format!("{PROGNAME}: error: "));
        line.push(self.message());
        line
    }
}

impl fmt::Display for InstallError {
    /// The message, with a path that is not UTF-8 rendered lossily — which is
    /// exactly why [`InstallError::render`] and not this is what reaches
    /// stderr. `std::fmt` has no byte-preserving path.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message().to_string_lossy())
    }
}

impl std::error::Error for InstallError {}

/// `<verb> "<path>": <what %m said>`, the shape three variants share.
fn about(path: &Path, verb: &str, reason: &str) -> OsString {
    let mut line = OsString::from(verb);
    quoted(&mut line, path);
    line.push(format!(": {reason}"));
    line
}

/// Append `"path"`, quotes included and bytes intact.
fn quoted(line: &mut OsString, path: &Path) {
    line.push("\"");
    line.push(path);
    line.push("\"");
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
            path: exe.to_path_buf(),
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
                return Err(InstallError::Unreadable { path: link, reason });
            }
            taken => {
                return Err(InstallError::Occupied {
                    path: link,
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
                    path: link.clone(),
                    reason: strerror(&err),
                })?;
                symlink(target, link)?;
            }
            LinkOp::Keep { .. } => {}
        }
        // A closed stream is not worth a second error message.
        let _ = write_os_line(out, &op.note());
    }
    Ok(())
}

/// Action: `symlink(2)`, as the one place the platform shows through.
#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<(), InstallError> {
    std::os::unix::fs::symlink(target, link).map_err(|err| InstallError::Symlink {
        path: link.to_path_buf(),
        reason: strerror(&err),
    })
}

/// Windows' file symlinks need either developer mode or a privilege; the error
/// says so through `%m` when they are not available.
#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> Result<(), InstallError> {
    std::os::windows::fs::symlink_file(target, link).map_err(|err| InstallError::Symlink {
        path: link.to_path_buf(),
        reason: strerror(&err),
    })
}

#[cfg(not(any(unix, windows)))]
fn symlink(_target: &Path, _link: &Path) -> Result<(), InstallError> {
    Err(InstallError::Unsupported)
}

/// Action: write one message and its newline, keeping its bytes.
///
/// # Errors
/// Whatever the stream reports; both callers ignore it, because a closed
/// stream is not worth a second error message.
fn write_os_line(out: &mut impl Write, line: &OsStr) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        out.write_all(line.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        // No stable byte view of an `OsStr` off Unix. Windows paths are UTF-16
        // and lose nothing through a lossy conversion that is only reached for
        // an unpaired surrogate.
        out.write_all(line.to_string_lossy().as_bytes())?;
    }
    out.write_all(b"\n")
}

/// What `%m` would print: `strerror(errno)` and nothing else.
fn strerror(err: &std::io::Error) -> String {
    rinitdb::validate::strerror(err)
}

/// The whole command: read the process, plan, apply, report.
pub fn run(args: &InstallLinks, stdout: &mut impl Write, stderr: &mut impl Write) -> ExitCode {
    let plan = current_exe()
        .and_then(|exe| link_plan(&exe, &args.dir, &Applet::ALL, args.force, &RealFs))
        .and_then(|ops| apply(&ops, stdout));
    match plan {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = write_os_line(stderr, &err.render());
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
        link_plan(
            Path::new(EXE),
            Path::new("/tmp/bin"),
            &Applet::ALL,
            force,
            fs,
        )
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
            &Applet::ALL,
            false,
            &FakeFs::empty(),
        )
        .expect_err("relative target");
        assert_eq!(
            err,
            InstallError::RelativeTarget {
                path: PathBuf::from("target/debug/pgdrop"),
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
                        path: PathBuf::from("/tmp/bin/psql"),
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
                path: PathBuf::from("/tmp/bin/initdb"),
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
                path: PathBuf::from("./pgdrop"),
            },
            InstallError::Occupied {
                path: PathBuf::from("/tmp/bin/psql"),
                noun: "a regular file",
            },
            InstallError::Unreadable {
                path: PathBuf::from("/tmp/bin/psql"),
                reason: "Permission denied".to_owned(),
            },
            InstallError::Remove {
                path: PathBuf::from("/tmp/bin/psql"),
                reason: "Permission denied".to_owned(),
            },
            InstallError::Symlink {
                path: PathBuf::from("/tmp/bin/psql"),
                reason: "No such file or directory".to_owned(),
            },
            InstallError::Unsupported,
        ];
        for err in errors {
            let line = err.render().to_string_lossy().into_owned();
            assert!(
                line.starts_with("pgdrop: error: "),
                "{line}: not a pg_log_error line"
            );
            assert!(!line.ends_with('\n'), "{line}: the caller adds the newline");
            // One home for the wording: Display is `message` too.
            assert_eq!(line, format!("pgdrop: error: {err}"));
        }
        assert_eq!(
            InstallError::Occupied {
                path: PathBuf::from("/tmp/bin/psql"),
                noun: "a regular file",
            }
            .render(),
            OsString::from(
                "pgdrop: error: refusing to replace \"/tmp/bin/psql\": it is a regular file, \
                 not a symbolic link"
            )
        );
    }

    /// The Acceptance line is "an error naming the path", and a path is bytes.
    /// `Path::display()` would put `U+FFFD` where the `0xFF` is, naming a path
    /// the operator cannot copy back.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_keeps_its_bytes_through_the_message() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let dir = PathBuf::from(OsString::from_vec(b"/tmp/b\xffad".to_vec()));
        let psql = dir.join("psql");
        let fs = FakeFs(
            [(psql.clone(), EntryKind::File)]
                .into_iter()
                .collect::<BTreeMap<_, _>>(),
        );
        let err = link_plan(Path::new(EXE), &dir, &Applet::ALL, false, &fs).expect_err("occupied");

        let rendered = err.render();
        let bytes = rendered.as_bytes();
        let wanted = psql.as_os_str().as_bytes();
        assert!(
            bytes.windows(wanted.len()).any(|window| window == wanted),
            "the path is not in the message byte for byte: {rendered:?}"
        );
        // U+FFFD, the substitution this test exists to keep out.
        assert!(
            !bytes.windows(3).any(|window| window == [0xEF, 0xBF, 0xBD]),
            "the message still substitutes: {rendered:?}"
        );

        let mut stream = Vec::new();
        write_os_line(&mut stream, &rendered).expect("write");
        assert_eq!(stream, [bytes, b"\n"].concat());
    }

    #[test]
    fn each_note_names_the_link_and_says_what_happened() {
        assert_eq!(
            create("initdb").note(),
            OsString::from(format!("pgdrop: created \"/tmp/bin/initdb\" -> \"{EXE}\""))
        );
        assert_eq!(
            LinkOp::Replace {
                link: PathBuf::from("/tmp/bin/psql"),
                target: PathBuf::from(EXE),
            }
            .note(),
            OsString::from(format!("pgdrop: replaced \"/tmp/bin/psql\" -> \"{EXE}\""))
        );
        let kept = LinkOp::Keep {
            link: PathBuf::from("/tmp/bin/postgres"),
        }
        .note()
        .to_string_lossy()
        .into_owned();
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
