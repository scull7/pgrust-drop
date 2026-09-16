//! Where the C PostgreSQL 18 tools live, for the byte-diff gates.
//!
//! A gate is only meaningful when both sides of the diff link the same C
//! library: `initdb` asks libc to resolve locales, so a glibc reference and a
//! musl build disagree about output that neither implementation got wrong.
//! ADR-0007 records the measurements. Discovery is therefore keyed on the
//! [`Libc`] of *this* test binary, decided at compile time, and each lane has
//! its own environment variable so a stray export cannot cross the streams.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The C library a binary links against.
///
/// Derived from `cfg!` for the crate being compiled, so a test built for
/// `x86_64-unknown-linux-musl` looks for a musl reference and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    /// GNU libc: Debian, Ubuntu, RHEL.
    Gnu,
    /// musl: Alpine, and the `*-unknown-linux-musl` targets.
    Musl,
    /// Apple's libSystem on macOS.
    Apple,
}

impl Libc {
    /// The libc this test binary was compiled against.
    pub const HOST: Self = if cfg!(target_vendor = "apple") {
        Self::Apple
    } else if cfg!(target_env = "musl") {
        Self::Musl
    } else {
        Self::Gnu
    };

    /// Lane name, as used in environment variables and CI job names.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gnu => "gnu",
            Self::Musl => "musl",
            Self::Apple => "apple",
        }
    }

    /// The environment variable that names this lane's reference directory.
    #[must_use]
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Gnu => "PGDROP_REF_BIN_GNU",
            Self::Musl => "PGDROP_REF_BIN_MUSL",
            Self::Apple => "PGDROP_REF_BIN_APPLE",
        }
    }

    /// Install layouts to try when no environment variable is set.
    ///
    /// The musl paths are Alpine's `postgresql18` package layout and are
    /// confirmed by the Alpine CI lane, not by hand.
    #[must_use]
    pub const fn default_dirs(self) -> &'static [&'static str] {
        match self {
            Self::Gnu => &["/usr/lib/postgresql/18/bin", "/usr/pgsql-18/bin"],
            Self::Musl => &["/usr/libexec/postgresql18", "/usr/lib/postgresql18/bin"],
            Self::Apple => &[
                "/opt/homebrew/opt/postgresql@18/bin",
                "/usr/local/opt/postgresql@18/bin",
                "/Applications/Postgres.app/Contents/Versions/18/bin",
            ],
        }
    }
}

/// Lane-agnostic override, tried after the lane's own variable.
///
/// It carries no libc of its own, so setting it is an assertion that the
/// directory matches [`Libc::HOST`]. Prefer [`Libc::env_var`] in CI.
pub const REF_BIN_ENV: &str = "PGDROP_REF_BIN";

/// Pure: pick the first candidate directory containing `tool`.
///
/// `env_dirs` are the overrides in priority order; `exists` answers whether a
/// path is an executable file, so the search is unit-testable.
pub fn locate<'a>(
    tool: &str,
    env_dirs: impl IntoIterator<Item = &'a str>,
    candidates: impl IntoIterator<Item = &'a str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    env_dirs
        .into_iter()
        .chain(candidates)
        .map(|dir| Path::new(dir).join(tool))
        .find(|path| exists(path))
}

/// Action: find the reference `tool` for this binary's libc lane, or `None`.
///
/// Tests that get `None` must print `SKIP (flagged, not silent)` and pass; see
/// `docs/test-stealing.md`.
#[must_use]
pub fn find(tool: &str) -> Option<PathBuf> {
    let lane = std::env::var(Libc::HOST.env_var()).ok();
    let generic = std::env::var(REF_BIN_ENV).ok();
    let overrides: Vec<&str> = lane
        .iter()
        .chain(generic.iter())
        .map(String::as_str)
        .collect();
    locate(
        tool,
        overrides,
        Libc::HOST.default_dirs().iter().copied(),
        Path::is_file,
    )
}

/// Set in CI to turn a missing reference into a failure instead of a skip.
///
/// A skipped gate and a passing gate look identical in a CI summary, so the
/// lane that is supposed to prove conformance can quietly stop proving it —
/// exactly the silent narrowing `AGENTS.md` forbids. CI sets this; a laptop
/// without the binaries still skips.
pub const REQUIRE_REF_ENV: &str = "PGDROP_REQUIRE_REF";

