//! The document formats of `src/fe_utils/print.c`: csv (`print.c:1836`),
//! html (`:1948`), asciidoc (`:2164`), latex (`:2388`), latex-longtable
//! (`:2557`) and troff-ms (`:2806`), each normal and expanded.
//!
//! None of them measures anything: a cell is escaped for the target language
//! and written, so every function here is a straight walk over the table.
//!
//! Footers follow upstream's split. The `_text` printers print
//! `footers_with_default()`, which is the `(n rows)` line for a query result;
//! the `_vertical` printers read `cont->footers` directly, which `printQuery`
//! leaves `NULL`, so an expanded table has no footer at all.
//! `print_latex_longtable_text` prints none either way.
//!
//! `cont->opt->prior_records` is always 0 here (`FETCH_COUNT` is not ported),
//! so the first record is `Record 1`.

use super::{Align, TableContent};

/// `(*ptr)[strspn(*ptr, " \t")] == '\0'`: empty, or only spaces and tabs.
fn only_blanks(cell: &[u8]) -> bool {
    cell.iter().all(|&b| b == b' ' || b == b'\t')
}

/// The alignment letter `cont->aligns[]` holds.
fn align_char(align: Align) -> u8 {
    match align {
        Align::Left => b'l',
        Align::Right => b'r',
    }
}

/// `csv_escaped_print()` (`print.c:1841`).
fn csv_escaped_print(out: &mut Vec<u8>, s: &[u8]) {
    out.push(b'"');
    for &b in s {
        if b == b'"' {
            // Double quotes are doubled.
            out.push(b'"');
        }
        out.push(b);
    }
    out.push(b'"');
}

/// `csv_print_field()` (`print.c:1856`): quote a field that holds the
/// separator, a CR, an LF or a double quote, or that is exactly `\.`, and
/// every field when the separator is `\` or `.`, so that no line can read as
/// COPY's end-of-data marker.
fn csv_print_field(out: &mut Vec<u8>, s: &[u8], sep: u8) {
    if s.contains(&sep)
        || s.iter().any(|&b| matches!(b, b'\r' | b'\n' | b'"'))
        || s == b"\\."
        || sep == b'\\'
        || sep == b'.'
    {
        csv_escaped_print(out, s);
    } else {
        out.extend_from_slice(s);
    }
}

/// `cont->opt->csvFieldSep[0]`.
fn csv_field_sep(cont: &TableContent<'_>) -> u8 {
    u8::try_from(cont.opt.csv_field_sep).expect("do_pset keeps csv_fieldsep to one ASCII byte")
}

/// `print_csv_text()` (`print.c:1881`). The title and footer are never
/// printed; lines end in `\n`, not RFC 4180's CRLF.
pub(super) fn print_csv_text(cont: &TableContent<'_>) -> Vec<u8> {
    let sep = csv_field_sep(cont);
    let mut out = Vec::new();
    if cont.opt.start_table && !cont.opt.tuples_only {
        for (i, header) in cont.headers.iter().enumerate() {
            if i != 0 {
                out.push(sep);
            }
            csv_print_field(&mut out, header, sep);
        }
        out.push(b'\n');
    }
    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            csv_print_field(&mut out, cell, sep);
            out.push(if i + 1 < row.len() { sep } else { b'\n' });
        }
    }
    out
}

/// `print_csv_vertical()` (`print.c:1921`): one `name<sep>value` line per
/// cell, with nothing between records.
pub(super) fn print_csv_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let sep = csv_field_sep(cont);
    let mut out = Vec::new();
    for row in &cont.cells {
        for (header, cell) in cont.headers.iter().zip(row) {
            csv_print_field(&mut out, header, sep);
            out.push(sep);
            csv_print_field(&mut out, cell, sep);
            out.push(b'\n');
        }
    }
    out
}

/// `html_escaped_print()` (`print.c:1953`). Leading spaces become `&nbsp;`,
/// "for EXPLAIN output".
fn html_escaped_print(out: &mut Vec<u8>, s: &[u8]) {
    let mut leading_space = true;
    for &b in s {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'\n' => out.extend_from_slice(b"<br />\n"),
            b'"' => out.extend_from_slice(b"&quot;"),
            b' ' if leading_space => out.extend_from_slice(b"&nbsp;"),
            _ => out.push(b),
        }
        if b != b' ' {
            leading_space = false;
        }
    }
}

