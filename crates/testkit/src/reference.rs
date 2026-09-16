//! Where the C PostgreSQL 18 tools live, for the byte-diff gates.
//!
//! Search order matches pgrust's own sim sweep: an explicit environment
//! variable first, then the PGDG Debian layout, then Homebrew.

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

/// The message a gate prints when the reference tool is absent.
#[must_use]
pub fn skip_message(tool: &str) -> String {
    format!(
        "SKIP (flagged, not silent): reference `{tool}` not found; set {REF_BIN_ENV} or install PostgreSQL 18"
    )
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
}
