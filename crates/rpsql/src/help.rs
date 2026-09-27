//! The text C psql prints for `--help`, `--help=commands` and
//! `--help=variables`: `usage()`, `slashUsage()` and `helpVariables()` in
//! `src/bin/psql/help.c` (PostgreSQL 18.6), reproduced verbatim.
//!
//! Each function returns the whole buffer upstream builds with `HELP0`/`HELPN`
//! before counting its lines and handing it to `PageOutput`. The command-line
//! paths call all three with `NOPAGER` (`startup.c:89`), so no pager and no
//! terminal width are involved and the text is the same on every terminal.
//!
//! Upstream's `#ifdef`s are resolved for the builds this port is gated against:
//! not `WIN32`, and `USE_READLINE` defined (PGDG, Alpine and Homebrew all link
//! a line editor), so `\s` is listed.

use crate::settings::{DEFAULT_CSV_FIELD_SEP, DEFAULT_FIELD_SEP, DEFAULT_WATCH_INTERVAL};
use crate::startup::Switches;

const PACKAGE_BUGREPORT: &str = "pgsql-bugs@lists.postgresql.org";
const PACKAGE_NAME: &str = "PostgreSQL";
const PACKAGE_URL: &str = "https://www.postgresql.org/";

/// `usage()` (`help.c:48`-`:139`), with the `HELP0` calls concatenated.
const USAGE_TEMPLATE: &str = r#"psql is the PostgreSQL interactive terminal.

Usage:
  psql [OPTION]... [DBNAME [USERNAME]]

General options:
  -c, --command=COMMAND    run only single command (SQL or internal) and exit
  -d, --dbname=DBNAME      database name to connect to
  -f, --file=FILENAME      execute commands from file, then exit
  -l, --list               list available databases, then exit
  -v, --set=, --variable=NAME=VALUE
                           set psql variable NAME to VALUE
                           (e.g., -v ON_ERROR_STOP=1)
  -V, --version            output version information, then exit
  -X, --no-psqlrc          do not read startup file (~/.psqlrc)
  -1 ("one"), --single-transaction
                           execute as a single transaction (if non-interactive)
  -?, --help[=options]     show this help, then exit
      --help=commands      list backslash commands, then exit
      --help=variables     list special variables, then exit

Input and output options:
  -a, --echo-all           echo all input from script
  -b, --echo-errors        echo failed commands
  -e, --echo-queries       echo commands sent to server
  -E, --echo-hidden        display queries that internal commands generate
  -L, --log-file=FILENAME  send session log to file
  -n, --no-readline        disable enhanced command line editing (readline)
  -o, --output=FILENAME    send query results to file (or |pipe)
  -q, --quiet              run quietly (no messages, only query output)
  -s, --single-step        single-step mode (confirm each query)
  -S, --single-line        single-line mode (end of line terminates SQL command)

Output format options:
  -A, --no-align           unaligned table output mode
      --csv                CSV (Comma-Separated Values) table output mode
  -F, --field-separator=STRING
                           field separator for unaligned output (default: "{field_sep}")
  -H, --html               HTML table output mode
  -P, --pset=VAR[=ARG]     set printing option VAR to ARG (see \pset command)
  -R, --record-separator=STRING
                           record separator for unaligned output (default: newline)
  -t, --tuples-only        print rows only
  -T, --table-attr=TEXT    set HTML table tag attributes (e.g., width, border)
  -x, --expanded           turn on expanded table output
  -z, --field-separator-zero
                           set field separator for unaligned output to zero byte
  -0, --record-separator-zero
                           set record separator for unaligned output to zero byte

Connection options:
  -h, --host=HOSTNAME      database server host or socket directory
  -p, --port=PORT          database server port
  -U, --username=USERNAME  database user name
  -w, --no-password        never prompt for password
  -W, --password           force password prompt (should happen automatically)

For more information, type "\?" (for internal commands) or "\help" (for SQL
commands) from within psql, or consult the psql section in the PostgreSQL
documentation.

Report bugs to <{bugreport}>.
{package} home page: <{url}>
"#;

/// `slashUsage()` (`help.c:148`-`:365`), with the `HELP0` calls concatenated
/// and the five `HELPN` arguments as placeholders.
const SLASH_USAGE_TEMPLATE: &str = r"General
  \copyright             show PostgreSQL usage and distribution terms
  \crosstabview [COLUMNS] execute query and display result in crosstab
  \errverbose            show most recent error message at maximum verbosity
  \g [(OPTIONS)] [FILE]  execute query (and send result to file or |pipe);
                         \g with no arguments is equivalent to a semicolon
  \gdesc                 describe result of query, without executing it
  \gexec                 execute query, then execute each value in its result
  \gset [PREFIX]         execute query and store result in psql variables
  \gx [(OPTIONS)] [FILE] as \g, but forces expanded output mode
  \q                     quit psql
  \restrict RESTRICT_KEY
                         enter restricted mode with provided key
  \unrestrict RESTRICT_KEY
                         exit restricted mode if key matches
  \watch [[i=]SEC] [c=N] [m=MIN]
                         execute query every SEC seconds, up to N times,
                         stop if less than MIN rows are returned

