//! Port of `src/bin/psql/t/001_basic.pl` (PostgreSQL 18.6), in upstream order.
//!
//! The server-free assertions (lines 12-14 and the `--help=foo` loop at
//! 51-63) run everywhere. The cluster cases start a PostgreSQL 18 cluster
//! from the reference `initdb` and `pg_ctl` (`regress::Cluster`) and run each
//! stolen assertion through rpsql and, when the lane has one, through C psql
//! too; without the tools they print `SKIP (flagged, not silent)`, and CI's
//! `PGDROP_REQUIRE_REF=1` turns that into a failure. Ported so far:
//! `\copyright` and `\help` (lines 75-77), the unsupported replication
//! command response (79-84), `\timing` (lines 86-108), the server crash
//! (136-150), `\errverbose with no previous error` (159-164),
//! `\errverbose after normal query with error` (170-181), the multiple
//! `-c`/`-f` switches (212-343), `\copy from with DEFAULT` (345-367),
//! `\g` output piped into a program (457-486) and COPY within pipelines
//! (488-533). The `ENCODING`, notification and remaining `\errverbose`
//! cases, and the rest of the file, land with Linear NAT-400 … NAT-405.
//!
//! The byte-diff gate NAT-398's Acceptance names —
//! `psql -X -c 'select 1'` through C psql and through rpsql — needs both the
//! reference binary and a server, so it is declared here and prints
//! `SKIP (flagged, not silent)` when either is missing. It is never narrowed
//! to something that can pass without them.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use testkit::env::Environment;
use testkit::normalize::EXTRA_VERSION;
use testkit::pattern::Pattern;
use testkit::{Gate, reference};

use regress::{Cluster, PsqlOutcome};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// `program_help_ok('psql');` — 001_basic.pl:12.
#[test]
fn program_help_ok() {
    testkit::program_help_ok(Path::new(RPSQL));
}

/// `program_version_ok('psql');` — 001_basic.pl:13.
#[test]
fn program_version_ok() {
    testkit::program_version_ok(Path::new(RPSQL));
}

/// `program_options_handling_ok('psql');` — 001_basic.pl:14.
///
/// Only a nonzero exit and a non-empty stderr are required, which is what lets
/// usage-rs's clap-shaped parse errors stand in for glibc getopt's (ADR-0004).
#[test]
fn program_options_handling_ok() {
    testkit::program_options_handling_ok(Path::new(RPSQL));
}

/// `# test --help=foo, analogous to program_help_ok()` — 001_basic.pl:51-:63:
/// for `commands` and `variables`, exit 0, stdout non-empty, stderr empty.
#[test]
fn psql_help_arg() {
    for arg in ["commands", "variables"] {
        let outcome =
            testkit::run(Path::new(RPSQL), [format!("--help={arg}")]).expect("spawn rpsql");
        assert_eq!(outcome.status, Some(0), "psql --help={arg} exit code 0");
        assert!(
            !outcome.stdout.is_empty(),
            "psql --help={arg} goes to stdout"
        );
        assert!(
            outcome.stderr.is_empty(),
            "psql --help={arg} nothing to stderr"
        );
    }
}

/// Ports of the cluster cases; each starts its own cluster, so each has its own.
const TIMING_WITH_SUCCESSFUL_QUERY_PORT: u16 = 55_401;
const TIMING_WITH_QUERY_ERROR_PORT: u16 = 55_402;
const ERRVERBOSE_WITH_NO_PREVIOUS_ERROR_PORT: u16 = 55_403;
const ERRVERBOSE_AFTER_NORMAL_QUERY_WITH_ERROR_PORT: u16 = 55_404;
const MULTIPLE_C_AND_F_SWITCHES_PORT: u16 = 55_405;
const COPY_FROM_WITH_DEFAULT_PORT: u16 = 55_406;
const COPY_ROUND_TRIP_PORT: u16 = 55_407;
const G_PIPE_PORT: u16 = 55_408;
const OUTPUT_REDIRECTION_PORT: u16 = 55_409;
const COPYRIGHT_AND_HELP_PORT: u16 = 55_416;
const HELP_SQL_GATE_PORT: u16 = 55_417;
const UNEXPECTED_PQRESULTSTATUS_PORT: u16 = 55_418;
const SERVER_CRASH_PORT: u16 = 55_419;
const COPY_IN_PIPELINE_PORT: u16 = 55_421;

/// The psql binaries a cluster case runs against: rpsql, and C psql when the
/// lane's reference installation has one (the skip is flagged otherwise).
fn every_psql(cluster: &Cluster) -> Vec<PathBuf> {
    let mut psqls = vec![PathBuf::from(RPSQL)];
    match cluster.reference_psql() {
        Some(psql) => psqls.push(psql),
        None => reference::skip("psql"),
    }
    psqls
}

/// `like($got, qr/…/, $name)` with the pattern's flags written inline.
fn assert_like(got: &str, pattern: &str, name: &str) {
    let re = Pattern::new(pattern).expect("a supported pattern");
    assert!(re.is_match(got), "{name}: {got:?} does not match {pattern}");
}

/// `unlike($got, qr/…/, $name)`.
fn assert_unlike(got: &str, pattern: &str, name: &str) {
    let re = Pattern::new(pattern).expect("a supported pattern");
    assert!(!re.is_match(got), "{name}: {got:?} matches {pattern}");
}

