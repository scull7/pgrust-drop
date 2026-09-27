//! Port of `src/test/regress/sql/psql.sql` (PostgreSQL 18.6), NAT-400's
//! output-format sections, gated section by section against
//! `expected/psql.out`.
//!
//! Two kinds of gate live here:
//!
//! - **Server-free.** The `-- test multi-line headers, …` and
//!   `-- test single-line header and data` sections print one prepared
//!   query's result under every `\pset` combination. The result is the same
//!   every time, so it is built here and each of the section's `execute q;`
//!   blocks is rendered through `rpsql`'s own `do_pset` and printer and
//!   compared with the block `psql.out` expects. These run on every lane.
//! - **Live.** A whole section run through `rpsql -X -a -q` against a
//!   PostgreSQL 18 cluster, compared byte for byte with its `psql.out` slice
//!   and with C psql's output. The cluster needs the reference `initdb` and
//!   `pg_ctl`; without them the gate prints `SKIP (flagged, not silent)`.
//!
//! Sections are cut by `regress::split`, keyed by `psql.sql`'s own comment
//! headers. They are ported in NAT-400's slices; a section with no gate here
//! yet is one a later slice owns.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::path::Path;

use rlibpq::{Backend, FieldDescription, QueryResult, QueryRunner, TransactionStatus};
use rpsql::print::{PrintError, print_query};
use rpsql::pset::do_pset;
use rpsql::settings::{PrintQueryOpt, PsqlSettings};
use testkit::reference;

use regress::{Cluster, PSQL_OUT, PSQL_SQL, Section, first_difference, section, split};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[test]
fn the_vendored_files_are_the_ones_postgresql_18_6_ships() {
    // ADR-0008: vendored bytes come from the tag or the tarball, and a
    // digest pins them so a local edit cannot pass for upstream.
    assert_eq!(
        hex(&rlibpq::sha256::sha256(PSQL_SQL.as_bytes())),
        regress::PSQL_SQL_SHA256,
        "crates/rpsql/tests/regress/psql.sql is not REL_18_6's src/test/regress/sql/psql.sql"
    );
    assert_eq!(
        hex(&rlibpq::sha256::sha256(PSQL_OUT.as_bytes())),
        regress::PSQL_OUT_SHA256,
        "crates/rpsql/tests/regress/expected/psql.out is not REL_18_6's src/test/regress/expected/psql.out"
    );
}

#[test]
fn psql_sql_and_psql_out_split_into_the_same_sections_losslessly() {
    let sections = split(PSQL_SQL, PSQL_OUT).expect("the vendored files split");
    let sql: String = sections.iter().map(|s| s.sql).collect();
    let out: String = sections.iter().map(|s| s.expected).collect();
    assert_eq!(sql, PSQL_SQL, "the sections must tile psql.sql");
    assert_eq!(out, PSQL_OUT, "the slices must tile psql.out");
    for s in &sections {
        assert!(s.sql.starts_with(s.header), "{s:?}");
        assert!(s.expected.starts_with(s.header), "{s:?}");
    }
    // The headers this issue's sections are keyed by, at the lines they
    // occupy in the two files.
    let at = |header: &str| {
        let s = section(header);
        (s.sql_line, s.out_line)
    };
    assert_eq!(at("-- show all pset options"), (219, 443));
    assert_eq!(
        at("-- test multi-line headers, wrapping, and newline indicators"),
        (222, 467)
    );
    assert_eq!(at("-- test single-line header and data"), (343, 1424));
}

#[test]
fn a_header_psql_out_never_echoes_is_an_error_not_a_silent_merge() {
    let err = split("-- a\nselect 1;\n\n-- b\nselect 2;\n", "-- a\nselect 1;\n")
        .expect_err("-- b is missing from the output");
    assert!(err.0.contains("\"-- b\""), "{}", err.0);
    // A comment right after a statement is a note, not a new section.
    let sections = split("-- a\nselect 1;\n-- note\n", "-- a\nselect 1;\n-- note\n").unwrap();
    assert_eq!(sections.len(), 1);
}

fn text_field(name: &str) -> FieldDescription {
    FieldDescription {
        name: name.as_bytes().to_vec(),
        tableid: 0,
        columnid: 0,
        typid: 25,
        typlen: -1,
        atttypmod: -1,
        format: 0,
    }
}

