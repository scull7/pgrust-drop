//! `pgdrop start`: an ephemeral cluster for a test suite (Linear NAT-409).
//!
//! `start` mints a data directory from the embedded template, runs pgrust's
//! server on a Unix socket in a directory of its own (and on a TCP port when
//! asked), waits until it accepts a connection, and prints how to reach it.
//! Nothing upstream does this in one step; the nearest are
//! `PostgreSQL::Test::Cluster`'s `init` and `start`
//! (`src/test/perl/PostgreSQL/Test/Cluster.pm:602`), and that is what the
//! settings here are modelled on.
//!
//! Data / Calculations / Actions:
//!
//! - [`Start`] is the command line, [`Setting`] and [`Listen`] the typed
//!   pieces of it.
//! - [`plan`] is the whole decision, a pure function of the flags and of
//!   what the caller already knows about the machine: the working directory,
//!   the fresh run directory it made, and whether `--datadir` already holds a
//!   cluster. It returns a [`StartPlan`]: the `initdb` and `postgres` command
//!   lines, the connection URI, and what `stop` may delete.
//! - The actions — creating the run directory, minting, spawning, polling
//!   for readiness, recording the PID for `stop` — are the next slice of
//!   NAT-409; until they land, `pgdrop start` reports that it is not
//!   implemented.
//!
//! ## The run directory
//!
//! Every `start` gets a fresh directory, `<tmp>/pgdrop-<pid>-<nonce>`, made
//! with `create_dir` so two concurrent starts can never share one. It is the
//! server's only `unix_socket_directories` entry, so two clusters never
//! collide on a socket even though both use port 5432's socket name, and it
//! holds the default data directory, `<run>/data`. `--keep` leaves it all in
//! place on `stop`.

use std::fmt::{self, Write as _};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use usage::Args;

/// Options for `pgdrop start`.
// One field per flag, as the issue specifies them; the typed [`StartPlan`]
// built from it is where the switches stop being bools.
#[allow(clippy::struct_excessive_bools)]
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct Start {
    /// Data directory to use; created from the template unless it already holds a cluster (default: a new temporary one)
    #[usage(long, value_name = "DIR")]
    pub datadir: Option<PathBuf>,
    /// TCP port to listen on, on 127.0.0.1; 0 means unix socket only
    #[usage(long, default = "0")]
    pub port: u16,
    /// Run the server in the foreground instead of detaching it
    #[usage(long)]
    pub foreground: bool,
    /// Print {"uri": …, "pid": …, "datadir": …} on stdout
    #[usage(long)]
    pub json: bool,
    /// Keep the data directory when the cluster stops
    #[usage(long)]
    pub keep: bool,
    /// Set a server parameter, as postgres -c NAME=VALUE (repeatable)
    #[usage(long, value_name = "NAME=VALUE")]
    pub set: Vec<String>,
}

/// The superuser `start` mints the cluster with and connects as, so the URI
/// is the same whoever runs it.
pub const SUPERUSER: &str = "postgres";

/// The database the URI names; `initdb` always creates it.
pub const DATABASE: &str = "postgres";

/// Port number the socket file is named for when there is no TCP port:
/// `DEF_PGPORT`, the server's default `port`.
pub const UNIX_ONLY_PORT: u16 = 5432;

/// The address a TCP listener binds, as `PostgreSQL::Test::Cluster` does
/// (`$test_localhost`, `Cluster.pm:143`).
pub const TCP_HOST: &str = "127.0.0.1";

/// `sizeof(((struct sockaddr_un *) NULL)->sun_path)`, the bound the server
/// holds a socket path to (`UNIXSOCK_PATH_BUFLEN`,
/// `src/include/libpq/pqcomm.h:60`; checked at
/// `src/backend/libpq/pqcomm.c:454`).
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
pub const UNIXSOCK_PATH_BUFLEN: usize = 104;
/// See the other definition: 108 on Linux.
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
pub const UNIXSOCK_PATH_BUFLEN: usize = 108;

