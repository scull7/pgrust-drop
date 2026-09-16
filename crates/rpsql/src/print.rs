//! Result rendering: the aligned and unaligned paths of
//! `src/fe_utils/print.c`.
//!
//! Scope. NAT-400 owns the full format matrix (wrapped, csv, html, latex,
//! troff, asciidoc, expanded). What this issue needs is the default one —
//! `PRINT_ALIGNED` with `border = 1`, the shape `psql -c 'select 1'` prints —
//! plus `PRINT_UNALIGNED`, because `-A` is in the option table this issue
//! ports and rendering it wrongly would be worse than refusing it. Every other
//! format is [`PrintError::Unsupported`], which the caller reports rather than
//! printing something that only looks right.
//!
//! The whole module is a pure function from a result to bytes; nothing here
//! opens a file or a pager.

use rlibpq::QueryResult;

use crate::settings::{PrintFormat, PrintQueryOpt};

#[cfg(test)]
use crate::settings::{Separator, TableOpt};

/// A format this issue does not render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintError {
    /// The format belongs to a later issue.
    Unsupported(PrintFormat),
}

impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(format) => write!(
                f,
                "output format {format:?} is not implemented yet (Linear NAT-400)"
            ),
        }
    }
}

impl std::error::Error for PrintError {}

/// Column alignment: `column_type_alignment()` (`print.c:3479`).
///
/// The numeric-ish types right-align; everything else left-aligns.
#[must_use]
pub fn column_type_alignment(typid: u32) -> Align {
    // The OIDs are `pg_type.dat`'s: int2, int4, int8, float4, float8, numeric,
    // oid, xid, xid8, cid, money.
    const RIGHT: [u32; 11] = [21, 23, 20, 700, 701, 1700, 26, 28, 5069, 29, 790];
    if RIGHT.contains(&typid) {
        Align::Right
    } else {
        Align::Left
    }
}

/// `cont->aligns[]`'s `'l'` and `'r'` (`print.h:151`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// `'l'`
    Left,
    /// `'r'`
    Right,
}

/// `printQuery()` (`print.c:3529`): render one result.
///
/// # Errors
/// [`PrintError::Unsupported`] for a format NAT-400 still owns.
pub fn print_query(result: &QueryResult, opt: &PrintQueryOpt) -> Result<Vec<u8>, PrintError> {
    let headers: Vec<Vec<u8>> = (0..result.nfields())
        .map(|i| result.fname(i).unwrap_or(b"").to_vec())
        .collect();
    let aligns: Vec<Align> = result
        .fields()
        .iter()
        .map(|f| column_type_alignment(f.typid))
        .collect();
    let null_print = opt.null_print.as_deref().unwrap_or("");
    let cells: Vec<Vec<Vec<u8>>> = (0..result.ntuples())
        .map(|r| {
            (0..result.nfields())
                .map(|c| {
                    if result.is_null(r, c) {
                        null_print.as_bytes().to_vec()
                    } else {
                        result.value(r, c).unwrap_or(b"").to_vec()
                    }
                })
                .collect()
        })
        .collect();

    match opt.topt.format {
        PrintFormat::Aligned => Ok(print_aligned_text(&headers, &aligns, &cells, opt)),
        PrintFormat::Unaligned => Ok(print_unaligned_text(&headers, &cells, opt)),
        other => Err(PrintError::Unsupported(other)),
    }
}

/// Display width of one *line* in characters.
///
/// `pg_wcssize()` (`mbprint.c:339`) measures display columns, counting a
/// double-width character as two. This port measures UTF-8 characters, which
/// agrees for every ASCII result — the corpus this issue gates — and is a
/// recorded divergence for the wide-character case that NAT-400 must close.
///
/// Callers pass a single line: a cell holding newlines is split first, because
/// `pg_wcssize` reports the width of the *widest* line, not of the whole
/// string.
fn line_width(line: &[u8]) -> usize {
    match std::str::from_utf8(line) {
        Ok(text) => text.chars().count(),
        // A non-UTF-8 cell is measured in bytes, as an unsafe encoding is
        // measured upstream (`mbprint.c:349`).
        Err(_) => line.len(),
    }
}

/// `pg_wcsformat()` (`mbprint.c:398`): a cell becomes one entry per embedded
/// newline, which is what `print_aligned_text` walks with `curr_nl_line`.
fn cell_lines(cell: &[u8]) -> Vec<&[u8]> {
    cell.split(|&c| c == b'\n').collect()
}

