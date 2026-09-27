//! Result rendering: `src/fe_utils/print.c`.
//!
//! Scope, so far. `PRINT_ALIGNED` at every border (0, 1, 2) in the ascii and
//! old-ascii line styles, and `PRINT_UNALIGNED`. NAT-400 owns the rest of the
//! matrix and delivers it in slices: wrapped and expanded next, then csv,
//! html, latex, latex-longtable, troff-ms and asciidoc, then the unicode line
//! style and `numericlocale`. Until then each of those is
//! [`PrintError::Unsupported`], which the caller reports rather than printing
//! something that only looks right.
//!
//! The whole module is a pure function from a result to bytes; nothing here
//! opens a file or a pager.

use rlibpq::QueryResult;

use crate::settings::{Expanded, LineStyle, PrintFormat, PrintQueryOpt, TableOpt};

/// What this port cannot render yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// An output format a later slice of NAT-400 ports.
    Format(PrintFormat),
    /// `\pset expanded on`, or `auto` once the table is too wide.
    Expanded,
    /// `\pset linestyle unicode`.
    Unicode,
    /// `\pset numericlocale on` over a right-aligned column.
    NumericLocale,
}

/// A table this port does not render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintError {
    /// The feature belongs to a later slice of NAT-400.
    Unsupported(Unsupported),
}

impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Unsupported(what) = self;
        match what {
            Unsupported::Format(format) => write!(f, "output format {}", format.name())?,
            Unsupported::Expanded => f.write_str("expanded output")?,
            Unsupported::Unicode => f.write_str("the unicode line style")?,
            Unsupported::NumericLocale => f.write_str("locale-adjusted numeric output")?,
        }
        f.write_str(" is not implemented yet (Linear NAT-400)")
    }
}

impl std::error::Error for PrintError {}

/// Column alignment: `column_type_alignment()` (`print.c:3615`).
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

/// `cont->aligns[]`'s `'l'` and `'r'` (`print.h:178`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// `'l'`
    Left,
    /// `'r'`
    Right,
}

/// `printTextLineFormat` (`print.h:43`): the characters of one kind of rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextLineFormat {
    /// `hrule`: horizontal line character.
    pub hrule: &'static str,
    /// `leftvrule`: left vertical line (+horizontal).
    pub leftvrule: &'static str,
    /// `midvrule`: intra-column vertical line (+horizontal).
    pub midvrule: &'static str,
    /// `rightvrule`: right vertical line (+horizontal).
    pub rightvrule: &'static str,
}

/// `printTextRule` (`print.h:52`), the index into [`TextFormat::lrule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// `PRINT_RULE_TOP`
    Top = 0,
    /// `PRINT_RULE_MIDDLE`
    Middle = 1,
    /// `PRINT_RULE_BOTTOM`
    Bottom = 2,
    /// `PRINT_RULE_DATA`
    Data = 3,
}

/// `printTextFormat` (`print.h:81`): a complete line style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextFormat {
    /// `name`
    pub name: &'static str,
    /// `lrule`, indexed by `printTextRule`.
    pub lrule: [TextLineFormat; 4],
    /// `midvrule_nl`: vertical line for continue after newline.
    pub midvrule_nl: &'static str,
    /// `midvrule_wrap`: vertical line for wrapped data.
    pub midvrule_wrap: &'static str,
    /// `midvrule_blank`: vertical line for blank data.
    pub midvrule_blank: &'static str,
    /// `header_nl_left`: left mark after newline.
    pub header_nl_left: &'static str,
    /// `header_nl_right`: right mark for newline.
    pub header_nl_right: &'static str,
    /// `nl_left`: left mark after newline.
    pub nl_left: &'static str,
    /// `nl_right`: right mark for newline.
    pub nl_right: &'static str,
    /// `wrap_left`: left mark after wrapped data.
    pub wrap_left: &'static str,
    /// `wrap_right`: right mark for wrapped data.
    pub wrap_right: &'static str,
    /// `wrap_right_border`: use the right-hand border for wrap marks when
    /// `border = 0`?
    pub wrap_right_border: bool,
}

