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

use std::fmt::Write as _;
use std::path::Path;

use rlibpq::{Backend, FieldDescription, QueryResult, QueryRunner, TransactionStatus};
use rpsql::print::{PrintError, print_query};
use rpsql::pset::do_pset;
use rpsql::settings::{PrintQueryOpt, PsqlSettings};
use testkit::reference;

use regress::{
    Cluster, PSQL_OUT, PSQL_SQL, Section, first_difference, head, only, section, sections, split,
    tail,
};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

fn hex(bytes: &[u8]) -> String {
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

#[test]
fn only_keeps_the_named_lines_with_what_each_printed() {
    let sql = "-- a\n\\x one\nselect 1;\n\n\\x two\n";
    let out = "-- a\n\\x one\nfirst\nselect 1;\n 1\n\\x two\nsecond\nlast\n";
    let section = Section {
        header: "-- a",
        sql,
        expected: out,
        sql_line: 1,
        out_line: 1,
    };
    let (kept_sql, kept_out) = only(&section, &["\\x one", "\\x two"]);
    assert_eq!(kept_sql, "\\x one\n\\x two\n");
    assert_eq!(kept_out, "\\x one\nfirst\n\\x two\nsecond\nlast\n");
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
const NUMERICLOCALE_PORT: u16 = 55_493;
const UNICODE_LINE_STYLE_PORT: u16 = 55_494;
const DISPLAY_WIDTH_PORT: u16 = 55_495;
const CONDITIONAL_AM_DISPLAY_PORT: u16 = 55_496;
const RELATION_LISTINGS_PORT: u16 = 55_497;
const PARTITIONED_RELATIONS_PORT: u16 = 55_498;
const ACCESS_METHODS_PORT: u16 = 55_499;
const PARTITION_AND_AM_LISTINGS_PORT: u16 = 55_500;
const FUNCTIONS_AND_OPERATORS_PORT: u16 = 55_501;
const FUNCTION_TYPE_OPERATOR_LISTINGS_PORT: u16 = 55_502;
const ROLES_AND_PRIVILEGES_PORT: u16 = 55_503;
const PUBLICATIONS_SUBSCRIPTIONS_EXTENSIONS_PORT: u16 = 55_504;
const SINGLE_QUERY_LISTINGS_PORT: u16 = 55_505;
const INVALID_MULTIPART_NAMES_PORT: u16 = 55_506;
const TEXT_SEARCH_AND_SQL_MED_PORT: u16 = 55_507;
const TABLE_DETAILS_PORT: u16 = 55_508;

/// Run `section` through rpsql against `cluster`, and through C psql when this
/// lane has one, and require both to print exactly `psql.out`'s slice.
fn gate_section(cluster: &Cluster, section: &Section<'_>) {
    gate_script(cluster, section, "");
}

/// [`gate_section`], with `preamble` run first: input lines that restore the
/// state an earlier part of the file left. `-a -q` echoes them and prints
/// nothing else for them, so they are expected back verbatim.
fn gate_script(cluster: &Cluster, section: &Section<'_>, preamble: &str) {
    gate_text(
        cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({}, after {preamble:?})",
            section.sql_line, section.out_line, section.header
        ),
        &format!("{preamble}{}", section.sql),
        &format!("{preamble}{}", section.expected),
        None,
    );
}

/// Run `script` through rpsql against `cluster`, and through C psql when this
/// lane has one, and require both to print exactly `expected`.
///
/// `reset`, when given, runs between the two to undo what `script` left in
/// the cluster; its output is not gated, but it must not fail.
fn gate_text(cluster: &Cluster, what: &str, script: &str, expected: &str, reset: Option<&str>) {
    let ours = cluster.run_script(Path::new(RPSQL), script);
    if let Some(diff) = first_difference(expected.as_bytes(), &ours) {
        panic!("rpsql, {what}: {diff}");
    }
    match cluster.reference_psql() {
        Some(psql) => {
            if let Some(reset) = reset {
                let out = cluster.run_script(Path::new(RPSQL), reset);
                let text = String::from_utf8_lossy(&out);
                assert!(!text.contains("ERROR"), "reset after {what}:\n{text}");
            }
            let theirs = cluster.run_script(&psql, script);
            if let Some(diff) = first_difference(&theirs, &ours) {
                panic!("rpsql vs C psql ({what}): {diff}");
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

/// The same two sections, `-- expanded output with short-width columns`
/// (`psql.sql:486`), `-- support table for output-format tests` (`:500`) and
/// `-- test header/footer/tuples_only behavior in aligned/unaligned/wrapped
/// cases` (`:504`), live: `prepare`, `execute`, a table of `int`s, and `\df
/// exp` and `\dfx exp` under `tuples_only` in aligned, unaligned and
/// wrapped, normal and expanded, through rpsql against a server. They run as
/// one script because each starts from the `\pset` state the one before
/// leaves (wrapped, old-ascii, `\pset columns 20`). The last section's six
/// `\d psql_serial_tab_id_seq` describe a sequence under its settings.
#[test]
fn the_output_format_sections_run_live() {
    let Some(cluster) = Cluster::start(OUTPUT_FORMAT_SECTIONS_PORT) else {
        return;
    };
    let run = sections(
        "-- test multi-line headers, wrapping, and newline indicators",
        "-- test header/footer/tuples_only behavior in aligned/unaligned/wrapped cases",
    );
    gate_text(
        &cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({} …)",
            run.sql_line, run.out_line, run.header
        ),
        run.sql,
        run.expected,
        // The support table, so that C psql can make it again.
        Some("drop table psql_serial_tab;\n"),
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

/// The same six sections live, with `-- special cases` and `-- illegal csv
/// separators` after csv's and then `-- check ambiguous format requests`
/// (`psql.sql:879`), all against one cluster: each prints `\d
/// psql_serial_tab_id_seq` and `\df exp` under `tuples_only`, normal and
/// expanded, in its format, then `q`.
///
/// The sequence is `-- support table for output-format tests`'s
/// (`psql.sql:500`), which [`the_output_format_sections_run_live`] gates;
/// here it is made once, ungated, for both psqls.
#[test]
fn the_document_format_sections_run_live() {
    let Some(cluster) = Cluster::start(DOCUMENT_FORMAT_SECTIONS_PORT) else {
        return;
    };
    let out = cluster.run_script(
        Path::new(RPSQL),
        "create table psql_serial_tab (id serial);\n",
    );
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");
    for header in [
        "-- test asciidoc output format",
        "-- test csv output format",
        "-- test html output format",
        "-- test latex output format",
        "-- test latex-longtable output format",
        "-- test troff-ms output format",
    ] {
        let whole = if header == "-- test csv output format" {
            sections(header, "-- illegal csv separators")
        } else {
            section(header)
        };
        gate_text(
            &cluster,
            &format!(
                "psql.sql:{} vs psql.out:{} ({})",
                whole.sql_line, whole.out_line, whole.header
            ),
            whole.sql,
            whole.expected,
            None,
        );
    }
    gate_section(&cluster, &section("-- check ambiguous format requests"));
}

/// `-- test numericlocale` (`psql.sql:584`), live, then the same setting
/// over values the stolen section's small integers never group, against C
/// psql alone since `psql.out` has no such block.
///
/// Both psqls run under `LC_ALL=C` (`Cluster::run_script`), whose
/// `localeconv()` is the one `DecimalLocale::POSIX` this port always uses:
/// groups of three, `,` between them, `.` for the point. The extra script
/// covers every column type `column_type_alignment` right-aligns, a text
/// column and a null (both left alone), a localized `money` value (not a
/// number, so left alone), `NaN`, and every format and `expanded`, since
/// `printQuery` rewrites the cell before any printer sees it
/// (`print.c:3589`).
#[test]
fn the_numericlocale_section_runs_live() {
    let Some(cluster) = Cluster::start(NUMERICLOCALE_PORT) else {
        return;
    };
    gate_section(
        &cluster,
        &section("-- test numericlocale (as best we can without control of psql's locale)"),
    );

    let mut script = String::from(
        "\\pset numericlocale on\n\\pset null 12345\n\
         prepare q as select 1234567::int8 as i8, -12345::int2 as i2, \
         -1234567::int4 as i4, 12345.5::float4 as f4, -1234567.125::float8 as f8, \
         1e20::float8 as e, 1234567.891::numeric as n, 'NaN'::numeric as nan, \
         12345::oid as o, 1234567::money as m, '1234567'::text as t, \
         null::int4 as z;\n",
    );
    for format in [
        "aligned",
        "unaligned",
        "csv",
        "html",
        "asciidoc",
        "latex",
        "troff-ms",
    ] {
        for expanded in ["off", "on"] {
            let _ = writeln!(
                script,
                "\\pset format {format}\n\\pset expanded {expanded}\nexecute q;"
            );
        }
    }
    diff_against_c_psql(&cluster, "numericlocale beyond psql.sql", &script);
}

/// The unicode line style, which `psql.sql` never draws: its two `execute
/// q;` sections (`psql.sql:222`-`:484`) with every `\pset linestyle` in them
/// turned to `unicode`, under each of the eight `unicode_border_linestyle`,
/// `unicode_column_linestyle` and `unicode_header_linestyle` combinations,
/// against C psql. That is aligned, wrapped and unaligned, normal and
/// expanded, at borders 0, 1 and 2, in every weight each junction of
/// `refresh_utf8format` (`print.c:3692`) can take.
#[test]
fn the_unicode_line_style_matches_c_psql() {
    let Some(cluster) = Cluster::start(UNICODE_LINE_STYLE_PORT) else {
        return;
    };
    let q_sections = sections(
        "-- test multi-line headers, wrapping, and newline indicators",
        "-- test single-line header and data",
    );
    let unicode = q_sections
        .sql
        .replace("\\pset linestyle old-ascii\n", "\\pset linestyle unicode\n")
        .replace("\\pset linestyle ascii\n", "\\pset linestyle unicode\n");
    assert_eq!(
        unicode.matches("\\pset linestyle unicode\n").count(),
        4,
        "psql.sql's two sections set the line style four times"
    );
    for border in ["single", "double"] {
        for column in ["single", "double"] {
            for header in ["single", "double"] {
                let script = format!(
                    "\\pset unicode_border_linestyle {border}\n\
                     \\pset unicode_column_linestyle {column}\n\
                     \\pset unicode_header_linestyle {header}\n{unicode}"
                );
                diff_against_c_psql(
                    &cluster,
                    &format!("unicode border {border}, column {column}, header {header}"),
                    &script,
                );
            }
        }
    }
}

/// Display width, which no upstream test exercises: `psql.sql`'s results are
/// all ASCII. Cells and headers with wide (CJK, fullwidth, emoji), combining
/// and zero-width characters, measured by `ucs_wcwidth` (`wchar.c:646`)
/// through `pg_wcssize` / `pg_wcsformat` (`mbprint.c:211`, `:294`) and cut by
/// `strlen_max_width` (`print.c:3747`), against C psql. That is aligned,
/// wrapped and unaligned, normal and expanded, at borders 0, 1 and 2, in the
/// ascii and unicode line styles, under `\pset columns` targets narrow
/// enough to break a line inside a run of wide characters and to leave a
/// column narrower than one of them. The cluster is UTF8, so the client
/// encoding is too.
#[test]
fn display_width_matches_c_psql() {
    let Some(cluster) = Cluster::start(DISPLAY_WIDTH_PORT) else {
        return;
    };
    let mut script = String::from(
        "\\pset title '表 title'\n\
         prepare q as select '中文字符ab'::text as w, 'e' || U&'\\0301' || 'x' as c, \
         '🎉ok１２' as \"é列\", E'ab\\n中x\\nzz中' as m, \
         'x中' || U&'\\200D' || 'y' as z, 12 as \"数\";\n",
    );
    for linestyle in ["ascii", "unicode"] {
        for format in ["aligned", "wrapped", "unaligned"] {
            for border in 0..=2 {
                for expanded in ["off", "on"] {
                    for columns in [8, 14, 40] {
                        let _ = writeln!(
                            script,
                            "\\pset linestyle {linestyle}\n\\pset format {format}\n\
                             \\pset border {border}\n\\pset expanded {expanded}\n\
                             \\pset columns {columns}\nexecute q;"
                        );
                    }
                }
            }
        }
    }
    diff_against_c_psql(&cluster, "display width", &script);
}

/// `-- check conditional am display` (`psql.sql:548`), live: `\d+`, `\dt+`,
/// `\dm+` and `\dv+` with and without `HIDE_TABLEAM`, and `\d+x`, over
/// tables, a view and a materialized view in two table access methods, and
/// `\d+ tbl_heap_psql`, `\d+ tbl_heap` (twice each) and `\d+x tbl_heap`,
/// which describe a table, its access method shown or not, and never
/// expanded.
///
/// The section's clean-up, from `RESET ROLE;` on, is cut too ([`head`]):
/// its `DROP SCHEMA … CASCADE` makes the server send a NOTICE, and rpsql
/// does not print server notices yet (psql's `NoticeProcessor`,
/// `common.c:281`). Everything before it is gated whole, and the clean-up
/// still runs, ungated, before the same script goes through C psql.
///
/// The section runs in the print state `psql.sql` leaves it: `\pset format
/// wrapped` (`psql.sql:535`) and `\pset columns 40` (`:432`), under which C
/// wraps nothing here because every header row is wider than 40
/// (`print.c:829`). The preamble restores both.
#[test]
fn the_conditional_am_display_section_runs_live() {
    let Some(cluster) = Cluster::start(CONDITIONAL_AM_DISPLAY_PORT) else {
        return;
    };
    let full = section("-- check conditional am display");
    let whole = head(&full, "RESET ROLE;");
    let preamble = "\\pset format wrapped\n\\pset columns 40\n";
    gate_text(
        &cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({})",
            whole.sql_line, whole.out_line, whole.header
        ),
        &format!("{preamble}{}", whole.sql),
        &format!("{preamble}{}", whole.expected),
        // The section's own clean-up, so that C psql starts where rpsql did.
        Some(tail(&full, "RESET ROLE;").sql),
    );
}

/// `listTables` beyond what `psql.sql` exercises, against C psql: every
/// relation type letter alone and combined, `S`, `+` and `x`, schema-qualified,
/// quoted, wildcard and database-qualified patterns, a trailing semicolon,
/// `HIDE_TABLEAM`, the not-found messages (which `-q` would silence, hence
/// `QUIET off`), and a pattern with too many dots. `ECHO_HIDDEN` is on for
/// most of it, so the catalog query itself is compared byte for byte, and
/// `noexec` once, which stops before the query runs.
#[test]
fn the_relation_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(RELATION_LISTINGS_PORT) else {
        return;
    };
    let script = "\\set QUIET off\n\
        create schema s1;\n\
        create table s1.\"Mixed\" (a int primary key);\n\
        create unlogged table s1.logless (a int);\n\
        create view s1.v as select 1 as one;\n\
        create materialized view s1.mv as select 1 as x;\n\
        create sequence s1.seq;\n\
        create table s1.part (a int) partition by range (a);\n\
        create index part_a on s1.part (a);\n\
        comment on table s1.logless is 'no WAL';\n\
        create table plain (b text);\n\
        \\d\n\
        \\set ECHO_HIDDEN on\n\
        \\d\n\
        \\dt\n\
        \\dti s1.*\n\
        \\di+ s1.*\n\
        \\dv s1.*\n\
        \\dm+ s1.*\n\
        \\ds s1.*\n\
        \\dE\n\
        \\dE nosuch\n\
        \\dt \"Mixed\"\n\
        \\dt s1.\"Mixed\"\n\
        \\dt s1.mixed\n\
        \\dt S1.?ogless\n\
        \\dt s1.m*\n\
        \\dtS pg_class\n\
        \\dtS+ pg_am\n\
        \\dt postgres.s1.*\n\
        \\dt+ s1.*;\n\
        \\dv nosuch;\n\
        \\dix s1.*\n\
        \\d+\n\
        \\set HIDE_TABLEAM on\n\
        \\dt+ s1.*\n\
        \\set ECHO_HIDDEN noexec\n\
        \\dt\n\
        \\set ECHO_HIDDEN off\n\
        \\dt a.b.c.d\n\
        \\dm\n\
        set client_min_messages = warning;\n\
        drop schema s1 cascade;\n\
        drop table plain;\n\
        \\d\n";
    diff_against_c_psql(&cluster, "relation listings", script);
}

/// `-- run test inside own schema and hide other partitions` and `-- only
/// partition related object should be displayed` (`psql.sql:1266`, `:1280`),
/// live: `\dP`, `\dPt`, `\dPi` and `\dPn` alone and combined, with and
/// without a pattern, over partitioned tables and indexes nested two deep.
///
/// The two sections start from what the end of the section before sets up
/// (`psql.sql:1259`-`:1264`: the `testpart` schema, and a role that owns it
/// and is the session's), so the gate starts at `create schema testpart;`,
/// which sits at the end of the section before. The second section drops
/// all of it again, role included, so C psql starts where rpsql did.
///
/// `-- \d on toast table` (`psql.sql:1328`), which follows them, runs on the
/// same cluster: `\d pg_toast.pg_toast_2619`, a TOAST table's columns, its
/// owning table and its index.
#[test]
fn the_partitioned_relations_sections_run_live() {
    let Some(cluster) = Cluster::start(PARTITIONED_RELATIONS_PORT) else {
        return;
    };
    let run = sections(
        "-- chunked results with an error after the first chunk",
        "-- only partition related object should be displayed",
    );
    gate_section(&cluster, &tail(&run, "create schema testpart;"));
    gate_section(
        &cluster,
        &section("-- \\d on toast table (use pg_statistic's toast table, which has a known name)"),
    );
}

/// `-- check printing info about access methods` (`psql.sql:1331`), live:
/// `\dA` with and without `+`, a pattern and an extra argument, and one each
/// of `\dAc`, `\dAf`, `\dAo`, `\dAp` with one or two patterns, `+` and `x`.
///
/// `pg_regress` runs `psql.sql` in a database where `create_am.sql:94` has
/// already made the table access method `heap2`, which `\dA` lists; the
/// preamble makes the same one, and the reset drops it between rpsql and
/// C psql.
#[test]
fn the_access_methods_section_runs_live() {
    let Some(cluster) = Cluster::start(ACCESS_METHODS_PORT) else {
        return;
    };
    let section = section("-- check printing info about access methods");
    let preamble = "CREATE ACCESS METHOD heap2 TYPE TABLE HANDLER heap_tableam_handler;\n";
    gate_text(
        &cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({}, after {preamble:?})",
            section.sql_line, section.out_line, section.header
        ),
        &format!("{preamble}{}", section.sql),
        &format!("{preamble}{}", section.expected),
        Some("DROP ACCESS METHOD heap2;\n"),
    );
}

/// `listPartitionedTables` and the access-method listings beyond what
/// `psql.sql` exercises, against C psql, with `ECHO_HIDDEN` on so each
/// catalog query is compared byte for byte: `\dP` with every letter and `+`
/// (both size queries), `x`, schema- and database-qualified patterns; `\dA`
/// with every listing's `+`, no pattern, one, two, `*` for the access method
/// (which adds no clause, so `\dAf`'s type subquery opens the `WHERE`), a
/// schema-qualified second pattern, a trailing semicolon on each, a third
/// argument, and the patterns `validateSQLNamePattern` refuses.
#[test]
fn the_partition_and_access_method_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(PARTITION_AND_AM_LISTINGS_PORT) else {
        return;
    };
    let script = "\\set QUIET off\n\
        create schema s2;\n\
        create table s2.parent (id int) partition by range (id);\n\
        create index parent_id on s2.parent (id);\n\
        create table s2.leaf partition of s2.parent for values from (0) to (10);\n\
        create table s2.mid partition of s2.parent for values from (10) to (20) partition by range (id);\n\
        create table s2.low partition of s2.mid for values from (10) to (15);\n\
        comment on table s2.parent is 'the root';\n\
        \\set ECHO_HIDDEN on\n\
        \\dP\n\
        \\dP+\n\
        \\dPn+\n\
        \\dPt+ s2.*\n\
        \\dPi+\n\
        \\dPin s2.*\n\
        \\dPtix\n\
        \\dP postgres.s2.m*;\n\
        \\dA\n\
        \\dA+ b*;\n\
        \\dAc\n\
        \\dAc+ btree\n\
        \\dAc gist pg_catalog.point\n\
        \\dAf+ hash\n\
        \\dAf * int4;\n\
        \\dAf btree int4 extra\n\
        \\dAo gist\n\
        \\dAo+ * pg_catalog.box_ops\n\
        \\dAp+ spgist\n\
        \\dApx hash integer_ops\n\
        \\set ECHO_HIDDEN off\n\
        \\dA regression.heap\n\
        \\dAc nonesuch.brin\n\
        \\dAf btree postgres.pg_catalog.int4\n\
        \\dAp btree nonesuch.pg_catalog.uuid_ops\n\
        \\dAo btree a.b.c.d\n\
        \\dP a.b.c.d\n\
        \\dPS\n\
        set client_min_messages = warning;\n\
        drop schema s2 cascade;\n\
        \\dP\n";
    diff_against_c_psql(&cluster, "partition and access method listings", script);
}

/// `-- check \dconfig`, `-- check \df, \do with argument specifications`
/// and `-- check \df+` (`psql.sql:1350`, `:1356`, `:1370`), live: `\dconfig`
/// with and without `+`, `\df` with argument-type patterns (a wildcard, a
/// schema-qualified type, an array type, `-` for "no such argument"),
/// `\dfa`, `\do` with one argument type and with two, and `\df+` over
/// three functions a fresh role owns, in one transaction the section rolls
/// back. The last section drops its role again, so C psql starts where
/// rpsql did.
#[test]
fn the_dconfig_df_and_do_sections_run_live() {
    let Some(cluster) = Cluster::start(FUNCTIONS_AND_OPERATORS_PORT) else {
        return;
    };
    gate_section(&cluster, &sections("-- check \\dconfig", "-- check \\df+"));
}

/// `\da`, `\df`, `\dT`, `\do` and `\dconfig` beyond what `psql.sql`
/// exercises, against C psql, with `ECHO_HIDDEN` on so each catalog query is
/// compared byte for byte: every `\df` kind letter alone and mixed, `S`, `+`
/// and `x`; argument patterns past the first, `-`, and the type names
/// `map_typename_pattern` rewrites; `\dT` with `+` over an enum (its
/// elements one per line), a composite and a domain, with and without `[]`;
/// `\do` with no, one, two and three argument types and `+`; `\dconfig`
/// with no pattern, a qualified one and `+`; and the refusals: a letter
/// `\df` does not take, too many dots, another database.
#[test]
fn the_function_type_and_operator_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(FUNCTION_TYPE_OPERATOR_LISTINGS_PORT) else {
        return;
    };
    let script = "\\set QUIET off\n\
        create schema s3;\n\
        create type s3.mood as enum ('sad', 'ok', 'happy');\n\
        create type s3.pair as (a int, b text);\n\
        create domain s3.posint as int check (value > 0);\n\
        comment on type s3.mood is 'how it feels';\n\
        create function s3.f(int, s3.mood) returns int language sql as 'select $1';\n\
        create function s3.trig() returns trigger language plpgsql as 'begin return null; end';\n\
        create procedure s3.p(int) language sql as 'select 1';\n\
        create aggregate s3.mysum(int) (sfunc = int4pl, stype = int);\n\
        create function s3.w() returns int window language internal as 'window_row_number';\n\
        create operator s3.=== (leftarg = int, rightarg = s3.mood, function = s3.f);\n\
        create operator s3.!!! (rightarg = int, function = int4um);\n\
        set work_mem = 20480;\n\
        \\set ECHO_HIDDEN on\n\
        \\da\n\
        \\da s3.*\n\
        \\daS sum\n\
        \\df\n\
        \\df+ s3.*\n\
        \\dfa s3.*\n\
        \\dfn s3.*\n\
        \\dfp\n\
        \\dft\n\
        \\dfw s3.*\n\
        \\dfnt s3.*\n\
        \\dfap s3.*\n\
        \\dfx s3.f\n\
        \\dfS int4pl\n\
        \\df s3.f int s3.mood\n\
        \\df s3.f integer -;\n\
        \\df int4pl int int\n\
        \\df array_* INT[] -\n\
        \\df numeric decimal\n\
        \\dT\n\
        \\dT+ s3.*\n\
        \\dTS int4\n\
        \\dT float\n\
        \\dT pg_catalog.int4[]\n\
        \\dT varchar[]\n\
        \\dT+ s3.pair\n\
        \\do\n\
        \\do+ s3.*\n\
        \\do s3.=== int\n\
        \\do s3.=== int s3.mood\n\
        \\do s3.!!! - int\n\
        \\do s3.!!! int\n\
        \\do + int int extra\n\
        \\doS ~~\n\
        \\dconfig\n\
        \\dconfig+\n\
        \\dconfig work_*\n\
        \\dconfig+ s.work_mem\n\
        \\dconfigx+ WORK_MEM\n\
        \\set ECHO_HIDDEN off\n\
        \\dfz\n\
        \\dfnq s3.*\n\
        \\df a.b.c.d\n\
        \\df postgres.s3.f\n\
        \\da a.b.c.d\n\
        \\dT nonesuch.s3.mood\n\
        \\do a.b.c.d\n\
        \\df s3.f a.b.c.d\n\
        reset work_mem;\n\
        set client_min_messages = warning;\n\
        drop schema s3 cascade;\n";
    diff_against_c_psql(&cluster, "function, type and operator listings", script);
}

/// Every line of `psql.sql`'s invalid-name sections (`:1679`-`:1919`) that
/// names `\dD`, `\ddp`, `\dg`, `\dp` or `\drds`, verbatim and in order.
const INVALID_ROLE_AND_PRIVILEGE_NAMES: &str = "\\dD host.regression.public.gtestdomain1\n\
    \\dD ].public.gtestdomain1\n\
    \\dD nonesuch.public.gtestdomain1\n\
    \\ddp host.regression.pg_catalog.pg_class\n\
    \\ddp {.pg_catalog.pg_class\n\
    \\ddp nonesuch.pg_catalog.pg_class\n\
    \\dg nonesuch.pg_database_owner\n\
    \\dg regression.pg_database_owner\n\
    \\dp host.regression.public.a_star\n\
    \\dp \"regres+ion\".public.a_star\n\
    \\dp nonesuch.public.a_star\n\
    \\drds nonesuch.lc_messages\n\
    \\drds regression.lc_messages\n\
    \\dD \"no.such.domain\"\n\
    \\ddp \"no.such.default.access.privilege\"\n\
    \\dg \"no.such.role\"\n\
    \\dp \"no.such.access.privilege\"\n\
    \\drds \"no.such.setting\"\n\
    \\dD \"no.such.schema\".\"no.such.domain\"\n\
    \\ddp \"no.such.schema\".\"no.such.default.access.privilege\"\n\
    \\dg \"no.such.schema\".\"no.such.role\"\n\
    \\dp \"no.such.schema\".\"no.such.access.privilege\"\n\
    \\drds \"no.such.schema\".\"no.such.setting\"\n\
    \\dD regression.\"no.such.schema\".\"no.such.domain\"\n\
    \\dp regression.\"no.such.schema\".\"no.such.access.privilege\"\n\
    \\dD \"no.such.database\".\"no.such.schema\".\"no.such.domain\"\n\
    \\ddp \"no.such.database\".\"no.such.schema\".\"no.such.default.access.privilege\"\n\
    \\dp \"no.such.database\".\"no.such.schema\".\"no.such.access.privilege\"\n";

/// `-- check \drg and \du`, `-- Test display of empty privileges.` and
/// `-- Test display of default privileges with \pset null.`
/// (`psql.sql:1921`, `:1947`, `:1971`), the last three sections of the file,
/// live and whole: `\drg` over grants with every mix of ADMIN, INHERIT and
/// SET, `\du` folding a role's attributes, `\dD+`, `\df+`, `\dp` and `\dT+`
/// over objects whose privileges were all revoked, and `\z` and `\zx` under
/// `\pset null`. Each section cleans up after itself, so the cluster is then
/// reused for the roles and privileges commands beyond what `psql.sql`
/// exercises, against C psql, with `ECHO_HIDDEN` on so each catalog query is
/// compared byte for byte:
///
/// - `\du` and `\dg` with `S`, `+` and `x`, over roles with every attribute,
///   a connection limit of 0, 1 and more, and an expiry;
/// - `\drg` with and without `S` and a pattern;
/// - `\drds` with no, one and two patterns, a third, and nothing found;
/// - `\dp` and every spelling of `\z` over column privileges and permissive
///   and restrictive policies;
/// - `\ddp` over default privileges for each kind of object, matched by
///   schema and by owner;
/// - `\dD` with a collation, `NOT NULL`, a default, two checks and a comment;
/// - and, verbatim, every line of `psql.sql`'s invalid-name sections
///   (`:1679`-`:1919`) that names one of these commands.
#[test]
fn the_roles_and_privileges_sections_run_live() {
    let Some(cluster) = Cluster::start(ROLES_AND_PRIVILEGES_PORT) else {
        return;
    };
    // `test_setup.sql:24`, which the regression database has by the time
    // `psql.sql` runs: a fresh role may create in `public`.
    gate_script(
        &cluster,
        &sections(
            "-- check \\drg and \\du",
            "-- Test display of default privileges with \\pset null.",
        ),
        "GRANT ALL ON SCHEMA public TO public;\n",
    );

    let script = format!(
        "\\set QUIET off\n\
        create role s4_all superuser createdb createrole replication bypassrls \
        connection limit 1 valid until 'infinity';\n\
        create role s4_a login connection limit 0;\n\
        create role s4_b noinherit connection limit 5;\n\
        comment on role s4_b is 'the second';\n\
        grant s4_a to s4_b with admin true, inherit false;\n\
        grant s4_b to s4_all;\n\
        alter role s4_a set work_mem = '1MB';\n\
        alter role s4_a in database postgres set search_path = s4;\n\
        create schema s4;\n\
        create table s4.t (a int, b int);\n\
        create view s4.v as select a from s4.t;\n\
        create sequence s4.s;\n\
        grant select on s4.t to s4_a;\n\
        grant update (b), insert (a, b) on s4.t to s4_b;\n\
        create policy p on s4.t for select to s4_a, s4_b using (a > 0);\n\
        create policy q on s4.t as restrictive using (true) with check (b > 0);\n\
        alter default privileges for role s4_a in schema s4 grant select on tables to s4_b;\n\
        alter default privileges for role s4_a grant usage on sequences to s4_b;\n\
        alter default privileges for role s4_a revoke execute on functions from public;\n\
        alter default privileges for role s4_b grant usage on types to s4_a;\n\
        alter default privileges for role s4_b grant create on schemas to s4_a;\n\
        alter default privileges for role s4_b grant select on large objects to s4_a;\n\
        create domain s4.d as text collate \"C\" not null default 'x' \
        check (value <> '') check (length(value) < 9);\n\
        comment on domain s4.d is 'short text';\n\
        create domain s4.i as int;\n\
        \\set ECHO_HIDDEN on\n\
        \\du\n\
        \\du+ s4_*\n\
        \\duS pg_read_all_*\n\
        \\dg s4_?\n\
        \\du+x s4_all\n\
        \\drg\n\
        \\drgS pg_*\n\
        \\drg s4_*\n\
        \\drds\n\
        \\drds s4_a\n\
        \\drds s4_a postgres\n\
        \\drds * postgres\n\
        \\drds nonesuch\n\
        \\drds nonesuch nodb\n\
        \\drds s4_a postgres extra\n\
        \\dp\n\
        \\dp s4.*\n\
        \\dpS pg_catalog.pg_class\n\
        \\z s4.t\n\
        \\zS s4.v\n\
        \\zx s4.t\n\
        \\zSx s4.s\n\
        \\zxS s4.s\n\
        \\ddp\n\
        \\ddp s4\n\
        \\ddp s4_b\n\
        \\dD\n\
        \\dD+ s4.*\n\
        \\dDS pg_catalog.*\n\
        \\dD+x s4.d\n\
        \\set ECHO_HIDDEN off\n\
        {INVALID_ROLE_AND_PRIVILEGE_NAMES}\
        \\drg a.b\n\
        \\du a.b\n\
        \\drds a b.c\n\
        \\z+\n\
        set client_min_messages = warning;\n\
        drop schema s4 cascade;\n\
        drop owned by s4_a;\n\
        drop owned by s4_b;\n\
        drop role s4_all, s4_a, s4_b;\n"
    );
    diff_against_c_psql(&cluster, "roles and privileges listings", &script);
}

/// Every line of `psql.sql`'s invalid-name sections (`:1679`-`:1919`) that
/// names `\dRp`, `\dRs` or `\dx`, in file order.
const INVALID_PUBLICATION_SUBSCRIPTION_EXTENSION_NAMES: [&str; 12] = [
    "\\dRp public.mypub",
    "\\dRp regression.mypub",
    "\\dRs public.mysub",
    "\\dRs regression.mysub",
    "\\dx regression.plpgsql",
    "\\dx nonesuch.plpgsql",
    "\\dRp \"no.such.publication\"",
    "\\dRs \"no.such.subscription\"",
    "\\dx \"no.such.installed.extension\"",
    "\\dRp \"no.such.schema\".\"no.such.publication\"",
    "\\dRs \"no.such.schema\".\"no.such.subscription\"",
    "\\dx \"no.such.schema\".\"no.such.installed.extension\"",
];

/// What [`the_publication_subscription_and_extension_listings_match_c_psql`]
/// lists, set up once by rpsql so that both sides see the same OIDs, which
/// `\dRp+`'s footer queries and `\dx+`'s contents query paste in.
///
/// Taken from `publication.sql` (`:11`, `:14`, `:18`, `:45`, `:655`-`:660`)
/// and `subscription.sql` (`:64`, `:73`), plus publications over a column
/// list, a row filter, schemas, generated columns and
/// `publish_via_partition_root`, and a subscription with every option `+`
/// shows set away from its default.
///
/// A publication warns that `wal_level` is too low, and a subscription that
/// it is not connected: server notices, which rpsql does not print yet, so
/// the setup keeps them from being sent.
const PUBLICATIONS_SUBSCRIPTIONS_SETUP: &str = "set client_min_messages = error;\n\
    create schema s5;\n\
    create schema s5b;\n\
    create table s5.t (a int primary key, b int, c text);\n\
    create table s5.u (a int, g int generated always as (a * 2) stored);\n\
    create table s5b.w (y int);\n\
    CREATE PUBLICATION testpub_default;\n\
    COMMENT ON PUBLICATION testpub_default IS 'test publication';\n\
    CREATE PUBLICATION testpub_ins_trunct WITH (publish = insert);\n\
    CREATE PUBLICATION testpub_foralltables FOR ALL TABLES WITH (publish = 'insert');\n\
    CREATE PUBLICATION testpub_both_filters;\n\
    CREATE TABLE testpub_tbl_both_filters (a int, b int, c int, PRIMARY KEY (a,c));\n\
    ALTER TABLE testpub_tbl_both_filters REPLICA IDENTITY USING INDEX testpub_tbl_both_filters_pkey;\n\
    ALTER PUBLICATION testpub_both_filters ADD TABLE testpub_tbl_both_filters (a,c) WHERE (c != 1);\n\
    create publication s5_cols for table s5.t (a, c) where (b > 0 and c <> 'x'), s5.u;\n\
    create publication s5_schemas for tables in schema s5b, s5, table public.testpub_tbl_both_filters;\n\
    create publication s5_gen for table s5.u \
    with (publish_generated_columns = stored, publish_via_partition_root = true);\n\
    CREATE SUBSCRIPTION regress_testsub3 CONNECTION 'dbname=regress_doesnotexist' \
    PUBLICATION testpub WITH (slot_name = NONE, connect = false);\n\
    CREATE SUBSCRIPTION regress_testsub4 CONNECTION 'dbname=regress_doesnotexist' \
    PUBLICATION testpub WITH (slot_name = NONE, connect = false, origin = none);\n\
    create subscription regress_s5_all connection 'dbname=regress_doesnotexist port=1' \
    publication testpub_default, s5_cols with (slot_name = none, connect = false, \
    binary = true, streaming = parallel, two_phase = true, disable_on_error = true, \
    password_required = false, run_as_owner = true, synchronous_commit = local);\n";

/// `\dRp`, `\dRp+`, `\dRs`, `\dRs+`, `\dx` and `\dx+`, with and without a
/// pattern, with `x`, with nothing found (loud and quiet), and with too many
/// dots, under `ECHO_HIDDEN` so every catalog query is compared too.
const PUBLICATION_SUBSCRIPTION_EXTENSION_COMMANDS: &str = "\\set QUIET off\n\
    \\set ECHO_HIDDEN on\n\
    \\dRp\n\
    \\dRp testpub_*\n\
    \\dRpx s5_gen\n\
    \\dRp+\n\
    \\dRp+ s5_*\n\
    \\dRp+ testpub_both_filters\n\
    \\dRp+x s5_cols\n\
    \\dRp+ nonesuch\n\
    \\dRp nonesuch\n\
    \\dRs\n\
    \\dRs regress_testsub?\n\
    \\dRs+\n\
    \\dRs+x regress_s5_all\n\
    \\dRs nonesuch\n\
    \\dx\n\
    \\dx plpgsql\n\
    \\dx+\n\
    \\dx+ plpgsql\n\
    \\dx+x plpgsql\n\
    \\dx+ nonesuch\n\
    \\dx nonesuch\n\
    \\set ECHO_HIDDEN off\n\
    \\dRp+ a.b\n\
    \\dRs+ a.b\n\
    \\dx+ a.b\n\
    \\set QUIET on\n\
    \\dRp+ nonesuch\n\
    \\dx+ nonesuch\n\
    \\pset title 'ignored'\n\
    \\dRp+ s5_schemas\n\
    \\pset title\n";

/// A publication with footers and one without, normal and expanded, in every
/// format at borders 0 and 2, with `tuples_only`, and with `\pset footer
/// off`; then unaligned with a zero-byte record separator. `wrapped` needs a
/// target width this port does not take from the terminal, hence
/// `\pset columns`.
fn publication_in_every_format() -> String {
    let mut script = String::from("\\pset columns 60\n");
    for format in [
        "aligned",
        "wrapped",
        "unaligned",
        "csv",
        "html",
        "asciidoc",
        "latex",
        "latex-longtable",
        "troff-ms",
    ] {
        let _ = writeln!(script, "\\pset format {format}");
        for settings in [
            "\\pset border 0\n",
            "\\pset border 2\n",
            "\\pset tuples_only on\n",
            "\\pset tuples_only off\n\\pset footer off\n",
        ] {
            script.push_str(settings);
            script.push_str(
                "\\dRp+ s5_schemas\n\\dRp+x s5_schemas\n\
                 \\dRp+ testpub_foralltables\n\\dRp+x testpub_foralltables\n",
            );
        }
        script.push_str("\\pset footer on\n\\pset border 1\n");
    }
    script.push_str(
        "\\pset format unaligned\n\\pset recordsep_zero\n\\dRp+ s5_schemas\n\\dRp+x s5_schemas\n",
    );
    script
}

/// Publications, subscriptions and extensions, `NAT-401`'s fourth group.
///
/// `psql.sql` names `\dRp`, `\dRs` and `\dx` only in its invalid-name
/// sections (`:1679`-`:1919`), whose other commands later slices bring, so
/// those twelve lines are gated alone ([`only`]), verbatim, against their
/// `psql.out` output and C psql's.
///
/// The rest runs against C psql over [`PUBLICATIONS_SUBSCRIPTIONS_SETUP`]:
/// [`PUBLICATION_SUBSCRIPTION_EXTENSION_COMMANDS`], then
/// [`publication_in_every_format`], which is what reaches the footers
/// `printTableAddFooter` adds in every printer.
#[test]
fn the_publication_subscription_and_extension_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(PUBLICATIONS_SUBSCRIPTIONS_EXTENSIONS_PORT) else {
        return;
    };
    let (sql, expected) = only(
        &sections(
            "-- check describing invalid multipart names",
            "-- again, but with dotted database and dotted schema qualifications.",
        ),
        &INVALID_PUBLICATION_SUBSCRIPTION_EXTENSION_NAMES,
    );
    gate_text(
        &cluster,
        "psql.sql's invalid \\dRp, \\dRs and \\dx names vs psql.out",
        &sql,
        &expected,
        None,
    );

    let out = cluster.run_script(Path::new(RPSQL), PUBLICATIONS_SUBSCRIPTIONS_SETUP);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");
    let script = format!(
        "{PUBLICATION_SUBSCRIPTION_EXTENSION_COMMANDS}{}",
        publication_in_every_format()
    );
    diff_against_c_psql(
        &cluster,
        "publication, subscription and extension listings",
        &script,
    );
}