/// The `<table …>` line and the caption both HTML printers open with.
fn html_table_start(out: &mut Vec<u8>, cont: &TableContent<'_>) {
    out.extend_from_slice(format!("<table border=\"{}\"", cont.opt.border).as_bytes());
    if let Some(attr) = &cont.opt.table_attr {
        out.push(b' ');
        out.extend_from_slice(attr.as_bytes());
    }
    out.extend_from_slice(b">\n");
    if let Some(title) = cont.title
        && !cont.opt.tuples_only
    {
        out.extend_from_slice(b"  <caption>");
        html_escaped_print(out, title.as_bytes());
        out.extend_from_slice(b"</caption>\n");
    }
}

/// A `<td>`'s content: `&nbsp; ` for a blank cell, else the escaped cell.
fn html_cell(out: &mut Vec<u8>, align: Align, cell: &[u8]) {
    let align = match align {
        Align::Right => "right",
        Align::Left => "left",
    };
    out.extend_from_slice(format!("    <td align=\"{align}\">").as_bytes());
    if only_blanks(cell) {
        out.extend_from_slice(b"&nbsp; ");
    } else {
        html_escaped_print(out, cell);
    }
    out.extend_from_slice(b"</td>\n");
}

/// The `</table>` and the footer paragraph both HTML printers close with.
fn html_table_stop(out: &mut Vec<u8>, cont: &TableContent<'_>, footer: Option<&str>) {
    out.extend_from_slice(b"</table>\n");
    if let Some(footer) = footer
        && !cont.opt.tuples_only
    {
        out.extend_from_slice(b"<p>");
        html_escaped_print(out, footer.as_bytes());
        out.extend_from_slice(b"<br />\n</p>");
    }
    out.push(b'\n');
}

/// `print_html_text()` (`print.c:1994`).
pub(super) fn print_html_text(cont: &TableContent<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    if cont.opt.start_table {
        html_table_start(&mut out, cont);
        if !cont.opt.tuples_only {
            out.extend_from_slice(b"  <tr>\n");
            for header in &cont.headers {
                out.extend_from_slice(b"    <th align=\"center\">");
                html_escaped_print(&mut out, header);
                out.extend_from_slice(b"</th>\n");
            }
            out.extend_from_slice(b"  </tr>\n");
        }
    }
    for row in &cont.cells {
        out.extend_from_slice(b"  <tr valign=\"top\">\n");
        for (cell, &align) in row.iter().zip(&cont.aligns) {
            html_cell(&mut out, align, cell);
        }
        out.extend_from_slice(b"  </tr>\n");
    }
    if cont.opt.stop_table {
        html_table_stop(&mut out, cont, cont.default_footer().as_deref());
    }
    out
}

/// `print_html_vertical()` (`print.c:2083`).
pub(super) fn print_html_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    if cont.opt.start_table {
        html_table_start(&mut out, cont);
    }
    for (record, row) in (1_u64..).zip(&cont.cells) {
        if cont.opt.tuples_only {
            out.extend_from_slice(b"\n  <tr><td colspan=\"2\">&nbsp;</td></tr>\n");
        } else {
            out.extend_from_slice(
                format!("\n  <tr><td colspan=\"2\" align=\"center\">Record {record}</td></tr>\n")
                    .as_bytes(),
            );
        }
        for ((header, cell), &align) in cont.headers.iter().zip(row).zip(&cont.aligns) {
            out.extend_from_slice(b"  <tr valign=\"top\">\n    <th>");
            html_escaped_print(&mut out, header);
            out.extend_from_slice(b"</th>\n");
            // `html_cell` ends the `<td>` line; the row closes after it.
            html_cell(&mut out, align, cell);
            out.extend_from_slice(b"  </tr>\n");
        }
    }
    if cont.opt.stop_table {
        html_table_stop(&mut out, cont, None);
    }
    out
}

/// `asciidoc_escaped_print()` (`print.c:2169`): only `|` is escaped.
fn asciidoc_escaped_print(out: &mut Vec<u8>, s: &[u8]) {
    for &b in s {
        if b == b'|' {
            out.extend_from_slice(b"\\|");
        } else {
            out.push(b);
        }
    }
}

