//! The current cluster: what `pgdrop start` leaves for a bare `pgdrop psql`
//! and `pgdrop stop` (Linear NAT-409), so that
//! `pgdrop start && pgdrop psql -c 'select 1' && pgdrop stop` needs no
//! arguments.
//!
//! Every successful `start` writes a [`Pointer`] to one per-user file,
//! `$XDG_RUNTIME_DIR/pgdrop/current`, else `$HOME/.cache/pgdrop/current`
//! ([`pointer_path`]), atomically: a temporary file in the same directory,
//! renamed over it. Two concurrent starts both succeed and the pointer
//! names whichever renamed last; neither cluster is touched by it.
//!
//! A bare command is one that names no cluster itself: `pgdrop stop` with
//! no `--datadir`, `pgdrop psql` with no host, port, user or database on
//! its command line, and in either case none of [`EXPLICIT_ENV`] set.
//! Only a bare command reads the pointer; anything explicit wins. `stop`
//! removes the pointer when it names the cluster stopped.
//!
//! Nothing upstream has this: libpq's defaults are its environment and a
//! compiled-in socket directory, pg_ctl's `-D` or `PGDATA`
//! (`docs/divergences.md`).
//!
//! Data / Calculations / Actions: [`Pointer`] and its text, [`pointer_path`],
//! [`explicit_env`] and [`psql_is_bare`] are pure; [`location`], [`read`],
//! [`write`], [`clear_if_names`] and [`running`] touch the machine.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use rpsql::startup::{self, Invocation};

use crate::stop::{self, Owner, PIDFILE};

/// The pointer's directory under the runtime or cache directory.
pub const POINTER_DIR: &str = "pgdrop";

/// The pointer's file name.
pub const POINTER_FILE: &str = "current";

/// The pointer's first line; a file without it is not one `start` wrote.
const POINTER_MAGIC: &str = "pgdrop current 1";

/// The variables that name a cluster: `PGDATA` for `pgdrop stop` (as for
/// pg_ctl), and libpq's that choose a server, user or database
/// (`PQconninfoOptions`' `envvar`s, `src/interfaces/libpq/fe-connect.c:200`).
/// Any of them set, to anything but the empty string, keeps a bare command
/// off the pointer.
pub const EXPLICIT_ENV: [&str; 7] = [
    "PGDATA",
    "PGHOST",
    "PGHOSTADDR",
    "PGPORT",
    "PGUSER",
    "PGDATABASE",
    "PGSERVICE",
];

/// The cluster `start` started last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    /// The socket directory, or `127.0.0.1` with a TCP port.
    pub host: String,
    pub port: u16,
    /// What `start` printed as the URI.
    pub uri: String,
    /// Absolute.
    pub datadir: String,
}

impl Pointer {
    /// Pure: the file's text. The data directory is last and runs to the
    /// final newline, so any path survives the round trip; the host is a
    /// run directory `start` named, and the URI is percent-encoded, so
    /// neither holds a newline.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "{POINTER_MAGIC}\nhost={}\nport={}\nuri={}\ndatadir={}\n",
            self.host, self.port, self.uri, self.datadir
        )
    }

    /// Pure: [`Pointer::render`]'s inverse; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(POINTER_MAGIC)?.strip_prefix('\n')?;
        let (host, rest) = rest.strip_prefix("host=")?.split_once('\n')?;
        let (port, rest) = rest.strip_prefix("port=")?.split_once('\n')?;
        let (uri, rest) = rest.strip_prefix("uri=")?.split_once('\n')?;
        let datadir = rest.strip_prefix("datadir=")?.strip_suffix('\n')?;
        Some(Self {
            host: host.to_owned(),
            port: port.parse().ok()?,
            uri: uri.to_owned(),
            datadir: datadir.to_owned(),
        })
    }

    /// Pure: whether this points at the cluster in `datadir`, compared by
    /// path components (`/a/./b` is `/a/b`).
    #[must_use]
    pub fn names(&self, datadir: &Path) -> bool {
        Path::new(&self.datadir) == datadir
    }
}

/// A variable's value, if it is set to an absolute path. The XDG base
/// directory specification has relative values ignored; an empty one is
/// unset.
fn absolute(value: Option<&OsStr>) -> Option<&Path> {
    value.map(Path::new).filter(|path| path.is_absolute())
}

/// Pure: where the pointer lives, `$XDG_RUNTIME_DIR/pgdrop/current`, else
/// `$HOME/.cache/pgdrop/current`; `None` for a process with neither.
#[must_use]
pub fn pointer_path(xdg_runtime_dir: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    absolute(xdg_runtime_dir)
        .map(Path::to_path_buf)
        .or_else(|| absolute(home).map(|home| home.join(".cache")))
        .map(|dir| dir.join(POINTER_DIR).join(POINTER_FILE))
}

/// Pure: the first of [`EXPLICIT_ENV`] that `get` says is set, non-empty.
pub fn explicit_env(get: impl Fn(&str) -> Option<OsString>) -> Option<&'static str> {
    EXPLICIT_ENV
        .into_iter()
        .find(|name| get(name).is_some_and(|value| !value.is_empty()))
}

