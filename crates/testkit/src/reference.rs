//! Where the C PostgreSQL 18 tools live, for the byte-diff gates.
//!
//! A gate is only meaningful when both sides of the diff link the same C
//! library: `initdb` asks libc to resolve locales, so a glibc reference and a
//! musl build disagree about output that neither implementation got wrong.
//! ADR-0007 records the measurements. Discovery is therefore keyed on the
//! [`Libc`] of *this* test binary, decided at compile time, and each lane has
//! its own environment variable so a stray export cannot cross the streams.
//! The lane-agnostic [`REF_BIN_ENV`] is still honoured, after the lane's own
//! variable, as an assertion by whoever set it that the directory matches.
//!
//! A machine without PostgreSQL 18 cannot run a gate at all, so by default a
//! missing reference is a flagged skip and the test passes. That default is
//! right for a laptop and wrong for CI, where a skipped gate proves nothing
//! while looking green. [`REQUIRE_REF_ENV`] is the opt-in that turns the skip
//! into a failure; CI sets it.

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
    /// The gnu paths are PGDG's Debian/Ubuntu and RHEL layouts; the musl paths
    /// are Alpine's `postgresql18` package layout, confirmed by the Alpine CI
    /// lane rather than by hand; the Apple paths are Homebrew's and
    /// Postgres.app's.
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

/// Lane-agnostic override, tried after the lane's own variable
/// ([`Libc::env_var`]).
///
/// It carries no libc of its own, so setting it is an assertion that the
/// directory matches [`Libc::HOST`]. Prefer the lane variable in CI.
pub const REF_BIN_ENV: &str = "PGDROP_REF_BIN";

/// Environment variable that makes a missing reference binary fatal.
///
/// Set it to `1` or `true` (see [`policy_from_env`]) and a gate whose
/// reference tool is absent fails instead of skipping. Unset — the local
/// default — behaviour is unchanged.
pub const REQUIRE_REF_ENV: &str = "PGDROP_REQUIRE_REF";

/// Tools no PostgreSQL distribution packages, so [`RefPolicy::Require`] cannot
/// demand them.
///
/// `libpq_uri_regress` is built from `src/interfaces/libpq/test/` only when the
/// source tree's own test suite is built; no distribution's server or client
/// package installs it. Its gate keeps flagged-skipping until something builds
/// it from source (NAT-374).
pub const UNSHIPPED_TOOLS: [&str; 1] = ["libpq_uri_regress"];

/// Data: what a gate must do when its reference binary is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPolicy {
    /// The default: announce a flagged skip and pass.
    Skip,
    /// [`REQUIRE_REF_ENV`] is set: a missing reference is a failure.
    Require,
}

/// Data: the decision a policy reaches about one absent tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingRef {
    /// Announce `SKIP (flagged, not silent)` and let the test pass.
    Announce,
    /// Fail the test: this reference was required and is not installed.
    Fail,
}

/// Pure: read a [`RefPolicy`] out of [`REQUIRE_REF_ENV`]'s raw value.
///
/// `1` and `true` (either case) opt in; unset, empty, `0` and anything else
/// keep the permissive default, so a stray value never turns CI silently
/// strict or silently lax.
#[must_use]
pub fn policy_from_env(raw: Option<&str>) -> RefPolicy {
    let opted_in = raw
        .map(str::trim)
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if opted_in {
        RefPolicy::Require
    } else {
        RefPolicy::Skip
    }
}

/// Pure: what to do about `tool` being missing under `policy`.
///
/// [`UNSHIPPED_TOOLS`] are exempt: requiring a binary no package ships would
/// only make CI red for a reason no one can fix.
#[must_use]
pub fn missing_ref_action(policy: RefPolicy, tool: &str) -> MissingRef {
    match policy {
        RefPolicy::Skip => MissingRef::Announce,
        RefPolicy::Require if UNSHIPPED_TOOLS.contains(&tool) => MissingRef::Announce,
        RefPolicy::Require => MissingRef::Fail,
    }
}