/// Every line of `psql.sql`'s invalid-name sections (`:1679`-`:1919`) that
/// names `\db`, `\dc`, `\dC`, `\dd`, `\dL`, `\dn`, `\dO`, `\dX` or `\dy`,
/// in file order, but for [`INVALID_LISTING_NAMES_IN_REGRESSION`].
const INVALID_LISTING_NAMES: [&str; 47] = [
    "\\db nonesuch.pg_default",
    "\\db regression.pg_default",
    "\\dc host.regression.public.conversion",
    "\\dc (.public.conversion",
    "\\dc nonesuch.public.conversion",
    "\\dC host.regression.pg_catalog.int8",
    "\\dC ).pg_catalog.int8",
    "\\dC nonesuch.pg_catalog.int8",
    "\\dd host.regression.pg_catalog.pg_class",
    "\\dd [.pg_catalog.pg_class",
    "\\dd nonesuch.pg_catalog.pg_class",
    "\\dL host.regression.plpgsql",
    "\\dL *.plpgsql",
    "\\dL nonesuch.plpgsql",
    "\\dn host.regression.public",
    "\\dn \"\"\"\".public",
    "\\dn nonesuch.public",
    "\\dO host.regression.pg_catalog.POSIX",
    "\\dO .pg_catalog.POSIX",
    "\\dO nonesuch.pg_catalog.POSIX",
    "\\dX host.regression.public.func_deps_stat",
    "\\dX \"^regression$\".public.func_deps_stat",
    "\\dX nonesuch.public.func_deps_stat",
    "\\dy regression.myevt",
    "\\dy nonesuch.myevt",
    "\\db \"no.such.tablespace\"",
    "\\dc \"no.such.conversion\"",
    "\\dC \"no.such.cast\"",
    "\\dd \"no.such.object.description\"",
    "\\dL \"no.such.language\"",
    "\\dn \"no.such.schema\"",
    "\\dO \"no.such.collation\"",
    "\\dX \"no.such.extended.statistics\"",
    "\\dy \"no.such.event.trigger\"",
    "\\db \"no.such.schema\".\"no.such.tablespace\"",
    "\\dc \"no.such.schema\".\"no.such.conversion\"",
    "\\dC \"no.such.schema\".\"no.such.cast\"",
    "\\dd \"no.such.schema\".\"no.such.object.description\"",
    "\\dL \"no.such.schema\".\"no.such.language\"",
    "\\dO \"no.such.schema\".\"no.such.collation\"",
    "\\dX \"no.such.schema\".\"no.such.extended.statistics\"",
    "\\dy \"no.such.schema\".\"no.such.event.trigger\"",
    "\\dc \"no.such.database\".\"no.such.schema\".\"no.such.conversion\"",
    "\\dC \"no.such.database\".\"no.such.schema\".\"no.such.cast\"",
    "\\dd \"no.such.database\".\"no.such.schema\".\"no.such.object.description\"",
    "\\dO \"no.such.database\".\"no.such.schema\".\"no.such.collation\"",
    "\\dX \"no.such.database\".\"no.such.schema\".\"no.such.extended.statistics\"",
];