/// Pure: whether a `pgdrop psql` command line names no server, user or
/// database: psql's own parse (`startup.c`'s option loop) runs, and leaves
/// `-h`, `-p`, `-U`, `-d` and both bare words unset. A command line that
/// only prints help or a version, or that psql refuses, is not bare.
#[must_use]
pub fn psql_is_bare(args: &[OsString]) -> bool {
    match startup::plan(args) {
        Invocation::Run(session) => {
            session.host.is_none()
                && session.port.is_none()
                && session.username.is_none()
                && session.dbname.is_none()
        }
        _ => false,
    }
}

/// Action: [`pointer_path`] for this process.
#[must_use]
pub fn location() -> Option<PathBuf> {
    pointer_path(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Action: [`explicit_env`] for this process.
#[must_use]
pub fn explicit_env_here() -> Option<&'static str> {
    explicit_env(|name| std::env::var_os(name))
}

/// Why the pointer could not be used.
#[derive(Debug)]
pub enum PointerError {
    /// The file is not one `start` wrote.
    Bad(PathBuf),
    Io {
        what: &'static str,
        path: PathBuf,
        error: io::Error,
    },
}

impl std::fmt::Display for PointerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PointerError::Bad(path) => write!(
                f,
                "\"{}\" was not written by pgdrop start; remove it",
                path.display()
            ),
            PointerError::Io { what, path, error } => {
                write!(f, "could not {what} \"{}\": {error}", path.display())
            }
        }
    }
}

impl std::error::Error for PointerError {}

/// Action: the pointer at `path`, `None` when there is none.
///
/// # Errors
///
/// It cannot be read, or `start` did not write it.
pub fn read(path: &Path) -> Result<Option<Pointer>, PointerError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Pointer::parse(&text)
            .map(Some)
            .ok_or_else(|| PointerError::Bad(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(PointerError::Io {
            what: "read",
            path: path.to_path_buf(),
            error,
        }),
    }
}

