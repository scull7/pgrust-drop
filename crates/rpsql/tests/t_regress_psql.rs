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
    tail, without,
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
/// leaves (wrapped, old-ascii, `\pset columns 20`).
///
/// The last section also prints `\d psql_serial_tab_id_seq` six times. That
/// is `describeTableDetails`, a later NAT-401 slice, and nothing else reads
/// what it prints, so those lines are cut from both files ([`without`]).
#[test]
fn the_output_format_sections_run_live() {
    let Some(cluster) = Cluster::start(OUTPUT_FORMAT_SECTIONS_PORT) else {
        return;
    };
    let run = sections(
        "-- test multi-line headers, wrapping, and newline indicators",
        "-- test header/footer/tuples_only behavior in aligned/unaligned/wrapped cases",
    );
    let (sql, expected) = without(&run, &["\\d psql_serial_tab_id_seq"]);
    gate_text(
        &cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({} …, without \\d <sequence>)",
            run.sql_line, run.out_line, run.header
        ),
        &sql,
        &expected,
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
/// (`psql.sql:879`), all against one cluster: each prints `\df exp` under
/// `tuples_only`, normal and expanded, in its format, then `q`.
///
/// Each section also prints `\d psql_serial_tab_id_seq` twice. That is
/// `describeTableDetails`, a later NAT-401 slice, and nothing else reads what
/// it prints, so those lines are cut from both files ([`without`]).
#[test]
fn the_document_format_sections_run_live() {
    let Some(cluster) = Cluster::start(DOCUMENT_FORMAT_SECTIONS_PORT) else {
        return;
    };
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
        let (sql, expected) = without(&whole, &["\\d psql_serial_tab_id_seq"]);
        gate_text(
            &cluster,
            &format!(
                "psql.sql:{} vs psql.out:{} ({}, without \\d <sequence>)",
                whole.sql_line, whole.out_line, whole.header
            ),
            &sql,
            &expected,
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
/// tables, a view and a materialized view in two table access methods.
///
/// The section also describes three tables one by one (`\d+ tbl_heap_psql`,
/// `\d+ tbl_heap` twice each, and `\d+x tbl_heap`). That is
/// `describeTableDetails`, a later NAT-401 slice, and nothing else in the
/// section reads what it prints, so those five lines and their tables are
/// cut from both files ([`without`]).
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
    let (sql, expected) = without(
        &whole,
        &["\\d+ tbl_heap_psql", "\\d+ tbl_heap", "\\d+x tbl_heap"],
    );
    let preamble = "\\pset format wrapped\n\\pset columns 40\n";
    gate_text(
        &cluster,
        &format!(
            "psql.sql:{} vs psql.out:{} ({}, without \\d <table>)",
            whole.sql_line, whole.out_line, whole.header
        ),
        &format!("{preamble}{sql}"),
        &format!("{preamble}{expected}"),
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

/// A script `psql.out` has no expected output for: rpsql must render all of
/// it, and print what C psql prints when this lane has C psql.
///
/// "Rendered all of it" means no refusal of this port's, each of which says
/// `is not implemented yet`, and no server `ERROR`. Upstream's own "…are not
/// implemented: <pattern>" (`describe.c:6380`) is output to diff, not a
/// refusal.
fn diff_against_c_psql(cluster: &Cluster, what: &str, script: &str) {
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
}