/// The settings every `start` passes, after `--profile test` and before the
/// user's `--set`s. pgrust's `--profile test` (`main_main::PROFILE_TEST_SETTINGS`)
/// already turns off `fsync`, `synchronous_commit`, `full_page_writes` and
/// `autovacuum` and sets `wal_level = minimal`, as `Cluster.pm:685`-`:723`
/// does for a test node; it leaves `shared_buffers` at 128MB, and a test
/// cluster that may run beside others wants less (`Cluster.pm:714`'s
/// "conservative settings to ensure we can run multiple postmasters").
pub const PROFILE: &[(&str, &str)] = &[("shared_buffers", "16MB")];

/// Parameters `start` sets itself: the URI it prints depends on them, so a
/// `--set` of one is refused rather than silently breaking that URI.
pub const OWNED: &[(&str, &str)] = &[
    ("port", "--port"),
    ("listen_addresses", "--port"),
    ("unix_socket_directories", "the run directory"),
];

/// One `--set NAME=VALUE`, split at the first `=` as `postgres -c` splits it
/// (`ParseLongOption`, `src/backend/utils/misc/guc.c:6368`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub name: String,
    pub value: String,
}

impl Setting {
    /// Pure: split one `--set` argument.
    ///
    /// # Errors
    ///
    /// No `=` (`initdb.c:3272`'s "-c %s requires a value"), an empty name,
    /// or a name `start` sets itself ([`OWNED`]).
    pub fn parse(raw: &str) -> Result<Self, StartError> {
        let Some((name, value)) = raw.split_once('=') else {
            return Err(StartError::SetWithoutValue(raw.to_owned()));
        };
        if name.is_empty() {
            return Err(StartError::SetWithoutName(raw.to_owned()));
        }
        let canonical = canonical_name(name);
        if let Some((owned, flag)) = OWNED.iter().find(|(owned, _)| *owned == canonical) {
            return Err(StartError::SetOwned {
                name: (*owned).to_owned(),
                by: flag,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }

    /// The `-c` argument, `NAME=VALUE`.
    #[must_use]
    pub fn to_arg(&self) -> String {
        format!("{}={}", self.name, self.value)
    }
}

/// Pure: a parameter name as the server compares it: `-` read as `_`
/// (`guc.c:6393`) and case folded (`guc_name_compare`).
fn canonical_name(name: &str) -> String {
    name.replace('-', "_").to_ascii_lowercase()
}

/// Where the server listens besides its socket directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listen {
    /// `--port 0`: no TCP at all (`listen_addresses = ''`).
    UnixOnly,
    /// `--port N`: [`TCP_HOST`] port N as well.
    Tcp(NonZeroU16),
}

impl Listen {
    #[must_use]
    pub fn from_port(port: u16) -> Self {
        NonZeroU16::new(port).map_or(Listen::UnixOnly, Listen::Tcp)
    }

    /// The server's `port`, which also names the socket file.
    #[must_use]
    pub fn port(self) -> u16 {
        match self {
            Listen::UnixOnly => UNIX_ONLY_PORT,
            Listen::Tcp(port) => port.get(),
        }
    }

    /// The server's `listen_addresses`.
    #[must_use]
    pub fn listen_addresses(self) -> &'static str {
        match self {
            Listen::UnixOnly => "",
            Listen::Tcp(_) => TCP_HOST,
        }
    }
}

/// How the data directory came to be, which decides what `stop` removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `start` runs `initdb` into it; `stop` removes it unless `--keep`.
    Minted,
    /// `--datadir` already held a cluster; `stop` never removes it.
    Existing,
}

/// Everything `start` will do, decided before it does any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPlan {
    /// `<tmp>/pgdrop-<pid>-<nonce>`: the socket directory.
    pub run_dir: String,
    /// Absolute.
    pub datadir: String,
    pub origin: Origin,
    pub listen: Listen,
    pub settings: Vec<Setting>,
    pub foreground: bool,
    pub json: bool,
    /// `stop` leaves the run directory and a minted data directory alone.
    pub keep: bool,
}

/// Pure: the run directory's name, unique per process and per attempt.
#[must_use]
pub fn run_dir_name(pid: u32, nonce: u64) -> String {
    format!("pgdrop-{pid}-{nonce:x}")
}

