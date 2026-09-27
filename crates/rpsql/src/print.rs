//! Result rendering: `src/fe_utils/print.c`.
//!
//! Scope, so far. Every output format, normal and expanded: `PRINT_ALIGNED`,
//! `PRINT_WRAPPED` and `PRINT_UNALIGNED` here, at every border (0, 1, 2) in
//! the ascii and old-ascii line styles (`expanded auto` under a `\pset
//! columns` target), and the document formats (csv, html, asciidoc, latex,
//! latex-longtable, troff-ms) in [`markup`], in the ascii, old-ascii and
//! unicode line styles, with `numericlocale`. A table whose layout depends
//! on the terminal's width, which is never read, is
//! [`PrintError::Unsupported`] (see [`Unsupported::TerminalWidth`]), which
//! the caller reports rather than printing something that only looks right.
//!
//! The whole module is a pure function from a result to bytes; nothing here
//! opens a file or a pager.

use rlibpq::QueryResult;

use crate::settings::{Expanded, LineStyle, PrintFormat, PrintQueryOpt, TableOpt, XheaderWidth};

mod markup;
mod wchar;

/// What this port cannot render yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// A layout that depends on the target width while `\pset columns` is 0.
    /// C then takes the terminal's width when stdout is one (`print.c:803`-
    /// `:818`); this port never reads it, so it refuses rather than guess.
    /// That is `wrapped`, `expanded auto` over more than one column, and
    /// `xheader_width page`.
    TerminalWidth,
}

/// A table this port does not render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintError {
    /// A layout this port declines to guess.
    Unsupported(Unsupported),
}

impl std::fmt::Display for PrintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::Unsupported(Unsupported::TerminalWidth) = self;
        f.write_str(
            "a target width taken from the terminal (\\pset columns 0) is not implemented yet (Linear NAT-400)",
        )
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

/// What `setDecimalLocale()` (`print.c:3642`) keeps of `localeconv()` for
/// `\pset numericlocale`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecimalLocale<'a> {
    /// `decimal_point`
    pub decimal_point: &'a str,
    /// `thousands_sep`
    pub thousands_sep: &'a str,
    /// `groupdigits`: digits per thousands group.
    pub groupdigits: usize,
}

impl<'a> DecimalLocale<'a> {
    /// The C (POSIX) locale's: `decimal_point` `"."`, and `thousands_sep`
    /// and `grouping` empty, which `setDecimalLocale` replaces with `","`
    /// and 3.
    ///
    /// It is the only one this port uses, because it cannot call
    /// `localeconv()` (the crate is `deny(unsafe_code)`); see
    /// `docs/divergences.md`. It is what C psql uses too under `LC_ALL=C`,
    /// and under musl whatever the locale, since musl's `localeconv()` is
    /// the C locale's in every locale.
    pub const POSIX: DecimalLocale<'static> = DecimalLocale {
        decimal_point: ".",
        thousands_sep: ",",
        groupdigits: 3,
    };

    /// `setDecimalLocale()`'s calculation over one `struct lconv`'s
    /// `decimal_point`, `thousands_sep` and `grouping`.
    #[must_use]
    pub fn from_lconv(decimal_point: &'a str, thousands_sep: &'a str, grouping: &[u8]) -> Self {
        // "Don't accept an empty decimal_point string".
        let decimal_point = if decimal_point.is_empty() {
            "."
        } else {
            decimal_point
        };
        // Only the first group width counts, and one outside 1..=6
        // (`CHAR_MAX`, however `char` is signed) means 3.
        let groupdigits = match grouping.first() {
            Some(&g @ 1..=6) => usize::from(g),
            _ => 3,
        };
        // An empty `thousands_sep` is replaced by one that cannot be taken
        // for the decimal point.
        let thousands_sep = if !thousands_sep.is_empty() {
            thousands_sep
        } else if decimal_point != "," {
            ","
        } else {
            "."
        };
        Self {
            decimal_point,
            thousands_sep,
            groupdigits,
        }
    }
}