const ASCII_RULE: TextLineFormat = TextLineFormat {
    hrule: "-",
    leftvrule: "+",
    midvrule: "+",
    rightvrule: "+",
};

const ASCII_DATA: TextLineFormat = TextLineFormat {
    hrule: "",
    leftvrule: "|",
    midvrule: "|",
    rightvrule: "|",
};

/// `pg_asciiformat` (`print.c:56`).
pub const ASCII_FORMAT: TextFormat = TextFormat {
    name: "ascii",
    lrule: [ASCII_RULE, ASCII_RULE, ASCII_RULE, ASCII_DATA],
    midvrule_nl: "|",
    midvrule_wrap: "|",
    midvrule_blank: "|",
    header_nl_left: " ",
    header_nl_right: "+",
    nl_left: " ",
    nl_right: "+",
    wrap_left: ".",
    wrap_right: ".",
    wrap_right_border: true,
};

/// `pg_asciiformat_old` (`print.c:77`).
pub const ASCII_FORMAT_OLD: TextFormat = TextFormat {
    name: "old-ascii",
    lrule: [ASCII_RULE, ASCII_RULE, ASCII_RULE, ASCII_DATA],
    midvrule_nl: ":",
    midvrule_wrap: ";",
    midvrule_blank: " ",
    header_nl_left: "+",
    header_nl_right: " ",
    nl_left: " ",
    nl_right: " ",
    wrap_left: " ",
    wrap_right: " ",
    wrap_right_border: false,
};

/// `get_line_style()` (`print.c:3678`), for the styles this port draws.
fn line_style(opt: &TableOpt) -> Result<&'static TextFormat, PrintError> {
    match opt.line_style {
        LineStyle::Ascii => Ok(&ASCII_FORMAT),
        LineStyle::OldAscii => Ok(&ASCII_FORMAT_OLD),
        LineStyle::Unicode => Err(PrintError::Unsupported(Unsupported::Unicode)),
    }
}

/// `printTableContent` (`print.h:163`), as `printQuery` fills it: no footers
/// but the default one, and no translation.
struct TableContent<'a> {
    opt: &'a TableOpt,
    title: Option<&'a str>,
    headers: Vec<Vec<u8>>,
    /// One `Vec` per row, one cell per column.
    cells: Vec<Vec<Vec<u8>>>,
    aligns: Vec<Align>,
}

impl TableContent<'_> {
    /// `footers_with_default()` (`print.c:398`): the `(n rows)` line, or
    /// nothing under `\pset footer off`.
    fn default_footer(&self) -> Option<String> {
        self.opt.default_footer.then(|| {
            let n = self.cells.len();
            if n == 1 {
                format!("({n} row)")
            } else {
                format!("({n} rows)")
            }
        })
    }
}

/// `printQuery()` (`print.c:3550`) and `printTable()` (`print.c:3444`):
/// render one result.
///
/// # Errors
/// [`PrintError::Unsupported`] for what NAT-400 still owns.
pub fn print_query(result: &QueryResult, opt: &PrintQueryOpt) -> Result<Vec<u8>, PrintError> {
    let headers: Vec<Vec<u8>> = (0..result.nfields())
        .map(|i| result.fname(i).unwrap_or(b"").to_vec())
        .collect();
    let aligns: Vec<Align> = result
        .fields()
        .iter()
        .map(|f| column_type_alignment(f.typid))
        .collect();
    // `format_numeric_locale` rewrites right-aligned cells; it is a later
    // slice's, and a no-op without a right-aligned column.
    if opt.topt.numeric_locale && aligns.contains(&Align::Right) {
        return Err(PrintError::Unsupported(Unsupported::NumericLocale));
    }
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
    let cont = TableContent {
        opt: &opt.topt,
        title: opt.title.as_deref(),
        headers,
        cells,
        aligns,
    };

    match opt.topt.format {
        PrintFormat::Unaligned => {
            if opt.topt.expanded == Expanded::On {
                return Err(PrintError::Unsupported(Unsupported::Expanded));
            }
            Ok(print_unaligned_text(&cont))
        }
        // `PRINT_WRAPPED` shares `print_aligned_text` but is a later slice's.
        PrintFormat::Aligned => {
            if opt.topt.expanded == Expanded::On {
                return Err(PrintError::Unsupported(Unsupported::Expanded));
            }
            print_aligned_text(&cont)
        }
        other => Err(PrintError::Unsupported(Unsupported::Format(other))),
    }
}