/// The lines of those sections, for the same commands, that name the
/// database `regression` in a three-part name (`:1875`-`:1894`).
/// `psql.out` lists nothing for them, having been connected to
/// `regression`; a gate cluster's database is `postgres`, so they are
/// "cross-database references" here, and are gated against C psql only.
const INVALID_LISTING_NAMES_IN_REGRESSION: &str = "\\dc regression.\"no.such.schema\".\"no.such.conversion\"\n\\dC regression.\"no.such.schema\".\"no.such.cast\"\n\\dd regression.\"no.such.schema\".\"no.such.object.description\"\n\\dO regression.\"no.such.schema\".\"no.such.collation\"\n\\dX regression.\"no.such.schema\".\"no.such.extended.statistics\"\n";

/// The objects [`the_single_query_listings_match_c_psql`] lists: one of each
/// kind, commented, set up once by rpsql so that both sides see the same
/// OIDs. The tablespace is in place (`allow_in_place_tablespaces`), so it
/// needs no directory, and its location names its OID. A publication warns
/// that `wal_level` is too low, a server notice rpsql does not print yet,
/// so the setup keeps notices from being sent.
const SINGLE_QUERY_LISTINGS_SETUP: &str = "set client_min_messages = error;\n\
    set allow_in_place_tablespaces = on;\n\
    create tablespace s6_ts location '';\n\
    comment on tablespace s6_ts is 'in place';\n\
    create schema s6;\n\
    comment on schema s6 is 'slice six';\n\
    grant usage on schema s6 to public;\n\
    create publication s6_pub for tables in schema s6;\n\
    create publication s6_pub2 for tables in schema s6;\n\
    create conversion s6.myconv for 'LATIN1' to 'UTF8' from iso8859_1_to_utf8;\n\
    comment on conversion s6.myconv is 'latin one';\n\
    create type s6.c as (a int);\n\
    create function s6.c_to_int(s6.c) returns int language sql immutable as 'select $1.a';\n\
    create cast (s6.c as int) with function s6.c_to_int(s6.c) as assignment;\n\
    create cast (s6.c as text) with inout;\n\
    comment on cast (s6.c as int) is 'first field';\n\
    create table s6.t (a int constraint pos check (a > 0), b int);\n\
    create table s6.st (a int, b int);\n\
    comment on constraint pos on s6.t is 'positive';\n\
    create domain s6.d as int constraint dpos check (value > 0);\n\
    comment on constraint dpos on domain s6.d is 'positive domain';\n\
    create operator family s6.fam using hash;\n\
    comment on operator family s6.fam using hash is 'a family';\n\
    create operator class s6.int4_ops2 for type int4 using btree as \
    operator 1 <, operator 2 <=, operator 3 =, operator 4 >=, operator 5 >, \
    function 1 btint4cmp(int4, int4);\n\
    comment on operator class s6.int4_ops2 using btree is 'a class';\n\
    create rule r as on insert to s6.t where new.a > 100 do also \
    insert into s6.st values (new.a, new.b);\n\
    comment on rule r on s6.t is 'a rule';\n\
    create function s6.trg() returns trigger language plpgsql as 'begin return new; end';\n\
    create trigger tg before insert on s6.t for each row execute function s6.trg();\n\
    comment on trigger tg on s6.t is 'a trigger';\n\
    select lo_create(4242);\n\
    select lo_create(4243);\n\
    comment on large object 4242 is 'a blob';\n\
    grant select on large object 4242 to public;\n\
    create collation s6.coll (provider = libc, locale = 'C');\n\
    create collation s6.posix (provider = libc, lc_collate = 'POSIX', lc_ctype = 'C');\n\
    comment on collation s6.coll is 'plain C';\n\
    create statistics s6.st1 (dependencies, ndistinct) on a, b from s6.st;\n\
    create statistics s6.st2 (mcv) on a, (a + b) from s6.st;\n\
    create function s6.evt() returns event_trigger language plpgsql as 'begin end';\n\
    create event trigger s6_evt on ddl_command_start \
    when tag in ('CREATE TABLE', 'DROP TABLE') execute function s6.evt();\n\
    alter event trigger s6_evt disable;\n\
    create event trigger s6_evt2 on sql_drop execute function s6.evt();\n\
    alter event trigger s6_evt2 enable always;\n\
    comment on event trigger s6_evt2 is 'on drop';\n";

