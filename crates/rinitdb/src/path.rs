//! The parts of `src/port/path.c` initdb's closing instructions and its `-s`
//! block need: [`canonicalize_path`], [`get_parent_directory`],
//! [`join_path_components`] and [`get_share_path`], over Unix paths.
//!
//! Pure string functions. The Windows arms (drive letters, backslashes, the
//! trailing-quote repair) are not ported: this crate ships to Linux and macOS
//! (ADR-0007), where `skip_drive` is the identity and `IS_DIR_SEP` is `/`.

/// `canonicalize_path` (`src/port/path.c:337`): remove trailing and repeated
/// separators and `.` components, and resolve `..` against the components
/// before it where that is possible.
///
/// An absolute path cannot climb above `/` (`/..` is `/`); a relative one
/// keeps the `..` it cannot resolve. A path that reduces to nothing is `.`,
/// and the empty path stays empty (`:415`, `:559`).
#[must_use]
pub fn canonicalize_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let absolute = path.starts_with('/');
    let mut parsed: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            // Repeated and trailing separators (:378, :381), and "." (:442).
            "" | "." => {}
            ".." => match parsed.last() {
                // ABSOLUTE_WITH_N_DEPTH and RELATIVE_WITH_N_DEPTH remove the
                // last parsed name (:473, :506).
                Some(&last) if last != ".." => {
                    parsed.pop();
                }
                // ABSOLUTE_PATH_INIT ignores ".." right after "/" (:461).
                _ if absolute => {}
                // RELATIVE_PATH_INIT and RELATIVE_WITH_PARENT_REF keep it
                // as an irreducible double-dot (:491, :534).
                _ => parsed.push(".."),
            },
            name => parsed.push(name),
        }
    }
    let joined = parsed.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        // "If our output path is empty at this point, insert '.'" (:559).
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// `get_parent_directory` (`src/port/path.c:1085`), which is `trim_directory`
/// (`:1102`): remove trailing slashes, the last component and the slashes
/// ahead of it, but never a leading slash. A bare name has no parent and
/// becomes empty.
#[must_use]
pub fn get_parent_directory(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.is_empty() {
        return path;
    }
    let mut p = bytes.len() - 1;
    // back up over trailing slash(es)
    while bytes[p] == b'/' && p > 0 {
        p -= 1;
    }
    // back up over directory name
    while bytes[p] != b'/' && p > 0 {
        p -= 1;
    }
    // if multiple slashes before directory name, remove 'em all
    while p > 0 && bytes[p - 1] == b'/' {
        p -= 1;
    }
    // don't erase a leading slash
    if p == 0 && bytes[0] == b'/' {
        p = 1;
    }
    &path[..p]
}

/// `join_path_components` (`src/port/path.c:286`): `head/tail`, with no
/// separator when `head` is empty and nothing added when `tail` is.
#[must_use]
pub fn join_path_components(head: &str, tail: &str) -> String {
    match (head.is_empty(), tail.is_empty()) {
        (_, true) => head.to_owned(),
        (true, false) => tail.to_owned(),
        (false, false) => format!("{head}/{tail}"),
    }
}

/// `get_share_path` (`src/port/path.c:919`): where a build whose compiled-in
/// directories are `PGSHAREDIR` and `PGBINDIR` finds its share directory
/// when its executable is `my_exec_path`.
#[must_use]
pub fn get_share_path(my_exec_path: &str) -> String {
    make_relative_path(
        crate::pg_config::PGSHAREDIR,
        crate::pg_config::PGBINDIR,
        my_exec_path,
    )
}

/// `make_relative_path` (`src/port/path.c:755`): take the common prefix of
/// `target_path` and `bin_path` (ending on a separator); if the rest of
/// `bin_path` is the tail of `my_exec_path`'s directory, swap it for the rest
/// of `target_path`, and otherwise return `target_path` itself, canonicalized
/// either way.
fn make_relative_path(target_path: &str, bin_path: &str, my_exec_path: &str) -> String {
    let prefix_len = target_path
        .bytes()
        .zip(bin_path.bytes())
        .enumerate()
        .take_while(|(_, (t, b))| t == b || (*t == b'/' && *b == b'/'))
        .filter(|(_, (t, _))| *t == b'/')
        .last()
        .map_or(0, |(i, _)| i + 1);
    if prefix_len == 0 {
        return canonicalize_path(target_path);
    }
    let bin_tail = &bin_path[prefix_len..];
    let exec_dir = canonicalize_path(get_parent_directory(my_exec_path));
    // `tail_start > 0` with a separator before it, and dir_strcmp (`:707`),
    // which on Unix is strcmp.
    if let Some(head) = exec_dir.strip_suffix(bin_tail)
        && head.ends_with('/')
    {
        let head = trim_trailing_separator(head);
        return canonicalize_path(&join_path_components(head, &target_path[prefix_len..]));
    }
    canonicalize_path(target_path)
}

