//! Where the C PostgreSQL 18 tools live, for the byte-diff gates.
//!
//! Search order matches pgrust's own sim sweep: an explicit environment
//! variable first, then the PGDG Debian layout, then Homebrew.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Environment variable naming the directory that holds the reference
/// `initdb`, `psql`, `pg_controldata`, … binaries.
pub const REF_BIN_ENV: &str = "PGDROP_REF_BIN";

/// Directories tried after [`REF_BIN_ENV`], in order.
pub const DEFAULT_REF_DIRS: [&str; 4] = [
    "/usr/lib/postgresql/18/bin",
    "/opt/homebrew/opt/postgresql@18/bin",
    "/usr/local/opt/postgresql@18/bin",
    "/opt/homebrew/bin",
];

/// Pure: pick the first candidate directory containing `tool`.
///
/// `env_dir` is the value of [`REF_BIN_ENV`] if set; `exists` answers whether a
/// path is an executable file, so the search is unit-testable.
pub fn locate<'a>(
    tool: &str,
    env_dir: Option<&'a str>,
    candidates: impl IntoIterator<Item = &'a str>,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    env_dir
        .into_iter()
        .chain(candidates)
        .map(|dir| Path::new(dir).join(tool))
        .find(|path| exists(path))
}

/// Action: find the reference `tool` on this machine, or `None`.
///
/// Tests that get `None` must print `SKIP (flagged, not silent)` and pass; see
/// `docs/test-stealing.md`.
#[must_use]
pub fn find(tool: &str) -> Option<PathBuf> {
    let env_dir = std::env::var(REF_BIN_ENV).ok();
    locate(tool, env_dir.as_deref(), DEFAULT_REF_DIRS, Path::is_file)
}

/// The flag every skipped gate carries, so skips are greppable in a log.
pub const SKIP_FLAG: &str = "SKIP (flagged, not silent)";

/// The message a gate prints when the reference tool is absent.
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
        let found = locate("initdb", Some("/env/bin"), ["/default/bin"], |p| {
            p == Path::new("/env/bin/initdb") || p == Path::new("/default/bin/initdb")
        });
        assert_eq!(found, Some(PathBuf::from("/env/bin/initdb")));
    }

    #[test]
    fn falls_through_to_the_first_existing_default() {
        let found = locate("psql", None, ["/a/bin", "/b/bin"], |p| {
            p == Path::new("/b/bin/psql")
        });
        assert_eq!(found, Some(PathBuf::from("/b/bin/psql")));
    }

    #[test]
    fn none_when_nothing_exists() {
        assert_eq!(locate("psql", Some("/x"), ["/y"], |_| false), None);
    }

    #[test]
    fn skip_message_names_the_override() {
        assert!(skip_message("initdb").contains(REF_BIN_ENV));
    }

    #[test]
    fn skip_message_carries_the_flag() {
        assert!(skip_message("initdb").starts_with(SKIP_FLAG));
    }
}
