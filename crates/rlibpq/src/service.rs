//! Connection service files: `parseServiceInfo` and `parseServiceFile`
//! (`fe-connect.c:5929`, `:5997`), the first thing `conninfo_add_defaults`
//! does (`fe-connect.c:6636`).
//!
//! Data / Calculations / Actions:
//!
//! - [`Files`] is the two questions the C asks of the filesystem — does
//!   `stat` succeed, and what does `fopen` + `fgets` read — so the lookup
//!   order is a calculation over an answer set. An in-memory map answers them
//!   in the unit tests below, which port `t/006_service.pl`'s cases.
//! - [`parse_service_file`] and [`parse_service_info`] are pure.
//! - [`Filesystem`] is the only action: the real filesystem.
//!
//! A path is bytes here, as it is in C: `PGSERVICEFILE` can name a file whose
//! name is not UTF-8, and the error message quotes it unchanged.

use std::collections::BTreeMap;
use std::io::Read as _;

use crate::conninfo::{ConnInfo, Env};
use crate::cstr::is_space;
use crate::error::ConnError;
use crate::pg_config::SYSCONFDIR;

/// `char buf[1024]` in `parseServiceFile` (`fe-connect.c:6008`): `fgets`
/// stores at most one byte less than this, and a line that fills it is an
/// error (`fe-connect.c:6025`).
const LINE_BUFFER: usize = 1024;

/// What `parseServiceInfo` asks of the filesystem.
pub trait Files {
    /// `stat(path, &stat_buf) == 0` (`fe-connect.c:5963`, `:5978`).
    fn exists(&self, path: &[u8]) -> bool;

    /// `fopen(path, "r")` and every byte `fgets` then reads, or `None` when
    /// `fopen` fails (`fe-connect.c:6012`). A read error after a successful
    /// open ends the file where it happened, as `fgets` returning NULL does.
    fn read(&self, path: &[u8]) -> Option<Vec<u8>>;
}

/// Files held in memory, keyed by path: a value a test states instead of a
/// directory it has to create.
impl Files for BTreeMap<Vec<u8>, Vec<u8>> {
    fn exists(&self, path: &[u8]) -> bool {
        self.contains_key(path)
    }

    fn read(&self, path: &[u8]) -> Option<Vec<u8>> {
        self.get(path).cloned()
    }
}

/// Action: the process's real filesystem.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filesystem;

impl Files for Filesystem {
    fn exists(&self, path: &[u8]) -> bool {
        std::fs::metadata(os_path(path)).is_ok()
    }

    fn read(&self, path: &[u8]) -> Option<Vec<u8>> {
        let mut file = std::fs::File::open(os_path(path)).ok()?;
        let mut contents = Vec::new();
        // fopen() on a directory succeeds on Linux and the first fgets() fails;
        // read_to_end fails the same way and keeps what it had, so the file
        // just ends — it is not "not found".
        let _ = file.read_to_end(&mut contents);
        Some(contents)
    }
}

fn os_path(path: &[u8]) -> &std::path::Path {
    use std::os::unix::ffi::OsStrExt as _;
    std::path::Path::new(std::ffi::OsStr::from_bytes(path))
}

/// `parseServiceInfo` (`fe-connect.c:5929`): if a service name was given, in
/// `options` or in `PGSERVICE`, absorb the options its group sets into every
/// row of `options` that is still unset.
///
/// The files, in order: `PGSERVICEFILE` if it is set, else
/// `~/.pg_service.conf` if it exists; then `pg_service.conf` in
/// `PGSYSCONFDIR`, or in [`SYSCONFDIR`], if that exists. The second file is
/// not read once the first had the group.
///
/// The home directory is `HOME`; see `docs/divergences.md` for the
/// `getpwuid` fallback `pqGetHomeDirectory` has (`fe-connect.c:8173`).
///
/// # Errors
/// The first [`ConnError`] a file produced, or [`ConnError::ServiceNotFound`]
/// when no file had the group. Options a file set before its error stay set,
/// as they do in C.
pub fn parse_service_info(
    options: &mut ConnInfo,
    env: &Env,
    files: &impl Files,
) -> Result<(), ConnError> {
    // fe-connect.c:5944: PGSERVICE is special-cased here, because this runs
    // before the environment defaults of the other options are inserted.
    let Some(service) = options
        .get("service")
        .or_else(|| env.get("PGSERVICE"))
        .map(<[u8]>::to_vec)
    else {
        // If no service name given, nothing to do
        return Ok(());
    };

    let first = match env.get("PGSERVICEFILE") {
        Some(file) => Some(file.to_vec()),
        None => env
            .get("HOME")
            .filter(|home| !home.is_empty())
            .map(|home| join(home, b".pg_service.conf"))
            .filter(|file| files.exists(file)),
    };
    let mut group_found = false;
    if let Some(file) = first {
        group_found = parse_service_file(&file, files.read(&file), &service, options)?;
        if group_found {
            return Ok(());
        }
    }

    // next_file:
    let sysconfdir = env.get("PGSYSCONFDIR").unwrap_or(SYSCONFDIR.as_bytes());
    let file = join(sysconfdir, b"pg_service.conf");
    if files.exists(&file) {
        group_found = parse_service_file(&file, files.read(&file), &service, options)?;
    }

    // last_file:
    if group_found {
        Ok(())
    } else {
        Err(ConnError::ServiceNotFound(service.into()))
    }
}