/// One display line of a cell, as `pg_wcsformat` leaves it in a `lineptr`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    /// The bytes to print, control characters already escaped.
    bytes: Vec<u8>,
    /// `lineptr.width`: display width.
    width: usize,
}

/// `pg_wcsformat()` (`mbprint.c:294`): split a cell at its newlines and make
/// every other character printable.
///
/// `pg_wcssize()` (`mbprint.c:211`) is the same walk counting instead of
/// copying, so it is not repeated: the widest [`Line`] is its
/// `result_width` and the number of lines its `result_height`.
///
/// A carriage return becomes `\r`, a tab spaces to the next multiple of 8, an
/// other ASCII control character `\xNN` and a C1 control character `\uNNNN`,
/// as for a UTF-8 client encoding. Every other character is one column wide:
/// the East-Asian-width and non-spacing tables `ucs_wcwidth` searches
/// (`wchar.c:646`) are not ported, which is a recorded divergence. A cell
/// that is not UTF-8 is walked byte by byte, one column per byte.
///
/// Upstream steps and measures by the client encoding (`PQmblen`, `PQdsplen`,
/// `mbprint.c:304`, `:307`); this walk is UTF-8's whatever it is, because the
/// client encoding is not tracked yet. That too is a recorded divergence.
fn format_cell(cell: &[u8]) -> Vec<Line> {
    fn push_escaped(line: &mut Line, text: &str) {
        line.bytes.extend_from_slice(text.as_bytes());
        line.width += text.len();
    }

    let mut lines = vec![Line {
        bytes: Vec::new(),
        width: 0,
    }];
    let push = |c: Option<char>, raw: &[u8], lines: &mut Vec<Line>| {
        let line = lines.last_mut().expect("there is always a current line");
        match raw {
            b"\n" => lines.push(Line {
                bytes: Vec::new(),
                width: 0,
            }),
            b"\r" => push_escaped(line, "\\r"),
            b"\t" => loop {
                line.bytes.push(b' ');
                line.width += 1;
                if line.width.is_multiple_of(8) {
                    break;
                }
            },
            [b] if *b < 0x20 || *b == 0x7f => push_escaped(line, &format!("\\x{b:02X}")),
            _ => match c {
                Some(c) if ('\u{80}'..'\u{a0}').contains(&c) => {
                    push_escaped(line, &format!("\\u{:04X}", u32::from(c)));
                }
                _ => {
                    line.bytes.extend_from_slice(raw);
                    line.width += 1;
                }
            },
        }
    };
    if let Ok(text) = std::str::from_utf8(cell) {
        let mut buf = [0; 4];
        for c in text.chars() {
            push(Some(c), c.encode_utf8(&mut buf).as_bytes(), &mut lines);
        }
    } else {
        for b in cell {
            push(None, std::slice::from_ref(b), &mut lines);
        }
    }
    lines
}

/// The widest line of a formatted cell: `pg_wcssize`'s `result_width`.
fn widest(lines: &[Line]) -> usize {
    lines.iter().map(|l| l.width).max().unwrap_or(0)
}

fn pad(out: &mut Vec<u8>, n: usize) {
    out.extend(std::iter::repeat_n(b' ', n));
}