/// Each listing with and without a pattern, with `+`, `S` and `x`, and with
/// nothing found, under `ECHO_HIDDEN` so every catalog query is compared
/// too; then too many dots for each. `\db+` names only the new tablespace,
/// whose size cannot change between the two runs.
const SINGLE_QUERY_LISTING_COMMANDS: &str = "\\set QUIET off\n\
    \\set ECHO_HIDDEN on\n\
    \\db\n\
    \\db s6_*\n\
    \\db+ s6_ts\n\
    \\dbx s6_ts\n\
    \\db nonesuch\n\
    \\dc\n\
    \\dc+\n\
    \\dc+ s6.*\n\
    \\dcS iso_8859_1_*\n\
    \\dcx myconv\n\
    \\dC s6.c\n\
    \\dC+ s6.c\n\
    \\dC bigint\n\
    \\dC+ int8\n\
    \\dC\n\
    \\dCx s6.*\n\
    \\dd\n\
    \\dd s6.*\n\
    \\dd tg\n\
    \\ddS\n\
    \\ddx s6.pos\n\
    \\dd nonesuch\n\
    \\dl\n\
    \\dl+\n\
    \\dlx\n\
    \\dl ignored\n\
    \\dL\n\
    \\dL+\n\
    \\dLS\n\
    \\dLS+ internal\n\
    \\dLx plpgsql\n\
    \\dn\n\
    \\dn+\n\
    \\dnS\n\
    \\dn s6\n\
    \\dn+ s6\n\
    \\dnx s6\n\
    \\dn s?\n\
    \\dn nonesuch\n\
    \\dO\n\
    \\dO+ s6.*\n\
    \\dOS pg_catalog.c*\n\
    \\dOx s6.coll\n\
    \\dO nonesuch\n\
    \\dX\n\
    \\dX s6.st1\n\
    \\dXx st2\n\
    \\dX nonesuch\n\
    \\dy\n\
    \\dy+\n\
    \\dy s6_evt2\n\
    \\dy+x s6_evt\n\
    \\dy nonesuch\n\
    \\set ECHO_HIDDEN off\n\
    \\db a.b\n\
    \\dc a.b.c.d\n\
    \\dC a.b.c.d\n\
    \\dd a.b.c.d\n\
    \\dL a.b.c\n\
    \\dn a.b.c\n\
    \\dO a.b.c.d\n\
    \\dX a.b.c.d\n\
    \\dy a.b\n\
    \\dn postgres.s6\n\
    \\dL postgres.plpgsql\n";

