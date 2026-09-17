//! Where the C PostgreSQL 18 tools live, for the byte-diff gates.
//!
//! A gate is only meaningful when both sides of the diff link the same C
//! library: `initdb` asks libc to resolve locales, so a glibc reference and a
//! musl build disagree about output that neither implementation got wrong.
//! ADR-0007 records the measurements. Discovery is therefore keyed on the
//! [`Libc`] of *this* test binary, decided at compile time, and each lane has
//! its own environment variable so a stray export cannot cross the streams.

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

/// The message a gate prints when the reference tool is absent.
///
/// It names the lane so a skipped gate cannot be mistaken for the wrong libc
/// being installed.
#[must_use]
pub fn skip_message(tool: &str) -> String {
    let lane = Libc::HOST;
    format!(
        "SKIP (flagged, not silent): no {} reference `{tool}`; set {} (see scripts/fetch-ref-binaries.sh)",
        lane.as_str(),
        lane.env_var()
    )
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
    fn skip_message_names_the_lane_and_its_override() {
        let message = skip_message("initdb");
        assert!(message.contains(Libc::HOST.env_var()));
        assert!(message.contains(Libc::HOST.as_str()));
    }
}
