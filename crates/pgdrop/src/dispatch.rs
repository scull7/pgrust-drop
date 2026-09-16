//! Pure routing from `argv` to an applet.
//!
//! Applet command lines (`initdb …`, `psql …`, `postgres …`) are the upstream
//! tools' own and must reach them untouched, so they never go through the
//! usage-rs parser: an applet is selected either by `argv[0]`'s basename (a
//! symlink named `initdb`) or by the first word (`pgdrop initdb`), and every
//! remaining word is handed over verbatim. Only pgdrop's own surface —
//! `start`, `--help`, `--version` — is a usage-rs command (ADR-0004).

use std::ffi::{OsStr, OsString};
use std::path::Path;

use usage::{Args, Cli, Subcommands};

/// The tools pgdrop can stand in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applet {
    Initdb,
    Psql,
    Postgres,
}

impl Applet {
    /// The executable name each applet answers to, as `argv[0]` or first word.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Applet::Initdb => "initdb",
            Applet::Psql => "psql",
            Applet::Postgres => "postgres",
        }
    }

    fn from_name(name: &OsStr) -> Option<Self> {
        match name.to_str()? {
            "initdb" => Some(Applet::Initdb),
            "psql" => Some(Applet::Psql),
            "postgres" => Some(Applet::Postgres),
            _ => None,
        }
    }
}

/// pgrust in one self-contained binary.
///
/// Run a tool by name (`pgdrop initdb …`, `pgdrop psql …`, `pgdrop postgres …`)
/// or through a symlink with that name; arguments pass through untouched.
#[derive(Cli, Debug)]
#[usage(bin = "pgdrop", version, unknown_flags = "error")]
struct Pgdrop {
    #[usage(subcommand)]
    command: Command,
}

#[derive(Subcommands, Debug)]
enum Command {
    /// Initialize a database cluster (Rust initdb; arguments as for initdb)
    Initdb,
    /// The interactive terminal (Rust psql; arguments as for psql)
    Psql,
    /// The pgrust server (arguments as for postgres)
    Postgres,
    /// Start an ephemeral cluster for a test suite
    Start(Start),
}

/// Options for `pgdrop start` (NAT-409); the shape is declared now so the
/// help page and the spec are stable.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// TCP port to listen on; 0 means unix socket only
    #[usage(long, default = "0")]
    pub port: u16,
    /// Keep the data directory when the cluster stops
    #[usage(long)]
    pub keep: bool,
}

/// Where a command line goes.
#[derive(Debug)]
pub enum Dispatch {
    /// Run this applet with these arguments (program name already stripped).
    Applet(Applet, Vec<OsString>),
    /// `pgdrop start …`
    Start(Start),
    /// Rendered root help page.
    PrintHelp(String),
    PrintVersion,
    /// Rendered usage-rs diagnostic, exit status 2.
    Unparsable(String),
}

/// Route a full `argv` (including the program name).
#[must_use]
pub fn dispatch(argv: &[OsString]) -> Dispatch {
    let Some((argv0, rest)) = argv.split_first() else {
        return root(&[]);
    };
    if let Some(applet) = applet_from_argv0(argv0) {
        return Dispatch::Applet(applet, rest.to_vec());
    }
    match rest
        .split_first()
        .and_then(|(first, tail)| Some((Applet::from_name(first)?, tail)))
    {
        Some((applet, tail)) => Dispatch::Applet(applet, tail.to_vec()),
        None => root(rest),
    }
}

/// A symlink or copy named `initdb`, `psql` or `postgres` selects that applet.
/// A trailing `.exe` is tolerated for the sake of a future Windows build.
fn applet_from_argv0(argv0: &OsStr) -> Option<Applet> {
    let path = Path::new(argv0);
    let name = path.file_name()?;
    let stem = if path.extension().is_some_and(|ext| ext == "exe") {
        path.file_stem()?
    } else {
        name
    };
    Applet::from_name(stem)
}