/// `snprintf("%s/%s", dir, name)`.
fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = dir.to_vec();
    path.push(b'/');
    path.extend_from_slice(name);
    path
}

/// `parseServiceFile` (`fe-connect.c:5997`): read `service`'s group out of the
/// file `path` names, whose bytes are `contents` (`None` when it could not be
/// opened), into the unset rows of `options`. Returns `group_found`.
///
/// The group ends at the next `[` line. Inside it, a line is `key=value` with
/// nothing trimmed around the `=`, and `key` must be a conninfo keyword other
/// than `service`.
///
/// # Errors
/// [`ConnError::ServiceFileNotFound`], [`ConnError::ServiceFileLineTooLong`],
/// [`ConnError::ServiceFileSyntaxError`] or
/// [`ConnError::NestedServiceSpecification`].
pub fn parse_service_file(
    path: &[u8],
    contents: Option<Vec<u8>>,
    service: &[u8],
    options: &mut ConnInfo,
) -> Result<bool, ConnError> {
    let Some(contents) = contents else {
        return Err(ConnError::ServiceFileNotFound(path.into()));
    };
    let mut group_found = false;

    for (index, raw) in fgets_lines(&contents).enumerate() {
        let linenr = index + 1;
        // What strlen() sees of the buffer fgets() filled.
        let raw = &raw[..raw.iter().position(|&b| b == 0).unwrap_or(raw.len())];
        if raw.len() >= LINE_BUFFER - 1 {
            return Err(ConnError::ServiceFileLineTooLong {
                file: path.into(),
                line: linenr,
            });
        }

        // ignore whitespace at end of line, especially the newline, and
        // leading whitespace too
        let end = raw.iter().rposition(|&b| !is_space(b)).map_or(0, |p| p + 1);
        let line = &raw[..end];
        let start = line.iter().position(|&b| !is_space(b)).unwrap_or(end);
        let line = &line[start..];

        // ignore comments and empty lines
        if line.is_empty() || line[0] == b'#' {
            continue;
        }

        // Check for right groupname
        if line[0] == b'[' {
            if group_found {
                // end of desired group reached; return success
                return Ok(true);
            }
            group_found =
                line[1..].starts_with(service) && line.get(service.len() + 1) == Some(&b']');
        } else if group_found {
            // Finally, we are in the right group and can parse the line
            let syntax_error = || ConnError::ServiceFileSyntaxError {
                file: path.into(),
                line: linenr,
            };
            let equals = line
                .iter()
                .position(|&b| b == b'=')
                .ok_or_else(syntax_error)?;
            let (key, value) = (&line[..equals], &line[equals + 1..]);

            if key == b"service" {
                return Err(ConnError::NestedServiceSpecification {
                    file: path.into(),
                    line: linenr,
                });
            }

            // Set the parameter --- but don't override any previous explicit
            // setting.
            let row = ConnInfo::index_of(key).ok_or_else(syntax_error)?;
            options.set_if_unset(row, value);
        }
    }

    Ok(group_found)
}

