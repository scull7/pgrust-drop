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

use regress::{
    Cluster, PSQL_OUT, PSQL_SQL, Section, first_difference, section, sections, split, tail,
};

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

/// `text`'s OID, and `int4`'s, which right-aligns (`print.c:3615`).
const TEXT: u32 = 25;
const INT4: u32 = 23;

fn field(name: &str, typid: u32) -> FieldDescription {
    FieldDescription {
        name: name.as_bytes().to_vec(),
        tableid: 0,
        columnid: 0,
        typid,
        typlen: if typid == INT4 { 4 } else { -1 },
        atttypmod: -1,
        format: 0,
    }
}

/// A result fed through the protocol state machine as a server would send
/// it: one `(name, type)` per column and every cell in text format.
fn result(columns: &[(&str, u32)], rows: &[Vec<String>]) -> QueryResult {
    let mut runner = QueryRunner::new();
    runner
        .push(Backend::RowDescription(
            columns
                .iter()
                .map(|&(name, typid)| field(name, typid))
                .collect(),
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

/// A `text`-only result.
fn text_result(headers: &[&str], rows: &[Vec<String>]) -> QueryResult {
    let columns: Vec<(&str, u32)> = headers.iter().map(|&h| (h, TEXT)).collect();
    result(&columns, rows)
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

/// The `q` a document-format section prepares: `text` columns, one value
/// each, then `n as int` over `generate_series(1,2)`.
fn document_q(text_columns: &[(&str, &str)]) -> QueryResult {
    let mut columns: Vec<(&str, u32)> = text_columns.iter().map(|&(h, _)| (h, TEXT)).collect();
    columns.push(("int", INT4));
    let rows: Vec<Vec<String>> = (1..=2)
        .map(|n| {
            let mut row: Vec<String> = text_columns.iter().map(|&(_, v)| v.to_string()).collect();
            row.push(n.to_string());
            row
        })
        .collect();
    result(&columns, &rows)
}

/// The document-format sections, in file order, each with its `q`:
/// `psql.sql:611`, `:653`, `:701`, `:746`, `:795` and `:852`, and the number
/// of `execute q;` blocks each has.
fn document_sections() -> [(&'static str, QueryResult, usize); 6] {
    let junk = ("junk", "  <foo>\n<bar>");
    let latex_junk = ("junk", "  #<foo>%&^~|\n{bar}");
    let empty = ("empty", "   ");
    [
        (
            "-- test asciidoc output format",
            document_q(&[("a|title", "some|text"), ("empty ", "        ")]),
            6,
        ),
        (
            "-- test csv output format",
            document_q(&[("a\"title", "some\"text"), junk, empty]),
            2,
        ),
        (
            "-- test html output format",
            document_q(&[("a&title", "some\"text"), junk, empty]),
            6,
        ),
        (
            "-- test latex output format",
            document_q(&[("a$title", "some\\more_text"), latex_junk, empty]),
            8,
        ),
        (
            "-- test latex-longtable output format",
            document_q(&[("a$title", "some\\more_text"), latex_junk, empty]),
            10,
        ),
        (
            "-- test troff-ms output format",
            document_q(&[("a\\title", "some\\text"), junk, empty]),
            6,
        ),
    ]
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
            // The block runs to the next echoed input line. Output never
            // starts with `\pset ` (though latex's starts with `\`).
            while i < lines.len()
                && !lines[i].starts_with("\\pset ")
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
const OUTPUT_FORMAT_SECTIONS_PORT: u16 = 55_491;
const DOCUMENT_FORMAT_SECTIONS_PORT: u16 = 55_492;

/// Run `section` through rpsql against `cluster`, and through C psql when this
/// lane has one, and require both to print exactly `psql.out`'s slice.
fn gate_section(cluster: &Cluster, section: &Section<'_>) {
    gate_script(cluster, section, "");
}

/// [`gate_section`], with `preamble` run first: input lines that restore the
/// state an earlier part of the file left. `-a -q` echoes them and prints
/// nothing else for them, so they are expected back verbatim.
fn gate_script(cluster: &Cluster, section: &Section<'_>, preamble: &str) {
    let script = format!("{preamble}{}", section.sql);
    let expected = format!("{preamble}{}", section.expected);
    let ours = cluster.run_script(Path::new(RPSQL), &script);
    if let Some(diff) = first_difference(expected.as_bytes(), &ours) {
        panic!(
            "rpsql, psql.sql:{} vs psql.out:{} ({}, after {preamble:?}): {diff}",
            section.sql_line, section.out_line, section.header
        );
    }
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cluster.run_script(&psql, &script);
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

/// `-- test asciidoc output format` through `-- test troff-ms output format`
/// (`psql.sql:595`-`:877`), replayed server-free in file order.
///
/// Each section prints `q` in its format, normal and expanded, at the borders
/// and `tableattr`s it lists: 38 `execute q;` blocks, all of which must
/// render and match. The sections' `\d` and `\df` blocks wait for NAT-401.
#[test]
fn every_document_format_block_matches_psql_out() {
    let mut popt = startup_popt();
    for (header, q, blocks) in document_sections() {
        let r = replay(&section(header), &q, &mut popt);
        assert_eq!(
            (r.matched, r.deferred.len()),
            (blocks, 0),
            "{header}: matched {} and deferred {:#?}",
            r.matched,
            r.deferred
        );
    }
}

/// The same six sections live, from each one's `prepare q as` on, with
/// `-- special cases` and `-- illegal csv separators` after csv's and then
/// `-- check ambiguous format requests` (`psql.sql:879`), all against one
/// cluster.
///
/// Each section's head (`\d psql_serial_tab_id_seq`, `\df exp`) needs
/// `\d` (NAT-401), so the gate starts at `prepare q as` with the one piece
/// of state the head leaves that the tail depends on, `\pset format`, set
/// by a preamble.
#[test]
fn the_document_format_sections_run_live() {
    let Some(cluster) = Cluster::start(DOCUMENT_FORMAT_SECTIONS_PORT) else {
        return;
    };
    for (header, format) in [
        ("-- test asciidoc output format", "asciidoc"),
        ("-- test csv output format", "csv"),
        ("-- test html output format", "html"),
        ("-- test latex output format", "latex"),
        ("-- test latex-longtable output format", "latex-longtable"),
        ("-- test troff-ms output format", "troff-ms"),
    ] {
        let whole = if format == "csv" {
            sections(header, "-- illegal csv separators")
        } else {
            section(header)
        };
        gate_script(
            &cluster,
            &tail(&whole, "prepare q as"),
            &format!("\\pset format {format}\n"),
        );
    }
    gate_section(&cluster, &section("-- check ambiguous format requests"));
}