/// `psql_like()` — 001_basic.pl:17: exit 0, nothing on stderr, stdout like
/// the pattern, through every psql in turn.
fn psql_like(cluster: &Cluster, sql: &str, expected_stdout: &str, test_name: &str) {
    for psql in every_psql(cluster) {
        let PsqlOutcome {
            ret,
            stdout,
            stderr,
        } = cluster.psql(&psql, sql, true);
        let name = format!("{test_name} ({})", psql.display());
        assert_eq!(ret, 0, "{name}: exit code 0; stderr {stderr:?}");
        assert_eq!(stderr, "", "{name}: no stderr");
        assert_like(&stdout, expected_stdout, &format!("{name}: matches"));
    }
}

/// `\copyright`, `\help without arguments` and `\help with argument` —
/// 001_basic.pl:75-77, through every psql in turn.
#[test]
fn copyright_and_help() {
    let Some(cluster) = Cluster::start(COPYRIGHT_AND_HELP_PORT) else {
        return;
    };
    psql_like(&cluster, "\\copyright", "Copyright", "\\copyright");
    psql_like(&cluster, "\\help", "ALTER", "\\help without arguments");
    psql_like(&cluster, "\\help SELECT", "SELECT", "\\help with argument");
}

/// Every path through `helpSQL` (`help.c:593`) and `\copyright`, byte for
/// byte against C psql: stdout, stderr and the exit status, no normalizer.
/// The listing's columns (stdout is a pipe, so C's `TIOCGWINSZ` fails and it
/// assumes 80), an exact name that stops at itself, a prefix, the two-word
/// and one-word fallbacks, `*`, a topic nothing matches, and the trailing
/// semicolons and spaces the scanner strips. Then, under `\\o`, `\\copyright`
/// and `\\h` still write to stdout (`puts`, `help.c:757`; `PageOutput`,
/// `:704`), while a query's result goes to the file. Not an upstream test:
/// upstream has no psql to compare against.
const HELP_SQL_GATE_SCRIPT: &str = "\\copyright
\\help
\\h SELECT
\\h select ;; \t
\\h DROP TABL
\\h drop table foo
\\h abort now please
\\h CREATE
\\h nosuch thing
\\h *
\\copyright extra
\\o help_o.out
\\copyright
\\h ABORT
SELECT 'to the file' AS o;
\\o
";

/// What [`HELP_SQL_GATE_SCRIPT`]'s `\\o` leaves in its file: the query's
/// result, and none of `\\copyright` and `\\h`.
const HELP_SQL_GATE_O_FILE: &str = "help_o.out";

/// Runs [`HELP_SQL_GATE_SCRIPT`] through rpsql and C psql.
#[test]
fn help_sql_matches_c_psql() {
    let Some(cluster) = Cluster::start(HELP_SQL_GATE_PORT) else {
        return;
    };
    let Some(reference) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    let run = |psql: &Path, name: &str| {
        let dir = cluster.tempdir(name);
        let mut command = cluster.command(psql);
        command.args(["-X", "-f", "-"]).current_dir(&dir);
        let outcome = regress::run(command, HELP_SQL_GATE_SCRIPT.as_bytes());
        let file = std::fs::read(dir.join(HELP_SQL_GATE_O_FILE))
            .unwrap_or_else(|e| panic!("{name}: {HELP_SQL_GATE_O_FILE}: {e}"));
        (outcome, file)
    };
    let (ours, our_file) = run(Path::new(RPSQL), "help_rpsql");
    let (theirs, their_file) = run(&reference, "help_c");
    assert_eq!(
        String::from_utf8_lossy(&our_file),
        String::from_utf8_lossy(&their_file),
        "{HELP_SQL_GATE_O_FILE}"
    );
    assert_eq!(
        String::from_utf8_lossy(&our_file),
        "      o      \n-------------\n to the file\n(1 row)\n\n",
        "only the query's result goes to \\o's file"
    );
    assert_eq!(ours.ret, theirs.ret, "exit status");
    assert_eq!(ours.stderr, theirs.stderr, "stderr");
    if let Some(at) = regress::first_difference(theirs.stdout.as_bytes(), ours.stdout.as_bytes()) {
        panic!("stdout differs from C psql's: {at}");
    }
    assert_eq!(ours.ret, 0, "stderr {:?}", ours.stderr);
    assert_eq!(
        ours.stderr, "psql:<stdin>:11: warning: \\copyright: extra argument \"extra\" ignored",
        "the one warning"
    );
}

/// `psql_fails_like()` — 001_basic.pl:33: a nonzero exit and stderr like the
/// pattern, through one psql, in the context of a WAL sender when
/// `replication` is given.
fn psql_fails_like_with(
    cluster: &Cluster,
    psql: &Path,
    sql: &str,
    expected_stderr: &str,
    test_name: &str,
    replication: Option<&str>,
) {
    let PsqlOutcome { ret, stderr, .. } = match replication {
        Some(replication) => cluster.psql_replication(psql, sql, replication),
        None => cluster.psql(psql, sql, true),
    };
    let name = format!("{test_name} ({})", psql.display());
    assert_ne!(ret, 0, "{name}: exit code not 0");
    assert_like(&stderr, expected_stderr, &format!("{name}: matches"));
}

/// `# Test clean handling of unsupported replication command responses` —
/// 001_basic.pl:79-84, through every psql in turn: `START_REPLICATION`
/// over a `replication=database` connection answers with CopyBothResponse,
/// which `AcceptResult` (`common.c:447`) reports by its number.
#[test]
fn handling_of_unexpected_pqresultstatus() {
    let Some(cluster) = Cluster::start(UNEXPECTED_PQRESULTSTATUS_PORT) else {
        return;
    };
    for psql in every_psql(&cluster) {
        psql_fails_like_with(
            &cluster,
            &psql,
            "START_REPLICATION 0/0",
            "unexpected PQresultStatus: 8$",
            "handling of unexpected PQresultStatus",
            Some("database"),
        );
        // Not upstream, which matches only the end of stderr: the whole of
        // it, and `ON_ERROR_STOP`'s `EXIT_USER`.
        let outcome = cluster.psql_replication(&psql, "START_REPLICATION 0/0", "database");
        assert_eq!(
            (outcome.ret, outcome.stderr.as_str()),
            (3, "psql:<stdin>:1: error: unexpected PQresultStatus: 8"),
            "the whole of stderr ({})",
            psql.display()
        );
    }
}

