//! The password file: `passwordFromFile` and `pwdfMatchesString`
//! (`fe-connect.c:7906`, `:7869`), the lookup `pqConnectOptions2` makes with
//! them when no password was given (`fe-connect.c:1422`-`:1465`), and the
//! warnings it prints on the way.
//!
//! Data / Calculations / Actions:
//!
//! - [`PassfileWarning`] and [`PasswordLookup`] are data: what the lookup
//!   found, and what C would have written to stderr while finding it.
//! - [`search`], [`password_from_file`] and [`PasswordLookup::new`] are pure
//!   over [`Files`] (`fopen` + `fstat`, then `fgets`), so the unit tests below
//!   state the file and its mode instead of creating either.
//! - Printing the warning is `Connection::connect`'s, the one action.

use crate::connection::is_unixsock_path;
use crate::conninfo::{ConnInfo, Env};
use crate::cstr::at;
use crate::pg_config::{DEF_PGPORT_STR, DEFAULT_PGSOCKET_DIR};
use crate::service::Files;

/// `PGPASSFILE`, `fe-connect.c:79`: the file under the home directory.
pub const PGPASSFILE: &[u8] = b".pgpass";

/// `DefaultHost`, `fe-connect.c:120`.
const DEFAULT_HOST: &[u8] = b"localhost";

/// `INITIAL_EXPBUFFER_SIZE`, `pqexpbuffer.h:76`: `buf.maxlen` after
/// `initPQExpBuffer`.
const INITIAL_EXPBUFFER_SIZE: usize = 256;

/// `S_IFMT` / `S_IFREG`, for `S_ISREG` (`fe-connect.c:7948`).
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;

/// `S_IRWXG | S_IRWXO` (`fe-connect.c:7958`).
const GROUP_OR_WORLD: u32 = 0o077;

/// The two warnings `passwordFromFile` prints to stderr itself, with
/// `fprintf`, before it ignores the file (`fe-connect.c:7950`, `:7960`).
/// Each holds the file name as it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassfileWarning {
    /// `fe-connect.c:7950` — not `S_ISREG`: a directory, a FIFO, a device.
    NotPlainFile(Vec<u8>),
    /// `fe-connect.c:7960` — any of `S_IRWXG | S_IRWXO` is set.
    GroupOrWorldAccess(Vec<u8>),
}

impl PassfileWarning {
    /// The bytes `fprintf(stderr, …)` writes, trailing newline included.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        let (before, file, after): (&[u8], &[u8], &[u8]) = match self {
            PassfileWarning::NotPlainFile(file) => (
                b"WARNING: password file \"",
                file,
                b"\" is not a plain file\n",
            ),
            PassfileWarning::GroupOrWorldAccess(file) => (
                b"WARNING: password file \"",
                file,
                b"\" has group or world access; permissions should be u=rw (0600) or less\n",
            ),
        };
        [before, file, after].concat()
    }
}

impl std::fmt::Display for PassfileWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

/// A password `passwordFromFile` returned, and the file it came from — what
/// `pgpassfileWarning` (`fe-connect.c:8053`) names if the server then rejects
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePassword {
    /// `conn->connhost[i].password`.
    pub password: Vec<u8>,
    /// `conn->pgpassfile`.
    pub passfile: Vec<u8>,
}

/// What `pqConnectOptions2`'s password-file step (`fe-connect.c:1422`-`:1465`)
/// leaves behind for the one host this build connects to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PasswordLookup {
    /// The password the file had for this host, if it was looked up at all.
    pub found: Option<FilePassword>,
    /// What `passwordFromFile` printed to stderr, if anything.
    pub warning: Option<PassfileWarning>,
}