/// `\dn`'s "Publications:" footers, which `printQuery` takes from
/// `opt->footers`, normal and expanded, in every format at borders 0 and 2
/// and with `tuples_only`. `wrapped` needs a target width this port does not
/// take from the terminal, hence `\pset columns`.
fn schema_footers_in_every_format() -> String {
    let mut script = String::from("\\pset columns 60\n");
    for format in [
        "aligned",
        "wrapped",
        "unaligned",
        "csv",
        "html",
        "asciidoc",
        "latex",
        "latex-longtable",
        "troff-ms",
    ] {
        let _ = writeln!(script, "\\pset format {format}");
        for settings in [
            "\\pset border 0\n",
            "\\pset border 2\n",
            "\\pset tuples_only on\n",
        ] {
            script.push_str(settings);
            script.push_str("\\dn s6\n\\dnx s6\n\\dn nonesuch\n\\dnx nonesuch\n");
        }
        script.push_str("\\pset tuples_only off\n\\pset border 1\n");
    }
    script
}

/// The single-query listings, `NAT-401`'s fifth group: `\db`, `\dc`, `\dC`,
/// `\dd`, `\dl`, `\dL`, `\dn`, `\dO`, `\dX` and `\dy`.
///
/// `psql.sql` names them only in its invalid-name sections (`:1679`-`:1919`),
/// whose `\dF` and `\de` commands came a slice later. Those lines are gated
/// alone ([`only`]), verbatim, against their `psql.out` output and C psql's,
/// in database `postgres`; [`the_invalid_multipart_name_sections_run_live`]
/// gates the sections whole, in `regression`. The rest runs against C psql over [`SINGLE_QUERY_LISTINGS_SETUP`]:
/// [`INVALID_LISTING_NAMES_IN_REGRESSION`],
/// [`SINGLE_QUERY_LISTING_COMMANDS`], then [`schema_footers_in_every_format`].
#[test]
fn the_single_query_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(SINGLE_QUERY_LISTINGS_PORT) else {
        return;
    };
    let (sql, expected) = only(
        &sections(
            "-- check describing invalid multipart names",
            "-- again, but with dotted database and dotted schema qualifications.",
        ),
        &INVALID_LISTING_NAMES,
    );
    gate_text(
        &cluster,
        "psql.sql's invalid \\db, \\dc, \\dC, \\dd, \\dL, \\dn, \\dO, \\dX and \\dy names vs psql.out",
        &sql,
        &expected,
        None,
    );

    let out = cluster.run_script(Path::new(RPSQL), SINGLE_QUERY_LISTINGS_SETUP);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");
    let script = format!(
        "{INVALID_LISTING_NAMES_IN_REGRESSION}{SINGLE_QUERY_LISTING_COMMANDS}{}",
        schema_footers_in_every_format()
    );
    diff_against_c_psql(&cluster, "single-query listings", &script);
}