/// Pure: the plan for `flags`.
///
/// `cwd` makes a relative `--datadir` absolute (the URI and `--json` name
/// it, and a caller may be in another directory); `run_dir` is the fresh
/// directory the caller created; `datadir_holds_cluster` is whether
/// `--datadir` already has a `PG_VERSION` (a default datadir never does).
///
/// # Errors
///
/// A bad `--set`, a path that is not UTF-8 (the server takes its command
/// line as text, `main_main::argv_from_os`), or a socket path too long for
/// `sun_path`.
pub fn plan(
    flags: &Start,
    cwd: &Path,
    run_dir: &Path,
    datadir_holds_cluster: bool,
) -> Result<StartPlan, StartError> {
    let settings = flags
        .set
        .iter()
        .map(|raw| Setting::parse(raw))
        .collect::<Result<Vec<_>, _>>()?;
    let listen = Listen::from_port(flags.port);
    let run_dir = utf8(cwd.join(run_dir))?;
    let socket = socket_path(&run_dir, listen.port());
    if socket.len() >= UNIXSOCK_PATH_BUFLEN {
        return Err(StartError::SocketPathTooLong {
            path: socket,
            max: UNIXSOCK_PATH_BUFLEN - 1,
        });
    }
    let (datadir, origin) = match &flags.datadir {
        Some(dir) if datadir_holds_cluster => (cwd.join(dir), Origin::Existing),
        Some(dir) => (cwd.join(dir), Origin::Minted),
        None => (Path::new(&run_dir).join("data"), Origin::Minted),
    };
    Ok(StartPlan {
        datadir: utf8(datadir)?,
        run_dir,
        origin,
        listen,
        settings,
        foreground: flags.foreground,
        json: flags.json,
        keep: flags.keep,
    })
}

fn utf8(path: PathBuf) -> Result<String, StartError> {
    path.into_os_string()
        .into_string()
        .map_err(|raw| StartError::NotUtf8(raw.into()))
}

/// Pure: `UNIXSOCK_PATH` (`src/include/libpq/pqcomm.h:44`),
/// `<dir>/.s.PGSQL.<port>`.
#[must_use]
pub fn socket_path(dir: &str, port: u16) -> String {
    format!("{dir}/.s.PGSQL.{port}")
}

impl StartPlan {
    /// The `initdb` arguments that mint the data directory, as
    /// `Cluster.pm:643` runs it (`--no-sync`, `--auth trust`), plus a fixed
    /// superuser for a fixed URI; `None` for an existing cluster.
    #[must_use]
    pub fn initdb_args(&self) -> Option<Vec<String>> {
        match self.origin {
            Origin::Existing => None,
            Origin::Minted => Some(vec![
                "--no-sync".to_owned(),
                "--no-instructions".to_owned(),
                "--auth".to_owned(),
                "trust".to_owned(),
                "--username".to_owned(),
                SUPERUSER.to_owned(),
                "--pgdata".to_owned(),
                self.datadir.clone(),
            ]),
        }
    }

    /// The `postgres` arguments: `--profile test`, then [`PROFILE`], then the
    /// listener (`Cluster.pm:726`-`:736`), then the user's `--set`s. pgrust
    /// expands `--profile` into `-c`s in place and a later `-c` wins, so each
    /// group can override the one before it.
    #[must_use]
    pub fn server_args(&self) -> Vec<String> {
        let mut args = vec![
            "-D".to_owned(),
            self.datadir.clone(),
            "--profile".to_owned(),
            "test".to_owned(),
        ];
        let listener = [
            ("port", self.listen.port().to_string()),
            (
                "listen_addresses",
                self.listen.listen_addresses().to_owned(),
            ),
            ("unix_socket_directories", quote_directory(&self.run_dir)),
        ];
        let settings = PROFILE
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .chain(
                listener
                    .iter()
                    .map(|(name, value)| format!("{name}={value}")),
            )
            .chain(self.settings.iter().map(Setting::to_arg));
        for setting in settings {
            args.push("-c".to_owned());
            args.push(setting);
        }
        args
    }