/// `_print_horizontal_line()` (`print.c:593`).
fn print_horizontal_line(
    out: &mut Vec<u8>,
    widths: &[usize],
    border: u16,
    pos: Rule,
    format: &TextFormat,
) {
    let lformat = &format.lrule[pos as usize];
    if border == 1 {
        out.extend_from_slice(lformat.hrule.as_bytes());
    } else if border == 2 {
        out.extend_from_slice(lformat.leftvrule.as_bytes());
        out.extend_from_slice(lformat.hrule.as_bytes());
    }
    for (i, width) in widths.iter().enumerate() {
        out.extend_from_slice(lformat.hrule.repeat(*width).as_bytes());
        if i + 1 < widths.len() {
            if border == 0 {
                out.push(b' ');
            } else {
                out.extend_from_slice(lformat.hrule.as_bytes());
                out.extend_from_slice(lformat.midvrule.as_bytes());
                out.extend_from_slice(lformat.hrule.as_bytes());
            }
        }
    }
    if border == 2 {
        out.extend_from_slice(lformat.hrule.as_bytes());
        out.extend_from_slice(lformat.rightvrule.as_bytes());
    } else if border == 1 {
        out.extend_from_slice(lformat.hrule.as_bytes());
    }
    out.push(b'\n');
}

/// `printTextLineWrap` (`print.h:61`): why a column's next display line
/// continues its cell. `PRINT_LINE_WRAP_WRAP` needs the wrapped format, which
/// is a later slice's, so a line here only ever continues after a newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineWrap {
    /// `PRINT_LINE_WRAP_NONE`
    None,
    /// `PRINT_LINE_WRAP_NEWLINE`
    Newline,
}