/// `psql.sql`'s invalid-name sections (`:1679`-`:1919`), whole: every `\d`
/// command with too many dotted parts, a database part, quoted dots, and
/// names that match nothing. With `\dF*` and `\de*` ported, no line of them
/// needs cutting out any more.
///
/// A few lines name the database `regression` (`pg_regress`'s), which is
/// then no cross-database reference, so the run connects to one of that
/// name, created empty: every name the sections look up is one that matches
/// nothing.
#[test]
fn the_invalid_multipart_name_sections_run_live() {
    let Some(cluster) = Cluster::start(INVALID_MULTIPART_NAMES_PORT) else {
        return;
    };
    let out = cluster.run_script(Path::new(RPSQL), "create database regression;\n");
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");

    let section = sections(
        "-- check describing invalid multipart names",
        "-- again, but with dotted database and dotted schema qualifications.",
    );
    let what = format!(
        "psql.sql:{} vs psql.out:{} ({})",
        section.sql_line, section.out_line, section.header
    );
    let ours = cluster.run_script_in(Path::new(RPSQL), "regression", section.sql);
    if let Some(diff) = first_difference(section.expected.as_bytes(), &ours) {
        panic!("rpsql, {what}: {diff}");
    }
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cluster.run_script_in(&psql, "regression", section.sql);
            if let Some(diff) = first_difference(&theirs, &ours) {
                panic!("rpsql vs C psql ({what}): {diff}");
            }
        }
        None => reference::skip("psql"),
    }
}

/// One object of each text search and SQL/MED kind, in a schema of its own,
/// commented, and a second one of each where a listing could tell them
/// apart (a visible one, one with no options, one with no handler). The
/// wrapper with a validator uses core's `postgresql_fdw_validator`, as
/// `foreign_data.sql` does, so no extension is needed.
const TEXT_SEARCH_AND_SQL_MED_SETUP: &str = "set client_min_messages = error;\n\
    create schema s7;\n\
    create text search parser s7.prs (start = prsd_start, gettoken = prsd_nexttoken,\n\
      end = prsd_end, lextypes = prsd_lextype, headline = prsd_headline);\n\
    comment on text search parser s7.prs is 'a parser';\n\
    create text search template s7.tmpl (init = dsimple_init, lexize = dsimple_lexize);\n\
    comment on text search template s7.tmpl is 'a template';\n\
    create text search dictionary s7.dict (template = s7.tmpl, accept = false);\n\
    comment on text search dictionary s7.dict is 'a dictionary';\n\
    create text search configuration s7.cfg (parser = s7.prs);\n\
    alter text search configuration s7.cfg add mapping for asciiword, word with s7.dict, simple;\n\
    alter text search configuration s7.cfg add mapping for int with simple;\n\
    comment on text search configuration s7.cfg is 'a configuration';\n\
    create text search configuration public.cfg2 (copy = pg_catalog.english);\n\
    create foreign data wrapper s7_fdw options (\"test wrapper\" 'true', b 'it''s');\n\
    grant usage on foreign data wrapper s7_fdw to public;\n\
    comment on foreign data wrapper s7_fdw is 'a wrapper';\n\
    create foreign data wrapper s7_fdw2 validator postgresql_fdw_validator;\n\
    create server s7_srv type 'kind' version '1.0' foreign data wrapper s7_fdw\n\
      options (host 'h', \"Odd Name\" 'x');\n\
    grant usage on foreign server s7_srv to public;\n\
    comment on server s7_srv is 'a server';\n\
    create server s7_srv2 foreign data wrapper s7_fdw2;\n\
    create user mapping for regress server s7_srv options (user 'u', password 'p');\n\
    create user mapping for public server s7_srv2;\n\
    create foreign table s7.ft (a int) server s7_srv options (tbl 't');\n\
    comment on foreign table s7.ft is 'a foreign table';\n\
    create foreign table public.ft2 (a int) server s7_srv2;\n";

/// Each listing with and without a pattern, with `+` and `x`, and with
/// nothing found, under `ECHO_HIDDEN` so every catalog query is compared
/// too (the `+` describers' per-object queries included); a `+` describer
/// that finds nothing, loud and quiet; then too many dots, and a database
/// part, for each.
const TEXT_SEARCH_AND_SQL_MED_COMMANDS: &str = "\\set QUIET off\n\
    \\set ECHO_HIDDEN on\n\
    \\dF\n\
    \\dF s7.*\n\
    \\dF+ s7.cfg\n\
    \\dF+ cfg2\n\
    \\dF+ *.simple\n\
    \\dFx s7.cfg\n\
    \\dF+x s7.cfg\n\
    \\dF nonesuch\n\
    \\dF+ nonesuch\n\
    \\dFp\n\
    \\dFp+\n\
    \\dFp s7.*\n\
    \\dFp+ s7.prs\n\
    \\dFpx s7.prs\n\
    \\dFp nonesuch\n\
    \\dFp+ nonesuch\n\
    \\dFd\n\
    \\dFd+\n\
    \\dFd s7.*\n\
    \\dFd+ s7.dict\n\
    \\dFdx+ s7.dict\n\
    \\dFd nonesuch\n\
    \\dFt\n\
    \\dFt+\n\
    \\dFt+ s7.*\n\
    \\dFtx s7.tmpl\n\
    \\dFt nonesuch\n\
    \\des\n\
    \\des+\n\
    \\des s7_srv\n\
    \\desx+ s7_srv\n\
    \\des nonesuch\n\
    \\deu\n\
    \\deu+\n\
    \\deu regress\n\
    \\deu+ public\n\
    \\deux+ s7_srv\n\
    \\deu nonesuch\n\
    \\dew\n\
    \\dew+\n\
    \\dew s7_fdw2\n\
    \\dewx+ s7_fdw\n\
    \\dew nonesuch\n\
    \\det\n\
    \\det+\n\
    \\det s7.*\n\
    \\detx+ ft2\n\
    \\det nonesuch\n\
    \\set ECHO_HIDDEN off\n\
    \\set QUIET on\n\
    \\dF+ nonesuch\n\
    \\dFp+ nonesuch\n\
    \\set QUIET off\n\
    \\dF a.b.c.d\n\
    \\dF+ a.b.c.d\n\
    \\dFp a.b.c.d\n\
    \\dFp+ a.b.c.d\n\
    \\dFd a.b.c.d\n\
    \\dFt a.b.c.d\n\
    \\des a.b\n\
    \\deu a.b\n\
    \\dew a.b\n\
    \\det a.b.c.d\n\
    \\dF+ postgres.s7.cfg\n\
    \\dFp+ other.s7.prs\n\
    \\det postgres.s7.ft\n";

/// `\dF+`'s two-line title and footerless table, and `\dFp+`'s footerless
/// methods then footed token types, normal and expanded, in every format at
/// borders 0 and 2, with `tuples_only`, and with `\pset footer off`, which
/// `\dFp+`'s token types override. `wrapped` needs a target width this port
/// does not take from the terminal, hence `\pset columns`.
fn text_search_describers_in_every_format() -> String {
    let mut script = String::from("\\pset columns 60\n");
    for format in [
        "aligned",
        "wrapped",
        "unaligned",
        "csv",
        "html",
        "asciidoc",
        "latex",
        "latex-longtable",
        "troff-ms",
    ] {
        let _ = writeln!(script, "\\pset format {format}");
        for settings in [
            "\\pset border 0\n",
            "\\pset border 2\n",
            "\\pset tuples_only on\n",
            "\\pset tuples_only off\n\\pset footer off\n",
        ] {
            script.push_str(settings);
            script.push_str("\\dF+ s7.cfg\n\\dF+x s7.cfg\n\\dFp+ s7.prs\n\\dFp+x s7.prs\n");
        }
        script.push_str("\\pset footer on\n\\pset border 1\n");
    }
    script
}