impl PasswordLookup {
    /// `pqConnectOptions2`, `fe-connect.c:1426`: when no password was given,
    /// look one up in `passfile` — or in `~/.pgpass` when that is unset or
    /// empty — keyed by this connection's host, port, database and user.
    ///
    /// `conninfo` is the one `conninfo_add_defaults` has filled in, so
    /// `passfile` already holds `PGPASSFILE` if that was set. The home
    /// directory is `HOME`, as for the service file (`docs/divergences.md`).
    ///
    /// The key is the one `pqConnectOptions2` builds by the time it gets here:
    /// the host is `host`, else `hostaddr` (`:1453`), else the default host
    /// `:1339`-`:1347` filled in; the database defaults to the user (`:1414`).
    /// This build connects to one host, so there is one lookup (see
    /// `docs/divergences.md` for the host list).
    #[must_use]
    pub fn new(conninfo: &ConnInfo, env: &Env, files: &impl Files) -> Self {
        let given = |keyword| conninfo.get(keyword).filter(|value| !value.is_empty());

        if given("password").is_some() {
            return Self::default();
        }
        // If password file wasn't specified, use ~/PGPASSFILE
        let passfile = match given("passfile") {
            Some(passfile) => passfile.to_vec(),
            None => match env.get("HOME").filter(|home| !home.is_empty()) {
                Some(home) => [home, b"/", PGPASSFILE].concat(),
                None => return Self::default(),
            },
        };

        let host = given("host").or_else(|| given("hostaddr")).unwrap_or(
            if DEFAULT_PGSOCKET_DIR.is_empty() {
                DEFAULT_HOST
            } else {
                DEFAULT_PGSOCKET_DIR.as_bytes()
            },
        );
        let user = conninfo.get("user").unwrap_or_default();
        let dbname = given("dbname").unwrap_or(user);
        let port = conninfo.get("port").unwrap_or_default();

        match password_from_file(host, port, dbname, user, &passfile, files) {
            Ok(password) => Self {
                found: password.map(|password| FilePassword { password, passfile }),
                warning: None,
            },
            Err(warning) => Self {
                found: None,
                warning: Some(warning),
            },
        }
    }
}

/// `passwordFromFile`, `fe-connect.c:7906`: the password the first matching
/// line of `pgpassfile` holds, or `None`.
///
/// Nothing is opened without a database and a user name. A file that cannot
/// be opened or `fstat`'ed is ignored silently.
///
/// # Errors
/// A file that is not a regular file, or that the group or others can
/// access: C prints the [`PassfileWarning`] and ignores the file.
pub fn password_from_file(
    hostname: &[u8],
    port: &[u8],
    dbname: &[u8],
    username: &[u8],
    pgpassfile: &[u8],
    files: &impl Files,
) -> Result<Option<Vec<u8>>, PassfileWarning> {
    if dbname.is_empty() || username.is_empty() {
        return Ok(None);
    }

    // 'localhost' matches pghost of '' or the default socket directory
    let hostname = if hostname.is_empty()
        || (is_unixsock_path(hostname) && hostname == DEFAULT_PGSOCKET_DIR.as_bytes())
    {
        DEFAULT_HOST
    } else {
        hostname
    };
    let port = if port.is_empty() {
        DEF_PGPORT_STR.as_bytes()
    } else {
        port
    };

    // If password file cannot be opened, ignore it.
    let Some(mode) = files.mode(pgpassfile) else {
        return Ok(None);
    };
    if mode & S_IFMT != S_IFREG {
        return Err(PassfileWarning::NotPlainFile(pgpassfile.to_vec()));
    }
    // If password file is insecure, alert the user and ignore it.
    if mode & GROUP_OR_WORLD != 0 {
        return Err(PassfileWarning::GroupOrWorldAccess(pgpassfile.to_vec()));
    }
    let Some(contents) = files.read(pgpassfile) else {
        return Ok(None);
    };
    Ok(search(&contents, &[hostname, port, dbname, username]))
}