/// The lines successive `fgets(buf, 1024, f)` calls return: each runs through
/// its newline, or stops after 1023 bytes, or at the end of the file.
fn fgets_lines(contents: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = contents;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let limit = rest.len().min(LINE_BUFFER - 1);
        let len = rest[..limit]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(limit, |newline| newline + 1);
        let (line, tail) = rest.split_at(len);
        rest = tail;
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conninfo::parse_conninfo;

    type Tree = BTreeMap<Vec<u8>, Vec<u8>>;

    fn tree(files: &[(&str, &str)]) -> Tree {
        files
            .iter()
            .map(|(path, contents)| (path.as_bytes().to_vec(), contents.as_bytes().to_vec()))
            .collect()
    }

    fn lookup(conninfo: &str, env: &Env, files: &Tree) -> Result<ConnInfo, ConnError> {
        let mut options = parse_conninfo(conninfo.as_bytes()).expect("parses");
        parse_service_info(&mut options, env, files).map(|()| options)
    }

    fn get<'a>(options: &'a ConnInfo, keyword: &str) -> Option<&'a str> {
        options
            .get(keyword)
            .map(|v| std::str::from_utf8(v).expect("utf-8"))
    }

    // ---- t/006_service.pl, as pure lookups -------------------------------
    //
    // `$td` is `/td`. `pg_service_valid.conf` is `[my_srv]` followed by
    // `$node->connstr` split on blanks (`:28`-`:33`), and `PGSERVICEFILE`
    // starts out naming the empty file (`:37`-`:38`, `:55`) and `PGSYSCONFDIR`
    // naming `$td` (`:50`). A connection that would succeed is one whose
    // lookup set the node's `port` and `host`; the live half is
    // `tests/t_006_service.rs`.

    const VALID: &str = "[my_srv]\nport=5432\nhost=/tmp/node\n";

    fn td_env() -> Env {
        Env::empty()
            .with("PGSYSCONFDIR", "/td")
            .with("PGSERVICEFILE", "/td/pg_service_empty.conf")
    }

    fn td_tree() -> Tree {
        tree(&[
            ("/td/pg_service_valid.conf", VALID),
            ("/td/pg_service_empty.conf", ""),
        ])
    }

    fn assert_reaches_the_node(options: &ConnInfo) {
        assert_eq!(get(options, "port"), Some("5432"));
        assert_eq!(get(options, "host"), Some("/tmp/node"));
    }

    /// `:57`-`:91`: combinations of service name and a valid service file.
    #[test]
    fn checks_combinations_of_service_name_and_a_valid_service_file() {
        let env = td_env().with("PGSERVICEFILE", "/td/pg_service_valid.conf");
        let files = td_tree();

        // connection with correct "service" string and PGSERVICEFILE
        assert_reaches_the_node(&lookup("service=my_srv", &env, &files).unwrap());
        // connection with correct "service" URI and PGSERVICEFILE
        assert_reaches_the_node(&lookup("postgres://?service=my_srv", &env, &files).unwrap());
        // connection with incorrect "service" string and PGSERVICEFILE
        assert_eq!(
            lookup("service=undefined-service", &env, &files)
                .unwrap_err()
                .to_string(),
            "definition of service \"undefined-service\" not found"
        );
        // connection with correct PGSERVICE and PGSERVICEFILE
        let env = env.with("PGSERVICE", "my_srv");
        assert_reaches_the_node(&lookup("", &env, &files).unwrap());
        // connection with incorrect PGSERVICE and PGSERVICEFILE
        let env = env.with("PGSERVICE", "undefined-service");
        assert_eq!(
            lookup("", &env, &files).unwrap_err().to_string(),
            "definition of service \"undefined-service\" not found"
        );
    }

    /// `:93`-`:101`: case of incorrect service file.
    #[test]
    fn checks_case_of_incorrect_service_file() {
        let env = td_env().with("PGSERVICEFILE", "/td/pg_service_missing.conf");
        // connection with correct "service" string and incorrect PGSERVICEFILE
        assert_eq!(
            lookup("service=my_srv", &env, &td_tree()),
            Err(ConnError::ServiceFileNotFound(
                "/td/pg_service_missing.conf".into()
            ))
        );
    }

    /// `:103`-`:143`: case of service file named "pg_service.conf" in
    /// PGSYSCONFDIR.
    #[test]
    fn checks_case_of_service_file_named_pg_service_conf_in_pgsysconfdir() {
        let env = td_env();
        let mut files = td_tree();
        files.insert(b"/td/pg_service.conf".to_vec(), VALID.as_bytes().to_vec());

        // connection with correct "service" string and pg_service.conf
        assert_reaches_the_node(&lookup("service=my_srv", &env, &files).unwrap());
        // connection with correct "service" URI and default pg_service.conf
        assert_reaches_the_node(&lookup("postgres://?service=my_srv", &env, &files).unwrap());
        // connection with incorrect "service" string and default pg_service.conf
        assert_eq!(
            lookup("service=undefined-service", &env, &files)
                .unwrap_err()
                .to_string(),
            "definition of service \"undefined-service\" not found"
        );
        // connection with correct PGSERVICE and default pg_service.conf
        let env = env.with("PGSERVICE", "my_srv");
        assert_reaches_the_node(&lookup("", &env, &files).unwrap());
        // connection with incorrect PGSERVICE and default pg_service.conf
        let env = env.with("PGSERVICE", "undefined-service");
        assert_eq!(
            lookup("", &env, &files).unwrap_err().to_string(),
            "definition of service \"undefined-service\" not found"
        );
    }

    // ---- parseServiceInfo's order, fe-connect.c:5929 ---------------------

    #[test]
    fn no_service_name_reads_no_file() {
        let options = lookup("host=h", &Env::empty(), &Tree::new()).unwrap();
        assert_eq!(get(&options, "host"), Some("h"));
    }

    /// `fe-connect.c:5954`-`:5964`: without `PGSERVICEFILE`, `~/.pg_service.conf`
    /// is read if it exists.
    #[test]
    fn the_home_directory_file_is_read_without_pgservicefile() {
        let env = Env::empty().with("HOME", "/home/u");
        let files = tree(&[("/home/u/.pg_service.conf", "[s]\ndbname=fromhome\n")]);
        let options = lookup("service=s", &env, &files).unwrap();
        assert_eq!(get(&options, "dbname"), Some("fromhome"));
    }

    /// A missing `~/.pg_service.conf` is skipped silently, where a missing
    /// `PGSERVICEFILE` is an error; `PGSYSCONFDIR`'s file is then tried.
    #[test]
    fn a_missing_home_file_falls_through_to_the_sysconfdir_file() {
        let env = Env::empty()
            .with("HOME", "/home/u")
            .with("PGSYSCONFDIR", "/etc/pg");
        let files = tree(&[("/etc/pg/pg_service.conf", "[s]\ndbname=fromsys\n")]);
        let options = lookup("service=s", &env, &files).unwrap();
        assert_eq!(get(&options, "dbname"), Some("fromsys"));
    }

    /// Without `PGSYSCONFDIR` the compiled-in `SYSCONFDIR` is searched.
    #[test]
    fn the_compiled_sysconfdir_is_the_last_resort() {
        let path = format!("{SYSCONFDIR}/pg_service.conf");
        let files = tree(&[(path.as_str(), "[s]\ndbname=compiled\n")]);
        let options = lookup("service=s", &Env::empty(), &files).unwrap();
        assert_eq!(get(&options, "dbname"), Some("compiled"));
    }

    /// `fe-connect.c:5968`: the second file is read only if the first did not
    /// have the group.
    #[test]
    fn the_first_file_with_the_group_wins_outright() {
        let env = Env::empty()
            .with("PGSERVICEFILE", "/a")
            .with("PGSYSCONFDIR", "/etc/pg");
        let files = tree(&[
            ("/a", "[s]\nhost=first\n"),
            (
                "/etc/pg/pg_service.conf",
                "[s]\nhost=second\ndbname=second\n",
            ),
        ]);
        let options = lookup("service=s", &env, &files).unwrap();
        assert_eq!(get(&options, "host"), Some("first"));
        assert_eq!(get(&options, "dbname"), None);
    }

    /// An explicit setting is never overridden (`fe-connect.c:6118`-`:6127`), and the
    /// `service` option beats `PGSERVICE`.
    #[test]
    fn an_explicit_option_beats_the_service_file() {
        let env = Env::empty()
            .with("PGSERVICEFILE", "/a")
            .with("PGSERVICE", "other");
        let files = tree(&[("/a", "[s]\nhost=file\nport=1\n[other]\nhost=wrong\n")]);
        let options = lookup("service=s host=explicit", &env, &files).unwrap();
        assert_eq!(get(&options, "host"), Some("explicit"));
        assert_eq!(get(&options, "port"), Some("1"));
    }

    // ---- parseServiceFile's grammar, fe-connect.c:5997 -------------------

    fn parse(contents: &str, service: &str) -> Result<(bool, ConnInfo), ConnError> {
        let mut options = ConnInfo::new();
        parse_service_file(
            b"/f",
            Some(contents.as_bytes().to_vec()),
            service.as_bytes(),
            &mut options,
        )
        .map(|found| (found, options))
    }

    #[test]
    fn blanks_comments_and_other_groups_are_skipped() {
        let (found, options) = parse(
            "# comment\n\n[a]\nhost=a\n  [s]  \n\t# in group\n  dbname=d \r\n[b]\nhost=b\n",
            "s",
        )
        .unwrap();
        assert!(found);
        assert_eq!(get(&options, "dbname"), Some("d"));
        assert_eq!(get(&options, "host"), None);
    }

    /// `fe-connect.c:6057`: the header must start with the name and have `]`
    /// right after it. Whatever follows the `]` is ignored, and a prefix is
    /// not a match.
    #[test]
    fn a_group_header_matches_by_prefix_then_a_closing_bracket() {
        assert!(parse("[s] trailing\nhost=h\n", "s").unwrap().0);
        assert!(!parse("[s2]\nhost=h\n", "s").unwrap().0);
        assert!(!parse("[s\nhost=h\n", "s").unwrap().0);
        assert!(parse("[]\nhost=h\n", "").unwrap().0);
    }

    /// Neither side of the `=` is trimmed, so a blank before it makes the key
    /// unknown and one after it is part of the value.
    #[test]
    fn nothing_is_trimmed_around_the_equals_sign() {
        let (_, options) = parse("[s]\nhost= spaced\n", "s").unwrap();
        assert_eq!(get(&options, "host"), Some(" spaced"));
        assert_eq!(
            parse("[s]\nhost =h\n", "s").unwrap_err(),
            ConnError::ServiceFileSyntaxError {
                file: "/f".into(),
                line: 2
            }
        );
    }

    #[test]
    fn a_line_without_equals_or_with_an_unknown_key_is_a_syntax_error() {
        for bad in ["[s]\nhost\n", "[s]\nrequiressl=1\n"] {
            assert_eq!(
                parse(bad, "s").unwrap_err().to_string(),
                "syntax error in service file \"/f\", line 2"
            );
        }
        // Outside the group, nothing is parsed, so nothing is an error.
        assert!(!parse("[other]\nnot a setting\n", "s").unwrap().0);
    }

    #[test]
    fn a_nested_service_is_refused() {
        assert_eq!(
            parse("[s]\nhost=h\nservice=t\n", "s")
                .unwrap_err()
                .to_string(),
            "nested service specifications not supported in service file \"/f\", line 3"
        );
    }

    /// `fe-connect.c:6025`: `fgets` into 1024 bytes, and a line of 1023 or
    /// more — newline included — is an error, counted as the line it started.
    #[test]
    fn a_line_of_1023_bytes_is_too_long() {
        let fits = format!("[s]\nhost={}\n", "h".repeat(LINE_BUFFER - 8));
        assert_eq!(fits.lines().nth(1).unwrap().len() + 1, LINE_BUFFER - 2);
        assert!(parse(&fits, "s").is_ok());

        let long = format!("[s]\nhost={}\n", "h".repeat(LINE_BUFFER - 7));
        assert_eq!(
            parse(&long, "s").unwrap_err().to_string(),
            "line 2 too long in service file \"/f\""
        );
        // Outside the group too: the check comes before anything else.
        let long = format!("# {}\n[s]\n", "x".repeat(LINE_BUFFER));
        assert_eq!(
            parse(&long, "s").unwrap_err(),
            ConnError::ServiceFileLineTooLong {
                file: "/f".into(),
                line: 1
            }
        );
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_not_found() {
        let mut options = ConnInfo::new();
        assert_eq!(
            parse_service_file(b"/nope", None, b"s", &mut options),
            Err(ConnError::ServiceFileNotFound("/nope".into()))
        );
    }

    /// Options set before an error stay set, as `options[i].val` does in C.
    #[test]
    fn an_error_keeps_what_was_set_before_it() {
        let mut options = ConnInfo::new();
        let _ = parse_service_file(
            b"/f",
            Some(b"[s]\nhost=h\nbad\n".to_vec()),
            b"s",
            &mut options,
        );
        assert_eq!(get(&options, "host"), Some("h"));
    }

    #[test]
    fn fgets_splits_at_newlines_and_at_1023_bytes() {
        let lines: Vec<&[u8]> = fgets_lines(b"a\nb").collect();
        assert_eq!(lines, [&b"a\n"[..], &b"b"[..]]);
        let long = vec![b'x'; LINE_BUFFER + 5];
        let lengths: Vec<usize> = fgets_lines(&long).map(<[u8]>::len).collect();
        assert_eq!(lengths, [LINE_BUFFER - 1, 6]);
    }

    /// The real filesystem answers as `stat` and `fopen` do.
    #[test]
    fn the_filesystem_answers_exists_and_read() {
        let dir = std::env::temp_dir().join(format!("rlibpq-service-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pg_service.conf");
        std::fs::write(&file, "[s]\n").unwrap();
        let path = file.to_str().unwrap().as_bytes();
        assert!(Filesystem.exists(path));
        assert_eq!(Filesystem.read(path), Some(b"[s]\n".to_vec()));
        let missing = dir.join("missing");
        let missing = missing.to_str().unwrap().as_bytes();
        assert!(!Filesystem.exists(missing));
        assert_eq!(Filesystem.read(missing), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