/// The text search and SQL/MED families, `NAT-401`'s last listing group:
/// `\dF`, `\dFp`, `\dFd`, `\dFt` with the `+` describers of `\dF` and `\dFp`,
/// and `\des`, `\deu`, `\dew`, `\det`. `psql.sql` names them only in the
/// invalid-name sections, which [`the_invalid_multipart_name_sections_run_live`]
/// gates; here they run against C psql over
/// [`TEXT_SEARCH_AND_SQL_MED_SETUP`]: [`TEXT_SEARCH_AND_SQL_MED_COMMANDS`],
/// then [`text_search_describers_in_every_format`].
#[test]
fn the_text_search_and_sql_med_listings_match_c_psql() {
    let Some(cluster) = Cluster::start(TEXT_SEARCH_AND_SQL_MED_PORT) else {
        return;
    };
    let out = cluster.run_script(Path::new(RPSQL), TEXT_SEARCH_AND_SQL_MED_SETUP);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");
    let script = format!(
        "{TEXT_SEARCH_AND_SQL_MED_COMMANDS}{}",
        text_search_describers_in_every_format()
    );
    diff_against_c_psql(&cluster, "text search and SQL/MED listings", &script);
}

/// One relation of every kind `describeOneTableDetails` tells apart, in
/// schema `s8`, with at least one of each thing a footer reports: indexes of
/// every label (primary key, unique, unique constraint, exclusion, `WITHOUT
/// OVERLAPS`, deferrable, clustered, replica identity, nulls not distinct,
/// partial, in a tablespace), check, foreign-key and not-null constraints
/// (inherited, `NO INHERIT`, `NOT VALID`), references from a plain and a
/// partitioned table, policies under every row-security setting, statistics
/// objects with some, all and no kinds and a target, rules and triggers in
/// every firing mode (an inherited and a disabled internal trigger too),
/// publications (all tables, a schema, a column list with a row filter),
/// inheritance, partitions (a default, a partitioned and a foreign one, and
/// a partitioned table with none), a typed table, each replica identity,
/// options, generated, identity and collated columns, column comments,
/// storage, compression and statistics targets, and sequences owned by a
/// column, by an identity column and by nothing. The tablespace is an
/// in-place one (`allow_in_place_tablespaces`), so no directory is needed.
const TABLE_DETAILS_SETUP: &str = "set client_min_messages = error;\n\
    create role regress_s8_role;\n\
    set allow_in_place_tablespaces = on;\n\
    create tablespace regress_s8_spc location '';\n\
    create schema s8;\n\
    create schema s8pub;\n\
    create function s8.trgf() returns trigger language plpgsql as $$begin return new; end$$;\n\
    create table s8.t (a int primary key, b text collate \"C\" not null default 'x',\n\
      c int unique deferrable initially deferred, d int check (d > 0),\n\
      e int generated always as (a * 2) stored, f int generated always as (a + 1) virtual,\n\
      g int generated always as identity, h numeric(10,2) default 1.5,\n\
      constraint t_d_c unique (d), constraint t_h_chk check (h < 100) no inherit)\n\
      with (fillfactor = 70, toast.autovacuum_enabled = false);\n\
    comment on column s8.t.a is 'the key';\n\
    alter table s8.t alter column b set statistics 50;\n\
    alter table s8.t alter column b set storage external;\n\
    alter table s8.t alter column b set compression pglz;\n\
    create unique index t_nnd on s8.t (h) nulls not distinct;\n\
    create index t_partial on s8.t (d) where d > 10;\n\
    create index t_expr on s8.t ((a + d));\n\
    alter index s8.t_expr alter column 1 set statistics 10;\n\
    cluster s8.t using t_pkey;\n\
    create table s8.ref (x int references s8.t, y int, z int,\n\
      constraint ref_yz foreign key (y) references s8.t (d) on delete cascade);\n\
    alter table s8.ref disable trigger all;\n\
    create table s8.ex (r int4range, valid daterange, exclude using gist (r with &&),\n\
      constraint ex_pk primary key (r, valid without overlaps));\n\
    create table s8.ri (a int not null, b int);\n\
    create unique index ri_a on s8.ri (a) tablespace regress_s8_spc;\n\
    alter table s8.ri replica identity using index ri_a;\n\
    create table s8.rif (a int) tablespace regress_s8_spc;\n\
    alter table s8.rif replica identity full;\n\
    create table s8.rin (a int);\n\
    alter table s8.rin replica identity nothing;\n\
    create table s8.nn (a int, b int not null no inherit, c int);\n\
    alter table s8.nn add constraint nn_c not null c not valid;\n\
    create table s8.parent1 (p int not null, q int check (q > 0));\n\
    create table s8.parent2 (r int);\n\
    create table s8.child (s int) inherits (s8.parent1, s8.parent2);\n\
    create table s8.child2 () inherits (s8.parent1);\n\
    create table s8.pol (a int, owner text);\n\
    alter table s8.pol enable row level security;\n\
    create policy p_all on s8.pol using (owner = current_user);\n\
    create policy p_sel on s8.pol as restrictive for select to regress_s8_role\n\
      using (a > 0);\n\
    create policy p_ins on s8.pol for insert with check (a < 100);\n\
    create table s8.polf (a int);\n\
    alter table s8.polf enable row level security;\n\
    alter table s8.polf force row level security;\n\
    create table s8.polfp (a int);\n\
    alter table s8.polfp enable row level security;\n\
    alter table s8.polfp force row level security;\n\
    create policy p_upd on s8.polfp for update using (true);\n\
    create table s8.pole (a int);\n\
    alter table s8.pole enable row level security;\n\
    create table s8.pold (a int);\n\
    create policy p_del on s8.pold for delete using (a = 1);\n\
    create table s8.st (a int, b int, c int);\n\
    create statistics s8.st_some (ndistinct, mcv) on a, b from s8.st;\n\
    create statistics s8.st_all on a, c from s8.st;\n\
    create statistics s8.st_expr on (a + b) from s8.st;\n\
    alter statistics s8.st_all set statistics 100;\n\
    create table s8.rl (a int);\n\
    create rule rl_on as on insert to s8.rl do also notify rl;\n\
    create rule rl_off as on update to s8.rl do instead nothing;\n\
    create rule rl_always as on delete to s8.rl do also notify rl_d;\n\
    create table s8.rl2 (a int);\n\
    create rule rl_replica as on insert to s8.rl2 do also notify rl2;\n\
    alter table s8.rl disable rule rl_off;\n\
    alter table s8.rl enable always rule rl_always;\n\
    alter table s8.rl2 enable replica rule rl_replica;\n\
    create trigger tg_on before insert on s8.rl for each row execute function s8.trgf();\n\
    create trigger tg_off after update on s8.rl for each row execute function s8.trgf();\n\
    create trigger tg_always after delete on s8.rl for each statement execute function s8.trgf();\n\
    create trigger tg_replica before insert on s8.rl2 for each row execute function s8.trgf();\n\
    alter table s8.rl disable trigger tg_off;\n\
    alter table s8.rl enable always trigger tg_always;\n\
    alter table s8.rl2 enable replica trigger tg_replica;\n\
    create table s8.pt (a int, b text, primary key (a)) partition by range (a);\n\
    create table s8.pt1 partition of s8.pt for values from (0) to (10);\n\
    create table s8.pt2 partition of s8.pt for values from (10) to (20) partition by list (a);\n\
    create table s8.pt2a partition of s8.pt2 for values in (10, 11);\n\
    create table s8.ptd partition of s8.pt default;\n\
    create table s8.ptref (a int references s8.pt) partition by hash (a);\n\
    create table s8.ptref0 partition of s8.ptref for values with (modulus 2, remainder 0);\n\
    create trigger tg_pt after insert on s8.pt for each row execute function s8.trgf();\n\
    create table s8.empty_pt (a int) partition by list (a);\n\
    create foreign data wrapper s8_fdw;\n\
    create server s8_srv foreign data wrapper s8_fdw;\n\
    create foreign table s8.ft (a int options (column_name 'x', \"odd\" 'it''s') not null\n\
      default 1, b text) server s8_srv options (schema_name 's', table_name 't');\n\
    comment on column s8.ft.b is 'a foreign column';\n\
    create table s8.pt3 (a int) partition by list (a);\n\
    create foreign table s8.ptf partition of s8.pt3 for values in (1) server s8_srv;\n\
    create foreign table s8.ft2 (a int) server s8_srv;\n\
    create type s8.ct as (a int, b text collate \"C\");\n\
    comment on column s8.ct.a is 'a field';\n\
    create table s8.typed of s8.ct (a primary key);\n\
    create unlogged table s8.ul (a serial);\n\
    create unlogged sequence s8.useq;\n\
    create sequence s8.oseq owned by s8.rin.a;\n\
    create view s8.v as select a, b from s8.t where a > 0;\n\
    comment on column s8.v.a is 'a view column';\n\
    create rule v_ins as on insert to s8.v do instead insert into s8.t (a, b) values (new.a, new.b);\n\
    create trigger tg_v instead of update on s8.v for each row execute function s8.trgf();\n\
    create materialized view s8.mv as select a from s8.t;\n\
    create index mv_a on s8.mv (a);\n\
    create publication s8_all for all tables;\n\
    create publication s8_cols for table s8.t (a, b) where (a > 5);\n\
    create publication s8_schema for tables in schema s8pub;\n\
    create table s8pub.pubt (a int);\n";