/// The blank line that puts an asciidoc table in a new paragraph, and its
/// `.title`.
fn asciidoc_title(out: &mut Vec<u8>, cont: &TableContent<'_>) {
    // Print the table in a new paragraph.
    out.push(b'\n');
    if let Some(title) = cont.title
        && !cont.opt.tuples_only
    {
        out.push(b'.');
        out.extend_from_slice(title.as_bytes());
        out.push(b'\n');
    }
}

/// The frame and grid attributes `\pset border` 0, 1 and 2 select (any other
/// border has none), closing the attribute list and opening the table.
fn asciidoc_frame(out: &mut Vec<u8>, border: u16) {
    out.extend_from_slice(match border {
        0 => b",frame=\"none\",grid=\"none\"".as_slice(),
        1 => b",frame=\"none\"",
        2 => b",frame=\"all\",grid=\"all\"",
        _ => b"",
    });
    out.extend_from_slice(b"]\n|====\n");
}

/// The literal block of footers asciidoc closes a table with.
fn asciidoc_footer(out: &mut Vec<u8>, footer: &str) {
    out.extend_from_slice(b"\n....\n");
    out.extend_from_slice(footer.as_bytes());
    out.extend_from_slice(b"\n....\n");
}

/// `print_asciidoc_text()` (`print.c:2187`).
pub(super) fn print_asciidoc_text(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let mut out = Vec::new();
    if cont.opt.start_table {
        asciidoc_title(&mut out, cont);
        out.push(b'[');
        if !tuples_only {
            out.extend_from_slice(b"options=\"header\",");
        }
        out.extend_from_slice(b"cols=\"");
        for (i, &align) in cont.aligns.iter().enumerate() {
            if i != 0 {
                out.push(b',');
            }
            out.extend_from_slice(match align {
                Align::Right => b">l",
                Align::Left => b"<l",
            });
        }
        out.push(b'"');
        asciidoc_frame(&mut out, cont.opt.border);
        if !tuples_only {
            for (i, header) in cont.headers.iter().enumerate() {
                if i != 0 {
                    out.push(b' ');
                }
                out.extend_from_slice(b"^l|");
                asciidoc_escaped_print(&mut out, header);
            }
            out.push(b'\n');
        }
    }
    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            let last = i + 1 == row.len();
            if i != 0 {
                out.push(b' ');
            }
            out.push(b'|');
            // Protect against needless spaces.
            if only_blanks(cell) {
                if !last {
                    out.push(b' ');
                }
            } else {
                asciidoc_escaped_print(&mut out, cell);
            }
            if last {
                out.push(b'\n');
            }
        }
    }
    out.extend_from_slice(b"|====\n");
    if cont.opt.stop_table
        && let Some(footer) = cont.default_footer()
        && !tuples_only
    {
        asciidoc_footer(&mut out, &footer);
    }
    out
}

/// `print_asciidoc_vertical()` (`print.c:2297`).
pub(super) fn print_asciidoc_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let mut out = Vec::new();
    if cont.opt.start_table {
        asciidoc_title(&mut out, cont);
        out.extend_from_slice(b"[cols=\"h,l\"");
        asciidoc_frame(&mut out, cont.opt.border);
    }
    for (record, row) in (1_u64..).zip(&cont.cells) {
        if cont.opt.tuples_only {
            out.extend_from_slice(b"2+|\n");
        } else {
            out.extend_from_slice(format!("2+^|Record {record}\n").as_bytes());
        }
        for ((header, cell), &align) in cont.headers.iter().zip(row).zip(&cont.aligns) {
            out.extend_from_slice(b"<l|");
            asciidoc_escaped_print(&mut out, header);
            out.extend_from_slice(match align {
                Align::Right => b" >l|",
                Align::Left => b" <l|",
            });
            if only_blanks(cell) {
                out.push(b' ');
            } else {
                asciidoc_escaped_print(&mut out, cell);
            }
            out.push(b'\n');
        }
    }
    out.extend_from_slice(b"|====\n");
    out
}