/// pgdrop's own surface, parsed by usage-rs.
fn root(words: &[OsString]) -> Dispatch {
    let refs: Vec<&OsStr> = words.iter().map(OsString::as_os_str).collect();
    match Pgdrop::parse_from(&refs) {
        // Applet names are routed before the parser; these arms exist so the
        // enum stays total and the help page lists every command.
        Ok(Pgdrop {
            command: Command::Initdb,
        }) => Dispatch::Applet(Applet::Initdb, Vec::new()),
        Ok(Pgdrop {
            command: Command::Psql,
        }) => Dispatch::Applet(Applet::Psql, Vec::new()),
        Ok(Pgdrop {
            command: Command::Postgres,
        }) => Dispatch::Applet(Applet::Postgres, Vec::new()),
        Ok(Pgdrop {
            command: Command::Start(start),
        }) => Dispatch::Start(start),
        Err(usage::Error::Help { cmd, long }) => Dispatch::PrintHelp(
            Pgdrop::render_help(cmd, long)
                .map(|page| page.to_string())
                .unwrap_or_default(),
        ),
        Err(usage::Error::Version { .. }) => Dispatch::PrintVersion,
        Err(err) => Dispatch::Unparsable(Pgdrop::render_failure(&refs, &err).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn expect_applet(list: &[&str], applet: Applet, args: &[&str]) {
        match dispatch(&argv(list)) {
            Dispatch::Applet(got, got_args) => {
                assert_eq!(got, applet, "{list:?}");
                assert_eq!(got_args, argv(args), "{list:?}");
            }
            other => panic!("{list:?}: {other:?}"),
        }
    }

    #[test]
    fn symlink_name_selects_the_applet_and_passes_everything_through() {
        let tail = ["-D", "x", "--help", "--", "-z"];
        for (argv0, applet) in [
            ("/usr/local/bin/initdb", Applet::Initdb),
            ("psql", Applet::Psql),
            ("./postgres", Applet::Postgres),
            ("initdb.exe", Applet::Initdb),
        ] {
            let mut list = vec![argv0];
            list.extend(tail);
            expect_applet(&list, applet, &tail);
        }
    }

    #[test]
    fn first_word_selects_the_applet_and_passes_everything_through() {
        expect_applet(
            &["pgdrop", "initdb", "-D", "x", "--version"],
            Applet::Initdb,
            &["-D", "x", "--version"],
        );
        expect_applet(
            &["pgdrop", "psql", "-X", "-c", "select 1"],
            Applet::Psql,
            &["-X", "-c", "select 1"],
        );
        expect_applet(&["pgdrop", "postgres"], Applet::Postgres, &[]);
        expect_applet(
            &["./target/debug/pgdrop", "initdb", "--help"],
            Applet::Initdb,
            &["--help"],
        );
    }

    #[test]
    fn start_parses_its_own_flags() {
        match dispatch(&argv(&["pgdrop", "start", "--port", "5433", "--keep"])) {
            Dispatch::Start(start) => assert_eq!(
                start,
                Start {
                    port: 5433,
                    keep: true
                }
            ),
            other => panic!("{other:?}"),
        }
        match dispatch(&argv(&["pgdrop", "start"])) {
            Dispatch::Start(start) => assert_eq!(
                start,
                Start {
                    port: 0,
                    keep: false
                }
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn root_help_version_and_errors() {
        match dispatch(&argv(&["pgdrop", "--help"])) {
            Dispatch::PrintHelp(text) => {
                for name in ["initdb", "psql", "postgres", "start"] {
                    assert!(text.contains(name), "help lacks {name}:\n{text}");
                }
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            dispatch(&argv(&["pgdrop", "--version"])),
            Dispatch::PrintVersion
        ));
        assert!(matches!(
            dispatch(&argv(&["pgdrop", "bogus"])),
            Dispatch::Unparsable(_)
        ));
        assert!(matches!(
            dispatch(&argv(&["pgdrop"])),
            Dispatch::Unparsable(_)
        ));
        assert!(matches!(
            dispatch(&argv(&["pgdrop", "start", "--port", "many"])),
            Dispatch::Unparsable(_)
        ));
    }

    #[test]
    fn empty_argv_is_handled() {
        assert!(matches!(dispatch(&[]), Dispatch::Unparsable(_)));
    }

    #[test]
    fn applet_names_round_trip() {
        for applet in [Applet::Initdb, Applet::Psql, Applet::Postgres] {
            assert_eq!(Applet::from_name(OsStr::new(applet.name())), Some(applet));
        }
        assert_eq!(Applet::from_name(OsStr::new("pgdrop")), None);
    }
}