/// `# test \timing` — 001_basic.pl:86-93.
#[test]
fn timing_with_successful_query() {
    let Some(cluster) = Cluster::start(TIMING_WITH_SUCCESSFUL_QUERY_PORT) else {
        return;
    };
    psql_like(
        &cluster,
        "\\timing on\nSELECT 1",
        "(?m)^1$\n^Time: \\d+[.,]\\d\\d\\d ms",
        "\\timing with successful query",
    );
}

/// `# test \timing with query that fails` — 001_basic.pl:95-108.
#[test]
fn timing_with_query_error() {
    let Some(cluster) = Cluster::start(TIMING_WITH_QUERY_ERROR_PORT) else {
        return;
    };
    for psql in every_psql(&cluster) {
        let PsqlOutcome { ret, stdout, .. } =
            cluster.psql(&psql, "\\timing on\nSELECT error", true);
        let name = |what: &str| format!("\\timing with query error: {what} ({})", psql.display());
        assert_ne!(ret, 0, "{}", name("query failed"));
        assert_like(
            &stdout,
            "(?m)^Time: \\d+[.,]\\d\\d\\d ms",
            &name("timing output appears"),
        );
        assert_unlike(
            &stdout,
            "(?m)^Time: 0[.,]000 ms",
            &name("timing was updated"),
        );
    }
}

/// `# test behavior and output on server crash` — 001_basic.pl:136-150,
/// through every psql in turn: the backend terminating itself mid-script is
/// reported as upstream's three lines, and psql exits 2 (`EXIT_BADCONN`)
/// without running what follows.
#[test]
fn server_crash() {
    let Some(cluster) = Cluster::start(SERVER_CRASH_PORT) else {
        return;
    };
    for psql in every_psql(&cluster) {
        let PsqlOutcome {
            ret,
            stdout,
            stderr,
        } = cluster.psql(
            &psql,
            "SELECT 'before' AS running;\n\
             SELECT pg_terminate_backend(pg_backend_pid());\n\
             SELECT 'AFTER' AS not_running;\n",
            true,
        );
        let name = |what: &str| format!("server crash: {what} ({})", psql.display());
        assert_eq!(ret, 2, "{}; stderr {stderr:?}", name("psql exit code"));
        assert_like(&stdout, "before", &name("output before crash"));
        assert_unlike(&stdout, "AFTER", &name("no output after crash"));
        assert_eq!(
            stderr,
            "psql:<stdin>:2: FATAL:  terminating connection due to administrator command
psql:<stdin>:2: server closed the connection unexpectedly
\tThis probably means the server terminated abnormally
\tbefore or while processing the request.
psql:<stdin>:2: error: connection to server was lost",
            "{}",
            name("error message")
        );
    }
}

/// `# test \errverbose`, its first case — 001_basic.pl:153-164.
///
/// Of the three cases after it, the first is
/// [`errverbose_after_normal_query_with_error`]; the other two
/// (`:183`-`:210`) need `FETCH_COUNT` and `\gdesc`, and land with them.
#[test]
fn errverbose_with_no_previous_error() {
    let Some(cluster) = Cluster::start(ERRVERBOSE_WITH_NO_PREVIOUS_ERROR_PORT) else {
        return;
    };
    psql_like(
        &cluster,
        "SELECT 1;\n\\errverbose",
        "^1\nThere is no previous error\\.$",
        "\\errverbose with no previous error",
    );
}

/// `\errverbose after normal query with error` — 001_basic.pl:166-181: the
/// error with its `LINE 1:` cursor, then `\errverbose` repeating it at
/// `VERBOSITY verbose`, cursor and all, through every psql in turn.
#[test]
fn errverbose_after_normal_query_with_error() {
    let Some(cluster) = Cluster::start(ERRVERBOSE_AFTER_NORMAL_QUERY_WITH_ERROR_PORT) else {
        return;
    };
    for psql in every_psql(&cluster) {
        let PsqlOutcome { stderr, .. } = cluster.psql(&psql, "SELECT error;\n\\errverbose", false);
        assert_like(
            &stderr,
            "(?m)\\A^psql:<stdin>:1: ERROR:  .*$\n\
             ^LINE 1: SELECT error;$\n\
             ^ *^.*$\n\
             ^psql:<stdin>:2: error: ERROR:  [0-9A-Z]{5}: .*$\n\
             ^LINE 1: SELECT error;$\n\
             ^ *^.*$\n\
             ^LOCATION: .*$",
            &format!(
                "\\errverbose after normal query with error ({})",
                psql.display()
            ),
        );
    }
}

/// `$node->command_ok([ 'psql', … ], $name)` or `command_fails`
/// (`Cluster.pm:2743`, `:2763`): run `psql` with `args` against the node and
/// require the exit status to be zero, or not.
fn command_ok_or_fails(cluster: &Cluster, psql: &Path, args: &[&str], ok: bool, name: &str) {
    let mut command = cluster.command(psql);
    command.args(args);
    let outcome = regress::run(command, b"");
    let name = format!("{name} ({})", psql.display());
    if ok {
        assert_eq!(outcome.ret, 0, "{name}: stderr {:?}", outcome.stderr);
    } else {
        assert_ne!(outcome.ret, 0, "{name}");
    }
}