/// Each relation of [`TABLE_DETAILS_SETUP`] with and without `+`, under
/// `ECHO_HIDDEN` so every one of `describeOneTableDetails`'s queries is
/// compared too; `HIDE_TABLEAM` and `HIDE_TOAST_COMPRESSION` both ways; a
/// pattern naming several relations, one naming a system relation with and
/// without `S`, a TOAST table found through its owner; `x`, which the
/// description ignores except for a sequence; nothing found, loud and
/// quiet; too many dots, and a database part; and `noexec`.
const TABLE_DETAILS_COMMANDS: &str = "\\set QUIET off\n\
    \\set ECHO_HIDDEN on\n\
    \\d s8.t\n\
    \\d+ s8.t\n\
    \\set HIDE_TOAST_COMPRESSION off\n\
    \\set HIDE_TABLEAM off\n\
    \\d+ s8.t\n\
    \\d+ s8.mv\n\
    \\d+ s8.pt\n\
    \\set HIDE_TOAST_COMPRESSION on\n\
    \\set HIDE_TABLEAM on\n\
    \\d s8.t_pkey\n\
    \\d+ s8.t_nnd\n\
    \\d s8.t_partial\n\
    \\d+ s8.t_expr\n\
    \\d s8.t_c_key\n\
    \\d s8.ref\n\
    \\d+ s8.ex\n\
    \\d s8.ex_pk\n\
    \\d s8.ri\n\
    \\d s8.ri_a\n\
    \\d+ s8.ri\n\
    \\d+ s8.rif\n\
    \\d+ s8.rin\n\
    \\d+ s8.nn\n\
    \\d s8.parent1\n\
    \\d+ s8.parent1\n\
    \\d+ s8.child\n\
    \\d s8.pol\n\
    \\d s8.polf\n\
    \\d s8.polfp\n\
    \\d s8.pole\n\
    \\d s8.pold\n\
    \\d s8.st\n\
    \\d s8.rl\n\
    \\d s8.rl2\n\
    \\d s8.pt\n\
    \\d+ s8.pt1\n\
    \\d s8.pt2\n\
    \\d+ s8.pt2\n\
    \\d+ s8.ptd\n\
    \\d s8.pt_pkey\n\
    \\d+ s8.pt_pkey\n\
    \\d s8.ptref\n\
    \\d s8.ptref0\n\
    \\d+ s8.empty_pt\n\
    \\d s8.ft\n\
    \\d+ s8.ft\n\
    \\d+ s8.ptf\n\
    \\d+ s8.pt3\n\
    \\d s8.ct\n\
    \\d+ s8.ct\n\
    \\d+ s8.typed\n\
    \\d s8.ul\n\
    \\d s8.ul_a_seq\n\
    \\d s8.useq\n\
    \\d s8.oseq\n\
    \\d s8.t_g_seq\n\
    \\dSx s8.t_g_seq\n\
    \\d s8.v\n\
    \\d+ s8.v\n\
    \\d s8.mv\n\
    \\d+ s8pub.pubt\n\
    \\d s8.p*\n\
    \\d pg_am\n\
    \\dS+ pg_am\n\
    \\d+ pg_catalog.pg_am_oid_index\n\
    \\d nonesuch\n\
    \\d s8.nonesuch\n\
    \\set ECHO_HIDDEN noexec\n\
    \\d s8.t\n\
    \\set ECHO_HIDDEN off\n\
    \\d a.b.c.d\n\
    \\d postgres.s8.t\n\
    \\d other.s8.t\n\
    \\set QUIET on\n\
    \\d nonesuch\n\
    \\set QUIET off\n";

/// A TOAST table with `+`, and its index. A TOAST table's name has its
/// owner's oid in it, and `\gset` is not ported yet, so these are
/// `pg_statistic`'s, whose name `psql.sql:1329` relies on too.
const TABLE_DETAILS_TOAST: &str = "\\d+ pg_toast.pg_toast_2619\n\\d pg_toast.pg_toast_2619_index\n";

/// `\d s8.t`, `\d s8.pt`, `\d s8.useq` and `\d s8.t_pkey` (a table, a
/// partitioned table, a sequence, an index), normal and with `x`, in every
/// format at borders 0 and 2, with `tuples_only`, and with `\pset footer
/// off`. A sequence alone is printed expanded by `x`. `wrapped` needs a
/// target width this port does not take from the terminal, hence
/// `\pset columns`.
fn table_details_in_every_format() -> String {
    let mut script = String::from("\\pset columns 60\n");
    for format in [
        "aligned",
        "wrapped",
        "unaligned",
        "csv",
        "html",
        "asciidoc",
        "latex",
        "latex-longtable",
        "troff-ms",
    ] {
        let _ = writeln!(script, "\\pset format {format}");
        for settings in [
            "\\pset border 0\n",
            "\\pset border 2\n",
            "\\pset tuples_only on\n",
            "\\pset tuples_only off\n\\pset footer off\n",
        ] {
            script.push_str(settings);
            script.push_str(
                "\\d s8.t\n\\d+ s8.pt\n\\d s8.useq\n\\d+x s8.useq\n\\d s8.t_pkey\n\\dSx s8.t_pkey\n",
            );
        }
        script.push_str("\\pset footer on\n\\pset border 1\n");
    }
    script
}

/// `describeTableDetails` and `describeOneTableDetails` beyond what
/// `psql.sql` exercises (a sequence, a table's columns and access method, a
/// TOAST table), against C psql, over [`TABLE_DETAILS_SETUP`]:
/// [`TABLE_DETAILS_COMMANDS`], [`TABLE_DETAILS_TOAST`], then
/// [`table_details_in_every_format`].
///
/// An index left invalid by a failed `create unique index concurrently` is
/// made separately, since its setup must fail.
#[test]
fn the_table_details_match_c_psql() {
    let Some(cluster) = Cluster::start(TABLE_DETAILS_PORT) else {
        return;
    };
    let out = cluster.run_script(Path::new(RPSQL), TABLE_DETAILS_SETUP);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("ERROR"), "setup:\n{text}");
    cluster.run_script(
        Path::new(RPSQL),
        "create table s8.inv (a int);\n\
         insert into s8.inv values (1), (1);\n\
         create unique index concurrently inv_a on s8.inv (a);\n",
    );
    let script = format!(
        "{TABLE_DETAILS_COMMANDS}\\d s8.inv\n\\d s8.inv_a\n{TABLE_DETAILS_TOAST}{}",
        table_details_in_every_format()
    );
    let ours = diff_against_c_psql(&cluster, "table details", &script);
    // Every footer the setup is there to reach, so that a setup statement
    // that stops making one cannot narrow the gate unnoticed.
    let text = String::from_utf8_lossy(&ours);
    for footer in TABLE_DETAILS_FOOTERS {
        assert!(text.contains(footer), "no {footer:?} in:\n{text}");
    }
}

/// What [`the_table_details_match_c_psql`] must print at least once.
const TABLE_DETAILS_FOOTERS: [&str; 65] = [
    "\n    \"t_pkey\" PRIMARY KEY, btree (a) CLUSTER\n",
    "UNIQUE CONSTRAINT, btree (c) DEFERRABLE INITIALLY DEFERRED\n",
    "\"t_nnd\" UNIQUE, btree (h) NULLS NOT DISTINCT\n",
    "\"ex_pk\" PRIMARY KEY (r, valid WITHOUT OVERLAPS)\n",
    "\"ex_r_excl\" EXCLUDE USING gist (r WITH &&)\n",
    "REPLICA IDENTITY, tablespace \"regress_s8_spc\"\n",
    "\"inv_a\" UNIQUE, btree (a) INVALID\n",
    "primary key, btree, for table \"s8.t\", clustered\n",
    "unique nulls not distinct, btree, for table \"s8.t\"\n",
    "btree, for table \"s8.t\", predicate (d > 10)\n",
    "deferrable, initially deferred\n",
    "for table \"s8.ri\", replica identity\n",
    "for table \"s8.inv\", invalid\n",
    "Tablespace: \"regress_s8_spc\"\n",
    "Check constraints:\n",
    "CHECK (h < 100::numeric) NO INHERIT\n",
    "Foreign-key constraints:\n",
    "Referenced by:\n",
    "    TABLE \"s8.ptref\" CONSTRAINT \"ptref_a_fkey\" FOREIGN KEY (a) REFERENCES s8.pt(a)\n",
    "Policies:\n",
    "AS RESTRICTIVE FOR SELECT\n      TO regress_s8_role\n      USING ((a > 0))\n",
    "Policies (forced row security enabled):\n",
    "Policies (row security enabled): (none)\n",
    "Policies (forced row security enabled): (none)\n",
    "Policies (row security disabled):\n",
    "Statistics objects:\n",
    "\"s8.st_some\" (ndistinct, mcv) ON a, b FROM s8.st\n",
    "\"s8.st_all\" ON a, c FROM s8.st; STATISTICS 100\n",
    "Rules:\n",
    "Disabled rules:\n",
    "Rules firing always:\n",
    "Rules firing on replica only:\n",
    "Triggers:\n",
    "Disabled user triggers:\n",
    "Disabled internal triggers:\n",
    "Triggers firing always:\n",
    "Triggers firing on replica only:\n",
    ", ON TABLE s8.pt\n",
    "Publications:\n    \"s8_all\"\n    \"s8_cols\" (a, b) WHERE (a > 5)\n",
    "    \"s8_schema\"\n",
    "Not-null constraints:\n",
    "NOT VALID\n",
    "\" NO INHERIT\n",
    "(inherited)\n",
    "View definition:\n",
    "Inherits: s8.parent1,\n          s8.parent2\n",
    "Child tables: s8.child,\n",
    "Partitions: s8.pt1 FOR VALUES FROM (0) TO (10),\n",
    ", PARTITIONED,\n",
    ", FOREIGN\n",
    "Number of partitions: 0\n",
    "Replica Identity: ???\n",
    "Replica Identity: FULL\n",
    "Typed table of type: s8.ct\n",
    "Access method: heap\n",
    "Options: fillfactor=70, toast.autovacuum_enabled=false\n",
    "| pglz ",
    "Server: s8_srv\nFDW options: (schema_name 's', table_name 't')\n",
    "Partition key: RANGE (a)\n",
    "Partition constraint: ",
    "Owning table: \"pg_catalog.pg_statistic\"\n",
    "Unlogged sequence \"s8.useq\"",
    "Owned by: s8.rin.a\n",
    "Sequence for identity column: s8.t.g\n",
    "-[ RECORD 1 ]",
];

/// A script `psql.out` has no expected output for: rpsql must render all of
/// it, and print what C psql prints when this lane has C psql.
///
/// "Rendered all of it" means no refusal of this port's, each of which says
/// `is not implemented yet`, and no server `ERROR`. Upstream's own "…are not
/// implemented: <pattern>" (`describe.c:6380`) is output to diff, not a
/// refusal.
///
/// Returns rpsql's output, for a caller that checks what it covered.
fn diff_against_c_psql(cluster: &Cluster, what: &str, script: &str) -> Vec<u8> {
    let ours = cluster.run_script(Path::new(RPSQL), script);
    let text = String::from_utf8_lossy(&ours);
    assert!(
        !text.contains("not implemented yet") && !text.contains("ERROR"),
        "rpsql ({what}):\n{text}"
    );
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = cluster.run_script(&psql, script);
            if let Some(diff) = first_difference(&theirs, &ours) {
                panic!("rpsql vs C psql ({what}): {diff}");
            }
        }
        None => reference::skip("psql"),
    }
    ours
}