/// The read loop of `passwordFromFile` (`fe-connect.c:7977`-`:8038`) over
/// the file's bytes: the de-escaped password of the first line whose four
/// fields match `key` (host, port, database, user), or `None`.
///
/// The loop is followed buffer by buffer, not line by line, because its
/// bytes are C strings: `fgets` reads into the `PQExpBuffer` at most
/// `maxlen - len - 1` bytes at a time, `strlen` then drops everything from
/// a NUL on — so the rest of that line, newline included, is gone, and the
/// next line is appended to what came before the NUL — and a last line
/// without a newline that exactly fills the buffer is never matched, since
/// `fgets` has not yet seen the end of the file when it returns and next
/// returns NULL.
#[must_use]
pub fn search(contents: &[u8], key: &[&[u8]; 4]) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut maxlen = INITIAL_EXPBUFFER_SIZE;
    let mut pos = 0;
    let mut eof = false;

    while !eof {
        // Make sure there's a reasonable amount of room in the buffer
        // (enlargePQExpBuffer(&buf, 128), pqexpbuffer.c:172).
        let needed = buf.len() + 128 + 1;
        if needed > maxlen {
            let mut newlen = 2 * maxlen;
            while needed > newlen {
                newlen *= 2;
            }
            maxlen = newlen;
        }

        // Read some data, appending it to what we already have
        let rest = &contents[pos..];
        if rest.is_empty() {
            break;
        }
        let room = maxlen - buf.len() - 1;
        let chunk = match rest[..rest.len().min(room)]
            .iter()
            .position(|&c| c == b'\n')
        {
            Some(newline) => &rest[..=newline],
            None if rest.len() < room => {
                eof = true;
                rest
            }
            None => &rest[..room],
        };
        pos += chunk.len();
        let strlen = chunk.iter().position(|&c| c == 0).unwrap_or(chunk.len());
        buf.extend_from_slice(&chunk[..strlen]);

        // If we don't yet have a whole line, loop around to read more
        if buf.last() != Some(&b'\n') && !eof {
            continue;
        }

        // ignore comments
        if at(&buf, 0) != b'#' {
            // strip trailing newline and carriage return
            let mut len = buf.len();
            while len > 0 && (buf[len - 1] == b'\n' || buf[len - 1] == b'\r') {
                len -= 1;
            }
            let line = &buf[..len];
            if len > 0
                && let Some(rest) = key
                    .iter()
                    .try_fold(line, |rest, token| pwdf_matches_string(rest, token))
            {
                // Found a match.
                return Some(deescape(rest));
            }
        }

        // No match, reset buffer to prepare for next line.
        buf.clear();
    }
    None
}

/// `pwdfMatchesString`, `fe-connect.c:7869`: if the field at the start of
/// `buf` matches `token` — or is `*` — the rest of `buf` after its `:`.
///
/// A backslash escapes the byte after it, so `\:` and `\\` are literal. A
/// line whose last field has no `:` after it matches nothing.
#[must_use]
pub fn pwdf_matches_string<'a>(buf: &'a [u8], token: &[u8]) -> Option<&'a [u8]> {
    let mut tbuf = 0;
    let mut ttok = 0;
    let mut bslash = false;

    if at(buf, 0) == b'*' && at(buf, 1) == b':' {
        return Some(&buf[2..]);
    }
    while at(buf, tbuf) != 0 {
        if at(buf, tbuf) == b'\\' && !bslash {
            tbuf += 1;
            bslash = true;
        }
        if at(buf, tbuf) == b':' && at(token, ttok) == 0 && !bslash {
            return Some(&buf[tbuf + 1..]);
        }
        bslash = false;
        if at(token, ttok) == 0 {
            return None;
        }
        if at(buf, tbuf) == at(token, ttok) {
            tbuf += 1;
            ttok += 1;
        } else {
            return None;
        }
    }
    None
}