/// `# Check behavior when using multiple -c and -f switches.` —
/// 001_basic.pl:212-343, in order, through each psql in turn.
///
/// The row counts accumulate across the cases, so each psql starts from a
/// fresh table: upstream's `CREATE TABLE` (`:217`) is preceded by a `DROP`
/// here, which is the only change.
#[test]
// Upstream's eleven cases in upstream's order, one row count carried from
// each to the next; split up, they would each need the state the one before
// left.
#[allow(clippy::too_many_lines)]
fn check_behavior_when_using_multiple_c_and_f_switches() {
    let Some(cluster) = Cluster::start(MULTIPLE_C_AND_F_SWITCHES_PORT) else {
        return;
    };
    for psql in every_psql(&cluster) {
        let tempdir = cluster.tempdir(&format!(
            "multiple-{}",
            psql.file_name().unwrap().to_string_lossy()
        ));
        let tempdir = tempdir.to_str().expect("a UTF-8 tempdir");
        cluster.safe_psql(
            &psql,
            "DROP TABLE IF EXISTS tab_psql_single; CREATE TABLE tab_psql_single (a int);",
        );
        let row_count = || cluster.safe_psql(&psql, "SELECT count(*) FROM tab_psql_single");
        let nonexistent = format!("\\copy tab_psql_single FROM '{tempdir}/nonexistent'");
        let is = |got: String, expected: &str, name: &str| {
            assert_eq!(got, expected, "{name} ({})", psql.display());
        };

        // Tests with ON_ERROR_STOP (`:219`).
        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--set",
                "ON_ERROR_STOP=1",
                "--command",
                "INSERT INTO tab_psql_single VALUES (1)",
                "--command",
                "INSERT INTO tab_psql_single VALUES (2)",
            ],
            true,
            "ON_ERROR_STOP, --single-transaction and multiple -c switches",
        );
        is(
            row_count(),
            "2",
            "--single-transaction commits transaction, ON_ERROR_STOP and multiple -c switches",
        );

        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--set",
                "ON_ERROR_STOP=1",
                "--command",
                "INSERT INTO tab_psql_single VALUES (3)",
                "--command",
                &nonexistent,
            ],
            false,
            "ON_ERROR_STOP, --single-transaction and multiple -c switches, error",
        );
        is(
            row_count(),
            "2",
            "client-side error rolls back transaction, ON_ERROR_STOP and multiple -c switches",
        );

        // Tests mixing files and commands (`:252`).
        let copy_sql_file = format!("{tempdir}/tab_copy.sql");
        let insert_sql_file = format!("{tempdir}/tab_insert.sql");
        std::fs::write(&copy_sql_file, format!("{nonexistent};")).unwrap();
        std::fs::write(&insert_sql_file, "INSERT INTO tab_psql_single VALUES (4);").unwrap();
        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--set",
                "ON_ERROR_STOP=1",
                "--file",
                &insert_sql_file,
                "--file",
                &insert_sql_file,
            ],
            true,
            "ON_ERROR_STOP, --single-transaction and multiple -f switches",
        );
        is(
            row_count(),
            "4",
            "--single-transaction commits transaction, ON_ERROR_STOP and multiple -f switches",
        );

        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--set",
                "ON_ERROR_STOP=1",
                "--file",
                &insert_sql_file,
                "--file",
                &copy_sql_file,
            ],
            false,
            "ON_ERROR_STOP, --single-transaction and multiple -f switches, error",
        );
        is(
            row_count(),
            "4",
            "client-side error rolls back transaction, ON_ERROR_STOP and multiple -f switches",
        );

        // Tests without ON_ERROR_STOP (`:290`). The last switch fails on
        // \copy. The command returns a failure and the transaction commits.
        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--file",
                &insert_sql_file,
                "--file",
                &insert_sql_file,
                "--command",
                &nonexistent,
            ],
            false,
            "no ON_ERROR_STOP, --single-transaction and multiple -f/-c switches",
        );
        is(
            row_count(),
            "6",
            "client-side error commits transaction, no ON_ERROR_STOP and multiple -f/-c switches",
        );

        // The last switch fails on \copy coming from an input file. The
        // command returns a success and the transaction commits (`:309`).
        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--file",
                &insert_sql_file,
                "--file",
                &insert_sql_file,
                "--file",
                &copy_sql_file,
            ],
            true,
            "no ON_ERROR_STOP, --single-transaction and multiple -f switches",
        );
        is(
            row_count(),
            "8",
            "client-side error commits transaction, no ON_ERROR_STOP and multiple -f switches",
        );

        // The last switch makes the command return a success, and the
        // contents of the transaction commit even if there is a failure
        // in-between (`:327`).
        command_ok_or_fails(
            &cluster,
            &psql,
            &[
                "--no-psqlrc",
                "--single-transaction",
                "--command",
                "INSERT INTO tab_psql_single VALUES (5)",
                "--file",
                &copy_sql_file,
                "--command",
                "INSERT INTO tab_psql_single VALUES (6)",
            ],
            true,
            "no ON_ERROR_STOP, --single-transaction and multiple -c switches",
        );
        is(
            row_count(),
            "10",
            "client-side error commits transaction, no ON_ERROR_STOP and multiple -c switches",
        );
    }
}

