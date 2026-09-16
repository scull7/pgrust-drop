//! The text C initdb prints, reproduced verbatim from `initdb.c` 18.6
//! (`usage()`, the `--version` line, `pg_log_error_hint` for `--help`).

/// `PG_VERSION` from `pg_config.h` for the tree this port tracks.
pub const PG_VERSION: &str = "18.6";

/// The name C initdb reports for itself (`get_progname(argv[0])`).
pub const PROGNAME: &str = "initdb";

const PACKAGE_BUGREPORT: &str = "pgsql-bugs@lists.postgresql.org";
const PACKAGE_NAME: &str = "PostgreSQL";
const PACKAGE_URL: &str = "https://www.postgresql.org/";

/// `usage(progname)` in `initdb.c`, with the `printf` calls concatenated.
/// Every space, including the odd ` [-D, --pgdata=]DATADIR` line, is upstream's.
const USAGE_TEMPLATE: &str = "\
{progname} initializes a PostgreSQL database cluster.

Usage:
  {progname} [OPTION]... [DATADIR]

Options:
  -A, --auth=METHOD         default authentication method for local connections
      --auth-host=METHOD    default authentication method for local TCP/IP connections
      --auth-local=METHOD   default authentication method for local-socket connections
 [-D, --pgdata=]DATADIR     location for this database cluster
  -E, --encoding=ENCODING   set default encoding for new databases
  -g, --allow-group-access  allow group read/execute on data directory
      --icu-locale=LOCALE   set ICU locale ID for new databases
      --icu-rules=RULES     set additional ICU collation rules for new databases
  -k, --data-checksums      use data page checksums
      --locale=LOCALE       set default locale for new databases
      --lc-collate=, --lc-ctype=, --lc-messages=LOCALE
      --lc-monetary=, --lc-numeric=, --lc-time=LOCALE
                            set default locale in the respective category for
                            new databases (default taken from environment)
      --no-locale           equivalent to --locale=C
      --builtin-locale=LOCALE
                            set builtin locale name for new databases
      --locale-provider={builtin|libc|icu}
                            set default locale provider for new databases
      --no-data-checksums   do not use data page checksums
      --pwfile=FILE         read password for the new superuser from file
  -T, --text-search-config=CFG
                            default text search configuration
  -U, --username=NAME       database superuser name
  -W, --pwprompt            prompt for a password for the new superuser
  -X, --waldir=WALDIR       location for the write-ahead log directory
      --wal-segsize=SIZE    size of WAL segments, in megabytes

Less commonly used options:
  -c, --set NAME=VALUE      override default setting for server parameter
  -d, --debug               generate lots of debugging output
      --discard-caches      set debug_discard_caches=1
  -L DIRECTORY              where to find the input files
  -n, --no-clean            do not clean up after errors
  -N, --no-sync             do not wait for changes to be written safely to disk
      --no-sync-data-files  do not sync files within database directories
      --no-instructions     do not print instructions for next steps
  -s, --show                show internal settings, then exit
      --sync-method=METHOD  set method for syncing files to disk
  -S, --sync-only           only sync database files to disk, then exit

Other options:
  -V, --version             output version information, then exit
  -?, --help                show this help, then exit

If the data directory is not specified, the environment variable PGDATA
is used.

Report bugs to <{bugreport}>.
{package} home page: <{url}>
";

/// The complete `--help` text for `progname`.
#[must_use]
pub fn usage(progname: &str) -> String {
    USAGE_TEMPLATE
        .replace("{progname}", progname)
        .replace("{bugreport}", PACKAGE_BUGREPORT)
        .replace("{package}", PACKAGE_NAME)
        .replace("{url}", PACKAGE_URL)
}

/// `puts("initdb (PostgreSQL) " PG_VERSION)`.
#[must_use]
pub fn version_line(progname: &str) -> String {
    format!("{progname} (PostgreSQL) {PG_VERSION}")
}

/// The message of `pg_log_error_hint("Try \"%s --help\" for more information.",
/// progname)`, without the `progname: hint: ` that `src/common/logging.c`
/// prefixes.
///
/// `initdb.c` emits this from four sites (`:3274`, `:3400`, `:3420` and the
/// `getopt_long` `default:` arm), two of which are reached through
/// [`crate::error::InitdbError`] and two directly, so the string itself lives
/// here — in the module that holds the text C prints — and nowhere else.
#[must_use]
pub fn try_help(progname: &str) -> String {
    format!("Try \"{progname} --help\" for more information.")
}

/// [`try_help`] as a whole stderr line, the way `pg_log_generic_v` renders a
/// hint with no `pg_log_error` above it.
#[must_use]
pub fn try_help_hint(progname: &str) -> String {
    format!("{progname}: hint: {}", try_help(progname))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_starts_and_ends_like_upstream() {
        let text = usage("initdb");
        assert!(text.starts_with("initdb initializes a PostgreSQL database cluster.\n\nUsage:\n  initdb [OPTION]... [DATADIR]\n"));
        assert!(text.ends_with("Report bugs to <pgsql-bugs@lists.postgresql.org>.\nPostgreSQL home page: <https://www.postgresql.org/>\n"));
    }

    #[test]
    fn usage_has_no_unfilled_placeholders() {
        // `{builtin|libc|icu}` is upstream text; only our own placeholders must be gone.
        let text = usage("initdb");
        for placeholder in ["{progname}", "{bugreport}", "{package}", "{url}"] {
            assert!(
                !text.contains(placeholder),
                "{placeholder} left in help text"
            );
        }
    }

    #[test]
    fn usage_keeps_the_pgdata_bracket_line_verbatim() {
        assert!(
            usage("initdb")
                .contains("\n [-D, --pgdata=]DATADIR     location for this database cluster\n")
        );
    }

    #[test]
    fn usage_lines_fit_program_help_ok() {
        // program_help_ok caps help lines at 95 characters.
        let text = usage("initdb");
        let too_long: Vec<&str> = text.lines().filter(|l| l.chars().count() > 95).collect();
        assert!(too_long.is_empty(), "{too_long:?}");
    }

    #[test]
    fn version_and_hint_text() {
        assert_eq!(version_line("initdb"), "initdb (PostgreSQL) 18.6");
        assert_eq!(
            try_help_hint("initdb"),
            "initdb: hint: Try \"initdb --help\" for more information."
        );
    }
}