/// Pure: the directories [`locate`] consults, in order: the overrides
/// (lane variable, then the generic one), then the lane's install layouts.
#[must_use]
pub fn searched_dirs<'a>(
    env_dirs: impl IntoIterator<Item = &'a str>,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    env_dirs.into_iter().chain(candidates).collect()
}

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

/// Action: the override directories this process was given, lane variable
/// first, then [`REF_BIN_ENV`].
fn override_dirs() -> Vec<String> {
    [Libc::HOST.env_var(), REF_BIN_ENV]
        .into_iter()
        .filter_map(|var| std::env::var(var).ok())
        .collect()
}

/// Action: find the reference `tool` for this binary's libc lane, or `None`.
///
/// Tests that get `None` must hand the tool to [`skip`], which applies the
/// active [`RefPolicy`]; see `docs/test-stealing.md`.
#[must_use]
pub fn find(tool: &str) -> Option<PathBuf> {
    let overrides = override_dirs();
    locate(
        tool,
        overrides.iter().map(String::as_str),
        Libc::HOST.default_dirs().iter().copied(),
        Path::is_file,
    )
}

/// Action: the reference `tool`, or `None` when the gate may skip — with the
/// skip already announced, or the test already failed, by [`skip`].
///
/// # Panics
///
/// When [`REQUIRE_REF_ENV`] demands the reference and it is not installed, so
/// a lane cannot report success without having run the diff.
#[must_use]
pub fn find_or_skip(tool: &str) -> Option<PathBuf> {
    let found = find(tool);
    if found.is_none() {
        skip(tool);
    }
    found
}

/// The flag every skipped gate carries, so skips are greppable in a log.
pub const SKIP_FLAG: &str = "SKIP (flagged, not silent)";

/// The message a gate prints when the reference tool is absent.
///
/// It names the lane so a skipped gate cannot be mistaken for the wrong libc
/// being installed, and both variables that would have found the tool.
#[must_use]
pub fn skip_message(tool: &str) -> String {
    let lane = Libc::HOST;
    format!(
        "{SKIP_FLAG}: no {} reference `{tool}`; set {} or {REF_BIN_ENV}, or install PostgreSQL 18 (see scripts/fetch-ref-binaries.sh)",
        lane.as_str(),
        lane.env_var()
    )
}

/// Pure: the message a gate fails with under [`RefPolicy::Require`].
///
/// It names the tool and every directory searched, because the only useful
/// answer to this failure is "install PostgreSQL 18 there, or point the lane
/// variable somewhere it is".
#[must_use]
pub fn require_message(tool: &str, searched: &[&str]) -> String {
    format!(
        "{REQUIRE_REF_ENV} is set, so the byte-diff gate for `{tool}` may not be skipped, \
         but no {} reference `{tool}` was found in: {}",
        Libc::HOST.as_str(),
        searched.join(", ")
    )
}

/// Action: put a flagged skip on screen.
///
/// Not `println!`/`eprintln!`: libtest captures both and replays them only for
/// a *failing* test or under `--nocapture`, so a skip announced that way is
/// invisible in the CI log of a passing run — a silently narrowed gate, which
/// is exactly what AGENTS.md forbids. Writing to the process's own stderr
/// handle goes around the capture, so the flag always shows.
///
/// This is the raw announcement and applies no policy; a gate that has a tool
/// name to be strict about calls [`skip`] instead.
pub fn announce_skip(reason: &str) {
    // Nothing useful to do if stderr is closed; the test still passes.
    let _ = writeln!(std::io::stderr().lock(), "{reason}");
}

/// Action: read the [`RefPolicy`] this process is running under.
#[must_use]
pub fn policy() -> RefPolicy {
    let raw = std::env::var(REQUIRE_REF_ENV).ok();
    policy_from_env(raw.as_deref())
}