/// A `text`-only result, fed through the protocol state machine as a server
/// would send it.
fn text_result(headers: &[&str], rows: &[Vec<String>]) -> QueryResult {
    let mut runner = QueryRunner::new();
    runner
        .push(Backend::RowDescription(
            headers.iter().map(|h| text_field(h)).collect(),
        ))
        .unwrap();
    for row in rows {
        runner
            .push(Backend::DataRow(
                row.iter().map(|c| Some(c.as_bytes().to_vec())).collect(),
            ))
            .unwrap();
    }
    runner
        .push(Backend::CommandComplete(
            format!("SELECT {}", rows.len()).into_bytes(),
        ))
        .unwrap();
    runner
        .push(Backend::ReadyForQuery(TransactionStatus::Idle))
        .unwrap();
    runner.into_results().remove(0)
}

/// `psql.sql:224`-`:227`'s `q`: two rows, `n = 1` and `n = 2 … 10` grouped,
/// each cell the group's strings joined by newlines. `repeat('y', 0)` is the
/// empty string, so the second row's second cell ends in a newline.
fn multi_line_q() -> QueryResult {
    let xs = |ns: std::ops::RangeInclusive<usize>| {
        ns.map(|n| "x".repeat(2 * n)).collect::<Vec<_>>().join("\n")
    };
    let ys = |ns: std::ops::RangeInclusive<usize>| {
        ns.map(|n| "y".repeat(20 - 2 * n))
            .collect::<Vec<_>>()
            .join("\n")
    };
    text_result(
        &["ab\n\nc", "a\nbc"],
        &[vec![xs(1..=1), ys(1..=1)], vec![xs(2..=10), ys(2..=10)]],
    )
}

/// `psql.sql:344`'s `q`: ten single-line rows.
fn single_line_q() -> QueryResult {
    let rows: Vec<Vec<String>> = (1..=10)
        .map(|n| vec!["x".repeat(2 * n), "y".repeat(20 - 2 * n)])
        .collect();
    text_result(&["0123456789abcdef", "0123456789"], &rows)
}

/// What replaying one section's `execute q;` blocks came to.
#[derive(Debug, Default, PartialEq, Eq)]
struct Replay {
    /// Rendered, and byte-identical to `psql.out`.
    matched: usize,
    /// Refused as [`PrintError::Unsupported`]: a later slice's format.
    deferred: Vec<String>,
}

/// Replay `section`'s expected output: every echoed `\pset` goes through
/// `do_pset`, quietly as `-q` has it, and every `execute q;` block is
/// rendered from `q` and compared with the lines psql.out has after it.
///
/// A block this port refuses is recorded, not failed; one it renders must
/// match. That is the property the whole printer keeps: refuse, never fake.
fn replay(section: &Section<'_>, q: &QueryResult, popt: &mut PrintQueryOpt) -> Replay {
    let lines: Vec<&str> = section.expected.split_inclusive('\n').collect();
    let mut replay = Replay::default();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end_matches('\n');
        i += 1;
        if let Some(rest) = line.strip_prefix("\\pset ") {
            let mut words = rest.split_whitespace();
            let param = words.next().expect("\\pset has a parameter");
            do_pset(param, words.next(), popt, true)
                .unwrap_or_else(|e| panic!("psql.out:{}: {line}: {e}", section.out_line + i - 1));
        } else if line == "execute q;" {
            let first = i;
            while i < lines.len()
                && !lines[i].starts_with('\\')
                && lines[i] != "execute q;\n"
                && lines[i] != "deallocate q;\n"
            {
                i += 1;
            }
            let expected: String = lines[first..i].concat();
            let at = format!(
                "psql.out:{}-{} (format {}, border {}, expanded {:?}, linestyle {})",
                section.out_line + first,
                section.out_line + i - 1,
                popt.topt.format.name(),
                popt.topt.border,
                popt.topt.expanded,
                popt.topt.line_style.name()
            );
            match print_query(q, popt) {
                Ok(ours) => {
                    if let Some(diff) = first_difference(expected.as_bytes(), &ours) {
                        panic!("{at}: {diff}");
                    }
                    replay.matched += 1;
                }
                Err(PrintError::Unsupported(_)) => replay.deferred.push(at),
            }
        }
    }
    replay
}

/// The print options psql starts a script with (`startup.c:165`-`:238`).
fn startup_popt() -> PrintQueryOpt {
    let mut pset = PsqlSettings::default();
    pset.apply_separator_defaults();
    pset.popt
}

/// Ports used by the live gates here; each gate starts its own cluster.
const SHOW_ALL_PSET_OPTIONS_PORT: u16 = 55_490;

/// Run `section` through rpsql against `cluster`, and through C psql when this
/// lane has one, and require both to print exactly `psql.out`'s slice.
fn gate_section(cluster: &Cluster, section: &Section<'_>) {
    let ours = cluster.run_script(Path::new(RPSQL), section.sql);
    if let Some(diff) = first_difference(section.expected.as_bytes(), &ours) {
        panic!(
            "rpsql, psql.sql:{} vs psql.out:{} ({}): {diff}",
            section.sql_line, section.out_line, section.header
        );
    }
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cluster.run_script(&psql, section.sql);
            if let Some(diff) = first_difference(&theirs, &ours) {
                panic!("rpsql vs C psql ({}): {diff}", section.header);
            }
        }
        None => reference::skip("psql"),
    }
}