/// `latex_escaped_print()` (`print.c:2393`), after Scott Pakin's "The
/// Comprehensive LATEX Symbol List"; a newline becomes `\\`, which upstream
/// itself calls "not right".
fn latex_escaped_print(out: &mut Vec<u8>, s: &[u8]) {
    for &b in s {
        match b {
            b'#' => out.extend_from_slice(b"\\#"),
            b'$' => out.extend_from_slice(b"\\$"),
            b'%' => out.extend_from_slice(b"\\%"),
            b'&' => out.extend_from_slice(b"\\&"),
            b'<' => out.extend_from_slice(b"\\textless{}"),
            b'>' => out.extend_from_slice(b"\\textgreater{}"),
            b'\\' => out.extend_from_slice(b"\\textbackslash{}"),
            b'^' => out.extend_from_slice(b"\\^{}"),
            b'_' => out.extend_from_slice(b"\\_"),
            b'{' => out.extend_from_slice(b"\\{"),
            b'|' => out.extend_from_slice(b"\\textbar{}"),
            b'}' => out.extend_from_slice(b"\\}"),
            b'~' => out.extend_from_slice(b"\\~{}"),
            b'\n' => out.extend_from_slice(b"\\\\"),
            _ => out.push(b),
        }
    }
}

/// The centred title the `tabular` printers open with.
fn latex_title(out: &mut Vec<u8>, cont: &TableContent<'_>) {
    if let Some(title) = cont.title
        && !cont.opt.tuples_only
    {
        out.extend_from_slice(b"\\begin{center}\n");
        latex_escaped_print(out, title.as_bytes());
        out.extend_from_slice(b"\n\\end{center}\n\n");
    }
}

/// The `tabular` close and the footers after it.
fn latex_tabular_stop(out: &mut Vec<u8>, border: u16, footer: Option<&str>, tuples_only: bool) {
    if border == 2 {
        out.extend_from_slice(b"\\hline\n");
    }
    out.extend_from_slice(b"\\end{tabular}\n\n\\noindent ");
    if let Some(footer) = footer
        && !tuples_only
    {
        latex_escaped_print(out, footer.as_bytes());
        out.extend_from_slice(b" \\\\\n");
    }
    out.push(b'\n');
}

/// `print_latex_text()` (`print.c:2455`). A border above 3 draws as 3.
pub(super) fn print_latex_text(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let border = cont.opt.border.min(3);
    let ncolumns = cont.headers.len();
    let mut out = Vec::new();
    if cont.opt.start_table {
        latex_title(&mut out, cont);
        out.extend_from_slice(b"\\begin{tabular}{");
        if border >= 2 {
            out.extend_from_slice(b"| ");
        }
        for (i, &align) in cont.aligns.iter().enumerate() {
            out.push(align_char(align));
            if border != 0 && i + 1 < ncolumns {
                out.extend_from_slice(b" | ");
            }
        }
        if border >= 2 {
            out.extend_from_slice(b" |");
        }
        out.extend_from_slice(b"}\n");
        if !tuples_only && border >= 2 {
            out.extend_from_slice(b"\\hline\n");
        }
        if !tuples_only {
            for (i, header) in cont.headers.iter().enumerate() {
                if i != 0 {
                    out.extend_from_slice(b" & ");
                }
                out.extend_from_slice(b"\\textit{");
                latex_escaped_print(&mut out, header);
                out.push(b'}');
            }
            out.extend_from_slice(b" \\\\\n\\hline\n");
        }
    }
    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            latex_escaped_print(&mut out, cell);
            if i + 1 < row.len() {
                out.extend_from_slice(b" & ");
            } else {
                out.extend_from_slice(b" \\\\\n");
                if border == 3 {
                    out.extend_from_slice(b"\\hline\n");
                }
            }
        }
    }
    if cont.opt.stop_table {
        latex_tabular_stop(
            &mut out,
            border,
            cont.default_footer().as_deref(),
            tuples_only,
        );
    }
    out
}

/// `LONGTABLE_WHITESPACE` (`print.c:2592`).
fn is_longtable_whitespace(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n')
}