/// The width `pg_wcssize` reports for a whole cell: its widest line.
fn cell_width(cell: &[u8]) -> usize {
    cell_lines(cell)
        .into_iter()
        .map(line_width)
        .max()
        .unwrap_or(0)
}

/// `print_aligned_text()` (`print.c:669`) for `border = 1` and no wrapping.
///
/// Wrapping (`PRINT_WRAPPED`) is NAT-400's, so the only reason a cell spans
/// more than one output line here is an embedded newline; the continuation
/// marks are `pg_asciiformat`'s `nl_left` `" "` and `nl_right` `"+"`
/// (`print.c:56`), and the header's are `header_nl_left`/`header_nl_right`,
/// which for ascii are the same two strings.
fn print_aligned_text(
    headers: &[Vec<u8>],
    aligns: &[Align],
    cells: &[Vec<Vec<u8>>],
    opt: &PrintQueryOpt,
) -> Vec<u8> {
    let col_count = headers.len();
    let mut out = Vec::new();

    // Widest of the header and every cell, per column (`print.c:742`-`:779`).
    let mut max_width: Vec<usize> = headers.iter().map(|h| cell_width(h)).collect();
    for row in cells {
        for (i, cell) in row.iter().enumerate() {
            max_width[i] = max_width[i].max(cell_width(cell));
        }
    }

    if opt.topt.start_table && !opt.topt.tuples_only {
        if let Some(title) = &opt.title {
            out.extend_from_slice(title.as_bytes());
            out.push(b'\n');
        }

        // Headers, centered, one output line per embedded newline
        // (`print.c:867`).
        let header_lines: Vec<Vec<&[u8]>> = headers.iter().map(|h| cell_lines(h)).collect();
        let header_height = header_lines.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..header_height {
            for (i, lines) in header_lines.iter().enumerate() {
                out.push(b' ');
                match lines.get(k) {
                    Some(line) => {
                        let nbspace = max_width[i] - line_width(line);
                        out.extend(std::iter::repeat_n(b' ', nbspace / 2));
                        out.extend_from_slice(line);
                        out.extend(std::iter::repeat_n(b' ', nbspace.div_ceil(2)));
                    }
                    // A header that has run out of lines is blank-padded
                    // (`print.c:369`).
                    None => out.extend(std::iter::repeat_n(b' ', max_width[i])),
                }
                out.push(if lines.len() > k + 1 { b'+' } else { b' ' });
                if i < col_count - 1 {
                    out.push(b'|');
                }
            }
            out.push(b'\n');
        }

        // `_print_horizontal_line(PRINT_RULE_MIDDLE)` (`print.c:610`).
        for (i, width) in max_width.iter().enumerate() {
            out.extend(std::iter::repeat_n(b'-', width + 2));
            if i < col_count - 1 {
                out.push(b'+');
            }
        }
        out.push(b'\n');
    }

    // Cells, one output line per embedded newline (`print.c:890`).
    for row in cells {
        let row_lines: Vec<Vec<&[u8]>> = row.iter().map(|cell| cell_lines(cell)).collect();
        let row_height = row_lines.iter().map(Vec::len).max().unwrap_or(1);
        for k in 0..row_height {
            for (j, lines) in row_lines.iter().enumerate() {
                let finalspaces = j < col_count - 1;
                // The left mark: `nl_left` is `" "` for ascii, so this is a
                // space whether or not a newline is being continued.
                out.push(b' ');
                // `wrap[j]` for the line just emitted: is there another one?
                let continues = lines.len() > k + 1;
                match lines.get(k) {
                    Some(line) => {
                        let pad = max_width[j] - line_width(line);
                        match aligns[j] {
                            Align::Right => {
                                out.extend(std::iter::repeat_n(b' ', pad));
                                out.extend_from_slice(line);
                            }
                            Align::Left => {
                                out.extend_from_slice(line);
                                // Left-aligned cells pad when the column is
                                // not the last *or* when a mark follows
                                // (`print.c:1147`).
                                if finalspaces || continues {
                                    out.extend(std::iter::repeat_n(b' ', pad));
                                }
                            }
                        }
                    }
                    // Past this column's last line: pad for the others
                    // (`print.c:1085`).
                    None => {
                        if finalspaces {
                            out.extend(std::iter::repeat_n(b' ', max_width[j]));
                        }
                    }
                }
                if continues {
                    out.push(b'+');
                } else if finalspaces {
                    out.push(b' ');
                }
                if finalspaces {
                    out.push(b'|');
                }
            }
            out.push(b'\n');
        }
    }

    if opt.topt.stop_table {
        // `footers_with_default()` (`print.c:389`).
        if opt.topt.default_footer && !opt.topt.tuples_only {
            let n = cells.len();
            let noun = if n == 1 { "row" } else { "rows" };
            out.extend_from_slice(format!("({n} {noun})\n").as_bytes());
        }
        out.push(b'\n');
    }

    out
}

