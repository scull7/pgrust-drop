//! Port of `src/bin/psql/t/001_basic.pl` (PostgreSQL 18.6), in upstream order.
//!
//! The server-free assertions (lines 12-14 and the `--help=foo` loop at
//! 51-63) run everywhere. The cluster cases start a PostgreSQL 18 cluster
//! from the reference `initdb` and `pg_ctl` (`regress::Cluster`) and run each
//! stolen assertion through rpsql and, when the lane has one, through C psql
//! too; without the tools they print `SKIP (flagged, not silent)`, and CI's
//! `PGDROP_REQUIRE_REF=1` turns that into a failure. Ported so far: `\timing`
//! (lines 86-108) and `\errverbose with no previous error` (159-164). The
//! `\copyright`, `\help`, `ENCODING`, notification, crash and remaining
//! `\errverbose` cases, and the rest of the file, land with Linear
//! NAT-400 … NAT-405.
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

/// `# test \errverbose`, its first case — 001_basic.pl:153-164.
///
/// The three cases after it (`:170`-`:210`) need `LINE 1:` and its caret,
/// which rlibpq does not draw yet (`reportErrorPosition`; see
/// `docs/divergences.md`), and `FETCH_COUNT` and `\gdesc`; they are NAT-403's
/// remaining work, not narrowed here.
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