/// `# Test \copy from with DEFAULT option` — 001_basic.pl:345-367.
#[test]
fn copy_from_with_default() {
    let Some(cluster) = Cluster::start(COPY_FROM_WITH_DEFAULT_PORT) else {
        return;
    };
    let psql = PathBuf::from(RPSQL);
    cluster.safe_psql(
        &psql,
        "CREATE TABLE copy_default (
		id integer PRIMARY KEY,
		text_value text NOT NULL DEFAULT 'test',
		ts_value timestamp without time zone NOT NULL DEFAULT '2022-07-05'
	)",
    );
    let tempdir = cluster.tempdir("copy-default");
    let copy_default_sql_file = tempdir.join("copy_default.csv");
    std::fs::write(
        &copy_default_sql_file,
        "1,value,2022-07-04\n2,placeholder,2022-07-03\n3,placeholder,placeholder\n",
    )
    .unwrap();
    // Each psql loads the same three rows, so the table is emptied before
    // each; upstream runs the one psql once.
    for psql in every_psql(&cluster) {
        cluster.safe_psql(&psql, "TRUNCATE copy_default");
        let sql = format!(
            "\\copy copy_default from {} with (format 'csv', default 'placeholder');\n\tSELECT * FROM copy_default",
            copy_default_sql_file.display()
        );
        psql_like_with(
            &cluster,
            &psql,
            &sql,
            "1\\|value\\|2022-07-04 00:00:00\n2|test|2022-07-03 00:00:00\n3|test|2022-07-05 00:00:00",
            "\\copy from with DEFAULT",
        );
    }
}

/// [`psql_like`] through one psql.
fn psql_like_with(
    cluster: &Cluster,
    psql: &Path,
    sql: &str,
    expected_stdout: &str,
    test_name: &str,
) {
    let PsqlOutcome {
        ret,
        stdout,
        stderr,
    } = cluster.psql(psql, sql, true);
    let name = format!("{test_name} ({})", psql.display());
    assert_eq!(ret, 0, "{name}: exit code 0; stderr {stderr:?}");
    assert_eq!(stderr, "", "{name}: no stderr");
    assert_like(&stdout, expected_stdout, &format!("{name}: matches"));
}

/// `# Test \g output piped into a program.` — 001_basic.pl:457-486.
///
/// "The program is perl -pe '' to simply copy the input to the output"
/// (:458); upstream names its own perl, `$^X`, and here it is the `perl` on
/// `PATH`, which every lane's image carries. Each psql writes the one file
/// in turn, and each file is read back before the next psql overwrites it.
#[test]
fn g_output_piped_into_a_program() {
    let Some(cluster) = Cluster::start(G_PIPE_PORT) else {
        return;
    };
    let tempdir = cluster.tempdir("g-pipe");
    let g_file = tempdir.join("g_file_1.out");
    let pipe_cmd = format!("perl -pe '' >{}", g_file.display());
    let slurp = || std::fs::read_to_string(&g_file).expect("the pipe wrote its file");

    for psql in every_psql(&cluster) {
        psql_like_with(
            &cluster,
            &psql,
            &format!("SELECT 'one' \\g | {pipe_cmd}"),
            "",
            "one command \\g",
        );
        assert_like(&slurp(), "one", "one command \\g: the file");

        psql_like_with(
            &cluster,
            &psql,
            &format!("SELECT 'two' \\; SELECT 'three' \\g | {pipe_cmd}"),
            "",
            "two commands \\g",
        );
        assert_like(&slurp(), "(?s)two.*three", "two commands \\g: the file");

        psql_like_with(
            &cluster,
            &psql,
            &format!("\\set SHOW_ALL_RESULTS 0\nSELECT 'four' \\; SELECT 'five' \\g | {pipe_cmd}"),
            "",
            "two commands \\g with only last result",
        );
        let c3 = slurp();
        assert_like(&c3, "five", "two commands \\g with only last result: five");
        assert_unlike(&c3, "four", "two commands \\g with only last result: four");

        psql_like_with(
            &cluster,
            &psql,
            &format!("copy (values ('foo'),('bar')) to stdout \\g | {pipe_cmd}"),
            "",
            "copy output passed to \\g pipe",
        );
        assert_like(
            &slurp(),
            "(?s)foo.*bar",
            "copy output passed to \\g pipe: the file",
        );
    }
}

/// `# Test COPY within pipelines.` — 001_basic.pl:488-533, in order,
/// through every psql in turn. "These abort the connection from the
/// frontend so they cannot be tested via SQL" (:489); the first case also
/// waits for the server to log that it lost the protocol's synchronisation.
#[test]
fn copy_in_pipelines() {
    let Some(cluster) = Cluster::start(COPY_IN_PIPELINE_PORT) else {
        return;
    };
    let psqls = every_psql(&cluster);
    cluster.safe_psql(Path::new(RPSQL), "CREATE TABLE psql_pipeline()");
    let expected = "COPY in a pipeline is not supported, aborting connection";
    for psql in &psqls {
        let log_location = cluster.log_len();
        psql_fails_like_with(
            &cluster,
            psql,
            "\\startpipeline
COPY psql_pipeline FROM STDIN;
SELECT 'val1';
\\syncpipeline
\\endpipeline",
            expected,
            "COPY FROM in pipeline: fails",
            None,
        );
        cluster.wait_for_log(
            "FATAL: .*terminating connection because protocol synchronization was lost",
            log_location,
        );

        // "Remove \syncpipeline here." (:505)
        psql_fails_like_with(
            &cluster,
            psql,
            "\\startpipeline
COPY psql_pipeline TO STDOUT;
SELECT 'val1';
\\endpipeline",
            expected,
            "COPY TO in pipeline: fails",
            None,
        );

        psql_fails_like_with(
            &cluster,
            psql,
            "\\startpipeline
\\copy psql_pipeline from stdin;
SELECT 'val1';
\\syncpipeline
\\endpipeline",
            expected,
            "\\copy from in pipeline: fails",
            None,
        );

        // "Sync attempt after a COPY TO/FROM." (:525)
        psql_fails_like_with(
            &cluster,
            psql,
            "\\startpipeline
\\copy psql_pipeline to stdout;
\\syncpipeline
\\endpipeline",
            expected,
            "\\copy to in pipeline: fails",
            None,
        );

        // Not upstream, which asks only for a nonzero exit: psql gives up
        // with `exit(EXIT_BADCONN)` (`common.c:1943`) whatever
        // `ON_ERROR_STOP` says, so each case above exits 2.
        let outcome = cluster.psql(
            psql,
            "\\startpipeline\nCOPY psql_pipeline TO STDOUT;\n\\endpipeline",
            true,
        );
        assert_eq!(
            (outcome.ret, outcome.stderr.as_str()),
            (
                2,
                "psql:<stdin>:3: COPY in a pipeline is not supported, aborting connection"
            ),
            "EXIT_BADCONN ({})",
            psql.display()
        );
    }
}