/// `trim_trailing_separator` (`src/port/path.c:1134`), which never removes a
/// leading `/`.
fn trim_trailing_separator(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() && !path.is_empty() {
        &path[..1]
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `src/test/regress/sql/misc_functions.sql:89`-`:110` with the answers
    /// in `expected/misc_functions.out:187`-`:317`, in upstream order.
    #[test]
    fn test_canonicalize_path() {
        for (path, expected) in [
            ("/", "/"),
            ("/./abc/def/", "/abc/def"),
            ("/./../abc/def", "/abc/def"),
            ("/./../../abc/def/", "/abc/def"),
            ("/abc/.././def/ghi", "/def/ghi"),
            ("/abc/./../def/ghi//", "/def/ghi"),
            ("/abc/def/../..", "/"),
            ("/abc/def/../../..", "/"),
            ("/abc/def/../../../../ghi/jkl", "/ghi/jkl"),
            (".", "."),
            ("./", "."),
            ("./abc/..", "."),
            ("abc/../", "."),
            ("abc/../def", "def"),
            ("..", ".."),
            ("../abc/def", "../abc/def"),
            ("../abc/..", ".."),
            ("../abc/../def", "../def"),
            ("../abc/../../def/ghi", "../../def/ghi"),
            ("./abc/./def/.", "abc/def"),
            ("./abc/././def/.", "abc/def"),
            ("./abc/./def/.././ghi/../../../jkl/mno", "../jkl/mno"),
        ] {
            assert_eq!(canonicalize_path(path), expected, "{path}");
        }
    }

    #[test]
    fn the_empty_path_is_returned_as_is() {
        assert_eq!(canonicalize_path(""), "");
    }

    #[test]
    fn the_parent_directory_trims_as_trim_directory_does() {
        for (path, expected) in [
            (
                "/usr/lib/postgresql/18/bin/initdb",
                "/usr/lib/postgresql/18/bin",
            ),
            ("/initdb", "/"),
            ("/", "/"),
            ("initdb", ""),
            ("bin/initdb", "bin"),
            ("bin//initdb//", "bin"),
            ("", ""),
        ] {
            assert_eq!(get_parent_directory(path), expected, "{path}");
        }
    }

    #[test]
    fn joining_adds_a_separator_only_between_two_parts() {
        assert_eq!(
            join_path_components("/usr/bin", "pg_ctl"),
            "/usr/bin/pg_ctl"
        );
        assert_eq!(join_path_components("/", "pg_ctl"), "//pg_ctl");
        assert_eq!(join_path_components("", "pg_ctl"), "pg_ctl");
        assert_eq!(join_path_components("/usr/bin", ""), "/usr/bin");
    }

    #[test]
    fn the_share_path_follows_a_relocated_bin_directory() {
        // path.c:740's own example, with this port's PGSHAREDIR/PGBINDIR.
        assert_eq!(
            get_share_path("/opt/pgsql/bin/postgres"),
            "/opt/pgsql/share"
        );
        assert_eq!(get_share_path("/usr/local/bin/pgdrop"), "/usr/local/share");
        assert_eq!(get_share_path("/bin/initdb"), "/share");
        // No `bin` tail: the compiled-in directory, as is.
        assert_eq!(
            get_share_path("/work/target/debug/rinitdb"),
            "/usr/local/pgsql/share"
        );
        // The tail must be a whole component: `sbin` does not end in `/bin`.
        assert_eq!(get_share_path("/usr/sbin/initdb"), "/usr/local/pgsql/share");
        assert_eq!(get_share_path("initdb"), "/usr/local/pgsql/share");
    }

    #[test]
    fn make_relative_path_needs_a_common_prefix_ending_on_a_separator() {
        // path.c:765: '/usr/lib' and '/usr/libexec' share only '/usr/'.
        assert_eq!(
            make_relative_path("/usr/libexec/pg", "/usr/lib/bin", "/opt/lib/bin/x"),
            "/opt/libexec/pg"
        );
        assert_eq!(make_relative_path("share", "bin", "/opt/bin/x"), "share");
    }

    #[test]
    fn trailing_separators_go_but_a_leading_one_stays() {
        assert_eq!(trim_trailing_separator("/opt//"), "/opt");
        assert_eq!(trim_trailing_separator("/"), "/");
        assert_eq!(trim_trailing_separator("//"), "/");
        assert_eq!(trim_trailing_separator(""), "");
    }
}