    /// The connection URI. With a TCP port, `postgresql://postgres@127.0.0.1:N/postgres`;
    /// otherwise the socket directory, percent-encoded as the host
    /// (`doc/src/sgml/libpq.sgml:1067`).
    #[must_use]
    pub fn uri(&self) -> String {
        let host = match self.listen {
            Listen::UnixOnly => percent_encode(&self.run_dir),
            Listen::Tcp(_) => TCP_HOST.to_owned(),
        };
        format!(
            "postgresql://{SUPERUSER}@{host}:{}/{DATABASE}",
            self.listen.port()
        )
    }

    /// The `--json` line for a server with process ID `pid`.
    #[must_use]
    pub fn json(&self, pid: u32) -> String {
        format!(
            "{{\"uri\": {}, \"pid\": {pid}, \"datadir\": {}}}\n",
            json_string(&self.uri()),
            json_string(&self.datadir)
        )
    }

    /// Whether `stop` removes the data directory.
    #[must_use]
    pub fn removes_datadir(&self) -> bool {
        self.origin == Origin::Minted && !self.keep
    }
}

/// Pure: one `unix_socket_directories` entry. Bare unless it needs the
/// double quotes `SplitDirectoriesString` understands
/// (`src/backend/utils/adt/varlena.c:3708`): a comma would split it, a
/// leading `"` would start a quoted name, and whitespace at either end would
/// be trimmed. Inside quotes, `""` is one `"`.
#[must_use]
pub fn quote_directory(dir: &str) -> String {
    let needs = dir.is_empty()
        || dir.contains(',')
        || dir.starts_with('"')
        || dir.starts_with(char::is_whitespace)
        || dir.ends_with(char::is_whitespace);
    if needs {
        format!("\"{}\"", dir.replace('"', "\"\""))
    } else {
        dir.to_owned()
    }
}

/// Pure: RFC 3986 percent-encoding of everything but the unreserved
/// characters, which libpq decodes in every URI component
/// (`conninfo_uri_decode`, `src/interfaces/libpq/fe-connect.c:7186`).
#[must_use]
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Pure: a JSON string literal (RFC 8259 §7).
#[must_use]
pub fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Why `start` refused its command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// `--set NAME` without `=VALUE`.
    SetWithoutValue(String),
    /// `--set =VALUE`.
    SetWithoutName(String),
    /// `--set` of a parameter `start` sets itself.
    SetOwned { name: String, by: &'static str },
    /// A path the server could not be given as text.
    NotUtf8(PathBuf),
    /// `<run>/.s.PGSQL.<port>` does not fit `sun_path`.
    SocketPathTooLong { path: String, max: usize },
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::SetWithoutValue(raw) => write!(f, "--set {raw} requires a value"),
            StartError::SetWithoutName(raw) => {
                write!(f, "--set {raw} requires a parameter name")
            }
            StartError::SetOwned { name, by } => {
                write!(f, "--set cannot change {name}: it is set by {by}")
            }
            StartError::NotUtf8(path) => {
                write!(f, "path \"{}\" is not valid UTF-8", path.display())
            }
            StartError::SocketPathTooLong { path, max } => write!(
                f,
                "Unix-domain socket path \"{path}\" is too long (maximum {max} bytes); \
                 set TMPDIR to a shorter directory"
            ),
        }
    }
}