/// The script [`copy_round_trip_matches_c_psql`] runs, from the directory
/// its files are written to. Rows carry every byte COPY's text and CSV
/// formats escape — tab, newline, carriage return, backslash, a lone `\.`,
/// quotes, the delimiter — plus NULL against the empty string, non-ASCII
/// text, and a bytea with a zero byte.
const COPY_ROUND_TRIP_SQL: &str = r#"SET timezone = 'UTC'; SET client_min_messages = warning;
DROP TABLE IF EXISTS rt_src, rt_dst;
CREATE TABLE rt_src (i int, t text, b bytea, n numeric, ts timestamptz, j jsonb, a int[]);
INSERT INTO rt_src VALUES
  (1, E'tab\there\nnewline\rcr\\backslash', '\x00ff10', 1.50, '2026-09-27 12:34:56.789+00', '{"k": [1, "v"]}', '{1,2,NULL}'),
  (2, NULL, NULL, NULL, NULL, NULL, NULL),
  (3, '', '\x', 0, 'infinity', '[]', '{}'),
  (4, E'\\.', 'x', -1e-3, '1999-12-31 23:59:59+00', 'null', '{-1}'),
  (5, 'é "quoted" ''single'' , comma', 'é', 123456789.123456789, '2000-01-01 00:00:00+00', '"s"', '{7}');
COPY rt_src TO STDOUT;
COPY rt_src TO STDOUT WITH (FORMAT csv, HEADER);
\copy rt_src to 'text.out'
\copy rt_src to 'csv.out' with (format csv, header, force_quote *)
\copy rt_src to 'binary.out' with (format binary)
\copy (select i, t from rt_src order by i desc) to 'query.out'
CREATE TABLE rt_dst (LIKE rt_src);
\copy rt_dst from 'text.out'
\copy rt_dst from 'csv.out' with (format csv, header)
\copy rt_dst from 'binary.out' with (format binary)
\copy rt_dst (i, t) from 'query.out'
COPY rt_dst (i, t) FROM STDIN;
6	inline\tdata
7	\N
\.
\copy rt_dst (i, t) from stdin with (format csv)
8,"quoted
line"
\.
\copy (select * from rt_dst order by i, t nulls first, b nulls first) to 'back.out'
SELECT count(*), count(DISTINCT (i, t, b, n, ts, j, a)) FROM rt_dst;
\copy rt_dst to stdout with (format csv)
\copy rt_dst to pstdout
"#;

/// The files [`COPY_ROUND_TRIP_SQL`] writes.
const COPY_ROUND_TRIP_FILES: [&str; 5] =
    ["text.out", "csv.out", "binary.out", "query.out", "back.out"];