/// `print_aligned_text()` (`print.c:635`), without wrapping: every column is
/// exactly as wide as its widest line, so `width_wrap[]` is `max_width[]` and
/// `strlen_max_width` always yields the whole line.
///
/// The pager is not ported, so neither is the pager arithmetic; the target
/// width `output_columns` matters only to `expanded auto`, which escapes to
/// the vertical format (a later slice) when the table is wider. That width is
/// `\pset columns`; the terminal's (`$COLUMNS`, `TIOCGWINSZ`) is not read yet,
/// so `expanded auto` under `\pset columns 0` is refused.
// One function, as upstream's is, so a reader can follow the two side by side.
#[allow(clippy::too_many_lines)]
fn print_aligned_text(cont: &TableContent<'_>) -> Result<Vec<u8>, PrintError> {
    let opt = cont.opt;
    let opt_tuples_only = opt.tuples_only;
    let format = line_style(opt)?;
    let dformat = &format.lrule[Rule::Data as usize];
    let opt_border = opt.border.min(2);
    let col_count = cont.headers.len();
    let mut out = Vec::new();

    // Scan every header and cell for the widest line (`print.c:710`).
    let header_lines: Vec<Vec<Line>> = cont.headers.iter().map(|h| format_cell(h)).collect();
    let width_header: Vec<usize> = header_lines.iter().map(|l| widest(l)).collect();
    let mut max_width = width_header.clone();
    let row_lines: Vec<Vec<Vec<Line>>> = cont
        .cells
        .iter()
        .map(|row| row.iter().map(|cell| format_cell(cell)).collect())
        .collect();
    for row in &row_lines {
        for (i, lines) in row.iter().enumerate() {
            max_width[i] = max_width[i].max(widest(lines));
        }
    }

    // The total display width, by border style (`print.c:763`).
    let mut width_total = match opt_border {
        0 => col_count,
        1 => (col_count * 3).saturating_sub(1),
        _ => col_count * 3 + 1,
    };
    let total_header_width = width_total + width_header.iter().sum::<usize>();
    width_total += max_width.iter().sum::<usize>();

    // Expanded auto escapes to vertical when the table is wider than the
    // target and has more than one column (`print.c:877`). Under `\pset
    // columns 0` the target is the terminal's (`print.c:804`-`:816`), which is
    // not read here, so whether C goes vertical cannot be decided: refuse.
    let output_columns = usize::try_from(opt.columns).unwrap_or(0);
    if opt.expanded == Expanded::Auto
        && col_count > 1
        && (output_columns == 0
            || output_columns < total_header_width
            || output_columns < width_total)
    {
        return Err(PrintError::Unsupported(Unsupported::Expanded));
    }

    if opt.start_table {
        // Title, centred over the table unless it is at least as wide
        // (`print.c:935`). It is printed raw, not formatted.
        if let Some(title) = cont.title
            && !opt_tuples_only
        {
            let width = widest(&format_cell(title.as_bytes()));
            if width < width_total {
                pad(&mut out, (width_total - width) / 2);
            }
            out.extend_from_slice(title.as_bytes());
            out.push(b'\n');
        }

        // Headers, centred, one display line per embedded newline
        // (`print.c:952`).
        if !opt_tuples_only {
            if opt_border == 2 {
                print_horizontal_line(&mut out, &max_width, opt_border, Rule::Top, format);
            }
            let mut more_col_wrapping = col_count;
            let mut curr_nl_line = 0;
            let mut header_done = vec![false; col_count];
            while more_col_wrapping > 0 {
                if opt_border == 2 {
                    out.extend_from_slice(dformat.leftvrule.as_bytes());
                }
                for i in 0..col_count {
                    if opt_border != 0 || (!format.wrap_right_border && i > 0) {
                        out.extend_from_slice(if curr_nl_line > 0 {
                            format.header_nl_left.as_bytes()
                        } else {
                            b" "
                        });
                    }
                    if header_done[i] {
                        pad(&mut out, max_width[i]);
                    } else {
                        let this_line = &header_lines[i][curr_nl_line];
                        let nbspace = max_width[i] - this_line.width;
                        pad(&mut out, nbspace / 2);
                        out.extend_from_slice(&this_line.bytes);
                        pad(&mut out, nbspace.div_ceil(2));
                        if curr_nl_line + 1 == header_lines[i].len() {
                            more_col_wrapping -= 1;
                            header_done[i] = true;
                        }
                    }
                    if opt_border != 0 || format.wrap_right_border {
                        out.extend_from_slice(if header_done[i] {
                            b" "
                        } else {
                            format.header_nl_right.as_bytes()
                        });
                    }
                    if opt_border != 0 && i + 1 < col_count {
                        out.extend_from_slice(dformat.midvrule.as_bytes());
                    }
                }
                curr_nl_line += 1;
                if opt_border == 2 {
                    out.extend_from_slice(dformat.rightvrule.as_bytes());
                }
                out.push(b'\n');
            }
            print_horizontal_line(&mut out, &max_width, opt_border, Rule::Middle, format);
        }
    }

    // Cells, one loop per row, one display line per pass (`print.c:1022`).
    // `wrap[]` outlives the row, as upstream's does; every column's is back
    // to `None` by the time a row ends.
    let mut wrap = vec![LineWrap::None; col_count];
    // A result with rows but no columns has no cells, and upstream's loop
    // walks cells, so it prints no row at all.
    let rows = if col_count == 0 {
        &[][..]
    } else {
        &row_lines[..]
    };
    for row in rows {
        let mut curr_nl_line = vec![0_usize; col_count];
        loop {
            let mut more_lines = false;
            if opt_border == 2 {
                out.extend_from_slice(dformat.leftvrule.as_bytes());
            }
            for j in 0..col_count {
                let lines = &row[j];
                let mut chars_to_output = max_width[j];
                let finalspaces = opt_border == 2 || j + 1 < col_count;

                // Left-hand newline mark (`print.c:1066`).
                if opt_border != 0 {
                    match wrap[j] {
                        LineWrap::Newline => out.extend_from_slice(format.nl_left.as_bytes()),
                        LineWrap::None => out.push(b' '),
                    }
                }

                match lines.get(curr_nl_line[j]) {
                    // Past this cell's last line: pad for the other columns.
                    None => {
                        if finalspaces {
                            pad(&mut out, chars_to_output);
                        }
                    }
                    Some(this_line) => {
                        chars_to_output = this_line.width;
                        if cont.aligns[j] == Align::Right {
                            pad(&mut out, max_width[j] - chars_to_output);
                        }
                        out.extend_from_slice(&this_line.bytes);
                        curr_nl_line[j] += 1;
                        if curr_nl_line[j] < lines.len() {
                            more_lines = true;
                        }
                    }
                }

                // The next display line's wrap status for this column.
                wrap[j] = if curr_nl_line[j] < lines.len() && curr_nl_line[j] != 0 {
                    LineWrap::Newline
                } else {
                    LineWrap::None
                };

                // Left-aligned cells pad when a column or a mark follows
                // (`print.c:1138`).
                if cont.aligns[j] != Align::Right && (finalspaces || wrap[j] != LineWrap::None) {
                    pad(&mut out, max_width[j] - chars_to_output);
                }

                // Right-hand newline mark.
                if wrap[j] == LineWrap::Newline {
                    out.extend_from_slice(format.nl_right.as_bytes());
                } else if finalspaces {
                    out.push(b' ');
                }

                // Column divider, chosen by the *next* column's state
                // (`print.c:1158`).
                if opt_border != 0 && j + 1 < col_count {
                    let divider = if wrap[j + 1] == LineWrap::Newline {
                        format.midvrule_nl
                    } else if curr_nl_line[j + 1] >= row[j + 1].len() {
                        format.midvrule_blank
                    } else {
                        dformat.midvrule
                    };
                    out.extend_from_slice(divider.as_bytes());
                }
            }
            if opt_border == 2 {
                out.extend_from_slice(dformat.rightvrule.as_bytes());
            }
            out.push(b'\n');
            if !more_lines {
                break;
            }
        }
    }

    if opt.stop_table {
        if opt_border == 2 {
            print_horizontal_line(&mut out, &max_width, opt_border, Rule::Bottom, format);
        }
        if let Some(footer) = cont.default_footer()
            && !opt_tuples_only
        {
            out.extend_from_slice(footer.as_bytes());
            out.push(b'\n');
        }
        out.push(b'\n');
    }

    Ok(out)
}