/// "De-escape password." (`fe-connect.c:8024`): up to the first unescaped
/// `:` or the end, each backslash dropped and the byte after it kept.
fn deescape(field: &[u8]) -> Vec<u8> {
    let mut password = Vec::new();
    let mut p1 = 0;
    while at(field, p1) != b':' && at(field, p1) != 0 {
        if at(field, p1) == b'\\' && at(field, p1 + 1) != 0 {
            p1 += 1;
        }
        password.push(at(field, p1));
        p1 += 1;
    }
    password
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::conninfo::parse_conninfo;

    /// Files with a mode each, for the checks the 0600 map cannot reach.
    struct Moded(BTreeMap<Vec<u8>, (u32, Vec<u8>)>);

    impl Moded {
        fn one(path: &str, mode: u32, contents: &str) -> Self {
            Self(BTreeMap::from([(
                path.as_bytes().to_vec(),
                (mode, contents.as_bytes().to_vec()),
            )]))
        }
    }

    impl Files for Moded {
        fn exists(&self, path: &[u8]) -> bool {
            self.0.contains_key(path)
        }

        fn read(&self, path: &[u8]) -> Option<Vec<u8>> {
            self.0.get(path).map(|(_, contents)| contents.clone())
        }

        fn mode(&self, path: &[u8]) -> Option<u32> {
            self.0.get(path).map(|(mode, _)| *mode)
        }
    }

    fn tree(path: &str, contents: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
        BTreeMap::from([(path.as_bytes().to_vec(), contents.as_bytes().to_vec())])
    }

    fn find(contents: &str, host: &str, port: &str, dbname: &str, user: &str) -> Option<String> {
        let key = [host, port, dbname, user].map(str::as_bytes);
        search(contents.as_bytes(), &key).map(|p| String::from_utf8(p).unwrap())
    }

    /// `src/test/authentication/t/001_password.pl:571`-`:575`: the file
    /// `.pgpass processing` starts with — a blank line, a comment longer than
    /// the 256-byte starting buffer, and one line whose password ends at the
    /// next `:`.
    const PGPASS_1: &str = "\n# This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file.\n*:*:postgres:scram_role:pass:this is not part of the password.\n";

    /// `001_password.pl:582`-`:586`, appended to [`PGPASS_1`]. Perl's `qq!!`
    /// makes `p\\ass` one backslash.
    const PGPASS_2: &str = "\n*:*:*:scram_role:p\\ass\n*:*:*:scram,role:p\\ass\n";

    /// `001_password.pl:579`-`:580`, `test_conn(…, 'password from pgpass', …)`
    /// for `user=scram_role` (found) and `user=md5_role` (no line, so no
    /// password and the connection fails).
    #[test]
    fn test_pgpass_processing() {
        let host = "/tmp/node";
        assert_eq!(
            find(PGPASS_1, host, "5432", "postgres", "scram_role").as_deref(),
            Some("pass")
        );
        assert_eq!(find(PGPASS_1, host, "5432", "postgres", "md5_role"), None);
    }

    /// `001_password.pl:582`-`:588`, and the later `test_conn`s of the same
    /// block (`:592`, `:610`, `:621`) that find their password there: the
    /// first matching line wins, and `p\ass` de-escapes to `pass`.
    #[test]
    fn test_pgpass_processing_appended() {
        let file = format!("{PGPASS_1}{PGPASS_2}");
        let host = "/tmp/node";
        assert_eq!(
            find(&file, host, "5432", "postgres", "scram_role").as_deref(),
            Some("pass")
        );
        assert_eq!(
            find(&file, host, "5432", "regex_testdb", "scram_role").as_deref(),
            Some("pass")
        );
        assert_eq!(
            find(&file, host, "5432", "postgres", "scram,role").as_deref(),
            Some("pass")
        );
        assert_eq!(find(&file, host, "5432", "postgres", "md5_role"), None);
    }

    #[test]
    fn each_field_matches_exactly_or_by_a_lone_star() {
        let file = "db.example:5433:app:alice:secret\n";
        let hit = |h, p, d, u| find(file, h, p, d, u);
        assert_eq!(
            hit("db.example", "5433", "app", "alice").as_deref(),
            Some("secret")
        );
        for (h, p, d, u) in [
            ("db.exampl", "5433", "app", "alice"),
            ("db.example.org", "5433", "app", "alice"),
            ("db.example", "5432", "app", "alice"),
            ("db.example", "5433", "ap", "alice"),
            ("db.example", "5433", "app", "alicia"),
        ] {
            assert_eq!(hit(h, p, d, u), None, "{h}:{p}:{d}:{u}");
        }
        // `*` is a wildcard only as the whole field.
        assert_eq!(
            find("*x:*:*:*:pw\n", "*x", "1", "d", "u").as_deref(),
            Some("pw")
        );
        assert_eq!(find("*x:*:*:*:pw\n", "ax", "1", "d", "u"), None);
        assert_eq!(find("a*:*:*:*:pw\n", "ab", "1", "d", "u"), None);
    }

    #[test]
    fn a_backslash_escapes_a_colon_or_a_backslash_in_any_field() {
        let file = "h\\:1:5432:d\\\\b:u:p\\:w\\\\d\\x:tail\n";
        assert_eq!(
            find(file, "h:1", "5432", "d\\b", "u").as_deref(),
            Some("p:w\\dx")
        );
        // A trailing lone backslash is kept in the password.
        assert_eq!(
            find("*:*:*:*:pw\\\n", "h", "1", "d", "u").as_deref(),
            Some("pw\\")
        );
    }

    #[test]
    fn a_line_needs_its_four_colons_and_the_password_may_be_empty() {
        assert_eq!(find("*:*:*:u\n", "h", "1", "d", "u"), None);
        assert_eq!(find("*:*:*:u:\n", "h", "1", "d", "u").as_deref(), Some(""));
        assert_eq!(find("*:*:*:*:\n", "h", "1", "d", "u").as_deref(), Some(""));
    }

    #[test]
    fn comments_blank_lines_and_line_endings() {
        let file = "# *:*:*:*:commented\n\r\n\n  *:*:*:*:indented\n";
        // A leading space is part of the host field, so nothing matches.
        assert_eq!(find(file, "h", "1", "d", "u"), None);
        // Only `#` in the first column starts a comment.
        assert_eq!(
            find(" # x\n*:*:*:*:pw\n", "h", "1", "d", "u").as_deref(),
            Some("pw")
        );
        // pg_strip_crlf removes every trailing \r and \n.
        assert_eq!(
            find("*:*:*:*:pw\r\r\n", "h", "1", "d", "u").as_deref(),
            Some("pw")
        );
        // The last line needs no newline.
        assert_eq!(
            find("*:*:*:*:pw", "h", "1", "d", "u").as_deref(),
            Some("pw")
        );
        assert_eq!(find("", "h", "1", "d", "u"), None);
    }

    /// Lines longer than the buffer are read whole: the buffer grows by
    /// doubling, and a match past the first 256 bytes is still found.
    #[test]
    fn a_line_longer_than_the_buffer_is_read_whole() {
        let long_host = "h".repeat(1000);
        let file = format!("{long_host}:*:*:*:pw\n");
        assert_eq!(
            find(&file, &long_host, "1", "d", "u").as_deref(),
            Some("pw")
        );
        let long_password = "p".repeat(5000);
        let file = format!("x:*:*:*:no\n*:*:*:*:{long_password}\n");
        assert_eq!(find(&file, "h", "1", "d", "u"), Some(long_password));
    }

    /// `strlen` after `fgets`: from a NUL on, the rest of that line is lost
    /// and the next line is appended where the NUL was.
    #[test]
    fn a_nul_splices_the_next_line_on_as_strlen_does() {
        let file = "*:*:*:*:\0ignored\nsecond\n";
        assert_eq!(find(file, "h", "1", "d", "u").as_deref(), Some("second"));
        let file = "*:*:*:u\0:ignored\n:pw\n";
        assert_eq!(find(file, "h", "1", "d", "u").as_deref(), Some("pw"));
    }

    /// A last line with no newline that fills the 256-byte buffer to its
    /// 255 usable bytes: `fgets` stops for want of room without seeing the
    /// end of the file, the line is incomplete, and the next `fgets`
    /// returns NULL — so C never matches it. One byte shorter, `fgets`
    /// reaches the end and the line is used.
    #[test]
    fn a_last_line_that_exactly_fills_the_buffer_is_never_matched() {
        let prefix = "*:*:*:*:";
        let exact = format!("{prefix}{}", "p".repeat(255 - prefix.len()));
        assert_eq!(exact.len(), 255);
        assert_eq!(find(&exact, "h", "1", "d", "u"), None);
        let short = &exact[..254];
        assert_eq!(
            find(short, "h", "1", "d", "u").as_deref(),
            Some(&short[prefix.len()..])
        );
        // With a newline it is an ordinary line.
        let file = format!("{exact}\n");
        assert_eq!(
            find(&file, "h", "1", "d", "u").as_deref(),
            Some(&exact[prefix.len()..])
        );
    }

    /// `fe-connect.c:7921`-`:7934`: an empty host and the default socket
    /// directory both look up as `localhost`, and an empty port as
    /// `DEF_PGPORT_STR`.
    #[test]
    fn localhost_matches_the_default_socket_directory_and_an_empty_host() {
        let files = tree("/pw", "localhost:5432:d:u:local\n*:*:*:*:other\n");
        let lookup = |host: &str, port: &str| {
            password_from_file(host.as_bytes(), port.as_bytes(), b"d", b"u", b"/pw", &files)
                .unwrap()
                .map(|p| String::from_utf8(p).unwrap())
        };
        assert_eq!(lookup("", "").as_deref(), Some("local"));
        assert_eq!(lookup("localhost", "5432").as_deref(), Some("local"));
        let default_dir = if DEFAULT_PGSOCKET_DIR.is_empty() {
            "localhost"
        } else {
            DEFAULT_PGSOCKET_DIR
        };
        assert_eq!(lookup(default_dir, "").as_deref(), Some("local"));
        // Another socket directory is matched by its own name.
        assert_eq!(
            lookup("/var/run/postgresql", "5432").as_deref(),
            Some("other")
        );
        assert_eq!(
            lookup(&format!("{default_dir}/"), "5432").as_deref(),
            Some("other")
        );
    }

    /// `fe-connect.c:7915`-`:7919`: without a database or a user the file is
    /// not even opened, so a file with bad permissions says nothing.
    #[test]
    fn no_database_or_no_user_skips_the_file_without_a_warning() {
        let files = Moded::one("/pw", 0o100_644, "*:*:*:*:pw\n");
        assert_eq!(
            password_from_file(b"h", b"1", b"", b"u", b"/pw", &files),
            Ok(None)
        );
        assert_eq!(
            password_from_file(b"h", b"1", b"d", b"", b"/pw", &files),
            Ok(None)
        );
    }

    /// The acceptance of NAT-393: a 0644 file is ignored with upstream's
    /// exact warning (`fe-connect.c:7960`); so is any group or world bit.
    #[test]
    fn a_file_with_group_or_world_access_is_ignored_with_the_warning() {
        for mode in [0o644, 0o640, 0o604, 0o610, 0o601, 0o660, 0o777] {
            let files = Moded::one("/home/u/.pgpass", 0o100_000 | mode, "*:*:*:*:pw\n");
            let warning =
                password_from_file(b"h", b"1", b"d", b"u", b"/home/u/.pgpass", &files).unwrap_err();
            assert_eq!(
                String::from_utf8(warning.message()).unwrap(),
                "WARNING: password file \"/home/u/.pgpass\" has group or world access; \
                 permissions should be u=rw (0600) or less\n",
                "mode {mode:o}"
            );
        }
        for mode in [0o600, 0o400, 0o200, 0o000, 0o700] {
            let files = Moded::one("/pw", 0o100_000 | mode, "*:*:*:*:pw\n");
            assert_eq!(
                password_from_file(b"h", b"1", b"d", b"u", b"/pw", &files),
                Ok(Some(b"pw".to_vec())),
                "mode {mode:o}"
            );
        }
    }

    /// `fe-connect.c:7950`: a directory, FIFO, … is not a plain file. The
    /// type is checked before the permissions.
    #[test]
    fn a_file_that_is_not_regular_is_ignored_with_the_warning() {
        for kind in [0o040_000, 0o010_000, 0o020_000, 0o120_000] {
            let files = Moded::one("/pw", kind | 0o755, "");
            let warning = password_from_file(b"h", b"1", b"d", b"u", b"/pw", &files).unwrap_err();
            assert_eq!(
                String::from_utf8(warning.message()).unwrap(),
                "WARNING: password file \"/pw\" is not a plain file\n",
                "type {kind:o}"
            );
        }
    }

    /// `fe-connect.c:7937`: a file that cannot be opened is ignored silently.
    #[test]
    fn a_missing_file_is_ignored_silently() {
        let files = BTreeMap::new();
        assert_eq!(
            password_from_file(b"h", b"1", b"d", b"u", b"/pw", &files),
            Ok(None)
        );
    }

    fn lookup(conninfo: &str, env: &Env, files: &impl Files) -> PasswordLookup {
        PasswordLookup::new(&parse_conninfo(conninfo.as_bytes()).unwrap(), env, files)
    }

    fn found(password: &str, passfile: &str) -> PasswordLookup {
        PasswordLookup {
            found: Some(FilePassword {
                password: password.as_bytes().to_vec(),
                passfile: passfile.as_bytes().to_vec(),
            }),
            warning: None,
        }
    }

    /// `fe-connect.c:1426`: a given password means no lookup at all — not
    /// even the permission check; an empty one does not count as given.
    #[test]
    fn a_given_password_skips_the_file() {
        let env = Env::empty().with("HOME", "/home/u");
        let files = Moded::one("/home/u/.pgpass", 0o100_644, "*:*:*:*:pw\n");
        assert_eq!(
            lookup("user=u dbname=d password=x", &env, &files),
            PasswordLookup::default()
        );
        assert!(
            lookup("user=u dbname=d password=", &env, &files)
                .warning
                .is_some()
        );
    }

    /// `fe-connect.c:1429`-`:1442`: `passfile` (which `PGPASSFILE` fills), else
    /// `$HOME/.pgpass`; with neither there is nothing to read.
    #[test]
    fn the_passfile_option_wins_over_home() {
        let mut files = tree("/home/u/.pgpass", "*:*:*:*:home\n");
        files.insert(b"/elsewhere".to_vec(), b"*:*:*:*:elsewhere\n".to_vec());
        let env = Env::empty().with("HOME", "/home/u");
        assert_eq!(
            lookup("user=u dbname=d", &env, &files),
            found("home", "/home/u/.pgpass")
        );
        assert_eq!(
            lookup("user=u dbname=d passfile=", &env, &files),
            found("home", "/home/u/.pgpass")
        );
        assert_eq!(
            lookup("user=u dbname=d passfile=/elsewhere", &env, &files),
            found("elsewhere", "/elsewhere")
        );
        assert_eq!(
            lookup("user=u dbname=d", &Env::empty(), &files),
            PasswordLookup::default()
        );
        assert_eq!(
            lookup("user=u dbname=d", &Env::empty().with("HOME", ""), &files),
            PasswordLookup::default()
        );
        // PGPASSFILE reaches the lookup through conninfo_add_defaults.
        let mut info = parse_conninfo(b"user=u dbname=d").unwrap();
        let env = env.with("PGPASSFILE", "/elsewhere");
        info.add_defaults(&env, &files).unwrap();
        assert_eq!(
            PasswordLookup::new(&info, &env, &files),
            found("elsewhere", "/elsewhere")
        );
    }

    /// The key `pqConnectOptions2` has built by `:1446`: `host`, else
    /// `hostaddr`, else the default host; the database defaults to the user.
    #[test]
    fn the_key_is_the_one_pq_connect_options2_has_built() {
        let env = Env::empty().with("HOME", "/h");
        let files = tree(
            "/h/.pgpass",
            "db1:*:*:*:by-host\n10.0.0.1:*:*:*:by-hostaddr\nlocalhost:*:*:*:default\n\
             *:*:u:u:dbname-is-user\n",
        );
        let pw = |conninfo| {
            lookup(conninfo, &env, &files)
                .found
                .map(|f| String::from_utf8(f.password).unwrap())
        };
        assert_eq!(
            pw("host=db1 hostaddr=10.0.0.1 user=u dbname=d").as_deref(),
            Some("by-host")
        );
        assert_eq!(
            pw("hostaddr=10.0.0.1 user=u dbname=d").as_deref(),
            Some("by-hostaddr")
        );
        assert_eq!(pw("user=u dbname=d").as_deref(), Some("default"));
        assert_eq!(pw("host=other user=u").as_deref(), Some("dbname-is-user"));
        assert_eq!(
            pw("host=other user=u dbname=").as_deref(),
            Some("dbname-is-user")
        );
        assert_eq!(pw("host=other user=u dbname=d"), None);
    }
}