Help
  \? [commands]          show help on backslash commands
  \? options             show help on psql command-line options
  \? variables           show help on special variables
  \h [NAME]              help on syntax of SQL commands, * for all commands

Query Buffer
  \e [FILE] [LINE]       edit the query buffer (or file) with external editor
  \ef [FUNCNAME [LINE]]  edit function definition with external editor
  \ev [VIEWNAME [LINE]]  edit view definition with external editor
  \p                     show the contents of the query buffer
  \r                     reset (clear) the query buffer
  \s [FILE]              display history or save it to file
  \w FILE                write query buffer to file

Input/Output
  \copy ...              perform SQL COPY with data stream to the client host
  \echo [-n] [STRING]    write string to standard output (-n for no newline)
  \i FILE                execute commands from file
  \ir FILE               as \i, but relative to location of current script
  \o [FILE]              send all query results to file or |pipe
  \qecho [-n] [STRING]   write string to \o output stream (-n for no newline)
  \warn [-n] [STRING]    write string to standard error (-n for no newline)

Conditional
  \if EXPR               begin conditional block
  \elif EXPR             alternative within current conditional block
  \else                  final alternative within current conditional block
  \endif                 end conditional block

Informational
  (options: S = show system objects, x = expanded mode, + = additional detail)
  \d[Sx+]                list tables, views, and sequences
  \d[S+]   NAME          describe table, view, sequence, or index
  \da[Sx]  [PATTERN]     list aggregates
  \dA[x+]  [PATTERN]     list access methods
  \dAc[x+] [AMPTRN [TYPEPTRN]]  list operator classes
  \dAf[x+] [AMPTRN [TYPEPTRN]]  list operator families
  \dAo[x+] [AMPTRN [OPFPTRN]]   list operators of operator families
  \dAp[x+] [AMPTRN [OPFPTRN]]   list support functions of operator families
  \db[x+]  [PATTERN]     list tablespaces
  \dc[Sx+] [PATTERN]     list conversions
  \dconfig[x+] [PATTERN] list configuration parameters
  \dC[x+]  [PATTERN]     list casts
  \dd[Sx]  [PATTERN]     show object descriptions not displayed elsewhere
  \dD[Sx+] [PATTERN]     list domains
  \ddp[x]  [PATTERN]     list default privileges
  \dE[Sx+] [PATTERN]     list foreign tables
  \des[x+] [PATTERN]     list foreign servers
  \det[x+] [PATTERN]     list foreign tables
  \deu[x+] [PATTERN]     list user mappings
  \dew[x+] [PATTERN]     list foreign-data wrappers
  \df[anptw][Sx+] [FUNCPTRN [TYPEPTRN ...]]
                         list [only agg/normal/procedure/trigger/window] functions
  \dF[x+]  [PATTERN]     list text search configurations
  \dFd[x+] [PATTERN]     list text search dictionaries
  \dFp[x+] [PATTERN]     list text search parsers
  \dFt[x+] [PATTERN]     list text search templates
  \dg[Sx+] [PATTERN]     list roles
  \di[Sx+] [PATTERN]     list indexes
  \dl[x+]                list large objects, same as \lo_list
  \dL[Sx+] [PATTERN]     list procedural languages
  \dm[Sx+] [PATTERN]     list materialized views
  \dn[Sx+] [PATTERN]     list schemas
  \do[Sx+] [OPPTRN [TYPEPTRN [TYPEPTRN]]]
                         list operators
  \dO[Sx+] [PATTERN]     list collations
  \dp[Sx]  [PATTERN]     list table, view, and sequence access privileges
  \dP[itnx+] [PATTERN]   list [only index/table] partitioned relations [n=nested]
  \drds[x] [ROLEPTRN [DBPTRN]]
                         list per-database role settings
  \drg[Sx] [PATTERN]     list role grants
  \dRp[x+] [PATTERN]     list replication publications
  \dRs[x+] [PATTERN]     list replication subscriptions
  \ds[Sx+] [PATTERN]     list sequences
  \dt[Sx+] [PATTERN]     list tables
  \dT[Sx+] [PATTERN]     list data types
  \du[Sx+] [PATTERN]     list roles
  \dv[Sx+] [PATTERN]     list views
  \dx[x+]  [PATTERN]     list extensions
  \dX[x]   [PATTERN]     list extended statistics
  \dy[x+]  [PATTERN]     list event triggers
  \l[x+]   [PATTERN]     list databases
  \sf[+]   FUNCNAME      show a function's definition
  \sv[+]   VIEWNAME      show a view's definition
  \z[Sx]   [PATTERN]     same as \dp