/// `-- show all pset options` (`psql.sql:219`): a bare `\pset` lists every
/// print option with its startup value.
#[test]
fn show_all_pset_options() {
    let Some(cluster) = Cluster::start(SHOW_ALL_PSET_OPTIONS_PORT) else {
        return;
    };
    gate_section(&cluster, &section("-- show all pset options"));
}

/// `-- test multi-line headers, wrapping, and newline indicators` and
/// `-- test single-line header and data` (`psql.sql:222`, `:343`), replayed
/// in file order so each starts from the state the one before left.
///
/// The first section has 36 `execute q;` blocks and the second 45. In each, the 12 neither expanded
/// nor wrapped — aligned and unaligned, at borders 0, 1 and 2, in ascii and
/// old-ascii — render here. The other 24 and 33 are NAT-400's next slice,
/// and the counts are pinned so that slice has to move them.
#[test]
fn the_aligned_and_unaligned_blocks_match_psql_out() {
    let mut popt = startup_popt();
    let multi = replay(
        &section("-- test multi-line headers, wrapping, and newline indicators"),
        &multi_line_q(),
        &mut popt,
    );
    let single = replay(
        &section("-- test single-line header and data"),
        &single_line_q(),
        &mut popt,
    );
    for (name, r, deferred) in [("multi-line", &multi, 24), ("single-line", &single, 33)] {
        assert_eq!(
            (r.matched, r.deferred.len()),
            (12, deferred),
            "{name}: matched {} and deferred {:#?}",
            r.matched,
            r.deferred
        );
        assert!(
            r.deferred
                .iter()
                .all(|at| at.contains("format wrapped") || !at.contains("expanded Off")),
            "{name}: only wrapped and expanded blocks may be deferred: {:#?}",
            r.deferred
        );
    }
}

/// Ports of the `\if` gates below.
const IF_SECTIONS_PORT: u16 = 55_491;
const BEGIN_END_MATCHING_PORT: u16 = 55_492;

/// The sections from the one opening with `from` up to, not including, the
/// one opening with `to`, joined into one: `psql.sql` separates the topics of
/// its `\if` tests with blank lines, but they share state — `\g` resends the
/// previous section's query, `:foo` is set in one and read in the next — so
/// they only pass as the one script `pg_regress` runs. `skip` names sections
/// left out whole.
fn joined_sections(from: &str, to: &str, skip: &[&str]) -> (String, String, Section<'static>) {
    let all = split(PSQL_SQL, PSQL_OUT).expect("the vendored files split");
    let start = all
        .iter()
        .position(|s| s.header == from)
        .unwrap_or_else(|| panic!("no section {from:?}"));
    let end = all
        .iter()
        .position(|s| s.header == to)
        .unwrap_or_else(|| panic!("no section {to:?}"));
    for header in skip {
        assert!(
            all[start..end].iter().any(|s| s.header == *header),
            "skipped section {header:?} must lie between {from:?} and {to:?}"
        );
    }
    let kept: Vec<&Section<'_>> = all[start..end]
        .iter()
        .filter(|s| !skip.contains(&s.header))
        .collect();
    let sql = kept.iter().map(|s| s.sql).collect();
    let out = kept.iter().map(|s| s.expected).collect();
    (sql, out, all[start].clone())
}

/// `-- tests for \if ... \endif` (`psql.sql:908`-`:1140`): `\if`, `\elif`,
/// `\else` and `\endif`, `:{?name}`, and every backslash command upstream
/// has, skipped inside `\if false` — against `psql.out` and against C psql.
///
/// One of the topics, `-- test that begin/end matching ignores to-be-ignored
/// text`, ends in `\sf`, which this port does not have yet; it is gated by
/// itself in [`if_begin_end_matching_matches_psql_out_but_for_sf`].
#[test]
fn if_sections_match_psql_out() {
    let (sql, expected, first) = joined_sections(
        "-- tests for \\if ... \\endif",
        "-- SHOW_CONTEXT",
        &["-- test that begin/end matching ignores to-be-ignored text"],
    );
    let Some(cluster) = Cluster::start(IF_SECTIONS_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &Section {
            sql: &sql,
            expected: &expected,
            ..first
        },
    );
}