/// NAT-403's Acceptance: "`\copy` round-trip identical bytes vs PGDG psql".
///
/// [`COPY_ROUND_TRIP_SQL`] copies a table out to files in the text, CSV and
/// binary formats and from a query, loads every file back, copies inline data
/// in through `COPY … FROM STDIN` and `\copy … from stdin`, and writes the
/// result out once more. It runs through rpsql and through C psql, each in a
/// directory of its own, and every file and both output streams must be the
/// same bytes. Not an upstream test: upstream has no psql to compare against.
#[test]
fn copy_round_trip_matches_c_psql() {
    let Some(cluster) = Cluster::start(COPY_ROUND_TRIP_PORT) else {
        return;
    };
    let Some(reference) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    let run = |psql: &Path, name: &str| {
        let dir = cluster.tempdir(name);
        let mut command = cluster.command(psql);
        command.args(["-X", "-f", "-"]).current_dir(&dir);
        let outcome = regress::run(command, COPY_ROUND_TRIP_SQL.as_bytes());
        let files: Vec<Vec<u8>> = COPY_ROUND_TRIP_FILES
            .iter()
            .map(|f| std::fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{name}: {f}: {e}")))
            .collect();
        (outcome, files)
    };
    let (ours, our_files) = run(Path::new(RPSQL), "round-trip-rpsql");
    let (theirs, their_files) = run(&reference, "round-trip-psql");

    assert_eq!(
        ours.ret, theirs.ret,
        "exit status; rpsql stderr {:?}",
        ours.stderr
    );
    assert_eq!(ours.stderr, theirs.stderr, "stderr");
    assert_eq!(ours.stdout, theirs.stdout, "stdout");
    for ((name, ours), theirs) in COPY_ROUND_TRIP_FILES
        .iter()
        .zip(&our_files)
        .zip(&their_files)
    {
        assert!(ours == theirs, "{name} differs from C psql's");
    }
    // The script ran to the end without an error on either side.
    assert_eq!(ours.ret, 0, "stderr {:?}", ours.stderr);
    assert_eq!(ours.stderr, "", "no stderr");
}

/// The script [`output_redirection_matches_c_psql`] runs, from the directory
/// its files are written to: `\o` and `\g` to files and pipes, `\copy` to and
/// from programs, the `SHELL_ERROR` / `SHELL_EXIT_CODE` each pipe leaves —
/// a clean exit, an exit code, a signal and a missing command — and where
/// COPY data and status lines go while `\o` is in force.
///
/// Every program that psql writes to reads all of its input before it
/// exits, and none writes to psql's stdout, so no line depends on how the
/// shell and psql interleave. (A `\g` pipe whose command exits unread makes
/// C psql's `could not print result table: Broken pipe` a race between the
/// shell's exit and psql's `fflush`, so the signal case is a `\copy … from
/// program` instead, which psql only reads.)
const OUTPUT_REDIRECTION_SQL: &str = r"\o | cat > o_pipe.out
select 1 as one;
\qecho qecho line
\echo echo line
\o
\echo :SHELL_ERROR :SHELL_EXIT_CODE
\o |cat >/dev/null; exit 3
\o
\echo :SHELL_ERROR :SHELL_EXIT_CODE
select 2 as two \g | cat > g_pipe.out
\echo :SHELL_ERROR :SHELL_EXIT_CODE
select 3 \g |cat >/dev/null; exit 4
\echo :SHELL_ERROR :SHELL_EXIT_CODE
select 'again' as a \g g_file.out
\g g_file_2.out
select 1 \g /nonexistent/dir/g.out
\copy (select 'prog') to program 'cat > prog.out'
\echo :SHELL_ERROR :SHELL_EXIT_CODE
\copy (select 1) to program 'cat >/dev/null; exit 7'
\echo :SHELL_ERROR :SHELL_EXIT_CODE
create temp table p (x text);
\copy p from program 'printf ''a\nb\n'''
\echo :SHELL_ERROR :SHELL_EXIT_CODE
\copy p from program 'no_such_command_rpsql 2>/dev/null'
\echo :SHELL_ERROR :SHELL_EXIT_CODE
\copy p from program 'kill -TERM $$'
\echo :SHELL_ERROR :SHELL_EXIT_CODE
select * from p;
\o o_file.out;
select 'to the o file';
copy (values ('x'),('y')) to stdout;
\copy (values ('z')) to stdout
\copy (values ('w')) to pstdout
select 'g beats o' \g g_over_o.out
\o
\o /nonexistent/dir/o.out
select 'still stdout';
";

/// The files [`OUTPUT_REDIRECTION_SQL`] writes.
const OUTPUT_REDIRECTION_FILES: [&str; 7] = [
    "o_pipe.out",
    "g_pipe.out",
    "g_file.out",
    "g_file_2.out",
    "prog.out",
    "o_file.out",
    "g_over_o.out",
];

/// `\o`, `\g`, `-o` and `\copy … program` against PGDG psql: stdout, stderr,
/// the exit status and every file written must be the same bytes. Not an
/// upstream test — upstream has no psql to compare against — but the
/// behaviours are `001_basic.pl:457`'s and `psql.sql:1507`'s, extended to
/// the pipe statuses neither of them checks.
#[test]
fn output_redirection_matches_c_psql() {
    let Some(cluster) = Cluster::start(OUTPUT_REDIRECTION_PORT) else {
        return;
    };
    let Some(reference) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    let run = |psql: &Path, name: &str| {
        let dir = cluster.tempdir(name);
        let mut command = cluster.command(psql);
        command.args(["-X", "-f", "-"]).current_dir(&dir);
        let script = regress::run(command, OUTPUT_REDIRECTION_SQL.as_bytes());
        // `-o` (`startup.c:594`), and `-o` that cannot be opened.
        let mut command = cluster.command(psql);
        command
            .args(["-X", "-o", "o_option.out", "-c", "select 'dash o'", "-c"])
            .arg("\\echo to stdout")
            .current_dir(&dir);
        let option = regress::run(command, b"");
        let mut command = cluster.command(psql);
        command
            .args(["-X", "-o", "/nonexistent/dir/o", "-c", "select 1"])
            .current_dir(&dir);
        let bad_option = regress::run(command, b"");
        let files: Vec<Vec<u8>> = OUTPUT_REDIRECTION_FILES
            .iter()
            .chain(&["o_option.out"])
            .map(|f| std::fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{name}: {f}: {e}")))
            .collect();
        ([script, option, bad_option], files)
    };
    let (ours, our_files) = run(Path::new(RPSQL), "redirection-rpsql");
    let (theirs, their_files) = run(&reference, "redirection-psql");

    for (what, (ours, theirs)) in ["the script", "-o", "a bad -o"]
        .iter()
        .zip(ours.iter().zip(&theirs))
    {
        assert_eq!(
            ours.ret, theirs.ret,
            "{what}: exit status; rpsql stderr {:?}",
            ours.stderr
        );
        assert_eq!(ours.stderr, theirs.stderr, "{what}: stderr");
        assert_eq!(ours.stdout, theirs.stdout, "{what}: stdout");
    }
    for ((name, ours), theirs) in OUTPUT_REDIRECTION_FILES
        .iter()
        .chain(&["o_option.out"])
        .zip(&our_files)
        .zip(&their_files)
    {
        assert!(
            ours == theirs,
            "{name} differs from C psql's: {:?} vs {:?}",
            String::from_utf8_lossy(ours),
            String::from_utf8_lossy(theirs)
        );
    }
}

/// The Acceptance gate for `--help`, `--help=commands` and `--help=variables`:
/// each through C psql and through rpsql, stdout, stderr and exit status
/// diffed as raw bytes with no normalizer. `--version`, the fourth invocation,
/// is [`version_matches_c_psql`].
///
/// The environment is `Utils.pm`'s scrub, which pins `LC_MESSAGES=C` so a
/// PGDG psql built with NLS prints the untranslated text, with `TERM` and
/// `COLUMNS` removed as well. The command-line paths pass `NOPAGER`
/// (`startup.c:89`, `:704`-`:715`), so neither a pager nor the window width
/// should reach the text; removing them makes that a property of the gate
/// rather than of the machine it runs on.
#[test]
fn help_matches_c_psql() {
    let Some(gate) = Gate::for_tool_or_skip("psql", RPSQL) else {
        return;
    };
    let env = Environment::postgres_test("001_basic.pl")
        .without("TERM")
        .without("COLUMNS");
    for arg in ["--help", "--help=commands", "--help=variables"] {
        gate.clone().with_env(env.clone()).arg(arg).assert_clean();
    }
}

/// The Acceptance gate: `psql -X -c 'select 1'` byte for byte against C psql,
/// on stdout, stderr and the exit status.
///
/// Both sides need a server to connect to. Without one, C psql fails to
/// connect and so does rpsql, and a gate over two connection failures would
/// prove nothing about `select 1` — so the gate runs only when a reference
/// psql *and* a cluster to point it at are both present, and flags the skip
/// otherwise.
#[test]
fn select_one_matches_c_psql() {
    let Some(gate) = Gate::for_tool_or_skip("psql", RPSQL) else {
        return;
    };
    if std::env::var_os("PGDROP_TEST_CLUSTER").is_none() {
        reference::announce_skip(&format!(
            "{}: no PostgreSQL 18 cluster to run `psql -X -c 'select 1'` against; \
             set PGDROP_TEST_CLUSTER to a connectable cluster's PGHOST",
            reference::SKIP_FLAG
        ));
        return;
    }
    let argv: Vec<OsString> = ["-X", "-c", "select 1"]
        .iter()
        .map(OsString::from)
        .collect();
    gate.with_args(argv).assert_clean();
}

/// The same gate over a small corpus of SQL shapes: DDL, DML, a SELECT and an
/// error with its caret.
///
/// The corpus is written here (AGENTS.md forbids copying pgrust's), one
/// statement per `-c`, and is gated whole so a difference in any one shape
/// fails the test.
#[test]
fn a_small_sql_corpus_matches_c_psql() {
    let Some(gate) = Gate::for_tool_or_skip("psql", RPSQL) else {
        return;
    };
    if std::env::var_os("PGDROP_TEST_CLUSTER").is_none() {
        reference::announce_skip(&format!(
            "{}: no PostgreSQL 18 cluster to run the SQL corpus against; \
             set PGDROP_TEST_CLUSTER to a connectable cluster's PGHOST",
            reference::SKIP_FLAG
        ));
        return;
    }
    let corpus = [
        "create table t (n int, s text)",
        "insert into t values (1, 'a'), (2, 'b')",
        "select n, s from t order by n",
        "update t set s = 'c' where n = 1",
        "delete from t where n = 2",
        "begin",
        "select count(*) from t",
        "commit",
        "selec 1",
        "drop table t",
    ];
    let mut argv: Vec<OsString> = vec![OsString::from("-X")];
    for statement in corpus {
        argv.push(OsString::from("-c"));
        argv.push(OsString::from(statement));
    }
    gate.with_args(argv).assert_clean();
}

/// `psql --version` through both binaries, which needs no server at all.
///
/// Normalized by `normalize::EXTRA_VERSION`, for the same reason the `initdb`
/// half is: C `psql` prints the compile-time `PG_VERSION` verbatim, and a
/// distribution that builds with `configure --with-extra-version` appends its
/// own vendor string to it, so PGDG's binary answers
/// `psql (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)` where a stock build
/// answers `psql (PostgreSQL) 18.6`. The normalizer strips only a trailing
/// parenthetical from that one line shape; the version number itself is still
/// compared, so 18.6 and 19.1 still differ.
#[test]
fn version_matches_c_psql() {
    let Some(gate) = Gate::for_tool_or_skip("psql", RPSQL) else {
        return;
    };
    gate.arg("--version")
        .normalizer(EXTRA_VERSION)
        .assert_clean();
}

/// Without a server, `-c` still reports a connection failure and exits 2
/// (`EXIT_BADCONN`, `settings.h:200`) rather than succeeding or hanging.
#[test]
fn a_connection_failure_exits_badconn() {
    let outcome = testkit::run(
        Path::new(RPSQL),
        [
            OsString::from("-X"),
            OsString::from("-h"),
            // A path that cannot hold a socket, so the failure is immediate
            // and does not depend on the network.
            OsString::from("/nonexistent-pgdrop-socket-dir"),
            OsString::from("-c"),
            OsString::from("select 1"),
        ],
    )
    .expect("spawn rpsql");
    assert_eq!(
        outcome.status,
        Some(i32::from(rpsql::settings::EXIT_BADCONN))
    );
    assert!(!outcome.stderr.is_empty(), "a failure must say why");
    assert!(outcome.stdout.is_empty(), "nothing is printed on stdout");
}
