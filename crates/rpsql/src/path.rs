//! The file-name calculations `\i` and `\ir` make before they open anything:
//! `src/port/path.c`'s [`canonicalize_path`], [`get_parent_directory`] and
//! [`join_path_components`], `port.h`'s [`is_absolute_path`], `common.c`'s
//! [`expand_tilde`], and `process_file`'s choice of file ([`include_path`]).
//!
//! Pure string functions over Unix paths. The Windows arms (drive letters,
//! backslashes, `has_drive_prefix`) are not ported: rpsql ships to Linux and
//! macOS (ADR-0007), where `skip_drive` is the identity and `IS_DIR_SEP` is
//! `/`. `rinitdb::path` ports the same three `path.c` functions for initdb;
//! they are ported again here rather than shared because psql should not link
//! initdb to canonicalize a path, and an MIT crate reaches no other tool's
//! internals.
//!
//! `canonicalize_path_enc` (`path.c:344`) takes the client encoding so as not
//! to split a multibyte character; rpsql's paths are UTF-8, in which no byte
//! of a multibyte character is `/` or `.`, so the encoding-blind walk is the
//! same function.

/// `canonicalize_path` (`src/port/path.c:337`): remove trailing and repeated
/// separators and `.` components, and resolve `..` against the components
/// before it where that is possible.
///
/// An absolute path cannot climb above `/` (`/..` is `/`); a relative one
/// keeps the `..` it cannot resolve. A path that reduces to nothing is `.`,
/// and the empty path stays empty.
#[must_use]
pub fn canonicalize_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let absolute = is_absolute_path(path);
    let mut parsed: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            // Repeated and trailing separators, and ".".
            "" | "." => {}
            ".." => match parsed.last() {
                // A ".." after a name removes the name.
                Some(&last) if last != ".." => {
                    parsed.pop();
                }
                // "/.." is "/".
                _ if absolute => {}
                // A relative path keeps the ".." it cannot resolve.
                _ => parsed.push(".."),
            },
            name => parsed.push(name),
        }
    }
    let joined = parsed.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        // "If our output path is empty at this point, insert '.'".
        (false, true) => ".".to_owned(),
        (false, false) => joined,
    }
}

/// `is_absolute_path` (`src/include/port.h:105`), which off Windows is
/// `is_nonwindows_absolute_path` (`port.h:84`): the path starts with `/`.
#[must_use]
pub fn is_absolute_path(path: &str) -> bool {
    path.starts_with('/')
}

/// `get_parent_directory` (`src/port/path.c:1085`), which is `trim_directory`
/// (`path.c:1102`): remove trailing slashes, the last component and the
/// slashes ahead of it, but never a leading slash. A bare name has no parent
/// and becomes empty.
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

/// `expand_tilde()` (`src/bin/psql/common.c:2697`): a leading `~` or `~/`
/// becomes `home`, the answer `get_home_path` (`src/port/path.c:1022`) gives.
///
/// `~user` needs `getpwnam`, which neither the standard library nor this
/// `#![deny(unsafe_code)]` crate reaches; it is left as typed, which is what
/// C does for a user it does not find (`common.c:2734`). With no home, the
/// name is left as typed too.
#[must_use]
pub fn expand_tilde(filename: &str, home: Option<&str>) -> String {
    let Some(rest) = filename.strip_prefix('~') else {
        return filename.to_owned();
    };
    let user_len = rest.find('/').unwrap_or(rest.len());
    match home {
        Some(home) if user_len == 0 && !home.is_empty() => format!("{home}{rest}"),
        _ => filename.to_owned(),
    }
}

/// The file `process_file` (`src/bin/psql/command.c:4920`) opens for
/// `filename`, which is neither absent nor `-`: canonicalized
/// (`command.c:4934`), and for `\ir` (`use_relative_path`) with a relative
/// name read from inside a file, joined to that file's directory and
/// canonicalized again (`command.c:4942`-`:4948`).
#[must_use]
pub fn include_path(filename: &str, use_relative_path: bool, inputfile: Option<&str>) -> String {
    let filename = canonicalize_path(filename);
    match inputfile {
        Some(current) if use_relative_path && !is_absolute_path(&filename) => canonicalize_path(
            &join_path_components(get_parent_directory(current), &filename),
        ),
        _ => filename,
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
            ("/scripts/main.sql", "/scripts"),
            ("/main.sql", "/"),
            ("/", "/"),
            ("main.sql", ""),
            ("sub/a.sql", "sub"),
            ("sub//a.sql//", "sub"),
            ("<stdin>", ""),
            ("", ""),
        ] {
            assert_eq!(get_parent_directory(path), expected, "{path}");
        }
    }

    #[test]
    fn joining_adds_a_separator_only_between_two_parts() {
        assert_eq!(join_path_components("sub", "b.sql"), "sub/b.sql");
        assert_eq!(join_path_components("/", "b.sql"), "//b.sql");
        assert_eq!(join_path_components("", "b.sql"), "b.sql");
        assert_eq!(join_path_components("sub", ""), "sub");
    }

    #[test]
    fn a_leading_tilde_is_home_and_a_named_user_is_left_as_typed() {
        let home = Some("/home/u");
        assert_eq!(expand_tilde("~", home), "/home/u");
        assert_eq!(expand_tilde("~/a.sql", home), "/home/u/a.sql");
        assert_eq!(expand_tilde("~bob/a.sql", home), "~bob/a.sql");
        assert_eq!(expand_tilde("a~/b", home), "a~/b");
        assert_eq!(expand_tilde("~/a.sql", None), "~/a.sql");
        assert_eq!(expand_tilde("~/a.sql", Some("")), "~/a.sql");
    }

    #[test]
    fn i_opens_the_canonical_name_and_ir_one_beside_the_current_file() {
        // `\i` never looks at the current file.
        assert_eq!(
            include_path("./sub//a.sql", false, Some("x/m.sql")),
            "sub/a.sql"
        );
        // `\ir` from a file: that file's directory, canonicalized again.
        assert_eq!(include_path("b.sql", true, Some("sub/a.sql")), "sub/b.sql");
        assert_eq!(include_path("../b.sql", true, Some("sub/a.sql")), "b.sql");
        assert_eq!(
            include_path("b.sql", true, Some("/s/sub/a.sql")),
            "/s/sub/b.sql"
        );
        assert_eq!(include_path("b.sql", true, Some("a.sql")), "b.sql");
        // An absolute name, or no current file, is `\i`'s answer.
        assert_eq!(
            include_path("/t/b.sql", true, Some("sub/a.sql")),
            "/t/b.sql"
        );
        assert_eq!(include_path("./b.sql", true, None), "b.sql");
        // `-f -` names its input `<stdin>`, which has no directory.
        assert_eq!(include_path("b.sql", true, Some("<stdin>")), "b.sql");
    }
}