/// `format_numeric_locale()` (`print.c:314`): group the integral digits of
/// a number and localize its decimal point. Anything that does not look
/// like a number — a `money` value already localized, `NaN` — is returned
/// unchanged.
#[must_use]
pub fn format_numeric_locale(cell: &[u8], locale: &DecimalLocale<'_>) -> Vec<u8> {
    if !cell.iter().all(|b| b"0123456789+-.eE".contains(b)) {
        return cell.to_vec();
    }
    let (sign, digits) = match cell.first() {
        Some(b'-' | b'+') => cell.split_at(1),
        _ => cell.split_at(0),
    };
    let int_len = digits.iter().take_while(|b| b.is_ascii_digit()).count();
    let group = locale.groupdigits;

    let mut out = Vec::with_capacity(cell.len() + int_len / group * locale.thousands_sep.len());
    out.extend_from_slice(sign);
    // Digits in the first group.
    let mut leading = match int_len % group {
        0 => group,
        n => n,
    };
    for (i, &digit) in digits[..int_len].iter().enumerate() {
        if i > 0 {
            leading -= 1;
            if leading == 0 {
                out.extend_from_slice(locale.thousands_sep.as_bytes());
                leading = group;
            }
        }
        out.push(digit);
    }
    let mut rest = &digits[int_len..];
    if let Some(fraction) = rest.strip_prefix(b".") {
        out.extend_from_slice(locale.decimal_point.as_bytes());
        rest = fraction;
    }
    // The fraction and the exponent, as they are.
    out.extend_from_slice(rest);
    out
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

/// `unicodeStyleRowFormat` (`print.c:101`): the header rule of one weight.
/// The pairs are indexed by the border's weight.
struct UnicodeRowStyle {
    horizontal: &'static str,
    vertical_and_right: [&'static str; 2],
    vertical_and_left: [&'static str; 2],
}

/// `unicodeStyleColumnFormat` (`print.c:108`): the column rule of one
/// weight. The pairs are indexed by the weight of the rule it crosses.
struct UnicodeColumnStyle {
    vertical: &'static str,
    vertical_and_horizontal: [&'static str; 2],
    up_and_horizontal: [&'static str; 2],
    down_and_horizontal: [&'static str; 2],
}

/// `unicodeStyleBorderFormat` (`print.c:116`): the frame of one weight.
struct UnicodeBorderStyle {
    up_and_right: &'static str,
    vertical: &'static str,
    down_and_right: &'static str,
    horizontal: &'static str,
    down_and_left: &'static str,
    /// Upstream's name for the bottom-right corner, U+2518 / U+255D (up and
    /// left).
    left_and_right: &'static str,
}

/// `unicode_style` (`print.c:140`), indexed by [`UnicodeLinestyle`]: single,
/// then double.
const UNICODE_ROW_STYLE: [UnicodeRowStyle; 2] = [
    UnicodeRowStyle {
        horizontal: "\u{2500}",
        vertical_and_right: ["\u{251C}", "\u{255F}"],
        vertical_and_left: ["\u{2524}", "\u{2562}"],
    },
    UnicodeRowStyle {
        horizontal: "\u{2550}",
        vertical_and_right: ["\u{255E}", "\u{2560}"],
        vertical_and_left: ["\u{2561}", "\u{2563}"],
    },
];

/// See [`UNICODE_ROW_STYLE`].
const UNICODE_COLUMN_STYLE: [UnicodeColumnStyle; 2] = [
    UnicodeColumnStyle {
        vertical: "\u{2502}",
        vertical_and_horizontal: ["\u{253C}", "\u{256A}"],
        up_and_horizontal: ["\u{2534}", "\u{2567}"],
        down_and_horizontal: ["\u{252C}", "\u{2564}"],
    },
    UnicodeColumnStyle {
        vertical: "\u{2551}",
        vertical_and_horizontal: ["\u{256B}", "\u{256C}"],
        up_and_horizontal: ["\u{2568}", "\u{2569}"],
        down_and_horizontal: ["\u{2565}", "\u{2566}"],
    },
];

/// See [`UNICODE_ROW_STYLE`].
const UNICODE_BORDER_STYLE: [UnicodeBorderStyle; 2] = [
    UnicodeBorderStyle {
        up_and_right: "\u{2514}",
        vertical: "\u{2502}",
        down_and_right: "\u{250C}",
        horizontal: "\u{2500}",
        down_and_left: "\u{2510}",
        left_and_right: "\u{2518}",
    },
    UnicodeBorderStyle {
        up_and_right: "\u{255A}",
        vertical: "\u{2551}",
        down_and_right: "\u{2554}",
        horizontal: "\u{2550}",
        down_and_left: "\u{2557}",
        left_and_right: "\u{255D}",
    },
];

/// `refresh_utf8format()` (`print.c:3692`): the unicode line style for the
/// three `unicode_*_linestyle` settings.
///
/// psql keeps one `pg_utf8format` and refreshes it at startup and on every
/// `\pset unicode_*_linestyle` (`startup.c:181`, `command.c:5167`), so it
/// always equals this function of the current settings; computing it when a
/// table is drawn is the same thing without the global.
#[must_use]
pub fn refresh_utf8format(opt: &TableOpt) -> TextFormat {
    let b = opt.unicode_border_linestyle as usize;
    let h = opt.unicode_header_linestyle as usize;
    let border = &UNICODE_BORDER_STYLE[b];
    let header = &UNICODE_ROW_STYLE[h];
    let column = &UNICODE_COLUMN_STYLE[opt.unicode_column_linestyle as usize];
    TextFormat {
        name: "unicode",
        lrule: [
            TextLineFormat {
                hrule: border.horizontal,
                leftvrule: border.down_and_right,
                midvrule: column.down_and_horizontal[b],
                rightvrule: border.down_and_left,
            },
            TextLineFormat {
                hrule: header.horizontal,
                leftvrule: header.vertical_and_right[b],
                midvrule: column.vertical_and_horizontal[h],
                rightvrule: header.vertical_and_left[b],
            },
            TextLineFormat {
                hrule: border.horizontal,
                leftvrule: border.up_and_right,
                midvrule: column.up_and_horizontal[b],
                rightvrule: border.left_and_right,
            },
            TextLineFormat {
                hrule: "",
                leftvrule: border.vertical,
                midvrule: column.vertical,
                rightvrule: border.vertical,
            },
        ],
        midvrule_nl: column.vertical,
        midvrule_wrap: column.vertical,
        midvrule_blank: column.vertical,
        // "Same for all unicode today" (`print.c:3731`): U+21B5 marks a
        // newline and U+2026 a wrap.
        header_nl_left: " ",
        header_nl_right: "\u{21B5}",
        nl_left: " ",
        nl_right: "\u{21B5}",
        wrap_left: "\u{2026}",
        wrap_right: "\u{2026}",
        wrap_right_border: true,
    }
}

/// `get_line_style()` (`print.c:3678`).
fn line_style(opt: &TableOpt) -> TextFormat {
    match opt.line_style {
        LineStyle::Ascii => ASCII_FORMAT,
        LineStyle::OldAscii => ASCII_FORMAT_OLD,
        LineStyle::Unicode => refresh_utf8format(opt),
    }
}

/// `printTableContent` (`print.h:163`), with no translation.
struct TableContent<'a> {
    opt: &'a TableOpt,
    title: Option<&'a str>,
    headers: Vec<Vec<u8>>,
    /// One `Vec` per row, one cell per column.
    cells: Vec<Vec<Vec<u8>>>,
    aligns: Vec<Align>,
    /// `cont->footers`, in `printTableAddFooter` order: empty is `NULL`,
    /// as `printQuery` leaves it unless `opt->footers` has some.
    footers: Vec<Vec<u8>>,
}

impl TableContent<'_> {
    /// The rows a printer walks. Upstream walks `cont->cells` until a NULL,
    /// and a result with rows but no columns has none (`print.c:3194`
    /// allocates `ncolumns * nrows + 1` zeroed cells), so it prints no row at
    /// all, while `(n rows)` still counts them.
    fn rows(&self) -> &[Vec<Vec<u8>>] {
        if self.headers.is_empty() {
            &[]
        } else {
            &self.cells
        }
    }

    /// `footers_with_default()` (`print.c:398`): the footers the caller
    /// added, or else the `(n rows)` line, or nothing under
    /// `\pset footer off`.
    fn footers_with_default(&self) -> Vec<Vec<u8>> {
        if !self.footers.is_empty() || !self.opt.default_footer {
            return self.footers.clone();
        }
        let n = self.cells.len();
        let footer = if n == 1 {
            format!("({n} row)")
        } else {
            format!("({n} rows)")
        };
        vec![footer.into_bytes()]
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
    let null_print = opt.null_print.as_deref().unwrap_or("");
    let cells: Vec<Vec<Vec<u8>>> = (0..result.ntuples())
        .map(|r| {
            (0..result.nfields())
                .map(|c| {
                    if result.is_null(r, c) {
                        null_print.as_bytes().to_vec()
                    } else {
                        let cell = result.value(r, c).unwrap_or(b"");
                        // `print.c:3589`: only a value, and only one that
                        // right-aligns.
                        if opt.topt.numeric_locale && aligns[c] == Align::Right {
                            format_numeric_locale(cell, &DecimalLocale::POSIX)
                        } else {
                            cell.to_vec()
                        }
                    }
                })
                .collect()
        })
        .collect();
    print_content(&TableContent {
        opt: &opt.topt,
        title: opt.title.as_deref(),
        headers,
        cells,
        aligns,
        footers: opt.footers.clone(),
    })
}

/// `printTable()` (`print.c:3444`) of a table its caller built cell by cell
/// with `printTableAddHeader`, `printTableAddCell` and `printTableAddFooter`,
/// as `describeRoles()` and `describePublications()` do: one `Vec` of cells
/// per row, one cell per header, printed as given — no `\pset null`, no
/// `numericlocale`. With no `footers`, the default one is printed if `opt`
/// has it on.
///
/// # Errors
/// As [`print_query`].
pub fn print_table(
    opt: &TableOpt,
    title: Option<&str>,
    headers: &[(&str, Align)],
    cells: Vec<Vec<Vec<u8>>>,
    footers: Vec<Vec<u8>>,
) -> Result<Vec<u8>, PrintError> {
    print_content(&TableContent {
        opt,
        title,
        headers: headers.iter().map(|(h, _)| h.as_bytes().to_vec()).collect(),
        cells,
        aligns: headers.iter().map(|&(_, a)| a).collect(),
        footers,
    })
}

/// `printTable()`'s switch (`print.c:3472`) over the printers.
fn print_content(cont: &TableContent<'_>) -> Result<Vec<u8>, PrintError> {
    // Only `expanded on` (C's `1`)
    // selects a vertical printer here; `auto` is decided inside
    // `print_aligned_text`, since the pager that would force it
    // (`print.c:3488`) is not ported. Every other format prints `auto` as
    // `off`, as upstream's does.
    let vertical = cont.opt.expanded == Expanded::On;
    match cont.opt.format {
        PrintFormat::Unaligned if vertical => Ok(print_unaligned_vertical(cont)),
        PrintFormat::Unaligned => Ok(print_unaligned_text(cont)),
        PrintFormat::Aligned | PrintFormat::Wrapped if vertical => print_aligned_vertical(cont),
        PrintFormat::Aligned | PrintFormat::Wrapped => print_aligned_text(cont),
        PrintFormat::Csv if vertical => Ok(markup::print_csv_vertical(cont)),
        PrintFormat::Csv => Ok(markup::print_csv_text(cont)),
        PrintFormat::Html if vertical => Ok(markup::print_html_vertical(cont)),
        PrintFormat::Html => Ok(markup::print_html_text(cont)),
        PrintFormat::Asciidoc if vertical => Ok(markup::print_asciidoc_vertical(cont)),
        PrintFormat::Asciidoc => Ok(markup::print_asciidoc_text(cont)),
        PrintFormat::Latex | PrintFormat::LatexLongtable if vertical => {
            Ok(markup::print_latex_vertical(cont))
        }
        PrintFormat::Latex => Ok(markup::print_latex_text(cont)),
        PrintFormat::LatexLongtable => Ok(markup::print_latex_longtable_text(cont)),
        PrintFormat::TroffMs if vertical => Ok(markup::print_troff_ms_vertical(cont)),
        PrintFormat::TroffMs => Ok(markup::print_troff_ms_text(cont)),
    }
}

/// The target width, `output_columns` (`print.c:801`-`:818`, `:1440`-
/// `:1458`): `\pset columns`, or 0 for no target.
///
/// Under `\pset columns 0` C reads `$COLUMNS` or `TIOCGWINSZ` when stdout is
/// a terminal and has no target otherwise. This port reads neither, so when
/// `depends` says the layout would change with a target it refuses; when it
/// would not, 0 is exactly C's answer either way.
fn output_columns(opt: &TableOpt, depends: bool) -> Result<usize, PrintError> {
    match usize::try_from(opt.columns) {
        Ok(columns) if columns > 0 => Ok(columns),
        _ if depends => Err(PrintError::Unsupported(Unsupported::TerminalWidth)),
        _ => Ok(0),
    }
}

/// One display line of a cell, as `pg_wcsformat` leaves it in a `lineptr`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    /// The bytes to print, control characters already escaped.
    bytes: Vec<u8>,
    /// `lineptr.width`: display width.
    width: usize,
    /// Whether the cell was walked as UTF-8, so that [`strlen_max_width`]
    /// steps through these bytes the way [`format_cell`] measured them.
    utf8: bool,
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
/// as for a UTF-8 client encoding. Every other character takes the columns
/// [`wchar::ucs_wcwidth`] gives it: 0 for a non-spacing one, 2 for a wide
/// one, 1 otherwise. A cell that is not UTF-8 is walked byte by byte, one
/// column per byte.
///
/// Upstream steps and measures by the client encoding (`PQmblen`, `PQdsplen`,
/// `mbprint.c:304`, `:307`); this walk is UTF-8's whatever it is, because the
/// client encoding is not tracked yet. That too is a recorded divergence.
fn format_cell(cell: &[u8]) -> Vec<Line> {
    fn push_escaped(line: &mut Line, text: &str) {
        line.bytes.extend_from_slice(text.as_bytes());
        line.width += text.len();
    }

    let text = std::str::from_utf8(cell).ok();
    let utf8 = text.is_some();
    let new_line = || Line {
        bytes: Vec::new(),
        width: 0,
        utf8,
    };
    let mut lines = vec![new_line()];
    let push = |c: Option<char>, raw: &[u8], lines: &mut Vec<Line>| {
        let line = lines.last_mut().expect("there is always a current line");
        match raw {
            b"\n" => lines.push(new_line()),
            b"\r" => push_escaped(line, "\\r"),
            b"\t" => loop {
                line.bytes.push(b' ');
                line.width += 1;
                if line.width.is_multiple_of(8) {
                    break;
                }
            },
            [b] if *b < 0x20 || *b == 0x7f => push_escaped(line, &format!("\\x{b:02X}")),
            _ => match c.map(|c| (c, wchar::ucs_wcwidth(u32::from(c)))) {
                // Only C1 is a multibyte control character (`mbprint.c:353`).
                Some((c, None)) => push_escaped(line, &format!("\\u{:04X}", u32::from(c))),
                Some((_, Some(w))) => {
                    line.bytes.extend_from_slice(raw);
                    line.width += w;
                }
                None => {
                    line.bytes.extend_from_slice(raw);
                    line.width += 1;
                }
            },
        }
    };
    if let Some(text) = text {
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

/// `strlen_max_width()` (`print.c:3747`): how many bytes of `bytes` fill at
/// most `target_width` display columns. `target_width` becomes the number of
/// columns actually filled. The first character is taken even when it alone
/// is wider than the target.
///
/// `bytes` is a [`Line`]'s, already escaped, so every character in it is
/// printable and is measured as [`format_cell`] measured it: by
/// [`wchar::ucs_wcwidth`] when `utf8`, one column per byte otherwise. A
/// zero-width character is therefore always taken with the text before it.
fn strlen_max_width(bytes: &[u8], utf8: bool, target_width: &mut usize) -> usize {
    let text = if utf8 {
        std::str::from_utf8(bytes).ok()
    } else {
        None
    };
    let mut pos = 0;
    let mut curr_width = 0;
    while pos < bytes.len() {
        // `PQdsplen` and `PQmblen`.
        let (char_width, len) = match text.and_then(|t| t[pos..].chars().next()) {
            // A control character was escaped, so `None` cannot occur here.
            Some(c) => (wchar::ucs_wcwidth(u32::from(c)).unwrap_or(0), c.len_utf8()),
            None => (1, 1),
        };
        if *target_width < curr_width + char_width && curr_width != 0 {
            break;
        }
        curr_width += char_width;
        pos += len;
    }
    *target_width = curr_width;
    pos
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
/// continues its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineWrap {
    /// `PRINT_LINE_WRAP_NONE`
    None,
    /// `PRINT_LINE_WRAP_WRAP`
    Wrap,
    /// `PRINT_LINE_WRAP_NEWLINE`
    Newline,
}

/// `print_aligned_text()` (`print.c:635`), for `PRINT_ALIGNED` and
/// `PRINT_WRAPPED`.
///
/// The pager is not ported, so neither is the pager arithmetic. The target
/// width `output_columns` matters to the wrapped format, which squeezes the
/// columns to fit it, and to `expanded auto`, which escapes to
/// [`print_aligned_vertical`] when the table is wider; see [`output_columns`]
/// for why `\pset columns 0` refuses both.
// One function, as upstream's is, so a reader can follow the two side by side.
#[allow(clippy::too_many_lines)]
fn print_aligned_text(cont: &TableContent<'_>) -> Result<Vec<u8>, PrintError> {
    let opt = cont.opt;
    let opt_tuples_only = opt.tuples_only;
    let format = &line_style(opt);
    let dformat = &format.lrule[Rule::Data as usize];
    let opt_border = opt.border.min(2);
    let col_count = cont.headers.len();
    let mut out = Vec::new();

    // Scan every header and cell for the widest line, and sum each column's
    // widths for its average (`print.c:710`).
    let header_lines: Vec<Vec<Line>> = cont.headers.iter().map(|h| format_cell(h)).collect();
    let width_header: Vec<usize> = header_lines.iter().map(|l| widest(l)).collect();
    let mut max_width = width_header.clone();
    let mut width_average = vec![0_usize; col_count];
    let row_lines: Vec<Vec<Vec<Line>>> = cont
        .cells
        .iter()
        .map(|row| row.iter().map(|cell| format_cell(cell)).collect())
        .collect();
    for row in &row_lines {
        for (i, lines) in row.iter().enumerate() {
            let width = widest(lines);
            max_width[i] = max_width[i].max(width);
            width_average[i] += width;
        }
    }
    // If we have rows, compute the average (`print.c:754`).
    if col_count != 0 && !row_lines.is_empty() {
        for average in &mut width_average {
            *average /= row_lines.len();
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

    // No word wrap by default: every column as wide as its widest line.
    let mut width_wrap = max_width.clone();

    let wrapped = opt.format == PrintFormat::Wrapped;
    let auto = opt.expanded == Expanded::Auto && col_count > 1;
    let output_columns = output_columns(opt, wrapped || auto)?;

    // The wrapped format shrinks, one column at a time, the column with the
    // highest ratio of its width to its average width, slightly biased
    // against wide ones, until the table fits; never below a header
    // (`print.c:820`).
    if wrapped && output_columns > 0 && output_columns >= total_header_width {
        while width_total > output_columns {
            let mut max_ratio = 0.0;
            let mut worst_col = None;
            for i in 0..col_count {
                if width_average[i] != 0 && width_wrap[i] > width_header[i] {
                    // C's `(double)` arithmetic; a width fits a double exactly.
                    #[allow(clippy::cast_precision_loss)]
                    let ratio =
                        width_wrap[i] as f64 / width_average[i] as f64 + max_width[i] as f64 * 0.01;
                    if ratio > max_ratio {
                        max_ratio = ratio;
                        worst_col = Some(i);
                    }
                }
            }
            // Nothing left to squeeze.
            let Some(worst_col) = worst_col else {
                break;
            };
            width_wrap[worst_col] -= 1;
            width_total -= 1;
        }
    }

    // Expanded auto escapes to vertical when the table is wider than the
    // target and has more than one column (`print.c:877`).
    if auto
        && output_columns > 0
        && (output_columns < total_header_width || output_columns < width_total)
    {
        return print_aligned_vertical(cont);
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
        // (`print.c:952`). A header is never wrapped.
        if !opt_tuples_only {
            if opt_border == 2 {
                print_horizontal_line(&mut out, &width_wrap, opt_border, Rule::Top, format);
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
                        pad(&mut out, width_wrap[i]);
                    } else {
                        let this_line = &header_lines[i][curr_nl_line];
                        let nbspace = width_wrap[i] - this_line.width;
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
            print_horizontal_line(&mut out, &width_wrap, opt_border, Rule::Middle, format);
        }
    }

    // Cells, one loop per row, one display line per pass (`print.c:1022`).
    // A display line holds a whole line of a cell, or as much of it as
    // `width_wrap` has room for. `wrap[]` outlives the row, as upstream's
    // does; every column's is back to `None` by the time a row ends.
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
        let mut bytes_output = vec![0_usize; col_count];
        loop {
            let mut more_lines = false;
            if opt_border == 2 {
                out.extend_from_slice(dformat.leftvrule.as_bytes());
            }
            for j in 0..col_count {
                let lines = &row[j];
                let mut chars_to_output = width_wrap[j];
                let finalspaces = opt_border == 2 || j + 1 < col_count;

                // Left-hand wrap or newline mark (`print.c:1066`).
                if opt_border != 0 {
                    match wrap[j] {
                        LineWrap::Wrap => out.extend_from_slice(format.wrap_left.as_bytes()),
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
                        let rest = &this_line.bytes[bytes_output[j]..];
                        let bytes_to_output =
                            strlen_max_width(rest, this_line.utf8, &mut chars_to_output);
                        // A single character wider than the column is
                        // printed as if it fitted (`print.c:1095`).
                        chars_to_output = chars_to_output.min(width_wrap[j]);
                        if cont.aligns[j] == Align::Right {
                            pad(&mut out, width_wrap[j] - chars_to_output);
                        }
                        out.extend_from_slice(&rest[..bytes_to_output]);
                        bytes_output[j] += bytes_to_output;

                        if bytes_output[j] < this_line.bytes.len() {
                            // More of this line to wrap.
                            more_lines = true;
                        } else {
                            // Advance to the cell's next line.
                            curr_nl_line[j] += 1;
                            if curr_nl_line[j] < lines.len() {
                                more_lines = true;
                            }
                            bytes_output[j] = 0;
                        }
                    }
                }

                // The next display line's wrap status for this column
                // (`print.c:1127`).
                wrap[j] = if curr_nl_line[j] >= lines.len() {
                    LineWrap::None
                } else if bytes_output[j] != 0 {
                    LineWrap::Wrap
                } else if curr_nl_line[j] != 0 {
                    LineWrap::Newline
                } else {
                    LineWrap::None
                };

                // Left-aligned cells pad when a column or a mark follows
                // (`print.c:1141`).
                if cont.aligns[j] != Align::Right && (finalspaces || wrap[j] != LineWrap::None) {
                    pad(&mut out, width_wrap[j] - chars_to_output);
                }

                // Right-hand wrap or newline mark.
                match wrap[j] {
                    LineWrap::Wrap => out.extend_from_slice(format.wrap_right.as_bytes()),
                    LineWrap::Newline => out.extend_from_slice(format.nl_right.as_bytes()),
                    LineWrap::None if finalspaces => out.push(b' '),
                    LineWrap::None => {}
                }

                // Column divider, chosen by the *next* column's state
                // (`print.c:1158`).
                if opt_border != 0 && j + 1 < col_count {
                    let divider = match wrap[j + 1] {
                        LineWrap::Wrap => format.midvrule_wrap,
                        LineWrap::Newline => format.midvrule_nl,
                        LineWrap::None if curr_nl_line[j + 1] >= row[j + 1].len() => {
                            format.midvrule_blank
                        }
                        LineWrap::None => dformat.midvrule,
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
            print_horizontal_line(&mut out, &width_wrap, opt_border, Rule::Bottom, format);
        }
        if !opt_tuples_only {
            for footer in cont.footers_with_default() {
                out.extend_from_slice(&footer);
                out.push(b'\n');
            }
        }
        out.push(b'\n');
    }

    Ok(out)
}

/// `print_aligned_vertical_line()` (`print.c:1225`): the rule above a record,
/// `[ RECORD n ]` in it unless `record` is 0, or the rule under the last.
///
/// It reads `topt->border` unclamped, where its caller clamps its own copy to
/// 2, so a border above 2 draws a record line with no frame; that is kept.
#[allow(clippy::too_many_arguments)]
fn print_aligned_vertical_line(
    out: &mut Vec<u8>,
    opt: &TableOpt,
    format: &TextFormat,
    record: usize,
    hwidth: usize,
    dwidth: usize,
    output_columns: usize,
    pos: Rule,
) {
    /// C's `reclen-- <= 0`.
    fn post_decrement_le_zero(reclen: &mut i64) -> bool {
        let was = *reclen;
        *reclen -= 1;
        was <= 0
    }

    let lformat = &format.lrule[pos as usize];
    let opt_border = opt.border;
    let xheader = opt.expanded_header_width;
    let hrule = lformat.hrule.as_bytes();
    let fill = if opt_border > 0 { hrule } else { b" " };
    let hwidth = i64::try_from(hwidth).unwrap_or(i64::MAX);
    let mut dwidth = i64::try_from(dwidth).unwrap_or(i64::MAX);

    if opt_border == 2 {
        out.extend_from_slice(lformat.leftvrule.as_bytes());
        out.extend_from_slice(hrule);
    } else if opt_border == 1 {
        out.extend_from_slice(hrule);
    }

    let mut reclen: i64 = 0;
    if record != 0 {
        let text = if opt_border == 0 {
            format!("* Record {record}")
        } else {
            format!("[ RECORD {record} ]")
        };
        out.extend_from_slice(text.as_bytes());
        reclen = i64::try_from(text.len()).unwrap_or(i64::MAX);
    }
    if opt_border != 2 {
        reclen += 1;
    }
    for _ in reclen..hwidth {
        out.extend_from_slice(fill);
    }
    reclen -= hwidth;

    if opt_border > 0 {
        if post_decrement_le_zero(&mut reclen) {
            out.extend_from_slice(hrule);
        }
        if post_decrement_le_zero(&mut reclen) {
            out.extend_from_slice(if xheader == XheaderWidth::Column {
                lformat.rightvrule.as_bytes()
            } else {
                lformat.midvrule.as_bytes()
            });
        }
        if post_decrement_le_zero(&mut reclen) && xheader != XheaderWidth::Column {
            out.extend_from_slice(hrule);
        }
    } else if post_decrement_le_zero(&mut reclen) {
        out.push(b' ');
    }

    if xheader != XheaderWidth::Column {
        let target = match xheader {
            XheaderWidth::Page => Some(i64::try_from(output_columns).unwrap_or(i64::MAX)),
            XheaderWidth::ExactWidth(width) => Some(i64::from(width)),
            XheaderWidth::Full | XheaderWidth::Column => None,
        };
        if let Some(target) = target
            && target > 0
        {
            // The frame around the data at each border; 2 is kept "for
            // consistency" though its right border makes it meaningless
            // (`print.c:1299`).
            let frame = match opt_border {
                0 => Some(0),
                1 => Some(3),
                2 => Some(7),
                _ => None,
            };
            if let Some(frame) = frame {
                dwidth = dwidth.min((target - hwidth - frame).max(0));
            }
        }
        reclen = reclen.max(0);
        dwidth = dwidth.max(reclen);
        for _ in reclen..dwidth {
            out.extend_from_slice(fill);
        }
        if opt_border == 2 {
            out.extend_from_slice(hrule);
            out.extend_from_slice(lformat.rightvrule.as_bytes());
        }
    }
    out.push(b'\n');
}

/// `print_aligned_vertical()` (`print.c:1324`): expanded output, one
/// `header | value` line per cell, each record under a `[ RECORD n ]` rule.
///
/// It prints `cont->footers`, which `printQuery` leaves `NULL` unless
/// `opt->footers` has some, so a query result has no footer but those and
/// the `(0 rows)` of an empty one. Record numbers
/// start at 1: `prior_records` belongs to `FETCH_COUNT`, which is not ported.
#[allow(clippy::too_many_lines)]
fn print_aligned_vertical(cont: &TableContent<'_>) -> Result<Vec<u8>, PrintError> {
    let opt = cont.opt;
    let opt_tuples_only = opt.tuples_only;
    let opt_border = opt.border.min(2);
    let format = &line_style(opt);
    let dformat = &format.lrule[Rule::Data as usize];
    let old_ascii = opt.line_style == LineStyle::OldAscii;
    let col_count = cont.headers.len();
    let wrapped = opt.format == PrintFormat::Wrapped;
    let mut out = Vec::new();

    // No cells at all: just the footer (`print.c:1354`).
    if (col_count == 0 || cont.cells.is_empty()) && opt.start_table && opt.stop_table {
        if !opt_tuples_only {
            for footer in cont.footers_with_default() {
                out.extend_from_slice(&footer);
                out.push(b'\n');
            }
        }
        out.push(b'\n');
        return Ok(out);
    }

    // The widest and tallest header, and the widest and tallest cell
    // (`print.c:1383`).
    let header_lines: Vec<Vec<Line>> = cont.headers.iter().map(|h| format_cell(h)).collect();
    let row_lines: Vec<Vec<Vec<Line>>> = cont
        .cells
        .iter()
        .map(|row| row.iter().map(|cell| format_cell(cell)).collect())
        .collect();
    let hwidth = header_lines.iter().map(|l| widest(l)).max().unwrap_or(0);
    let hmultiline = header_lines.iter().any(|l| l.len() > 1);
    let data = || row_lines.iter().flatten();
    let mut dwidth = data().map(|l| widest(l)).max().unwrap_or(0);
    let mut dmultiline = data().any(|l| l.len() > 1);

    if opt.start_table
        && let Some(title) = cont.title
        && !opt_tuples_only
    {
        out.extend_from_slice(title.as_bytes());
        out.push(b'\n');
    }

    let output_columns = output_columns(
        opt,
        wrapped || opt.expanded_header_width == XheaderWidth::Page,
    )?;

    // The data column's width: fit the target in wrapped mode, or line up
    // with the record header lines in aligned mode (`print.c:1460`).
    let mut swidth = match opt_border {
        // One space in the middle, and one for header newline marks.
        0 => 1 + usize::from(hmultiline),
        // Two spaces and a vrule, and one for old-ascii's left header marks.
        1 => 3 + usize::from(hmultiline && old_ascii),
        // Both vrules and their spacers, which double as the marks.
        _ => 7,
    };
    // A column for data newline marks, too, if needed.
    if dmultiline && opt_border < 2 && !old_ascii {
        swidth += 1;
    }
    // The width the record header lines need.
    let mut rwidth = 0;
    if !opt_tuples_only {
        let nrows = cont.cells.len();
        if nrows > 0 {
            rwidth = 1 + nrows.ilog10() as usize;
        }
        rwidth += match opt_border {
            0 => 9,  // "* RECORD "
            1 => 12, // "-[ RECORD  ]"
            _ => 15, // "+-[ RECORD  ]-+"
        };
    }
    // Twice, if wrapping turns out to need a mark column.
    loop {
        let width = (hwidth + swidth + dwidth).max(rwidth);
        let newdwidth = if wrapped && output_columns > 0 {
            // At least room for three columns of data, and for the record
            // header lines.
            let min_width = (hwidth + swidth + 3).max(rwidth);
            if output_columns >= width {
                width - hwidth - swidth
            } else if output_columns < min_width {
                min_width - hwidth - swidth
            } else {
                output_columns - hwidth - swidth
            }
        } else {
            width - hwidth - swidth
        };
        if newdwidth < dwidth && !dmultiline && opt_border < 2 && !old_ascii {
            dmultiline = true;
            swidth += 1;
        } else {
            dwidth = newdwidth;
            break;
        }
    }

    // The records (`print.c:1588`).
    let mut record = 1;
    for (r, row) in row_lines.iter().enumerate() {
        for (c, dlines) in row.iter().enumerate() {
            let hlines = &header_lines[c];
            let pos = if r == 0 && c == 0 {
                Rule::Top
            } else {
                Rule::Middle
            };

            // The record header above each record.
            if c == 0 {
                let lhwidth = hwidth + usize::from(opt_border < 2 && hmultiline && old_ascii);
                if !opt_tuples_only {
                    print_aligned_vertical_line(
                        &mut out,
                        opt,
                        format,
                        record,
                        lhwidth,
                        dwidth,
                        output_columns,
                        pos,
                    );
                    record += 1;
                } else if r != 0 || !opt.start_table || opt_border == 2 {
                    print_aligned_vertical_line(
                        &mut out,
                        opt,
                        format,
                        0,
                        lhwidth,
                        dwidth,
                        output_columns,
                        pos,
                    );
                }
            }

            // Header and data in parallel, newline by newline and wrap by
            // wrap, until both are exhausted.
            let mut hline = 0;
            let mut dline = 0;
            let mut hcomplete = false;
            let mut dcomplete = false;
            let mut offset = 0;
            let mut chars_to_output = dlines[0].width;
            while !dcomplete || !hcomplete {
                if opt_border == 2 {
                    out.extend_from_slice(dformat.leftvrule.as_bytes());
                }

                // The header: never wrapped, so only newlines to handle.
                if hcomplete {
                    let mut swidth = hwidth + usize::from(opt_border);
                    if opt_border < 2 && hmultiline && old_ascii {
                        swidth += 1;
                    }
                    if opt_border == 0 && !old_ascii && hmultiline {
                        swidth += 1;
                    }
                    // `"%*s", swidth, " "`: at least the one space.
                    pad(&mut out, swidth.max(1));
                } else {
                    if opt_border == 2 || (hmultiline && old_ascii) {
                        out.extend_from_slice(if hline > 0 {
                            format.header_nl_left.as_bytes()
                        } else {
                            b" "
                        });
                    }
                    let this_line = &hlines[hline];
                    out.extend_from_slice(&this_line.bytes);
                    pad(&mut out, hwidth - this_line.width);
                    let marks = opt_border > 0 || (hmultiline && !old_ascii);
                    if hline + 1 < hlines.len() {
                        // More lines after this one, after a newline.
                        if marks {
                            out.extend_from_slice(format.header_nl_right.as_bytes());
                        }
                        hline += 1;
                    } else {
                        if marks {
                            out.push(b' ');
                        }
                        hcomplete = true;
                    }
                }

                // The separator.
                if opt_border > 0 {
                    out.extend_from_slice(if offset != 0 {
                        format.midvrule_wrap.as_bytes()
                    } else if dline == 0 {
                        dformat.midvrule.as_bytes()
                    } else {
                        format.midvrule_nl.as_bytes()
                    });
                }

                // The data.
                if dcomplete {
                    // Out of data before the header ran out of lines.
                    if opt_border < 2 {
                        out.push(b'\n');
                    } else {
                        pad(&mut out, dwidth + 2);
                        out.extend_from_slice(dformat.rightvrule.as_bytes());
                        out.push(b'\n');
                    }
                    continue;
                }
                out.extend_from_slice(if offset == 0 {
                    b" "
                } else {
                    format.wrap_left.as_bytes()
                });
                let this_line = &dlines[dline];
                let rest = &this_line.bytes[offset..];
                let mut target_width = dwidth;
                let bytes_to_output = strlen_max_width(rest, this_line.utf8, &mut target_width);
                out.extend_from_slice(&rest[..bytes_to_output]);
                // What each call takes adds up to the line's width, so this
                // never goes below 0; it reaches 0 at the end of the line, or
                // early where a zero-width character follows one wider than
                // `dwidth`, which C then drops as this does.
                chars_to_output = chars_to_output.saturating_sub(target_width);
                offset += bytes_to_output;
                let spacer = dwidth.saturating_sub(target_width);
                let marks = opt_border > 1 || (dmultiline && !old_ascii);
                if chars_to_output != 0 {
                    // Continuing a wrapped line.
                    if marks {
                        pad(&mut out, spacer);
                        out.extend_from_slice(format.wrap_right.as_bytes());
                    }
                } else if dline + 1 < dlines.len() {
                    // Reached a newline in the cell.
                    if marks {
                        pad(&mut out, spacer);
                        out.extend_from_slice(format.nl_right.as_bytes());
                    }
                    dline += 1;
                    offset = 0;
                    chars_to_output = dlines[dline].width;
                } else {
                    // Reached the end of the cell.
                    if opt_border > 1 {
                        pad(&mut out, spacer);
                        out.push(b' ');
                    }
                    dcomplete = true;
                }
                if opt_border == 2 {
                    out.extend_from_slice(dformat.rightvrule.as_bytes());
                }
                out.push(b'\n');
            }
        }
    }

    if opt.stop_table {
        if opt_border == 2 {
            print_aligned_vertical_line(
                &mut out,
                opt,
                format,
                0,
                hwidth,
                dwidth,
                output_columns,
                Rule::Bottom,
            );
        }
        // `cont->footers`, not `footers_with_default()` (`print.c:1812`).
        if !opt_tuples_only && !cont.footers.is_empty() {
            if opt_border < 2 {
                out.push(b'\n');
            }
            for footer in &cont.footers {
                out.extend_from_slice(footer);
                out.push(b'\n');
            }
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
        if !opt_tuples_only {
            for footer in cont.footers_with_default() {
                if need_recordsep {
                    print_separator(&mut out, &opt.record_sep);
                }
                out.extend_from_slice(&footer);
                need_recordsep = true;
            }
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

/// `print_unaligned_vertical()` (`print.c:513`): one `header<fieldsep>value`
/// per cell, records apart by two record separators. Cells are written raw.
///
/// It prints `cont->footers`, not `footers_with_default()`, so a query
/// result has no footer but `opt->footers`, not even the default one.
fn print_unaligned_vertical(cont: &TableContent<'_>) -> Vec<u8> {
    let opt = cont.opt;
    let mut out = Vec::new();
    let mut need_recordsep = false;

    if opt.start_table {
        if let Some(title) = cont.title
            && !opt.tuples_only
        {
            out.extend_from_slice(title.as_bytes());
            need_recordsep = true;
        }
    } else {
        // Assume a continuing printout.
        need_recordsep = true;
    }

    for row in &cont.cells {
        for (i, cell) in row.iter().enumerate() {
            if need_recordsep {
                // Two record separators between records in this mode.
                print_separator(&mut out, &opt.record_sep);
                print_separator(&mut out, &opt.record_sep);
                need_recordsep = false;
            }
            out.extend_from_slice(&cont.headers[i]);
            print_separator(&mut out, &opt.field_sep);
            out.extend_from_slice(cell);
            if i + 1 < row.len() {
                print_separator(&mut out, &opt.record_sep);
            } else {
                need_recordsep = true;
            }
        }
    }

    if opt.stop_table {
        // `print.c:562`: a separator, then one before each footer.
        if !opt.tuples_only && !cont.footers.is_empty() {
            print_separator(&mut out, &opt.record_sep);
            for footer in &cont.footers {
                print_separator(&mut out, &opt.record_sep);
                out.extend_from_slice(footer);
            }
        }
        // As in `print_unaligned_text` (`print.c:575`).
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
    use crate::settings::{Separator, TableOpt, UnicodeLinestyle};
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
    fn a_wide_character_takes_two_columns_and_a_combining_one_none() {
        // `ucs_wcwidth` (`wchar.c:646`) through `pg_wcssize`: `中文` is four
        // columns wide and `é` spelt with a combining acute is one, so the
        // column is four wide and `h` centres in it as over `abcd`.
        let res = result(
            vec![text_field("h"), text_field("e")],
            vec![vec![Some("中文"), Some("e\u{301}")]],
        );
        assert_eq!(
            rendered(&res),
            "  h   | e \n------+---\n 中文 | e\u{301}\n(1 row)\n\n"
        );
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
    fn the_terminal_width_is_refused_not_faked() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let refused = |edit: &dyn Fn(&mut PrintQueryOpt)| {
            let mut opt = PrintQueryOpt::default();
            edit(&mut opt);
            print_query(&res, &opt).expect_err("must be refused")
        };
        // Under `\pset columns 0` C measures the terminal, when stdout is
        // one (`print.c:803`), and this port does not: whatever would change
        // with that width is refused, not guessed.
        let terminal = PrintError::Unsupported(Unsupported::TerminalWidth);
        assert_eq!(refused(&|o| o.topt.format = PrintFormat::Wrapped), terminal);
        assert_eq!(refused(&|o| o.topt.expanded = Expanded::Auto), terminal);
        assert_eq!(
            refused(&|o| {
                o.topt.expanded = Expanded::On;
                o.topt.format = PrintFormat::Wrapped;
            }),
            terminal
        );
        assert_eq!(
            refused(&|o| {
                o.topt.expanded = Expanded::On;
                o.topt.expanded_header_width = XheaderWidth::Page;
            }),
            terminal
        );

        // And each is a no-op where upstream's is.
        let ok = |edit: &dyn Fn(&mut PrintQueryOpt)| {
            let mut opt = PrintQueryOpt::default();
            edit(&mut opt);
            assert!(print_query(&res, &opt).is_ok());
        };
        ok(&|o| o.topt.expanded = Expanded::On);
        ok(&|o| {
            o.topt.expanded = Expanded::On;
            o.topt.expanded_header_width = XheaderWidth::ExactWidth(10);
        });
        ok(&|o| {
            o.topt.expanded = Expanded::On;
            o.topt.format = PrintFormat::Unaligned;
        });
        // Auto never applies to the unaligned format (`print.c:3475`).
        ok(&|o| {
            o.topt.expanded = Expanded::Auto;
            o.topt.format = PrintFormat::Unaligned;
        });
        // One column never goes vertical (`print.c:877`), whatever the width.
        let one_column = result(vec![int4_field("n")], vec![vec![Some("1")]]);
        let mut opt = PrintQueryOpt::default();
        opt.topt.expanded = Expanded::Auto;
        assert!(print_query(&one_column, &opt).is_ok());
        // The message names the issue that owns it, so the refusal is
        // actionable rather than a bare failure.
        let message = refused(&|o| o.topt.format = PrintFormat::Wrapped).to_string();
        assert!(message.contains("NAT-400"), "{message}");
    }

    #[test]
    fn unicode_draws_the_frame_in_box_drawing_characters() {
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a\nb")]],
        );
        let unicode = |edit: &dyn Fn(&mut PrintQueryOpt)| {
            render(
                &res,
                &with(|o| {
                    o.topt.line_style = LineStyle::Unicode;
                    edit(o);
                }),
            )
        };
        // Border 1: a header rule and a column rule, and U+21B5 where ascii
        // has its `+` newline mark.
        assert_eq!(
            unicode(&|_| {}),
            " n \u{2502} s \n\
             \u{2500}\u{2500}\u{2500}\u{253C}\u{2500}\u{2500}\u{2500}\n \
             1 \u{2502} a\u{21B5}\n   \u{2502} b\n(1 row)\n\n"
        );
        // Border 2, all single: ┌─┬─┐ ├─┼─┤ └─┴─┘.
        assert_eq!(
            unicode(&|o| o.topt.border = 2),
            "\u{250C}\u{2500}\u{2500}\u{2500}\u{252C}\u{2500}\u{2500}\u{2500}\u{2510}\n\
             \u{2502} n \u{2502} s \u{2502}\n\
             \u{251C}\u{2500}\u{2500}\u{2500}\u{253C}\u{2500}\u{2500}\u{2500}\u{2524}\n\
             \u{2502} 1 \u{2502} a\u{21B5}\u{2502}\n\
             \u{2502}   \u{2502} b \u{2502}\n\
             \u{2514}\u{2500}\u{2500}\u{2500}\u{2534}\u{2500}\u{2500}\u{2500}\u{2518}\n\
             (1 row)\n\n"
        );
        // A double border with single columns and header: each junction is
        // the one where those two weights meet, ╤ ╟ ┼ ╢ ╧.
        assert_eq!(
            unicode(&|o| {
                o.topt.border = 2;
                o.topt.unicode_border_linestyle = UnicodeLinestyle::Double;
            }),
            "\u{2554}\u{2550}\u{2550}\u{2550}\u{2564}\u{2550}\u{2550}\u{2550}\u{2557}\n\
             \u{2551} n \u{2502} s \u{2551}\n\
             \u{255F}\u{2500}\u{2500}\u{2500}\u{253C}\u{2500}\u{2500}\u{2500}\u{2562}\n\
             \u{2551} 1 \u{2502} a\u{21B5}\u{2551}\n\
             \u{2551}   \u{2502} b \u{2551}\n\
             \u{255A}\u{2550}\u{2550}\u{2550}\u{2567}\u{2550}\u{2550}\u{2550}\u{255D}\n\
             (1 row)\n\n"
        );
    }

    #[test]
    fn refresh_utf8format_picks_each_junction_by_the_weights_that_meet_there() {
        let mut opt = TableOpt::default();
        let single = refresh_utf8format(&opt);
        assert_eq!(single.name, "unicode");
        assert_eq!(single.lrule[Rule::Middle as usize].midvrule, "\u{253C}");
        // Double columns under a single header: ╫, crossing a double
        // header: ╬; the top and bottom junctions follow the border.
        opt.unicode_column_linestyle = UnicodeLinestyle::Double;
        let f = refresh_utf8format(&opt);
        assert_eq!(f.lrule[Rule::Middle as usize].midvrule, "\u{256B}");
        assert_eq!(f.lrule[Rule::Top as usize].midvrule, "\u{2565}");
        assert_eq!(f.lrule[Rule::Data as usize].midvrule, "\u{2551}");
        assert_eq!(f.midvrule_wrap, "\u{2551}");
        opt.unicode_header_linestyle = UnicodeLinestyle::Double;
        let f = refresh_utf8format(&opt);
        assert_eq!(f.lrule[Rule::Middle as usize].hrule, "\u{2550}");
        assert_eq!(f.lrule[Rule::Middle as usize].midvrule, "\u{256C}");
        assert_eq!(f.lrule[Rule::Middle as usize].leftvrule, "\u{255E}");
        opt.unicode_border_linestyle = UnicodeLinestyle::Double;
        let f = refresh_utf8format(&opt);
        assert_eq!(f.lrule[Rule::Top as usize].midvrule, "\u{2566}");
        assert_eq!(f.lrule[Rule::Bottom as usize].midvrule, "\u{2569}");
        assert_eq!(f.lrule[Rule::Middle as usize].leftvrule, "\u{2560}");
    }

    #[test]
    fn format_numeric_locale_groups_the_integral_digits() {
        let posix = |s: &str| {
            String::from_utf8(format_numeric_locale(s.as_bytes(), &DecimalLocale::POSIX)).unwrap()
        };
        assert_eq!(posix("0"), "0");
        assert_eq!(posix("999"), "999");
        assert_eq!(posix("1000"), "1,000");
        assert_eq!(posix("-1234567"), "-1,234,567");
        assert_eq!(posix("+123456.789"), "+123,456.789");
        assert_eq!(posix("1234e+90"), "1,234e+90");
        assert_eq!(posix(".5"), ".5");
        assert_eq!(posix(""), "");
        // Not a number: left alone, which is what keeps a localized `money`
        // value intact (`print.c:325`).
        assert_eq!(posix("$1,234.00"), "$1,234.00");
        assert_eq!(posix("NaN"), "NaN");
        assert_eq!(posix("-Infinity"), "-Infinity");

        let de = DecimalLocale::from_lconv(",", ".", &[3, 3]);
        assert_eq!(
            format_numeric_locale(b"-1234567.25", &de),
            b"-1.234.567,25".to_vec()
        );
        let indian = DecimalLocale::from_lconv(".", ",", &[2]);
        assert_eq!(
            format_numeric_locale(b"12345", &indian),
            b"1,23,45".to_vec()
        );
    }

    #[test]
    fn set_decimal_locale_fills_in_what_the_locale_leaves_empty() {
        assert_eq!(
            DecimalLocale::from_lconv(".", "", b""),
            DecimalLocale::POSIX
        );
        assert_eq!(DecimalLocale::from_lconv("", "", b""), DecimalLocale::POSIX);
        // An empty separator never matches a comma decimal point.
        assert_eq!(DecimalLocale::from_lconv(",", "", b"").thousands_sep, ".");
        // `CHAR_MAX`, signed or not, and 0 mean 3.
        for grouping in [[0x7f_u8], [0xff], [0], [7]] {
            assert_eq!(
                DecimalLocale::from_lconv(".", ",", &grouping).groupdigits,
                3
            );
        }
        assert_eq!(DecimalLocale::from_lconv(".", ",", &[4]).groupdigits, 4);
    }

    #[test]
    fn numericlocale_rewrites_only_right_aligned_values() {
        let res = result(
            vec![int4_field("n"), text_field("s"), int4_field("z")],
            vec![vec![Some("1234"), Some("5678"), None]],
        );
        let opt = with(|o| {
            o.topt.numeric_locale = true;
            o.null_print = Some("9999".to_string());
            o.topt.format = PrintFormat::Unaligned;
            o.topt.field_sep.separator = Some("|".to_string());
            o.topt.record_sep.separator = Some("\n".to_string());
        });
        // The text column and the null string are printed as they are.
        assert_eq!(render(&res, &opt), "n|s|z\n1,234|5678|9999\n(1 row)\n");
    }

    #[test]
    fn strlen_max_width_counts_display_columns() {
        // Two wide characters: one fits in three columns, and the first is
        // taken even when it alone is wider than the target.
        let mut target = 3;
        assert_eq!(strlen_max_width("中文".as_bytes(), true, &mut target), 3);
        assert_eq!(target, 2);
        let mut target = 1;
        assert_eq!(strlen_max_width("中文".as_bytes(), true, &mut target), 3);
        assert_eq!(target, 2);
        // A zero-width character goes with the one before it...
        let mut target = 1;
        assert_eq!(
            strlen_max_width("e\u{301}x".as_bytes(), true, &mut target),
            3
        );
        assert_eq!(target, 1);
        // ...unless that one was already past the target, as in C.
        let mut target = 1;
        assert_eq!(
            strlen_max_width("中\u{301}".as_bytes(), true, &mut target),
            3
        );
        assert_eq!(target, 2);
    }

    #[test]
    fn strlen_max_width_takes_whole_characters_and_at_least_one() {
        // `print.c:3747`.
        let mut target = 2;
        assert_eq!(strlen_max_width(b"abcd", false, &mut target), 2);
        assert_eq!(target, 2);
        let mut target = 9;
        assert_eq!(strlen_max_width(b"abcd", false, &mut target), 4);
        assert_eq!(target, 4);
        // A UTF-8 character is stepped over whole, as `PQmblen` does.
        let mut target = 1;
        assert_eq!(strlen_max_width("éé".as_bytes(), true, &mut target), 2);
        assert_eq!(target, 1);
        // The first character is taken even past the target.
        let mut target = 0;
        assert_eq!(strlen_max_width(b"ab", false, &mut target), 1);
        assert_eq!(target, 1);
    }

    #[test]
    fn wrapped_squeezes_a_column_to_the_target_and_marks_the_wraps() {
        // `print.c:820`: the only column shrinks from 10 to 4 so that the
        // table, 2 wider than its column at border 1, fits 6; `wrap_right`
        // and `wrap_left` mark each break (`print.c:1068`, `:1151`).
        let res = result(vec![text_field("t")], vec![vec![Some("aaaaaaaaaa")]]);
        let opt = with(|o| {
            o.topt.format = PrintFormat::Wrapped;
            o.topt.columns = 6;
        });
        assert_eq!(
            render(&res, &opt),
            "  t   \n------\n aaaa.\n.aaaa.\n.aa\n(1 row)\n\n"
        );
        // Never below the header: a target narrower than the headers leaves
        // the table as the aligned format draws it (`print.c:829`).
        let opt = with(|o| {
            o.topt.format = PrintFormat::Wrapped;
            o.topt.columns = 2;
        });
        let aligned = with(|o| o.topt.columns = 2);
        assert_eq!(render(&res, &opt), render(&res, &aligned));
    }

    #[test]
    fn expanded_prints_a_record_per_row_and_no_row_count() {
        // `print_aligned_vertical` (`print.c:1324`) prints `cont->footers`,
        // which `printQuery` leaves empty here, so there is no `(n rows)`.
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")], vec![Some("2"), Some("b")]],
        );
        let opt = with(|o| o.topt.expanded = Expanded::On);
        assert_eq!(
            render(&res, &opt),
            "-[ RECORD 1 ]\nn | 1\ns | a\n-[ RECORD 2 ]\nn | 2\ns | b\n\n"
        );
        // An empty result is the one that does get a footer (`print.c:1354`).
        let empty = result(vec![int4_field("n"), text_field("s")], vec![]);
        assert_eq!(render(&empty, &opt), "(0 rows)\n\n");
        // Unaligned: header, field separator, value; records apart by two
        // record separators (`print.c:513`).
        let opt = with(|o| {
            o.topt.expanded = Expanded::On;
            o.topt.format = PrintFormat::Unaligned;
            o.topt.field_sep.separator = Some("|".to_string());
            o.topt.record_sep.separator = Some("\n".to_string());
        });
        assert_eq!(render(&res, &opt), "n|1\ns|a\n\nn|2\ns|b\n");
        assert_eq!(render(&empty, &opt), "");
    }

    #[test]
    fn expanded_auto_goes_vertical_only_when_the_table_is_too_wide() {
        // `print.c:877`.
        let res = result(
            vec![int4_field("n"), text_field("s")],
            vec![vec![Some("1"), Some("a")]],
        );
        let vertical = render(&res, &with(|o| o.topt.expanded = Expanded::On));
        let horizontal = render(&res, &PrintQueryOpt::default());
        let auto = |columns| {
            render(
                &res,
                &with(|o| {
                    o.topt.expanded = Expanded::Auto;
                    o.topt.columns = columns;
                }),
            )
        };
        // " n | s " is 7 wide.
        assert_eq!(auto(7), horizontal);
        assert_eq!(auto(6), vertical);
    }

    #[test]
    fn wrapped_expanded_output_wraps_the_value_column() {
        // `print.c:1526`: the 10-wide value would need 14 columns; the
        // record line needs 13, so the value gets 13 less the header and the
        // separator (now 4 wide, a mark column added), and wraps at 8.
        let res = result(vec![text_field("t")], vec![vec![Some("aaaaaaaaaa")]]);
        let opt = with(|o| {
            o.topt.expanded = Expanded::On;
            o.topt.format = PrintFormat::Wrapped;
            o.topt.columns = 12;
        });
        assert_eq!(
            render(&res, &opt),
            "-[ RECORD 1 ]\nt | aaaaaaaa.\n  |.aa\n\n"
        );
    }

    #[test]
    fn xheader_width_bounds_the_record_line() {
        // `print_aligned_vertical_line` (`print.c:1225`): `full` rules the
        // whole value column, `column` stops at the divider, and `page` and
        // an exact width cut the rule to fit.
        let header = "abcdefghijklmn";
        let res = result(
            vec![text_field(header)],
            vec![vec![Some("xxxxxxxxxxxxxxxxxxxx")]],
        );
        let record_line = |xheader, columns| {
            let out = render(
                &res,
                &with(|o| {
                    o.topt.expanded = Expanded::On;
                    o.topt.expanded_header_width = xheader;
                    o.topt.columns = columns;
                }),
            );
            out.lines().next().unwrap().to_string()
        };
        let rule = |n| "-".repeat(n);
        assert_eq!(
            record_line(XheaderWidth::Full, 0),
            format!("-[ RECORD 1 ]--+-{}", rule(20))
        );
        assert_eq!(record_line(XheaderWidth::Column, 0), "-[ RECORD 1 ]--+");
        assert_eq!(
            record_line(XheaderWidth::ExactWidth(25), 0),
            format!("-[ RECORD 1 ]--+-{}", rule(8))
        );
        assert_eq!(
            record_line(XheaderWidth::Page, 25),
            format!("-[ RECORD 1 ]--+-{}", rule(8))
        );
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

    #[test]
    fn a_cell_is_measured_in_display_columns() {
        assert_eq!(widest(&format_cell("中文".as_bytes())), 4);
        assert_eq!(widest(&format_cell("e\u{301}".as_bytes())), 1);
        assert_eq!(widest(&format_cell("\u{200d}".as_bytes())), 0);
        // A tab stop counts the wide character's two columns.
        let lines = format_cell("中\tx".as_bytes());
        assert_eq!(lines[0].bytes, "中      x".as_bytes());
        assert_eq!(lines[0].width, 9);
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
                utf8: true,
            }]
        );
        assert_eq!(
            format_cell("\u{85}".as_bytes()),
            vec![Line {
                bytes: b"\\u0085".to_vec(),
                width: 6,
                utf8: true,
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
