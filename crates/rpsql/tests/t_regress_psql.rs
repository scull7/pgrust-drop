//! Port of `src/test/regress/sql/psql.sql` (PostgreSQL 18.6), NAT-400's
//! output-format sections and NAT-404's `\g` and extended-query sections,
//! gated section by section against `expected/psql.out`.
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
//! headers. They are ported in NAT-400's slices, and NAT-403's for `\copy`; a
//! section with no gate here yet is one a later slice owns.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::path::Path;

use rlibpq::{Backend, FieldDescription, QueryResult, QueryRunner, TransactionStatus};
use rpsql::print::{PrintError, print_query};
use rpsql::pset::do_pset;
use rpsql::settings::{PrintQueryOpt, PsqlSettings};

use regress::{
    Cluster, PSQL_OUT, PSQL_SQL, Section, first_difference, section, sections, sha256_hex, split,
};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

#[test]
fn the_vendored_files_are_the_ones_postgresql_18_6_ships() {
    // ADR-0008: vendored bytes come from the tag or the tarball, and a
    // digest pins them so a local edit cannot pass for upstream.
    assert_eq!(
        sha256_hex(PSQL_SQL.as_bytes()),
        regress::PSQL_SQL_SHA256,
        "crates/rpsql/tests/regress/psql.sql is not REL_18_6's src/test/regress/sql/psql.sql"
    );
    assert_eq!(
        sha256_hex(PSQL_OUT.as_bytes()),
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
    assert_eq!(at("-- \\g and \\gx"), (24, 31));
    assert_eq!(at("-- errors"), (88, 214));
    assert_eq!(at("-- \\gset"), (99, 231));
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
const G_AND_GX_PORT: u16 = 55_494;
const EXTENDED_QUERY_SECTIONS_PORT: u16 = 55_495;
const SHOW_ALL_PSET_OPTIONS_PORT: u16 = 55_490;
const OUTPUT_FORMAT_SECTIONS_PORT: u16 = 55_491;
const COPY_MUST_SKIP_IN_LINE_DATA_PORT: u16 = 55_492;

/// [`regress::gate_section`] for rpsql.
fn gate_section(cluster: &Cluster, section: &Section<'_>) {
    regress::gate_section(cluster, Path::new(RPSQL), section);
}

/// `-- \g and \gx` (`psql.sql:24`) and `-- \gx should work in FETCH_COUNT
/// mode too` (`psql.sql:31`): a bare `\g` or `\gx` sends the query before
/// it, and `\gx` turns expanded output on for that one query.
///
/// `FETCH_COUNT` is not honoured yet (NAT-403): every result is fetched
/// whole. The second section matches anyway, because a one-row result prints
/// the same in one piece as in chunks of one row.
///
/// `-- \g/\gx with pset options` (`psql.sql:41`), between these and the
/// next gate, is not gated here: its first block prints csv, which is
/// NAT-400's (PR #42). `\g (…)` itself is pinned by `command.rs`'s unit tests.
#[test]
fn g_and_gx() {
    let Some(cluster) = Cluster::start(G_AND_GX_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &sections(
            "-- \\g and \\gx",
            "-- \\gx should work in FETCH_COUNT mode too",
        ),
    );
}

/// `-- \parse (extended query protocol)` (`psql.sql:48`) through `-- errors`
/// (`psql.sql:88`): `\parse`, `\bind_named`, `\close_prepared` and `\bind`
/// through the extended query protocol, and the server's error for each
/// kind of failure. They run as one script because `\bind_named` executes
/// what `\parse` prepared, and its repeated-call block resends the last
/// query `\parse` read.
///
/// Two of the errors carry a statement position, and so the syntax cursor
/// `reportErrorPosition` (`fe-protocol3.c:1202`) draws: the simple query a
/// bare `\g` resends (`psql.sql:65`), and a `\bind` whose Parse fails
/// (`psql.sql:90`).
#[test]
fn the_extended_query_sections() {
    let Some(cluster) = Cluster::start(EXTENDED_QUERY_SECTIONS_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &sections("-- \\parse (extended query protocol)", "-- errors"),
    );
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
/// server-free in file order so each starts from the state the one before
/// left.
///
/// The first section has 36 `execute q;` blocks and the second 45: aligned,
/// unaligned and wrapped, normal and expanded, at borders 0, 1 and 2, in
/// ascii and old-ascii. Every one renders and matches; the counts are pinned
/// so a block cannot drop out of the gate unnoticed.
#[test]
fn every_execute_q_block_matches_psql_out() {
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
    for (name, r, blocks) in [("multi-line", &multi, 36), ("single-line", &single, 45)] {
        assert_eq!(
            (r.matched, r.deferred.len()),
            (blocks, 0),
            "{name}: matched {} and deferred {:#?}",
            r.matched,
            r.deferred
        );
    }
}

/// The same two sections and `-- expanded output with short-width columns`
/// (`psql.sql:486`), live: `prepare`, `execute` and a table of `int`s through
/// rpsql against a server. They run as one script because the third starts
/// from the `\pset` state the second leaves (wrapped, old-ascii, `\pset
/// columns 20`).
#[test]
fn the_output_format_sections_run_live() {
    let Some(cluster) = Cluster::start(OUTPUT_FORMAT_SECTIONS_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &sections(
            "-- test multi-line headers, wrapping, and newline indicators",
            "-- expanded output with short-width columns",
        ),
    );
}

/// `-- \copy must skip in-line data, even if the issued COPY command fails.`
/// (`psql.sql:1570`): the server refuses the COPY, and the data inlined
/// after it — which contains backslash commands — is read and dropped up to
/// its `\.`, not run.
#[test]
fn copy_must_skip_in_line_data_even_if_the_issued_copy_command_fails() {
    let Some(cluster) = Cluster::start(COPY_MUST_SKIP_IN_LINE_DATA_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &section("-- \\copy must skip in-line data, even if the issued COPY command fails."),
    );
}