/// The column specification of a `longtable` (`print.c:2586`-`:2624`).
///
/// `\pset tableattr` is a list of widths, as fractions of `\textwidth`, for
/// the left-aligned columns in turn: each takes the next width as `p{…}`, and
/// once the list runs out, the last width again. With no `tableattr` a column
/// is its alignment letter, and so is a left-aligned one before any width.
fn longtable_columns(out: &mut Vec<u8>, cont: &TableContent<'_>, border: u16) {
    let attr = cont.opt.table_attr.as_deref().map(str::as_bytes);
    let token = |s: &[u8]| -> usize {
        s.iter()
            .take_while(|&&b| !is_longtable_whitespace(b))
            .count()
    };
    let mut next = attr.unwrap_or_default();
    let mut last: Option<&[u8]> = None;
    let ncolumns = cont.aligns.len();
    for (i, &align) in cont.aligns.iter().enumerate() {
        if align == Align::Left && attr.is_some() {
            let skip = next
                .iter()
                .take_while(|&&b| is_longtable_whitespace(b))
                .count();
            next = &next[skip..];
            if next.is_empty() {
                if let Some(previous) = last {
                    out.extend_from_slice(b"p{");
                    out.extend_from_slice(&previous[..token(previous)]);
                    out.extend_from_slice(b"\\textwidth}");
                } else {
                    out.push(b'l');
                }
            } else {
                let len = token(next);
                out.extend_from_slice(b"p{");
                out.extend_from_slice(&next[..len]);
                last = Some(next);
                next = &next[len..];
                out.extend_from_slice(b"\\textwidth}");
            }
        } else {
            out.push(align_char(align));
        }
        if border != 0 && i + 1 < ncolumns {
            out.extend_from_slice(b" | ");
        }
    }
}

/// One `longtable` header row: `\small\textbf{\textit{…}}` per column.
fn longtable_header_row(out: &mut Vec<u8>, cont: &TableContent<'_>) {
    for (i, header) in cont.headers.iter().enumerate() {
        if i != 0 {
            out.extend_from_slice(b" & ");
        }
        out.extend_from_slice(b"\\small\\textbf{\\textit{");
        latex_escaped_print(out, header);
        out.extend_from_slice(b"}}");
    }
    out.extend_from_slice(b" \\\\\n");
}

/// `print_latex_longtable_text()` (`print.c:2562`). A border above 3 draws as
/// 3. The title is the caption of the table's foot, so it is printed only
/// with the headers.
pub(super) fn print_latex_longtable_text(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let border = cont.opt.border.min(3);
    let mut out = Vec::new();
    if cont.opt.start_table {
        out.extend_from_slice(b"\\begin{longtable}{");
        if border >= 2 {
            out.extend_from_slice(b"| ");
        }
        longtable_columns(&mut out, cont, border);
        if border >= 2 {
            out.extend_from_slice(b" |");
        }
        out.extend_from_slice(b"}\n");

        if !tuples_only {
            // The first page's head.
            if border >= 2 {
                out.extend_from_slice(b"\\toprule\n");
            }
            longtable_header_row(&mut out, cont);
            out.extend_from_slice(b"\\midrule\n\\endfirsthead\n");

            // The head of every later page.
            if border >= 2 {
                out.extend_from_slice(b"\\toprule\n");
            }
            longtable_header_row(&mut out, cont);
            // With a line under every row, that line is the rule.
            if border != 3 {
                out.extend_from_slice(b"\\midrule\n");
            }
            out.extend_from_slice(b"\\endhead\n");

            if let Some(title) = cont.title {
                let caption = |out: &mut Vec<u8>, continued: &[u8]| {
                    if border == 2 {
                        out.extend_from_slice(b"\\bottomrule\n");
                    }
                    out.extend_from_slice(b"\\caption[");
                    latex_escaped_print(out, title.as_bytes());
                    out.extend_from_slice(continued);
                    out.extend_from_slice(b"]{");
                    latex_escaped_print(out, title.as_bytes());
                    out.push(b'}');
                };
                caption(&mut out, b" (Continued)");
                out.extend_from_slice(b"\n\\endfoot\n");
                caption(&mut out, b"");
                out.extend_from_slice(b"\n\\endlastfoot\n");
            } else if border >= 2 {
                out.extend_from_slice(b"\\bottomrule\n\\endfoot\n");
                out.extend_from_slice(b"\\bottomrule\n\\endlastfoot\n");
            }
        }
    }
    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            // C's `i != 0 && i % ncolumns != 0`: between the cells of a row.
            if i != 0 {
                out.extend_from_slice(b"\n&\n");
            }
            out.extend_from_slice(b"\\raggedright{");
            latex_escaped_print(&mut out, cell);
            out.push(b'}');
            if i + 1 == row.len() {
                out.extend_from_slice(b" \\tabularnewline\n");
                if border == 3 {
                    out.extend_from_slice(b" \\hline\n");
                }
            }
        }
    }
    if cont.opt.stop_table {
        out.extend_from_slice(b"\\end{longtable}\n");
    }
    out
}