impl std::error::Error for StartError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> Start {
        Start::default()
    }

    fn plan_in(flags: &Start) -> StartPlan {
        plan(
            flags,
            Path::new("/work"),
            Path::new("/tmp/pgdrop-7-1f"),
            false,
        )
        .expect("a plan")
    }

    #[test]
    fn a_set_splits_at_the_first_equals_sign() {
        assert_eq!(
            Setting::parse("work_mem=64MB").unwrap(),
            Setting {
                name: "work_mem".into(),
                value: "64MB".into()
            }
        );
        assert_eq!(
            Setting::parse("search_path=a=b").unwrap(),
            Setting {
                name: "search_path".into(),
                value: "a=b".into()
            }
        );
        assert_eq!(Setting::parse("log_line_prefix=").unwrap().value, "");
        assert_eq!(
            Setting::parse("work_mem"),
            Err(StartError::SetWithoutValue("work_mem".into()))
        );
        assert_eq!(
            Setting::parse("=1"),
            Err(StartError::SetWithoutName("=1".into()))
        );
    }

    #[test]
    fn a_set_of_a_parameter_start_owns_is_refused_however_it_is_spelled() {
        for raw in [
            "port=1",
            "PORT=1",
            "listen_addresses=*",
            "listen-addresses=*",
            "Unix_Socket_Directories=/x",
        ] {
            assert!(
                matches!(Setting::parse(raw), Err(StartError::SetOwned { .. })),
                "{raw}"
            );
        }
        assert!(Setting::parse("portal=1").is_ok());
    }

    #[test]
    fn port_zero_is_unix_only_on_the_default_socket_name() {
        assert_eq!(Listen::from_port(0), Listen::UnixOnly);
        assert_eq!(Listen::UnixOnly.port(), 5432);
        assert_eq!(Listen::UnixOnly.listen_addresses(), "");
        let tcp = Listen::from_port(5433);
        assert_eq!(tcp.port(), 5433);
        assert_eq!(tcp.listen_addresses(), "127.0.0.1");
    }

    #[test]
    fn the_default_datadir_is_minted_inside_the_run_directory() {
        let plan = plan_in(&flags());
        assert_eq!(plan.run_dir, "/tmp/pgdrop-7-1f");
        assert_eq!(plan.datadir, "/tmp/pgdrop-7-1f/data");
        assert_eq!(plan.origin, Origin::Minted);
        assert!(plan.removes_datadir());
        let kept = plan_in(&Start {
            keep: true,
            ..flags()
        });
        assert!(!kept.removes_datadir());
    }

    #[test]
    fn a_named_datadir_is_made_absolute_and_an_existing_cluster_is_never_removed() {
        let named = Start {
            datadir: Some("rel/data".into()),
            ..flags()
        };
        let minted = plan_in(&named);
        assert_eq!(minted.datadir, "/work/rel/data");
        assert_eq!(minted.origin, Origin::Minted);
        assert!(minted.removes_datadir());

        let existing = plan(&named, Path::new("/work"), Path::new("/tmp/r"), true).unwrap();
        assert_eq!(existing.origin, Origin::Existing);
        assert_eq!(existing.initdb_args(), None);
        assert!(!existing.removes_datadir());
    }

    #[test]
    fn initdb_runs_as_the_test_harness_runs_it() {
        assert_eq!(
            plan_in(&flags()).initdb_args().unwrap(),
            [
                "--no-sync",
                "--no-instructions",
                "--auth",
                "trust",
                "--username",
                "postgres",
                "--pgdata",
                "/tmp/pgdrop-7-1f/data",
            ]
        );
    }

    #[test]
    fn the_server_gets_the_profile_then_the_listener_then_the_user_settings() {
        let plan = plan_in(&Start {
            set: vec!["work_mem=64MB".into(), "shared_buffers=1MB".into()],
            ..flags()
        });
        assert_eq!(
            plan.server_args(),
            [
                "-D",
                "/tmp/pgdrop-7-1f/data",
                "--profile",
                "test",
                "-c",
                "shared_buffers=16MB",
                "-c",
                "port=5432",
                "-c",
                "listen_addresses=",
                "-c",
                "unix_socket_directories=/tmp/pgdrop-7-1f",
                "-c",
                "work_mem=64MB",
                "-c",
                "shared_buffers=1MB",
            ]
        );
        let tcp = plan_in(&Start {
            port: 6543,
            ..flags()
        });
        let args = tcp.server_args();
        assert!(args.contains(&"port=6543".to_owned()), "{args:?}");
        assert!(
            args.contains(&"listen_addresses=127.0.0.1".to_owned()),
            "{args:?}"
        );
    }

    #[test]
    fn a_bad_set_refuses_the_whole_plan() {
        let bad = Start {
            set: vec!["work_mem=1MB".into(), "fsync".into()],
            ..flags()
        };
        assert_eq!(
            plan(&bad, Path::new("/"), Path::new("/tmp/r"), false),
            Err(StartError::SetWithoutValue("fsync".into()))
        );
    }

    #[test]
    fn a_socket_path_that_overflows_sun_path_is_refused() {
        // `<dir>/.s.PGSQL.5432` is 14 bytes longer than `<dir>`.
        let fits = format!("/{}", "d".repeat(UNIXSOCK_PATH_BUFLEN - 16));
        assert!(plan(&flags(), Path::new("/"), Path::new(&fits), false).is_ok());
        let long = format!("{fits}d");
        match plan(&flags(), Path::new("/"), Path::new(&long), false) {
            Err(StartError::SocketPathTooLong { path, max }) => {
                assert_eq!(path.len(), UNIXSOCK_PATH_BUFLEN);
                assert_eq!(max, UNIXSOCK_PATH_BUFLEN - 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn socket_directories_are_quoted_only_when_split_directories_string_needs_it() {
        assert_eq!(quote_directory("/tmp/pgdrop-1-2"), "/tmp/pgdrop-1-2");
        assert_eq!(quote_directory("/tmp/a b"), "/tmp/a b");
        assert_eq!(quote_directory("/tmp/a,b"), "\"/tmp/a,b\"");
        assert_eq!(quote_directory("/tmp/x "), "\"/tmp/x \"");
        assert_eq!(quote_directory("\"q\",x"), "\"\"\"q\"\",x\"");
    }

    #[test]
    fn the_uri_names_the_socket_directory_or_the_tcp_port() {
        assert_eq!(
            plan_in(&flags()).uri(),
            "postgresql://postgres@%2Ftmp%2Fpgdrop-7-1f:5432/postgres"
        );
        assert_eq!(
            plan_in(&Start {
                port: 6543,
                ..flags()
            })
            .uri(),
            "postgresql://postgres@127.0.0.1:6543/postgres"
        );
    }

    /// The URI means what it says to libpq's own parser (rlibpq's port of
    /// `conninfo_uri_parse`), even for a socket directory with characters a
    /// URI reserves.
    #[test]
    fn the_uri_round_trips_through_libpqs_parser() {
        let odd = plan(
            &flags(),
            Path::new("/"),
            Path::new("/tmp/odd dir:@?#%/pgdrop-1-2"),
            false,
        )
        .unwrap();
        let info = rlibpq::conninfo::parse_conninfo(odd.uri().as_bytes()).expect("a valid URI");
        assert_eq!(info.get("host"), Some(odd.run_dir.as_bytes()));
        assert_eq!(info.get("port"), Some(&b"5432"[..]));
        assert_eq!(info.get("user"), Some(&b"postgres"[..]));
        assert_eq!(info.get("dbname"), Some(&b"postgres"[..]));
    }

    #[test]
    fn json_carries_uri_pid_and_datadir() {
        assert_eq!(
            plan_in(&flags()).json(4242),
            "{\"uri\": \"postgresql://postgres@%2Ftmp%2Fpgdrop-7-1f:5432/postgres\", \
             \"pid\": 4242, \"datadir\": \"/tmp/pgdrop-7-1f/data\"}\n"
        );
        assert_eq!(json_string("a\"b\\c\n\u{1}é"), "\"a\\\"b\\\\c\\n\\u0001é\"");
    }

    #[test]
    fn run_directories_are_named_for_the_process_and_a_nonce() {
        assert_eq!(run_dir_name(7, 31), "pgdrop-7-1f");
        assert_ne!(run_dir_name(7, 31), run_dir_name(8, 31));
        assert_ne!(run_dir_name(7, 31), run_dir_name(7, 32));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_datadir_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/\xff");
        let bad = Start {
            datadir: Some(raw.into()),
            ..flags()
        };
        assert_eq!(
            plan(&bad, Path::new("/"), Path::new("/tmp/r"), false),
            Err(StartError::NotUtf8(raw.into()))
        );
    }
}