/// Action: resolve a gate whose reference `tool` is absent.
///
/// Announces a flagged skip and returns under the default policy. Under
/// [`RefPolicy::Require`] it panics instead, which libtest reports as a failing
/// test — the only failure channel available to a helper whose callers are test
/// bodies that expect `()` and then `return`.
///
/// # Panics
///
/// When [`REQUIRE_REF_ENV`] opts in and `tool` is not in [`UNSHIPPED_TOOLS`].
pub fn skip(tool: &str) {
    match missing_ref_action(policy(), tool) {
        MissingRef::Announce => announce_skip(&skip_message(tool)),
        MissingRef::Fail => {
            let overrides = override_dirs();
            let searched = searched_dirs(
                overrides.iter().map(String::as_str),
                Libc::HOST.default_dirs().iter().copied(),
            );
            panic!("{}", require_message(tool, &searched));
        }
    }
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
    fn skip_message_names_the_lane_and_both_overrides() {
        let message = skip_message("initdb");
        assert!(message.contains(Libc::HOST.env_var()), "{message}");
        assert!(message.contains(Libc::HOST.as_str()), "{message}");
        assert!(message.contains(REF_BIN_ENV), "{message}");
    }

    #[test]
    fn skip_message_carries_the_flag() {
        assert!(skip_message("initdb").starts_with(SKIP_FLAG));
    }

    #[test]
    fn an_unset_requirement_leaves_the_permissive_policy() {
        assert_eq!(policy_from_env(None), RefPolicy::Skip);
    }

    #[test]
    fn one_and_true_both_opt_into_the_strict_policy() {
        for raw in ["1", "true", "TRUE", " True "] {
            assert_eq!(policy_from_env(Some(raw)), RefPolicy::Require, "{raw:?}");
        }
    }

    #[test]
    fn an_empty_or_negative_value_is_not_an_opt_in() {
        for raw in ["", "0", "false", "no", "maybe"] {
            assert_eq!(policy_from_env(Some(raw)), RefPolicy::Skip, "{raw:?}");
        }
    }

    #[test]
    fn a_missing_reference_fails_under_the_strict_policy() {
        let action = missing_ref_action(RefPolicy::Require, "initdb");
        assert_eq!(action, MissingRef::Fail);
    }

    #[test]
    fn a_missing_reference_only_skips_under_the_default_policy() {
        let action = missing_ref_action(RefPolicy::Skip, "initdb");
        assert_eq!(action, MissingRef::Announce);
    }

    #[test]
    fn a_tool_no_package_ships_still_skips_under_the_strict_policy() {
        let action = missing_ref_action(RefPolicy::Require, "libpq_uri_regress");
        assert_eq!(action, MissingRef::Announce);
    }

    #[test]
    fn the_search_list_puts_the_overrides_first_lane_before_generic() {
        let searched = searched_dirs(["/lane/bin", "/env/bin"], ["/a/bin", "/b/bin"]);
        assert_eq!(searched, ["/lane/bin", "/env/bin", "/a/bin", "/b/bin"]);
    }

    #[test]
    fn the_search_list_is_just_the_lane_defaults_without_an_override() {
        let searched = searched_dirs([], Libc::HOST.default_dirs().iter().copied());
        assert_eq!(searched, Libc::HOST.default_dirs());
    }

    #[test]
    fn the_failure_message_names_the_tool_the_lane_and_every_directory_searched() {
        let message = require_message(
            "pg_controldata",
            &["/env/bin", "/usr/lib/postgresql/18/bin"],
        );
        assert!(message.contains("pg_controldata"), "{message}");
        assert!(message.contains(REQUIRE_REF_ENV), "{message}");
        assert!(message.contains(Libc::HOST.as_str()), "{message}");
        assert!(message.contains("/env/bin"), "{message}");
        assert!(message.contains("/usr/lib/postgresql/18/bin"), "{message}");
    }
}