/// `print_latex_vertical()` (`print.c:2718`), which `latex-longtable` uses
/// too. A border above 2 draws as 2.
pub(super) fn print_latex_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let border = cont.opt.border.min(2);
    let mut out = Vec::new();
    if cont.opt.start_table {
        latex_title(&mut out, cont);
        out.extend_from_slice(b"\\begin{tabular}{");
        out.extend_from_slice(match border {
            0 => b"cl".as_slice(),
            1 => b"c|l",
            _ => b"|c|l|",
        });
        out.extend_from_slice(b"}\n");
    }
    for (record, row) in (1_u64..).zip(&cont.cells) {
        if !tuples_only {
            if border == 2 {
                out.extend_from_slice(b"\\hline\n");
                out.extend_from_slice(
                    format!("\\multicolumn{{2}}{{|c|}}{{\\textit{{Record {record}}}}} \\\\\n")
                        .as_bytes(),
                );
            } else {
                out.extend_from_slice(
                    format!("\\multicolumn{{2}}{{c}}{{\\textit{{Record {record}}}}} \\\\\n")
                        .as_bytes(),
                );
            }
        }
        if border >= 1 {
            out.extend_from_slice(b"\\hline\n");
        }
        for (header, cell) in cont.headers.iter().zip(row) {
            latex_escaped_print(&mut out, header);
            out.extend_from_slice(b" & ");
            latex_escaped_print(&mut out, cell);
            out.extend_from_slice(b" \\\\\n");
        }
    }
    if cont.opt.stop_table {
        latex_tabular_stop(&mut out, border, None, tuples_only);
    }
    out
}

/// `troff_ms_escaped_print()` (`print.c:2811`): only `\` is escaped.
fn troff_ms_escaped_print(out: &mut Vec<u8>, s: &[u8]) {
    for &b in s {
        if b == b'\\' {
            out.extend_from_slice(b"\\(rs");
        } else {
            out.push(b);
        }
    }
}

/// The title display and the `.TS` opening both troff printers start with.
fn troff_ms_table_start(out: &mut Vec<u8>, cont: &TableContent<'_>, border: u16) {
    if let Some(title) = cont.title
        && !cont.opt.tuples_only
    {
        out.extend_from_slice(b".LP\n.DS C\n");
        troff_ms_escaped_print(out, title.as_bytes());
        out.extend_from_slice(b"\n.DE\n");
    }
    out.extend_from_slice(b".LP\n.TS\n");
    out.extend_from_slice(if border == 2 {
        b"center box;\n".as_slice()
    } else {
        b"center;\n"
    });
}

/// The `.TE` close and the footer display after it.
fn troff_ms_table_stop(out: &mut Vec<u8>, footer: Option<&str>, tuples_only: bool) {
    out.extend_from_slice(b".TE\n.DS L\n");
    if let Some(footer) = footer
        && !tuples_only
    {
        troff_ms_escaped_print(out, footer.as_bytes());
        out.push(b'\n');
    }
    out.extend_from_slice(b".DE\n");
}

/// `print_troff_ms_text()` (`print.c:2828`). A border above 2 draws as 2.
pub(super) fn print_troff_ms_text(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let border = cont.opt.border.min(2);
    let ncolumns = cont.headers.len();
    let mut out = Vec::new();
    if cont.opt.start_table {
        troff_ms_table_start(&mut out, cont, border);
        for (i, &align) in cont.aligns.iter().enumerate() {
            out.push(align_char(align));
            if border > 0 && i + 1 < ncolumns {
                out.extend_from_slice(b" | ");
            }
        }
        out.extend_from_slice(b".\n");
        if !tuples_only {
            for (i, header) in cont.headers.iter().enumerate() {
                if i != 0 {
                    out.push(b'\t');
                }
                out.extend_from_slice(b"\\fI");
                troff_ms_escaped_print(&mut out, header);
                out.extend_from_slice(b"\\fP");
            }
            out.extend_from_slice(b"\n_\n");
        }
    }
    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            troff_ms_escaped_print(&mut out, cell);
            out.push(if i + 1 < row.len() { b'\t' } else { b'\n' });
        }
    }
    if cont.opt.stop_table {
        troff_ms_table_stop(&mut out, cont.default_footer().as_deref(), tuples_only);
    }
    out
}