Large Objects
  \lo_export LOBOID FILE write large object to file
  \lo_import FILE [COMMENT]
                         read large object from file
  \lo_list[x+]           list large objects
  \lo_unlink LOBOID      delete a large object

Formatting
  \a                     toggle between unaligned and aligned output mode
  \C [STRING]            set table title, or unset if none
  \f [STRING]            show or set field separator for unaligned query output
  \H                     toggle HTML output mode (currently {html})
  \pset [NAME [VALUE]]   set table output option
                         (border|columns|csv_fieldsep|expanded|fieldsep|
                         fieldsep_zero|footer|format|linestyle|null|
                         numericlocale|pager|pager_min_lines|recordsep|
                         recordsep_zero|tableattr|title|tuples_only|
                         unicode_border_linestyle|unicode_column_linestyle|
                         unicode_header_linestyle|xheader_width)
  \t [on|off]            show only rows (currently {tuples_only})
  \T [STRING]            set HTML <table> tag attributes, or unset if none
  \x [on|off|auto]       toggle expanded output (currently {expanded})

Connection
  \c[onnect] {[DBNAME|- USER|- HOST|- PORT|-] | conninfo}
                         connect to new database (currently {currdb})
  \conninfo              display information about current connection
  \encoding [ENCODING]   show or set client encoding
  \password [USERNAME]   securely change the password for a user

Operating System
  \cd [DIR]              change the current working directory
  \getenv PSQLVAR ENVVAR fetch environment variable
  \setenv NAME [VALUE]   set or unset environment variable
  \timing [on|off]       toggle timing of commands (currently {timing})
  \! [COMMAND]           execute command in shell or start interactive shell

Variables
  \prompt [TEXT] NAME    prompt user to set internal variable
  \set [NAME [VALUE]]    set internal variable, or list all if no parameters
  \unset NAME            unset (delete) internal variable

Extended Query Protocol
  \bind [PARAM]...       set query parameters
  \bind_named STMT_NAME [PARAM]...
                         set query parameters for an existing prepared statement
  \close_prepared STMT_NAME
                         close an existing prepared statement
  \endpipeline           exit pipeline mode
  \flush                 flush output data to the server
  \flushrequest          send request to the server to flush its output buffer
  \getresults [NUM_RES]  read NUM_RES pending results, or all if no argument
  \parse STMT_NAME       create a prepared statement
  \sendpipeline          send an extended query to an ongoing pipeline
  \startpipeline         enter pipeline mode
  \syncpipeline          add a synchronisation point to an ongoing pipeline
";

/// `helpVariables()` (`help.c:374`-`:584`), with the `HELP0` calls
/// concatenated and the three `HELPN` arguments as placeholders.
const HELP_VARIABLES_TEMPLATE: &str = r#"List of specially treated variables