/// `print_unaligned_text()` (`print.c:180`).
fn print_unaligned_text(
    headers: &[Vec<u8>],
    cells: &[Vec<Vec<u8>>],
    opt: &PrintQueryOpt,
) -> Vec<u8> {
    let fieldsep = opt.topt.field_sep.bytes();
    let recordsep = opt.topt.record_sep.bytes();
    let mut out = Vec::new();
    let mut need_recordsep = false;

    if opt.topt.start_table {
        if let Some(title) = &opt.title
            && !opt.topt.tuples_only
        {
            out.extend_from_slice(title.as_bytes());
            out.extend_from_slice(&recordsep);
        }
        if !opt.topt.tuples_only {
            for (i, header) in headers.iter().enumerate() {
                if i > 0 {
                    out.extend_from_slice(&fieldsep);
                }
                out.extend_from_slice(header);
            }
            need_recordsep = true;
        }
    }

    for row in cells {
        if need_recordsep {
            out.extend_from_slice(&recordsep);
        }
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(&fieldsep);
            }
            out.extend_from_slice(cell);
        }
        need_recordsep = true;
    }

    if opt.topt.stop_table {
        if opt.topt.default_footer && !opt.topt.tuples_only {
            let n = cells.len();
            let noun = if n == 1 { "row" } else { "rows" };
            if need_recordsep {
                out.extend_from_slice(&recordsep);
            }
            out.extend_from_slice(format!("({n} {noun})").as_bytes());
        }
        out.extend_from_slice(&recordsep);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rlibpq::{Backend, FieldDescription, QueryRunner, TransactionStatus};

    fn int4_field(name: &str) -> FieldDescription {
        FieldDescription {
            name: name.as_bytes().to_vec(),
            tableid: 0,
            columnid: 0,
            typid: 23,
            typlen: 4,
            atttypmod: -1,
            format: 0,
        }
    }

    fn text_field(name: &str) -> FieldDescription {
        FieldDescription {
            typid: 25,
            ..int4_field(name)
        }
    }

    /// Build a result the way the wire does: rlibpq keeps its setters
    /// crate-private, and feeding the frames through `QueryRunner` is both
    /// available and closer to what psql actually prints.
    fn result(fields: Vec<FieldDescription>, rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        let mut runner = QueryRunner::new();
        let ntuples = rows.len();
        runner.push(Backend::RowDescription(fields)).unwrap();
        for row in rows {
            runner
                .push(Backend::DataRow(
                    row.into_iter()
                        .map(|c| c.map(|v| v.as_bytes().to_vec()))
                        .collect(),
                ))
                .unwrap();
        }
        runner
            .push(Backend::CommandComplete(
                format!("SELECT {ntuples}").into_bytes(),
            ))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        runner.into_results().remove(0)
    }

    fn rendered(res: &QueryResult) -> String {
        let opt = PrintQueryOpt::default();
        String::from_utf8(print_query(res, &opt).unwrap()).unwrap()
    }

    #[test]
    fn select_one_matches_psql() {
        // The Acceptance line of NAT-398: `psql -X -c 'select 1'`.
        let res = result(vec![int4_field("?column?")], vec![vec![Some("1")]]);
        assert_eq!(
            rendered(&res),
            " ?column? \n\
             ----------\n\
             \x20       1\n\
             (1 row)\n\
             \n"
        );
    }

    #[test]
    fn a_wider_cell_widens_the_column() {
        let res = result(
            vec![text_field("t")],
            vec![vec![Some("alpha")], vec![Some("b")]],
        );
        assert_eq!(rendered(&res), "   t   \n-------\n alpha\n b\n(2 rows)\n\n");
    }

    #[test]
    fn two_columns_get_a_divider() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        assert_eq!(rendered(&res), " n | s \n---+---\n 1 | a\n(1 row)\n\n");
    }

    #[test]
    fn a_null_prints_as_the_null_string() {
        let res = result(vec![text_field("t")], vec![vec![None]]);
        let opt = PrintQueryOpt {
            null_print: Some("(null)".to_string()),
            ..PrintQueryOpt::default()
        };
        let out = String::from_utf8(print_query(&res, &opt).unwrap()).unwrap();
        assert_eq!(out, "   t    \n--------\n (null)\n(1 row)\n\n");
    }

    #[test]
    fn no_rows_still_prints_the_header_and_footer() {
        let res = result(vec![int4_field("n")], vec![]);
        assert_eq!(rendered(&res), " n \n---\n(0 rows)\n\n");
    }

    #[test]
    fn tuples_only_drops_the_header_and_the_footer() {
        let res = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        let mut opt = PrintQueryOpt::default();
        opt.topt = TableOpt {
            tuples_only: true,
            ..opt.topt
        };
        let out = String::from_utf8(print_query(&res, &opt).unwrap()).unwrap();
        // The header is still measured for the column width, and
        // `stop_table` still ends the table with a blank line
        // (`print.c:1060`).
        assert_eq!(out, " 1\n\n");
    }

    #[test]
    fn unaligned_uses_the_separators() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let mut opt = PrintQueryOpt::default();
        opt.topt = TableOpt {
            format: PrintFormat::Unaligned,
            field_sep: Separator {
                separator: Some("|".to_string()),
                separator_zero: false,
            },
            record_sep: Separator {
                separator: Some("\n".to_string()),
                separator_zero: false,
            },
            ..opt.topt
        };
        let out = String::from_utf8(print_query(&res, &opt).unwrap()).unwrap();
        assert_eq!(out, "n|s\n1|a\n(1 row)\n");
    }

    #[test]
    fn a_newline_in_a_cell_splits_the_row_and_marks_it_with_a_plus() {
        // `pg_wcsformat` (`mbprint.c:398`) splits on newlines and
        // `print_aligned_text` marks each continued line with `nl_right`,
        // which is "+" for ascii (`print.c:56`). Writing the newline into the
        // table raw would corrupt the frame.
        let res = result(vec![text_field("?column?")], vec![vec![Some("a\nb")]]);
        assert_eq!(
            rendered(&res),
            " ?column? \n----------\n a       +\n b\n(1 row)\n\n"
        );
    }

    #[test]
    fn a_newline_does_not_widen_the_column_by_the_whole_string() {
        // `pg_wcssize` reports the widest *line*, not the length of the cell.
        assert_eq!(cell_width(b"a\nbbbb"), 4);
        assert_eq!(cell_width(b"aaaa\nb"), 4);
        let res = result(vec![text_field("t")], vec![vec![Some("a\nb")]]);
        assert_eq!(rendered(&res), " t \n---\n a+\n b\n(1 row)\n\n");
    }

    #[test]
    fn a_multi_line_cell_pads_its_neighbours_on_the_extra_lines() {
        // `print.c:1085`: a column past its last line is blank-padded so the
        // dividers stay aligned.
        let res = result(
            vec![text_field("a"), text_field("b")],
            vec![vec![Some("1\n2"), Some("x")]],
        );
        // The trailing space on the second line is upstream's: the left mark
        // is emitted for every column whenever `border != 0`, including for a
        // column that has run out of lines (`print.c:1072`).
        assert_eq!(
            rendered(&res),
            " a | b \n---+---\n 1+| x\n 2 | \n(1 row)\n\n"
        );
    }

    #[test]
    fn numeric_columns_are_right_aligned_and_text_is_not() {
        assert_eq!(column_type_alignment(23), Align::Right);
        assert_eq!(column_type_alignment(1700), Align::Right);
        assert_eq!(column_type_alignment(25), Align::Left);
    }

    #[test]
    fn a_format_this_issue_does_not_render_is_refused_not_faked() {
        let res = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        for format in [
            PrintFormat::Csv,
            PrintFormat::Html,
            PrintFormat::Wrapped,
            PrintFormat::Latex,
            PrintFormat::Asciidoc,
            PrintFormat::TroffMs,
        ] {
            let mut opt = PrintQueryOpt::default();
            opt.topt = TableOpt { format, ..opt.topt };
            let err = print_query(&res, &opt)
                .expect_err("a format this port cannot render must be refused");
            assert_eq!(err, PrintError::Unsupported(format));
            // The message names the issue that implements it, so the refusal
            // is actionable rather than a bare failure.
            assert!(
                err.to_string().contains("NAT-400"),
                "the refusal must name the issue: {err}"
            );
        }
    }
}