/// Which `tbl` format line is in force in [`print_troff_ms_vertical`]:
/// C's `current_format`, 0, 1 or 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TroffFormat {
    /// No format line yet.
    None,
    /// `c s.`: a record header spanning both columns.
    Header,
    /// `c l.` or `c | l.`: a name and its value.
    Body,
}

/// `print_troff_ms_vertical()` (`print.c:2920`). A border above 2 draws as 2.
///
/// `tbl` needs a new format line (`.T&`) whenever a record header follows a
/// body row and back, so the printer tracks which one is in force.
pub(super) fn print_troff_ms_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let tuples_only = cont.opt.tuples_only;
    let border = cont.opt.border.min(2);
    let mut out = Vec::new();
    let mut current = TroffFormat::None;
    if cont.opt.start_table {
        troff_ms_table_start(&mut out, cont, border);
        if tuples_only {
            out.extend_from_slice(b"c l;\n");
        }
    } else {
        // Assume tuples were printed already.
        current = TroffFormat::Body;
    }
    for (record, row) in (1_u64..).zip(&cont.cells) {
        if !tuples_only {
            if current != TroffFormat::Header {
                if border == 2 && record > 1 {
                    out.extend_from_slice(b"_\n");
                }
                if current != TroffFormat::None {
                    out.extend_from_slice(b".T&\n");
                }
                out.extend_from_slice(b"c s.\n");
                current = TroffFormat::Header;
            }
            out.extend_from_slice(format!("\\fIRecord {record}\\fP\n").as_bytes());
        }
        if border >= 1 {
            out.extend_from_slice(b"_\n");
        }
        for (header, cell) in cont.headers.iter().zip(row) {
            if !tuples_only && current != TroffFormat::Body {
                if current != TroffFormat::None {
                    out.extend_from_slice(b".T&\n");
                }
                out.extend_from_slice(if border == 1 {
                    b"c | l.\n".as_slice()
                } else {
                    b"c l.\n"
                });
                current = TroffFormat::Body;
            }
            troff_ms_escaped_print(&mut out, header);
            out.push(b'\t');
            troff_ms_escaped_print(&mut out, cell);
            out.push(b'\n');
        }
    }
    if cont.opt.stop_table {
        troff_ms_table_stop(&mut out, None, tuples_only);
    }
    out
}

#[cfg(test)]
mod tests {
    //! What `psql.out` does not reach, traced by hand from `print.c`: titles,
    //! `tuples_only` in the vertical printers, footers off, and the
    //! `longtable` width list running out. Everything `psql.sql` does reach
    //! is gated against `psql.out` in `tests/t_regress_psql.rs`.

    use super::*;
    use crate::settings::TableOpt;

    fn table<'a>(opt: &'a TableOpt, title: Option<&'a str>) -> TableContent<'a> {
        TableContent {
            opt,
            title,
            headers: vec![b"a<b".to_vec(), b"n".to_vec()],
            cells: vec![vec![b"x\\y".to_vec(), b"1".to_vec()]],
            aligns: vec![Align::Left, Align::Right],
        }
    }