psql variables:
Usage:
  psql --set=NAME=VALUE
  or \set NAME VALUE inside psql

  AUTOCOMMIT
    if set, successful SQL commands are automatically committed
  COMP_KEYWORD_CASE
    determines the case used to complete SQL key words
    [lower, upper, preserve-lower, preserve-upper]
  DBNAME
    the currently connected database name
  ECHO
    controls what input is written to standard output
    [all, errors, none, queries]
  ECHO_HIDDEN
    if set, display internal queries executed by backslash commands;
    if set to "noexec", just show them without execution
  ENCODING
    current client character set encoding
  ERROR
    "true" if last query failed, else "false"
  FETCH_COUNT
    the number of result rows to fetch and display at a time (0 = unlimited)
  HIDE_TABLEAM
    if set, table access methods are not displayed
  HIDE_TOAST_COMPRESSION
    if set, compression methods are not displayed
  HISTCONTROL
    controls command history [ignorespace, ignoredups, ignoreboth]
  HISTFILE
    file name used to store the command history
  HISTSIZE
    maximum number of commands to store in the command history
  HOST
    the currently connected database server host
  IGNOREEOF
    number of EOFs needed to terminate an interactive session
  LASTOID
    value of the last affected OID
  LAST_ERROR_MESSAGE
  LAST_ERROR_SQLSTATE
    message and SQLSTATE of last error, or empty string and "00000" if none
  ON_ERROR_ROLLBACK
    if set, an error doesn't stop a transaction (uses implicit savepoints)
  ON_ERROR_STOP
    stop batch execution after error
  PORT
    server port of the current connection
  PROMPT1
    specifies the standard psql prompt
  PROMPT2
    specifies the prompt used when a statement continues from a previous line
  PROMPT3
    specifies the prompt used during COPY ... FROM STDIN
  QUIET
    run quietly (same as -q option)
  ROW_COUNT
    number of rows returned or affected by last query, or 0
  SERVER_VERSION_NAME
  SERVER_VERSION_NUM
    server's version (in short string or numeric format)
  SHELL_ERROR
    "true" if the last shell command failed, "false" if it succeeded
  SHELL_EXIT_CODE
    exit status of the last shell command
  SHOW_ALL_RESULTS
    show all results of a combined query (\;) instead of only the last
  SHOW_CONTEXT
    controls display of message context fields [never, errors, always]
  SINGLELINE
    if set, end of line terminates SQL commands (same as -S option)
  SINGLESTEP
    single-step mode (same as -s option)
  SQLSTATE
    SQLSTATE of last query, or "00000" if no error
  USER
    the currently connected database user
  VERBOSITY
    controls verbosity of error reports [default, verbose, terse, sqlstate]
  VERSION
  VERSION_NAME
  VERSION_NUM
    psql's version (in verbose string, short string, or numeric format)
  WATCH_INTERVAL
    number of seconds \watch waits between executions (default {watch_interval})

Display settings:
Usage:
  psql --pset=NAME[=VALUE]
  or \pset NAME [VALUE] inside psql

  border
    border style (number)
  columns
    target width for the wrapped format
  csv_fieldsep
    field separator for CSV output format (default "{csv_field_sep}")
  expanded (or x)
    expanded output [on, off, auto]
  fieldsep
    field separator for unaligned output (default "{field_sep}")
  fieldsep_zero
    set field separator for unaligned output to a zero byte
  footer
    enable or disable display of the table footer [on, off]
  format
    set output format [unaligned, aligned, wrapped, html, asciidoc, ...]
  linestyle
    set the border line drawing style [ascii, old-ascii, unicode]
  null
    set the string to be printed in place of a null value
  numericlocale
    enable display of a locale-specific character to separate groups of digits
  pager
    control when an external pager is used [yes, no, always]
  recordsep
    record (line) separator for unaligned output
  recordsep_zero
    set record separator for unaligned output to a zero byte
  tableattr (or T)
    specify attributes for table tag in html format, or proportional
    column widths for left-aligned data types in latex-longtable format
  title
    set the table title for subsequently printed tables
  tuples_only
    if set, only actual table data is shown
  unicode_border_linestyle
  unicode_column_linestyle
  unicode_header_linestyle
    set the style of Unicode line drawing [single, double]
  xheader_width
    set the maximum width of the header for expanded output
    [full, column, page, integer value]

Environment variables:
Usage:
  NAME=VALUE [NAME=VALUE] psql ...
  or \setenv NAME [VALUE] inside psql

  COLUMNS
    number of columns for wrapped format
  PGAPPNAME
    same as the application_name connection parameter
  PGDATABASE
    same as the dbname connection parameter
  PGHOST
    same as the host connection parameter
  PGPASSFILE
    password file name
  PGPASSWORD
    connection password (not recommended)
  PGPORT
    same as the port connection parameter
  PGUSER
    same as the user connection parameter
  PSQL_EDITOR, EDITOR, VISUAL
    editor used by the \e, \ef, and \ev commands
  PSQL_EDITOR_LINENUMBER_ARG
    how to specify a line number when invoking the editor
  PSQL_HISTORY
    alternative location for the command history file
  PSQL_PAGER, PAGER
    name of external pager program
  PSQL_WATCH_PAGER
    name of external pager program used for \watch
  PSQLRC
    alternative location for the user's .psqlrc file
  SHELL
    shell used by the \! command
  TMPDIR
    directory for temporary files
"#;

/// What `slashUsage()` reads from `pset` to fill its `(currently …)` notes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlashUsageState<'a> {
    /// `\H`, `\t` and `\x`: `pset.popt.topt`'s format, `tuples_only` and
    /// `expanded`. This port has no `expanded = auto` yet (NAT-400).
    pub switches: Switches,
    /// `pset.timing`
    pub timing: bool,
    /// `PQdb(pset.db)`: `None` when there is no connection.
    pub currdb: Option<&'a str>,
}