/// `print_separator()` (`print.c:379`).
fn print_separator(out: &mut Vec<u8>, sep: &crate::settings::Separator) {
    out.extend_from_slice(&sep.bytes());
}

/// `print_unaligned_text()` (`print.c:422`). Cells are written raw: no
/// escaping, and an embedded newline stays a newline.
fn print_unaligned_text(cont: &TableContent<'_>) -> Vec<u8> {
    let opt = cont.opt;
    let opt_tuples_only = opt.tuples_only;
    let mut out = Vec::new();
    let mut need_recordsep = false;

    if opt.start_table {
        if let Some(title) = cont.title
            && !opt_tuples_only
        {
            out.extend_from_slice(title.as_bytes());
            print_separator(&mut out, &opt.record_sep);
        }
        if !opt_tuples_only {
            for (i, header) in cont.headers.iter().enumerate() {
                if i > 0 {
                    print_separator(&mut out, &opt.field_sep);
                }
                out.extend_from_slice(header);
            }
            need_recordsep = true;
        }
    } else {
        // Assume a continuing printout.
        need_recordsep = true;
    }

    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            if need_recordsep {
                print_separator(&mut out, &opt.record_sep);
                need_recordsep = false;
            }
            out.extend_from_slice(cell);
            if i + 1 < row.len() {
                print_separator(&mut out, &opt.field_sep);
            } else {
                need_recordsep = true;
            }
        }
    }

    if opt.stop_table {
        if let Some(footer) = cont.default_footer()
            && !opt_tuples_only
        {
            if need_recordsep {
                print_separator(&mut out, &opt.record_sep);
            }
            out.extend_from_slice(footer.as_bytes());
            need_recordsep = true;
        }
        // The last record ends in a newline whatever the record separator,
        // unless that separator is the zero byte (`print.c:497`).
        if need_recordsep {
            if opt.record_sep.separator_zero {
                print_separator(&mut out, &opt.record_sep);
            } else {
                out.push(b'\n');
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Separator, TableOpt};
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
        // (`print.c:1179`).
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
        // `pg_wcsformat` (`mbprint.c:294`) splits on newlines and
        // `print_aligned_text` marks each continued line with `nl_right`,
        // which is "+" for ascii (`print.c:71`). Writing the newline into the
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
        assert_eq!(widest(&format_cell(b"a\nbbbb")), 4);
        assert_eq!(widest(&format_cell(b"aaaa\nb")), 4);
        let res = result(vec![text_field("t")], vec![vec![Some("a\nb")]]);
        assert_eq!(rendered(&res), " t \n---\n a+\n b\n(1 row)\n\n");
    }

    #[test]
    fn a_multi_line_cell_pads_its_neighbours_on_the_extra_lines() {
        // `print.c:1078`: a column past its last line is blank-padded so the
        // dividers stay aligned.
        let res = result(
            vec![text_field("a"), text_field("b")],
            vec![vec![Some("1\n2"), Some("x")]],
        );
        // The trailing space on the second line is upstream's: the left mark
        // is emitted for every column whenever `border != 0`, including for a
        // column that has run out of lines (`print.c:1066`).
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
    fn a_format_this_slice_does_not_render_is_refused_not_faked() {
        let res = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        for format in [
            PrintFormat::Csv,
            PrintFormat::Html,
            PrintFormat::Wrapped,
            PrintFormat::Latex,
            PrintFormat::LatexLongtable,
            PrintFormat::Asciidoc,
            PrintFormat::TroffMs,
        ] {
            let mut opt = PrintQueryOpt::default();
            opt.topt = TableOpt { format, ..opt.topt };
            let err = print_query(&res, &opt)
                .expect_err("a format this port cannot render must be refused");
            assert_eq!(err, PrintError::Unsupported(Unsupported::Format(format)));
            // The message names the issue that implements it, so the refusal
            // is actionable rather than a bare failure.
            assert!(
                err.to_string().contains("NAT-400"),
                "the refusal must name the issue: {err}"
            );
        }
    }

    #[test]
    fn expanded_unicode_and_numericlocale_are_refused_not_faked() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let refused = |edit: &dyn Fn(&mut PrintQueryOpt)| {
            let mut opt = PrintQueryOpt::default();
            edit(&mut opt);
            print_query(&res, &opt).expect_err("must be refused")
        };
        assert_eq!(
            refused(&|o| o.topt.expanded = Expanded::On),
            PrintError::Unsupported(Unsupported::Expanded)
        );
        assert_eq!(
            refused(&|o| {
                o.topt.expanded = Expanded::On;
                o.topt.format = PrintFormat::Unaligned;
            }),
            PrintError::Unsupported(Unsupported::Expanded)
        );
        // Auto goes vertical only once the table is wider than the target.
        assert_eq!(
            refused(&|o| {
                o.topt.expanded = Expanded::Auto;
                o.topt.columns = 3;
            }),
            PrintError::Unsupported(Unsupported::Expanded)
        );
        assert_eq!(
            refused(&|o| o.topt.line_style = LineStyle::Unicode),
            PrintError::Unsupported(Unsupported::Unicode)
        );
        assert_eq!(
            refused(&|o| o.topt.numeric_locale = true),
            PrintError::Unsupported(Unsupported::NumericLocale)
        );

        // Under `\pset columns 0` C measures the terminal, when stdout is
        // one (`print.c:804`), and this port does not: refused, not guessed.
        assert_eq!(
            refused(&|o| o.topt.expanded = Expanded::Auto),
            PrintError::Unsupported(Unsupported::Expanded)
        );

        // And each is a no-op where upstream's is.
        let mut opt = PrintQueryOpt::default();
        opt.topt.expanded = Expanded::Auto;
        opt.topt.columns = 80;
        assert!(print_query(&res, &opt).is_ok());
        // One column never goes vertical (`print.c:877`), whatever the width.
        let one_column = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        let mut opt = PrintQueryOpt::default();
        opt.topt.expanded = Expanded::Auto;
        assert!(print_query(&one_column, &opt).is_ok());
        let text_only = result(vec![text_field("s")], vec![vec![Some("a")]]);
        let mut opt = PrintQueryOpt::default();
        opt.topt.numeric_locale = true;
        assert!(print_query(&text_only, &opt).is_ok());
    }

    /// Pins the recorded divergence: the walk is UTF-8's whatever the client
    /// encoding. Under `SQL_ASCII` C would count `é` as two columns and print
    /// `C2 85` raw; here they are one column and `\u0085`, as C does under
    /// UTF8. A byte sequence that is not UTF-8 is one column per byte.
    #[test]
    fn a_cell_is_measured_as_utf8_whatever_the_client_encoding() {
        let lines = format_cell("é".as_bytes());
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].width, 1);
        assert_eq!(lines[0].bytes, "é".as_bytes());

        let lines = format_cell(b"\xC2\x85");
        assert_eq!(lines[0].bytes, b"\\u0085");
        assert_eq!(lines[0].width, 6);

        let lines = format_cell(b"\xE9t\xE9");
        assert_eq!(lines[0].bytes, b"\xE9t\xE9");
        assert_eq!(lines[0].width, 3);
    }

    fn with(edit: impl Fn(&mut PrintQueryOpt)) -> PrintQueryOpt {
        let mut opt = PrintQueryOpt::default();
        edit(&mut opt);
        opt
    }

    fn render(res: &QueryResult, opt: &PrintQueryOpt) -> String {
        String::from_utf8(print_query(res, opt).unwrap()).unwrap()
    }

    #[test]
    fn border_zero_drops_the_frame_and_the_outer_padding() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let opt = with(|o| o.topt.border = 0);
        // The header keeps a trailing space: ascii draws its newline marks in
        // the right-hand border even at border 0 (`wrap_right_border`).
        assert_eq!(render(&res, &opt), "n s \n- -\n1 a\n(1 row)\n\n");
    }

    #[test]
    fn border_two_boxes_the_table() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let opt = with(|o| o.topt.border = 2);
        assert_eq!(
            render(&res, &opt),
            "+---+---+\n| n | s |\n+---+---+\n| 1 | a |\n+---+---+\n(1 row)\n\n"
        );
        // Any border above 2 is drawn as 2 (`print.c:675`).
        let opt = with(|o| o.topt.border = 7);
        assert!(render(&res, &opt).starts_with("+---+---+\n"));
    }

    #[test]
    fn a_title_is_centred_over_the_table() {
        let res = result(vec![text_field("column")], vec![vec![Some("a")]]);
        let opt = with(|o| o.title = Some("T".to_string()));
        // width_total is 6 + 2 for border 1: (8 - 1) / 2 = 3 spaces.
        assert!(render(&res, &opt).starts_with("   T\n"));
        let opt = with(|o| o.title = Some("a long title".to_string()));
        assert!(render(&res, &opt).starts_with("a long title\n"));
    }

    #[test]
    fn control_characters_are_escaped_and_tabs_expanded() {
        // `pg_wcsformat` (`mbprint.c:294`).
        assert_eq!(
            format_cell(b"a\rb\x01\tc"),
            vec![Line {
                // Eight columns in, so the tab is a full eight: C's loop is a
                // do-while and always advances at least once.
                bytes: b"a\\rb\\x01        c".to_vec(),
                width: 17,
            }]
        );
        assert_eq!(
            format_cell("\u{85}".as_bytes()),
            vec![Line {
                bytes: b"\\u0085".to_vec(),
                width: 6,
            }]
        );
        assert_eq!(format_cell(b"x\ny").len(), 2);
    }

    #[test]
    fn unaligned_ends_the_last_record_with_a_newline_whatever_the_separator() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")], vec![Some("2"), Some("b")]],
        );
        let sep = |s: &str| Separator {
            separator: Some(s.to_string()),
            separator_zero: false,
        };
        let opt = with(|o| {
            o.topt.format = PrintFormat::Unaligned;
            o.topt.field_sep = sep(",");
            o.topt.record_sep = sep(";");
        });
        assert_eq!(render(&res, &opt), "n,s;1,a;2,b;(2 rows)\n");
        // A zero-byte separator ends it instead (`print.c:497`).
        let opt = with(|o| {
            o.topt.format = PrintFormat::Unaligned;
            o.topt.field_sep = sep(",");
            o.topt.record_sep = Separator {
                separator: None,
                separator_zero: true,
            };
            o.topt.tuples_only = true;
        });
        assert_eq!(print_query(&res, &opt).unwrap(), b"1,a\x002,b\x00");
    }

    #[test]
    fn footer_off_drops_only_the_row_count() {
        let res = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        let opt = with(|o| o.topt.default_footer = false);
        assert_eq!(render(&res, &opt), " n \n---\n 1\n\n");
    }
}