/// Pure: does a missing reference fail the gate, given the variable's value?
///
/// Absent or `0` means skip; any other value means require.
#[must_use]
pub fn is_required(setting: Option<&str>) -> bool {
    matches!(setting, Some(value) if value != "0")
}

/// Action: the reference `tool`, or `None` when the gate may skip.
///
/// # Panics
///
/// When [`REQUIRE_REF_ENV`] demands the reference and it is not installed, so
/// a lane cannot report success without having run the diff.
#[must_use]
pub fn find_or_skip(tool: &str) -> Option<PathBuf> {
    let found = find(tool);
    assert!(
        !(found.is_none() && is_required(std::env::var(REQUIRE_REF_ENV).ok().as_deref())),
        "{REQUIRE_REF_ENV} is set: {}",
        skip_message(tool)
    );
    found
}

/// The flag every skipped gate carries, so skips are greppable in a log.
pub const SKIP_FLAG: &str = "SKIP (flagged, not silent)";

/// The message a gate prints when the reference tool is absent.
///
/// It names the lane so a skipped gate cannot be mistaken for the wrong libc
/// being installed.
#[must_use]
pub fn skip_message(tool: &str) -> String {
    format!("{SKIP_FLAG}: reference `{tool}` not found; set {REF_BIN_ENV} or install PostgreSQL 18")
}

/// Action: put a flagged skip on screen.
///
/// Not `println!`/`eprintln!`: libtest captures both and replays them only for
/// a *failing* test or under `--nocapture`, so a skip announced that way is
/// invisible in the CI log of a passing run — a silently narrowed gate, which
/// is exactly what AGENTS.md forbids. Writing to the process's own stderr
/// handle goes around the capture, so the flag always shows.
pub fn announce_skip(reason: &str) {
    // Nothing useful to do if stderr is closed; the test still passes.
    let _ = writeln!(std::io::stderr().lock(), "{reason}");
}

/// Action: announce that a gate is skipped because reference `tool` is absent.
pub fn skip(tool: &str) {
    announce_skip(&skip_message(tool));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_dir_wins_over_defaults() {
        let found = locate("initdb", ["/env/bin"], ["/default/bin"], |p| {
            p == Path::new("/env/bin/initdb") || p == Path::new("/default/bin/initdb")
        });
        assert_eq!(found, Some(PathBuf::from("/env/bin/initdb")));
    }

    #[test]
    fn lane_env_wins_over_the_generic_one() {
        let found = locate("initdb", ["/lane/bin", "/generic/bin"], [], |p| {
            p.starts_with("/lane") || p.starts_with("/generic")
        });
        assert_eq!(found, Some(PathBuf::from("/lane/bin/initdb")));
    }

    #[test]
    fn falls_through_to_the_first_existing_default() {
        let found = locate("psql", [], ["/a/bin", "/b/bin"], |p| {
            p == Path::new("/b/bin/psql")
        });
        assert_eq!(found, Some(PathBuf::from("/b/bin/psql")));
    }

    #[test]
    fn none_when_nothing_exists() {
        assert_eq!(locate("psql", ["/x"], ["/y"], |_| false), None);
    }

    #[test]
    fn host_libc_matches_the_compiled_target() {
        let expected = if cfg!(target_vendor = "apple") {
            Libc::Apple
        } else if cfg!(target_env = "musl") {
            Libc::Musl
        } else {
            Libc::Gnu
        };
        assert_eq!(Libc::HOST, expected);
    }

    #[test]
    fn every_lane_has_its_own_variable_and_dirs() {
        let lanes = [Libc::Gnu, Libc::Musl, Libc::Apple];
        for (i, a) in lanes.iter().enumerate() {
            assert!(!a.default_dirs().is_empty(), "{} has no dirs", a.as_str());
            for b in &lanes[i + 1..] {
                assert_ne!(a.env_var(), b.env_var());
                assert_ne!(a.as_str(), b.as_str());
            }
        }
    }

    #[test]
    fn a_missing_reference_only_fails_when_ci_demands_it() {
        assert!(!is_required(None));
        assert!(!is_required(Some("0")));
        assert!(is_required(Some("1")));
        assert!(is_required(Some("true")));
    }

    #[test]
    fn skip_message_names_the_lane_and_its_override() {
        let message = skip_message("initdb");
        assert!(message.contains(Libc::HOST.env_var()));
        assert!(message.contains(Libc::HOST.as_str()));
    }

    #[test]
    fn skip_message_carries_the_flag() {
        assert!(skip_message("initdb").starts_with(SKIP_FLAG));
    }
}