/// `ON(var)` (`help.c:39`).
fn on(var: bool) -> &'static str {
    if var { "on" } else { "off" }
}

/// `usage()`: the whole `--help` text.
#[must_use]
pub fn usage() -> String {
    USAGE_TEMPLATE
        .replace("{field_sep}", DEFAULT_FIELD_SEP)
        .replace("{bugreport}", PACKAGE_BUGREPORT)
        .replace("{package}", PACKAGE_NAME)
        .replace("{url}", PACKAGE_URL)
}

/// `slashUsage()`: the whole `--help=commands` / `\?` text for `state`.
#[must_use]
pub fn slash_usage(state: &SlashUsageState<'_>) -> String {
    // `help.c:307`-`:313`: the database name is quoted, "no connection" is not.
    let currdb = match state.currdb {
        Some(db) => format!("\"{db}\""),
        None => "no connection".to_string(),
    };
    // The database name goes last: it is the only substitution that carries
    // user text, which must not be scanned for the other placeholders.
    SLASH_USAGE_TEMPLATE
        .replace("{html}", on(state.switches.html))
        .replace("{tuples_only}", on(state.switches.tuples_only))
        .replace("{expanded}", on(state.switches.expanded))
        .replace("{timing}", on(state.timing))
        .replace("{currdb}", &currdb)
}

/// `helpVariables()`: the whole `--help=variables` text.
#[must_use]
pub fn help_variables() -> String {
    HELP_VARIABLES_TEMPLATE
        .replace("{watch_interval}", DEFAULT_WATCH_INTERVAL)
        .replace("{csv_field_sep}", &DEFAULT_CSV_FIELD_SEP.to_string())
        .replace("{field_sep}", DEFAULT_FIELD_SEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_starts_and_ends_like_upstream() {
        let text = usage();
        assert!(text.starts_with(
            "psql is the PostgreSQL interactive terminal.\n\nUsage:\n  psql [OPTION]... [DBNAME [USERNAME]]\n\n"
        ));
        assert!(text.contains("(default: \"|\")\n"));
        assert!(text.ends_with(
            "Report bugs to <pgsql-bugs@lists.postgresql.org>.\nPostgreSQL home page: <https://www.postgresql.org/>\n"
        ));
    }

    #[test]
    fn slash_usage_fills_every_currently_note() {
        let idle = slash_usage(&SlashUsageState::default());
        assert!(idle.contains("toggle HTML output mode (currently off)\n"));
        assert!(idle.contains("show only rows (currently off)\n"));
        assert!(idle.contains("toggle expanded output (currently off)\n"));
        assert!(idle.contains("toggle timing of commands (currently off)\n"));
        assert!(idle.contains("connect to new database (currently no connection)\n"));

        let busy = slash_usage(&SlashUsageState {
            switches: Switches {
                html: true,
                tuples_only: true,
                expanded: true,
            },
            timing: true,
            currdb: Some("{timing}"),
        });
        assert!(busy.contains("toggle HTML output mode (currently on)\n"));
        assert!(busy.contains("show only rows (currently on)\n"));
        assert!(busy.contains("toggle expanded output (currently on)\n"));
        assert!(busy.contains("toggle timing of commands (currently on)\n"));
        // A database name is quoted and taken literally.
        assert!(busy.contains("connect to new database (currently \"{timing}\")\n"));
    }

    #[test]
    fn slash_usage_keeps_the_conninfo_braces() {
        // `{[DBNAME|- USER|- HOST|- PORT|-] | conninfo}` is upstream text,
        // not a placeholder of ours.
        assert!(
            slash_usage(&SlashUsageState::default())
                .contains("  \\c[onnect] {[DBNAME|- USER|- HOST|- PORT|-] | conninfo}\n")
        );
    }

    #[test]
    fn help_variables_fills_its_defaults() {
        let text = help_variables();
        assert!(text.contains("between executions (default 2)\n"));
        assert!(text.contains("CSV output format (default \",\")\n"));
        assert!(text.contains("unaligned output (default \"|\")\n"));
    }

    #[test]
    fn no_placeholder_of_ours_is_left_unfilled() {
        let texts = [
            usage(),
            slash_usage(&SlashUsageState::default()),
            help_variables(),
        ];
        for text in &texts {
            for placeholder in [
                "{field_sep}",
                "{bugreport}",
                "{package}",
                "{url}",
                "{html}",
                "{tuples_only}",
                "{expanded}",
                "{timing}",
                "{currdb}",
                "{watch_interval}",
                "{csv_field_sep}",
            ] {
                assert!(!text.contains(placeholder), "{placeholder} left unfilled");
            }
        }
    }
}