    fn text(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_title_is_escaped_for_each_format_and_never_printed_in_csv() {
        let opt = TableOpt::default();
        let cont = table(&opt, Some("T&<\\"));
        assert!(text(print_html_text(&cont)).contains("  <caption>T&amp;&lt;\\</caption>\n"));
        assert!(text(print_asciidoc_text(&cont)).starts_with("\n.T&<\\\n[options"));
        assert!(
            text(print_latex_text(&cont)).starts_with(
                "\\begin{center}\nT\\&\\textless{}\\textbackslash{}\n\\end{center}\n\n"
            )
        );
        assert!(text(print_troff_ms_text(&cont)).starts_with(".LP\n.DS C\nT&<\\(rs\n.DE\n"));
        assert_eq!(text(print_csv_text(&cont)), "a<b,n\nx\\y,1\n");
    }

    #[test]
    fn a_longtable_title_is_the_caption_of_its_foot() {
        let opt = TableOpt {
            border: 2,
            ..TableOpt::default()
        };
        let out = text(print_latex_longtable_text(&table(&opt, Some("t_1"))));
        assert!(
            out.contains(
                "\\endhead\n\
                 \\bottomrule\n\\caption[t\\_1 (Continued)]{t\\_1}\n\\endfoot\n\
                 \\bottomrule\n\\caption[t\\_1]{t\\_1}\n\\endlastfoot\n"
            ),
            "{out}"
        );
    }

    #[test]
    fn longtable_widths_go_to_the_left_aligned_columns_and_the_last_repeats() {
        // `print.c:2589`-`:2620`: a right-aligned column keeps its letter.
        let opt = TableOpt {
            table_attr: Some(" 0.2 ".to_string()),
            ..TableOpt::default()
        };
        let cont = TableContent {
            headers: vec![b"a".to_vec(), b"n".to_vec(), b"b".to_vec()],
            cells: vec![],
            aligns: vec![Align::Left, Align::Right, Align::Left],
            ..table(&opt, None)
        };
        assert!(
            text(print_latex_longtable_text(&cont))
                .starts_with("\\begin{longtable}{p{0.2\\textwidth} | r | p{0.2\\textwidth}}\n")
        );
    }

    #[test]
    fn tuples_only_vertical_output_marks_records_without_numbering_them() {
        let opt = TableOpt {
            tuples_only: true,
            ..TableOpt::default()
        };
        let cont = table(&opt, Some("never"));
        assert_eq!(
            text(print_html_vertical(&cont)),
            "<table border=\"1\">\n\
             \n  <tr><td colspan=\"2\">&nbsp;</td></tr>\n\
             \x20 <tr valign=\"top\">\n    <th>a&lt;b</th>\n    <td align=\"left\">x\\y</td>\n  </tr>\n\
             \x20 <tr valign=\"top\">\n    <th>n</th>\n    <td align=\"right\">1</td>\n  </tr>\n\
             </table>\n\n"
        );
        assert_eq!(
            text(print_asciidoc_vertical(&cont)),
            "\n[cols=\"h,l\",frame=\"none\"]\n|====\n2+|\n<l|a<b <l|x\\y\n<l|n >l|1\n|====\n"
        );
        // `c l;` is the only format line: no `.T&` switching without headers.
        assert_eq!(
            text(print_troff_ms_vertical(&cont)),
            ".LP\n.TS\ncenter;\nc l;\n_\na<b\tx\\(rsy\nn\t1\n.TE\n.DS L\n.DE\n"
        );
        assert_eq!(
            text(print_latex_vertical(&cont)),
            "\\begin{tabular}{c|l}\n\\hline\n\
             a\\textless{}b & x\\textbackslash{}y \\\\\nn & 1 \\\\\n\
             \\end{tabular}\n\n\\noindent \n"
        );
    }

    #[test]
    fn footer_off_drops_the_row_count_but_not_the_closing_markup() {
        let opt = TableOpt {
            default_footer: false,
            ..TableOpt::default()
        };
        let cont = table(&opt, None);
        assert!(text(print_html_text(&cont)).ends_with("  </tr>\n</table>\n\n"));
        assert!(text(print_asciidoc_text(&cont)).ends_with("|x\\y |1\n|====\n"));
        assert!(text(print_latex_text(&cont)).ends_with("\\end{tabular}\n\n\\noindent \n"));
        assert!(text(print_troff_ms_text(&cont)).ends_with(".TE\n.DS L\n.DE\n"));
    }

    #[test]
    fn a_csv_separator_of_backslash_or_dot_quotes_every_field() {
        // `print.c:1874`: so that no line can read as `\.`.
        let opt = TableOpt {
            csv_field_sep: '.',
            ..TableOpt::default()
        };
        assert_eq!(
            text(print_csv_text(&table(&opt, None))),
            "\"a<b\".\"n\"\n\"x\\y\".\"1\"\n"
        );
    }
}