/// `-- test that begin/end matching ignores to-be-ignored text`
/// (`psql.sql:1113`-`:1122`): an `end` inside `\if false` must not close the
/// `begin atomic` around it.
///
/// The section's `\sf silly_function(int)` (`:1121`) is `exec_command_sf_sv`
/// (`command.c:2982`), which no slice has ported yet, so that one line is cut
/// from the script and exactly the lines it prints from `psql.out`; both cuts
/// must match exactly once, so the gate cannot quietly widen.
#[test]
fn if_begin_end_matching_matches_psql_out_but_for_sf() {
    let section = section("-- test that begin/end matching ignores to-be-ignored text");
    let sf = "\\sf silly_function(int)\n";
    let sf_output = "\\sf silly_function(int)\n\
                     CREATE OR REPLACE FUNCTION public.silly_function(integer)\n \
                     RETURNS integer\n \
                     LANGUAGE sql\n\
                     BEGIN ATOMIC\n \
                     SELECT $1;\n\
                     END\n";
    assert_eq!(section.sql.matches(sf).count(), 1, "the \\sf line to cut");
    assert_eq!(
        section.expected.matches(sf_output).count(),
        1,
        "the \\sf output to cut"
    );
    let sql = section.sql.replacen(sf, "", 1);
    let expected = section.expected.replacen(sf_output, "", 1);
    let Some(cluster) = Cluster::start(BEGIN_END_MATCHING_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &Section {
            sql: &sql,
            expected: &expected,
            ..section
        },
    );
}

/// Port of the gate below.
const SHOW_CONTEXT_THROUGH_RESULT_VARIABLES_PORT: u16 = 55_493;

/// The one error cursor in [`show_context_query_buffer_and_result_variables_match_psql_out_but_for_the_cursor`]'s
/// script, as C libpq draws it, and as rlibpq renders the same position
/// until `reportErrorPosition` is ported (`docs/divergences.md`).
const SYNTAX_ERROR_CURSOR: (&str, &str) = (
    "SELECT 1 UNION;\n\
     ERROR:  syntax error at or near \";\"\n\
     LINE 1: SELECT 1 UNION;\n                      ^\n",
    "SELECT 1 UNION;\n\
     ERROR:  syntax error at or near \";\" at character 15\n",
);

/// Replace the cursor C libpq draws with rlibpq's rendering, requiring it
/// exactly once.
fn without_the_cursor(output: &str, whose: &str) -> String {
    let (drawn, rendered) = SYNTAX_ERROR_CURSOR;
    assert_eq!(output.matches(drawn).count(), 1, "the cursor in {whose}");
    output.replacen(drawn, rendered, 1)
}

/// `-- SHOW_CONTEXT`, `-- test printing and clearing the query buffer` and
/// `-- tests for special result variables` up to `-- working \gdesc`
/// (`psql.sql:1142`-`:1223`), as the one script they are in `pg_regress`:
/// notices and errors under each `SHOW_CONTEXT`, `\p` and `\r`, and
/// `ERROR`, `SQLSTATE`, `ROW_COUNT` and `LAST_ERROR_*` after a working
/// query, a syntax error, an empty query and another error, at the default,
/// `terse` and `sqlstate` verbosities — against `psql.out` and C psql.
///
/// The syntax error at the default verbosity (`psql.sql:1186`) is where
/// libpq draws its error cursor, `reportErrorPosition`
/// (`fe-protocol3.c:1202`), which rlibpq does not port yet: it renders the
/// position as ` at character 15` instead (`docs/divergences.md`). Exactly
/// that block is rewritten in the expected output, and in C psql's, and must
/// occur exactly once in each, so the gate cannot quietly widen. The rest of
/// the section — `\gdesc` and the chunked `FETCH_COUNT` blocks — is
/// NAT-402's later slices.
#[test]
fn show_context_query_buffer_and_result_variables_match_psql_out_but_for_the_cursor() {
    let (sql, expected, first) = joined_sections("-- SHOW_CONTEXT", "-- working \\gdesc", &[]);
    let expected = without_the_cursor(&expected, "psql.out");
    let Some(cluster) = Cluster::start(SHOW_CONTEXT_THROUGH_RESULT_VARIABLES_PORT) else {
        return;
    };
    let ours = cluster.run_script(Path::new(RPSQL), &sql);
    if let Some(diff) = first_difference(expected.as_bytes(), &ours) {
        panic!(
            "rpsql, psql.sql:{} vs psql.out:{}: {diff}",
            first.sql_line, first.out_line
        );
    }
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cluster.run_script(&psql, &sql);
            let theirs = without_the_cursor(&String::from_utf8_lossy(&theirs), "C psql's output");
            if let Some(diff) = first_difference(theirs.as_bytes(), &ours) {
                panic!("rpsql vs C psql (-- SHOW_CONTEXT …): {diff}");
            }
        }
        None => reference::skip("psql"),
    }
}