/// Action: make `pointer` the current cluster: its directory created `0700`
/// if missing, the text written to a temporary file there, renamed over
/// `path`. A reader sees the old pointer or the new, never part of one.
///
/// # Errors
///
/// Any of those steps fails; the temporary file is removed.
pub fn write(path: &Path, pointer: &Pointer) -> Result<(), PointerError> {
    let io_error = |what, path: &Path| {
        let path = path.to_path_buf();
        move |error| PointerError::Io { what, path, error }
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    create_private_dirs(dir).map_err(io_error("create directory", dir))?;
    let temp = dir.join(format!(".{POINTER_FILE}.{}.tmp", std::process::id()));
    let written = std::fs::File::create(&temp)
        .and_then(|mut file| file.write_all(pointer.render().as_bytes()))
        .map_err(io_error("write", &temp))
        .and_then(|()| std::fs::rename(&temp, path).map_err(io_error("rename", &temp)));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(unix)]
fn create_private_dirs(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dirs(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Action: remove the pointer at `path` if it names the cluster in
/// `datadir`; anything else there is left alone.
///
/// # Errors
///
/// It cannot be read or removed.
pub fn clear_if_names(path: &Path, datadir: &Path) -> Result<(), PointerError> {
    match read(path) {
        Ok(Some(pointer)) if pointer.names(datadir) => match std::fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(PointerError::Io {
                what: "remove",
                path: path.to_path_buf(),
                error,
            }),
            _ => Ok(()),
        },
        Ok(_) | Err(PointerError::Bad(_)) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Action: whether a postmaster runs in `datadir`: its `postmaster.pid`
/// names a process that is there (`kill -0`) and has not exited
/// ([`stop::zombie`]).
#[must_use]
pub fn running(datadir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(datadir.join(PIDFILE)) else {
        return false;
    };
    match stop::pidfile_owner(&text) {
        Ok(Owner::Postmaster(pid)) => !stop::zombie(pid) && stop::signal(pid, "-0").is_ok(),
        _ => false,
    }
}

/// Action: `pgdrop psql ARGS`. A bare command line ([`psql_is_bare`], and
/// none of [`EXPLICIT_ENV`]) connects to the current cluster: its URI is
/// appended as the database name, which psql expands as a connection
/// string (`startup.c:277`). With no pointer, psql runs as given.
pub fn psql(
    args: &[OsString],
    stdout: &mut impl io::Write,
    stderr: &mut impl io::Write,
) -> std::process::ExitCode {
    match current_uri(args) {
        Ok(Some(uri)) => {
            let mut args = args.to_vec();
            args.push(uri.into());
            rpsql::run(&args, stdout, stderr)
        }
        Ok(None) => rpsql::run(args, stdout, stderr),
        Err(message) => {
            let _ = writeln!(stderr, "pgdrop: error: {message}");
            // psql's exit status for a connection that could not be made.
            std::process::ExitCode::from(2)
        }
    }
}

/// Action: the URI a bare `pgdrop psql` connects to; `None` when the command
/// line is not bare or there is no current cluster.
fn current_uri(args: &[OsString]) -> Result<Option<String>, String> {
    if explicit_env_here().is_some() || !psql_is_bare(args) {
        return Ok(None);
    }
    let Some(path) = location() else {
        return Ok(None);
    };
    match read(&path).map_err(|error| error.to_string())? {
        None => Ok(None),
        Some(pointer) if running(Path::new(&pointer.datadir)) => Ok(Some(pointer.uri)),
        Some(pointer) => Err(format!(
            "the cluster pgdrop start started last (data directory \"{}\") is not running; \
             start one with pgdrop start, or name a server with -h and -p or PGHOST and PGPORT",
            pointer.datadir
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer() -> Pointer {
        Pointer {
            host: "/tmp/pgdrop-1-2".into(),
            port: 5432,
            uri: "postgresql://postgres@%2Ftmp%2Fpgdrop-1-2:5432/postgres".into(),
            datadir: "/tmp/pgdrop-1-2/data".into(),
        }
    }

    #[test]
    fn the_pointer_lives_in_the_runtime_directory_else_the_cache() {
        let os = |text: &'static str| Some(OsStr::new(text));
        assert_eq!(
            pointer_path(os("/run/user/1000"), os("/home/me")),
            Some(PathBuf::from("/run/user/1000/pgdrop/current"))
        );
        assert_eq!(
            pointer_path(None, os("/home/me")),
            Some(PathBuf::from("/home/me/.cache/pgdrop/current"))
        );
        // Relative or empty values are ignored, as the XDG specification says.
        for runtime in [os(""), os("run"), None] {
            assert_eq!(
                pointer_path(runtime, os("/home/me")),
                Some(PathBuf::from("/home/me/.cache/pgdrop/current")),
                "{runtime:?}"
            );
        }
        assert_eq!(pointer_path(None, None), None);
        assert_eq!(pointer_path(os(""), os("home")), None);
    }

    #[test]
    fn the_pointer_round_trips_and_nothing_else_parses() {
        let odd = Pointer {
            datadir: "/tmp/a\ndatadir=/etc\n".into(),
            ..pointer()
        };
        for pointer in [pointer(), odd] {
            assert_eq!(Pointer::parse(&pointer.render()), Some(pointer));
        }
        assert_eq!(
            pointer().render(),
            "pgdrop current 1\nhost=/tmp/pgdrop-1-2\nport=5432\n\
             uri=postgresql://postgres@%2Ftmp%2Fpgdrop-1-2:5432/postgres\n\
             datadir=/tmp/pgdrop-1-2/data\n"
        );
        for bad in [
            "",
            "pgdrop current 2\nhost=h\nport=1\nuri=u\ndatadir=/d\n",
            "pgdrop current 1\nhost=h\nport=x\nuri=u\ndatadir=/d\n",
            "pgdrop current 1\nhost=h\nport=1\nuri=u\ndatadir=/d",
            "pgdrop current 1\nport=1\nhost=h\nuri=u\ndatadir=/d\n",
        ] {
            assert_eq!(Pointer::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_pointer_names_its_datadir_by_path_components() {
        assert!(pointer().names(Path::new("/tmp/pgdrop-1-2/data")));
        assert!(pointer().names(Path::new("/tmp/./pgdrop-1-2/data/")));
        assert!(!pointer().names(Path::new("/tmp/pgdrop-1-2")));
    }

    #[test]
    fn any_variable_that_names_a_cluster_is_explicit_but_an_empty_one() {
        assert_eq!(explicit_env(|_| None), None);
        for name in EXPLICIT_ENV {
            assert_eq!(
                explicit_env(|asked| (asked == name).then(|| OsString::from("x"))),
                Some(name)
            );
            assert_eq!(
                explicit_env(|asked| (asked == name).then(OsString::new)),
                None
            );
        }
        assert_eq!(
            explicit_env(|asked| (asked == "PGOPTIONS").then(|| OsString::from("-c x=1"))),
            None
        );
    }

    #[test]
    fn a_psql_command_line_is_bare_unless_it_names_a_server_user_or_database() {
        let bare =
            |list: &[&str]| psql_is_bare(&list.iter().map(OsString::from).collect::<Vec<_>>());
        assert!(bare(&[]));
        assert!(bare(&["-c", "select 1"]));
        assert!(bare(&[
            "-X",
            "-At",
            "-cselect 1",
            "-f",
            "x.sql",
            "-v",
            "a=b"
        ]));
        for named in [
            &["-h", "/tmp"][..],
            &["--host=/tmp"],
            &["-p", "5433"],
            &["--port", "5433"],
            &["-U", "me"],
            &["--username=me"],
            &["-d", "db"],
            &["--dbname", "postgresql://x"],
            &["db"],
            &["-c", "select 1", "db", "me"],
            // Not a session: help, a version, a refusal.
            &["--help"],
            &["-V"],
            &["--no-such-option"],
        ] {
            assert!(!bare(named), "{named:?}");
        }
    }
}
