//! `\pset`: `do_pset`, `printPsetInfo` and `pset_value_string` from
//! `src/bin/psql/command.c`.
//!
//! Every function here is a calculation over a [`PrintQueryOpt`]: the caller
//! writes the text that comes back, so the whole of `\pset` is testable
//! without a terminal or a server.

use crate::settings::{
    Expanded, LineStyle, Pager, PrintFormat, PrintQueryOpt, Separator, UnicodeLinestyle,
    XheaderWidth,
};
use crate::variables::{parse_variable_bool, parse_variable_num};

/// A refused `\pset`, carrying the text upstream's `pg_log_error` would print,
/// without the `psql: error: ` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsetError {
    /// The message.
    pub message: String,
}

impl PsetError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PsetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PsetError {}

/// The parameters a bare `\pset` lists, in `exec_command_pset`'s `my_list[]`
/// order (`command.c:2711`).
pub const PSET_LIST: [&str; 22] = [
    "border",
    "columns",
    "csv_fieldsep",
    "expanded",
    "fieldsep",
    "fieldsep_zero",
    "footer",
    "format",
    "linestyle",
    "null",
    "numericlocale",
    "pager",
    "pager_min_lines",
    "recordsep",
    "recordsep_zero",
    "tableattr",
    "title",
    "tuples_only",
    "unicode_border_linestyle",
    "unicode_column_linestyle",
    "unicode_header_linestyle",
    "xheader_width",
];

/// The body of a bare `\pset`: `printf("%-24s %s\n", …)` over [`PSET_LIST`]
/// (`command.c:2724`-`:2730`).
#[must_use]
pub fn list_all(popt: &PrintQueryOpt) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for param in PSET_LIST {
        let _ = writeln!(out, "{param:<24} {}", pset_value_string(param, popt));
    }
    out
}

/// `pg_strncasecmp(name, value, strlen(value)) == 0`: `value` is a
/// case-insensitive prefix of `name`. The empty string is a prefix of
/// everything, as it is in C.
fn is_prefix_of(value: &str, name: &str) -> bool {
    name.len() >= value.len()
        && name.as_bytes()[..value.len()].eq_ignore_ascii_case(value.as_bytes())
}

/// C's `atoi`: optional leading whitespace and sign, then as many decimal
/// digits as there are; 0 when there are none. Overflow wraps, where C's is
/// undefined; nothing in the regression suite reaches it.
fn atoi(value: &str) -> i32 {
    let value = value.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let magnitude = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0_i32, |n, d| {
            n.wrapping_mul(10).wrapping_add(i32::from(d - b'0'))
        });
    if negative {
        magnitude.wrapping_neg()
    } else {
        magnitude
    }
}

/// `ParseVariableBool(value, name, …)` with a name, which logs its own error
/// (`variables.c:141`).
fn parse_bool_named(value: &str, name: &str) -> Result<bool, PsetError> {
    let mut result = false;
    if parse_variable_bool(Some(value), Some(name), &mut result) {
        Ok(result)
    } else {
        Err(PsetError::new(format!(
            "unrecognized value \"{value}\" for \"{name}\": Boolean expected"
        )))
    }
}

/// `PsqlVarEnumError()` (`variables.c:487`).
fn enum_error(name: &str, value: &str, suggestions: &str) -> PsetError {
    PsetError::new(format!(
        "unrecognized value \"{value}\" for \"{name}\"\nAvailable values are: {suggestions}."
    ))
}

/// `set_unicode_line_style()` (`command.c:5030`).
fn unicode_line_style(value: &str) -> Option<UnicodeLinestyle> {
    if is_prefix_of(value, "single") {
        Some(UnicodeLinestyle::Single)
    } else if is_prefix_of(value, "double") {
        Some(UnicodeLinestyle::Double)
    } else {
        None
    }
}

/// `do_pset()` (`command.c:5073`): perform `param = value` on `popt`.
///
/// `Ok(Some(text))` is what `printPsetInfo` reports when `quiet` is false;
/// `Ok(None)` means nothing is printed, which is also what upstream does for
/// the three boolean parameters it returns from early (`numericlocale`,
/// `tuples_only`, `footer` with a value).
///
/// # Errors
/// An unknown parameter or a value the parameter refuses; `popt` may then be
/// partly updated exactly where upstream's is (it is not, for any parameter
/// here).
// One arm per parameter, in upstream's order; splitting the chain would only
// move the arms somewhere else.
#[allow(clippy::too_many_lines)]
pub fn do_pset(
    param: &str,
    value: Option<&str>,
    popt: &mut PrintQueryOpt,
    quiet: bool,
) -> Result<Option<String>, PsetError> {
    let topt = &mut popt.topt;
    match param {
        "format" => {
            if let Some(value) = value {
                // `formats[]` (`command.c:5089`), in its order; latex-longtable
                // is tried separately because latex is a prefix of it.
                const FORMATS: [(&str, PrintFormat); 8] = [
                    ("aligned", PrintFormat::Aligned),
                    ("asciidoc", PrintFormat::Asciidoc),
                    ("csv", PrintFormat::Csv),
                    ("html", PrintFormat::Html),
                    ("latex", PrintFormat::Latex),
                    ("troff-ms", PrintFormat::TroffMs),
                    ("unaligned", PrintFormat::Unaligned),
                    ("wrapped", PrintFormat::Wrapped),
                ];
                let mut matched: Option<(&str, PrintFormat)> = None;
                for (name, format) in FORMATS {
                    if is_prefix_of(value, name) {
                        if let Some((first, _)) = matched {
                            return Err(PsetError::new(format!(
                                "\\pset: ambiguous abbreviation \"{value}\" matches both \"{first}\" and \"{name}\""
                            )));
                        }
                        matched = Some((name, format));
                    }
                }
                if let Some((_, format)) = matched {
                    topt.format = format;
                } else if is_prefix_of(value, "latex-longtable") {
                    topt.format = PrintFormat::LatexLongtable;
                } else {
                    return Err(PsetError::new(
                        "\\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped",
                    ));
                }
            }
        }
        "linestyle" => {
            if let Some(value) = value {
                topt.line_style = if is_prefix_of(value, "ascii") {
                    LineStyle::Ascii
                } else if is_prefix_of(value, "old-ascii") {
                    LineStyle::OldAscii
                } else if is_prefix_of(value, "unicode") {
                    LineStyle::Unicode
                } else {
                    return Err(PsetError::new(
                        "\\pset: allowed line styles are ascii, old-ascii, unicode",
                    ));
                };
            }
        }
        "unicode_border_linestyle" | "unicode_column_linestyle" | "unicode_header_linestyle" => {
            if let Some(value) = value {
                let (slot, which) = match param {
                    "unicode_border_linestyle" => (&mut topt.unicode_border_linestyle, "border"),
                    "unicode_column_linestyle" => (&mut topt.unicode_column_linestyle, "column"),
                    _ => (&mut topt.unicode_header_linestyle, "header"),
                };
                // C refreshes `pg_utf8format` here (`command.c:5167`); the
                // printer derives it from these settings when it draws
                // (`print::refresh_utf8format`).
                *slot = unicode_line_style(value).ok_or_else(|| {
                    PsetError::new(format!(
                        "\\pset: allowed Unicode {which} line styles are single, double"
                    ))
                })?;
            }
        }
        "border" => {
            if let Some(value) = value {
                // `atoi` into an `unsigned short`: the conversion is modulo
                // 2^16, so -1 becomes 65535.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                {
                    topt.border = atoi(value) as u16;
                }
            }
        }
        "x" | "expanded" | "vertical" => {
            topt.expanded = match value {
                Some(value) if value.eq_ignore_ascii_case("auto") => Expanded::Auto,
                Some(value) => {
                    let mut on_off = false;
                    if !parse_variable_bool(Some(value), None, &mut on_off) {
                        return Err(enum_error(param, value, "on, off, auto"));
                    }
                    if on_off { Expanded::On } else { Expanded::Off }
                }
                // `!popt->topt.expanded`: auto and on both toggle to off.
                None => {
                    if topt.expanded == Expanded::Off {
                        Expanded::On
                    } else {
                        Expanded::Off
                    }
                }
            };
        }
        "xheader_width" => {
            if let Some(value) = value {
                topt.expanded_header_width = if value.eq_ignore_ascii_case("full") {
                    XheaderWidth::Full
                } else if value.eq_ignore_ascii_case("column") {
                    XheaderWidth::Column
                } else if value.eq_ignore_ascii_case("page") {
                    XheaderWidth::Page
                } else {
                    match atoi(value) {
                        0 => {
                            return Err(PsetError::new(
                                "\\pset: allowed xheader_width values are \"full\" (default), \"column\", \"page\", or a number specifying the exact width",
                            ));
                        }
                        width => XheaderWidth::ExactWidth(width),
                    }
                };
            }
        }
        "csv_fieldsep" => {
            if let Some(value) = value {
                // A one-byte string: one `char`, and an ASCII one.
                let mut chars = value.chars();
                let (Some(sep), None) = (chars.next(), chars.next()) else {
                    return Err(PsetError::new(
                        "\\pset: csv_fieldsep must be a single one-byte character",
                    ));
                };
                if !sep.is_ascii() {
                    return Err(PsetError::new(
                        "\\pset: csv_fieldsep must be a single one-byte character",
                    ));
                }
                if matches!(sep, '"' | '\n' | '\r') {
                    return Err(PsetError::new(
                        "\\pset: csv_fieldsep cannot be a double quote, a newline, or a carriage return",
                    ));
                }
                topt.csv_field_sep = sep;
            }
        }
        "numericlocale" => {
            // `return ParseVariableBool(…)`: no `printPsetInfo` on this path.
            if let Some(value) = value {
                topt.numeric_locale = parse_bool_named(value, param)?;
                return Ok(None);
            }
            topt.numeric_locale = !topt.numeric_locale;
        }
        "null" => {
            if let Some(value) = value {
                popt.null_print = Some(value.to_string());
            }
        }
        "fieldsep" => {
            if let Some(value) = value {
                topt.field_sep = Separator {
                    separator: Some(value.to_string()),
                    separator_zero: false,
                };
            }
        }
        "fieldsep_zero" => {
            topt.field_sep = Separator {
                separator: None,
                separator_zero: true,
            };
        }
        "recordsep" => {
            if let Some(value) = value {
                topt.record_sep = Separator {
                    separator: Some(value.to_string()),
                    separator_zero: false,
                };
            }
        }
        "recordsep_zero" => {
            topt.record_sep = Separator {
                separator: None,
                separator_zero: true,
            };
        }
        "t" | "tuples_only" => {
            if let Some(value) = value {
                topt.tuples_only = parse_bool_named(value, param)?;
                return Ok(None);
            }
            topt.tuples_only = !topt.tuples_only;
        }
        "C" | "title" => popt.title = value.map(str::to_string),
        "T" | "tableattr" => topt.table_attr = value.map(str::to_string),
        "pager" => {
            topt.pager = match value {
                Some(value) if value.eq_ignore_ascii_case("always") => Pager::Always,
                Some(value) => {
                    let mut on_off = false;
                    if !parse_variable_bool(Some(value), None, &mut on_off) {
                        return Err(enum_error(param, value, "on, off, always"));
                    }
                    if on_off { Pager::On } else { Pager::Off }
                }
                // Only "on" toggles to off; off and always both become on.
                None => {
                    if topt.pager == Pager::On {
                        Pager::Off
                    } else {
                        Pager::On
                    }
                }
            };
        }
        "pager_min_lines" => {
            if let Some(value) = value
                && !parse_variable_num(Some(value), Some(param), &mut topt.pager_min_lines)
            {
                return Err(PsetError::new(format!(
                    "invalid value \"{value}\" for \"{param}\": integer expected"
                )));
            }
        }
        "footer" => {
            if let Some(value) = value {
                topt.default_footer = parse_bool_named(value, param)?;
                return Ok(None);
            }
            topt.default_footer = !topt.default_footer;
        }
        "columns" => {
            if let Some(value) = value {
                topt.columns = atoi(value);
            }
        }
        _ => return Err(PsetError::new(format!("\\pset: unknown option: {param}"))),
    }

    if quiet {
        Ok(None)
    } else {
        Ok(print_pset_info(param, popt))
    }
}

/// `printPsetInfo()` (`command.c:5426`): the line reporting `param`'s state,
/// or `None` for a parameter it does not know (for which upstream logs the
/// same "unknown option" error `do_pset` would already have refused with).
// One arm per parameter, in upstream's order, as `do_pset`.
#[allow(clippy::too_many_lines)]
#[must_use]
pub fn print_pset_info(param: &str, popt: &PrintQueryOpt) -> Option<String> {
    let topt = &popt.topt;
    let text = match param {
        "border" => format!("Border style is {}.", topt.border),
        "columns" => {
            if topt.columns == 0 {
                "Target width is unset.".to_string()
            } else {
                format!("Target width is {}.", topt.columns)
            }
        }
        "x" | "expanded" | "vertical" => match topt.expanded {
            Expanded::On => "Expanded display is on.".to_string(),
            Expanded::Auto => "Expanded display is used automatically.".to_string(),
            Expanded::Off => "Expanded display is off.".to_string(),
        },
        "xheader_width" => match topt.expanded_header_width {
            XheaderWidth::Full => "Expanded header width is \"full\".".to_string(),
            XheaderWidth::Column => "Expanded header width is \"column\".".to_string(),
            XheaderWidth::Page => "Expanded header width is \"page\".".to_string(),
            XheaderWidth::ExactWidth(width) => format!("Expanded header width is {width}."),
        },
        "csv_fieldsep" => format!("Field separator for CSV is \"{}\".", topt.csv_field_sep),
        "fieldsep" => {
            if topt.field_sep.separator_zero {
                "Field separator is zero byte.".to_string()
            } else {
                format!(
                    "Field separator is \"{}\".",
                    topt.field_sep.separator.as_deref().unwrap_or("")
                )
            }
        }
        "fieldsep_zero" => "Field separator is zero byte.".to_string(),
        "footer" => {
            if topt.default_footer {
                "Default footer is on.".to_string()
            } else {
                "Default footer is off.".to_string()
            }
        }
        "format" => format!("Output format is {}.", topt.format.name()),
        "linestyle" => format!("Line style is {}.", topt.line_style.name()),
        "null" => format!(
            "Null display is \"{}\".",
            popt.null_print.as_deref().unwrap_or("")
        ),
        "numericlocale" => {
            if topt.numeric_locale {
                "Locale-adjusted numeric output is on.".to_string()
            } else {
                "Locale-adjusted numeric output is off.".to_string()
            }
        }
        "pager" => match topt.pager {
            Pager::On => "Pager is used for long output.".to_string(),
            Pager::Always => "Pager is always used.".to_string(),
            Pager::Off => "Pager usage is off.".to_string(),
        },
        // `ngettext`, whose English plural is anything but 1.
        "pager_min_lines" => {
            let n = topt.pager_min_lines;
            let noun = if n == 1 { "line" } else { "lines" };
            format!("Pager won't be used for less than {n} {noun}.")
        }
        "recordsep" => {
            if topt.record_sep.separator_zero {
                "Record separator is zero byte.".to_string()
            } else {
                match topt.record_sep.separator.as_deref().unwrap_or("") {
                    "\n" => "Record separator is <newline>.".to_string(),
                    sep => format!("Record separator is \"{sep}\"."),
                }
            }
        }
        "recordsep_zero" => "Record separator is zero byte.".to_string(),
        "T" | "tableattr" => match &topt.table_attr {
            Some(attr) => format!("Table attributes are \"{attr}\"."),
            None => "Table attributes unset.".to_string(),
        },
        "C" | "title" => match &popt.title {
            Some(title) => format!("Title is \"{title}\"."),
            None => "Title is unset.".to_string(),
        },
        "t" | "tuples_only" => {
            if topt.tuples_only {
                "Tuples only is on.".to_string()
            } else {
                "Tuples only is off.".to_string()
            }
        }
        "unicode_border_linestyle" => format!(
            "Unicode border line style is \"{}\".",
            topt.unicode_border_linestyle.name()
        ),
        "unicode_column_linestyle" => format!(
            "Unicode column line style is \"{}\".",
            topt.unicode_column_linestyle.name()
        ),
        "unicode_header_linestyle" => format!(
            "Unicode header line style is \"{}\".",
            topt.unicode_header_linestyle.name()
        ),
        _ => return None,
    };
    Some(text + "\n")
}

/// `pset_bool_string()` (`command.c:5688`).
fn bool_string(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

/// `pset_quoted_string()` (`command.c:5695`): single-quoted, with a newline
/// written `\n` and a quote `\'`.
fn quoted_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\'' => out.push_str("\\'"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// `pset_value_string()` (`command.c:5733`): `param`'s value, spelled so that
/// feeding it back to `\pset` restores it. An unknown parameter is `ERROR`,
/// as upstream's.
#[must_use]
pub fn pset_value_string(param: &str, popt: &PrintQueryOpt) -> String {
    let topt = &popt.topt;
    match param {
        "border" => topt.border.to_string(),
        "columns" => topt.columns.to_string(),
        "csv_fieldsep" => quoted_string(&topt.csv_field_sep.to_string()),
        "expanded" => match topt.expanded {
            Expanded::Auto => "auto".to_string(),
            Expanded::On => bool_string(true).to_string(),
            Expanded::Off => bool_string(false).to_string(),
        },
        "fieldsep" => quoted_string(topt.field_sep.separator.as_deref().unwrap_or("")),
        "fieldsep_zero" => bool_string(topt.field_sep.separator_zero).to_string(),
        "footer" => bool_string(topt.default_footer).to_string(),
        "format" => topt.format.name().to_string(),
        "linestyle" => topt.line_style.name().to_string(),
        "null" => quoted_string(popt.null_print.as_deref().unwrap_or("")),
        "numericlocale" => bool_string(topt.numeric_locale).to_string(),
        "pager" => topt.pager.number().to_string(),
        "pager_min_lines" => topt.pager_min_lines.to_string(),
        "recordsep" => quoted_string(topt.record_sep.separator.as_deref().unwrap_or("")),
        "recordsep_zero" => bool_string(topt.record_sep.separator_zero).to_string(),
        "tableattr" => topt
            .table_attr
            .as_deref()
            .map(quoted_string)
            .unwrap_or_default(),
        "title" => popt.title.as_deref().map(quoted_string).unwrap_or_default(),
        "tuples_only" => bool_string(topt.tuples_only).to_string(),
        "unicode_border_linestyle" => topt.unicode_border_linestyle.name().to_string(),
        "unicode_column_linestyle" => topt.unicode_column_linestyle.name().to_string(),
        "unicode_header_linestyle" => topt.unicode_header_linestyle.name().to_string(),
        "xheader_width" => match topt.expanded_header_width {
            XheaderWidth::Full => "full".to_string(),
            XheaderWidth::Column => "column".to_string(),
            XheaderWidth::Page => "page".to_string(),
            XheaderWidth::ExactWidth(width) => width.to_string(),
        },
        _ => "ERROR".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::PsqlSettings;

    /// The print options `main()` leaves before the first command: the
    /// defaults plus the separators `startup.c:227`-`:238` fills in.
    fn popt() -> PrintQueryOpt {
        let mut pset = PsqlSettings::default();
        pset.apply_separator_defaults();
        pset.popt
    }

    fn set(popt: &mut PrintQueryOpt, param: &str, value: Option<&str>) -> Option<String> {
        do_pset(param, value, popt, false).expect("the \\pset is accepted")
    }

    #[test]
    fn a_bare_pset_lists_every_parameter_as_psql_out_does() {
        // `expected/psql.out:445`-`:466`, the listing after `\pset` under
        // "-- show all pset options". Line 460 and 461 end in a space: the
        // `%s` of an unset tableattr and title is empty, not absent.
        let expected = "\
border                   1
columns                  0
csv_fieldsep             ','
expanded                 off
fieldsep                 '|'
fieldsep_zero            off
footer                   on
format                   aligned
linestyle                ascii
null                     ''
numericlocale            off
pager                    1
pager_min_lines          0
recordsep                '\\n'
recordsep_zero           off
tableattr                \n\
title                    \n\
tuples_only              off
unicode_border_linestyle single
unicode_column_linestyle single
unicode_header_linestyle single
xheader_width            full
";
        assert_eq!(list_all(&popt()), expected);
    }

    #[test]
    fn each_assignment_reports_the_new_state_unless_quiet() {
        let mut p = popt();
        assert_eq!(
            set(&mut p, "border", Some("2")).as_deref(),
            Some("Border style is 2.\n")
        );
        assert_eq!(p.topt.border, 2);
        assert_eq!(
            set(&mut p, "format", Some("unaligned")).as_deref(),
            Some("Output format is unaligned.\n")
        );
        assert_eq!(
            set(&mut p, "columns", Some("40")).as_deref(),
            Some("Target width is 40.\n")
        );
        assert_eq!(
            set(&mut p, "recordsep", Some("\n")).as_deref(),
            Some("Record separator is <newline>.\n")
        );
        assert_eq!(
            set(&mut p, "linestyle", Some("old")).as_deref(),
            Some("Line style is old-ascii.\n")
        );
        assert_eq!(do_pset("border", Some("0"), &mut p, true), Ok(None));
        assert_eq!(p.topt.border, 0);
    }

    #[test]
    fn the_boolean_parameters_with_a_value_report_nothing() {
        // `return ParseVariableBool(…)` skips `printPsetInfo`
        // (`command.c:5285`, `:5340`, `:5399`).
        let mut p = popt();
        assert_eq!(set(&mut p, "tuples_only", Some("on")), None);
        assert!(p.topt.tuples_only);
        assert_eq!(set(&mut p, "footer", Some("off")), None);
        assert!(!p.topt.default_footer);
        assert_eq!(set(&mut p, "numericlocale", Some("true")), None);
        assert!(p.topt.numeric_locale);
        // Without a value they toggle, and then they do report.
        assert_eq!(
            set(&mut p, "t", None).as_deref(),
            Some("Tuples only is off.\n")
        );
    }

    #[test]
    fn a_format_is_matched_by_unique_prefix() {
        let mut p = popt();
        set(&mut p, "format", Some("u"));
        assert_eq!(p.topt.format, PrintFormat::Unaligned);
        set(&mut p, "format", Some("WRAP"));
        assert_eq!(p.topt.format, PrintFormat::Wrapped);
        set(&mut p, "format", Some("latex"));
        assert_eq!(p.topt.format, PrintFormat::Latex);
        set(&mut p, "format", Some("latex-l"));
        assert_eq!(p.topt.format, PrintFormat::LatexLongtable);
    }

    #[test]
    fn an_ambiguous_format_abbreviation_is_refused() {
        // `expected/psql.out:4503`, under "-- check ambiguous format requests".
        let mut p = popt();
        assert_eq!(
            do_pset("format", Some("a"), &mut p, false),
            Err(PsetError::new(
                "\\pset: ambiguous abbreviation \"a\" matches both \"aligned\" and \"asciidoc\""
            ))
        );
        assert_eq!(p.topt.format, PrintFormat::Aligned);
        assert_eq!(
            do_pset("format", Some("x"), &mut p, false),
            Err(PsetError::new(
                "\\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped"
            ))
        );
    }

    #[test]
    fn csv_fieldsep_must_be_one_byte_and_not_a_quote_or_line_break() {
        // `expected/psql.out:3383`-`:3393`, under "-- illegal csv separators".
        let mut p = popt();
        let one_byte = "\\pset: csv_fieldsep must be a single one-byte character";
        let forbidden =
            "\\pset: csv_fieldsep cannot be a double quote, a newline, or a carriage return";
        for (value, message) in [
            ("", one_byte),
            ("--", one_byte),
            ("\"", forbidden),
            ("\n", forbidden),
            ("\r", forbidden),
            ("é", one_byte),
        ] {
            assert_eq!(
                do_pset("csv_fieldsep", Some(value), &mut p, false),
                Err(PsetError::new(message)),
                "{value:?}"
            );
        }
        assert_eq!(
            set(&mut p, "csv_fieldsep", Some("\t")).as_deref(),
            Some("Field separator for CSV is \"\t\".\n")
        );
    }

    #[test]
    fn expanded_takes_auto_a_boolean_or_nothing() {
        let mut p = popt();
        assert_eq!(
            set(&mut p, "x", Some("auto")).as_deref(),
            Some("Expanded display is used automatically.\n")
        );
        // Toggling from auto goes to off: `!2` is 0.
        assert_eq!(
            set(&mut p, "expanded", None).as_deref(),
            Some("Expanded display is off.\n")
        );
        assert_eq!(
            do_pset("vertical", Some("sideways"), &mut p, false),
            Err(PsetError::new(
                "unrecognized value \"sideways\" for \"vertical\"\nAvailable values are: on, off, auto."
            ))
        );
    }

    #[test]
    fn the_pager_toggle_turns_only_on_into_off() {
        let mut p = popt();
        set(&mut p, "pager", Some("always"));
        assert_eq!(pset_value_string("pager", &p), "2");
        set(&mut p, "pager", None);
        assert_eq!(p.topt.pager, Pager::On);
        set(&mut p, "pager", None);
        assert_eq!(p.topt.pager, Pager::Off);
        assert_eq!(
            set(&mut p, "pager_min_lines", Some("1")).as_deref(),
            Some("Pager won't be used for less than 1 line.\n")
        );
        assert_eq!(
            do_pset("pager_min_lines", Some("lots"), &mut p, false),
            Err(PsetError::new(
                "invalid value \"lots\" for \"pager_min_lines\": integer expected"
            ))
        );
    }

    #[test]
    fn border_is_atoi_into_an_unsigned_short() {
        let mut p = popt();
        set(&mut p, "border", Some("-1"));
        assert_eq!(p.topt.border, 65535);
        set(&mut p, "border", Some("3x"));
        assert_eq!(p.topt.border, 3);
        set(&mut p, "border", Some("none"));
        assert_eq!(p.topt.border, 0);
    }

    #[test]
    fn xheader_width_takes_a_keyword_or_a_nonzero_width() {
        let mut p = popt();
        set(&mut p, "xheader_width", Some("PAGE"));
        assert_eq!(pset_value_string("xheader_width", &p), "page");
        assert_eq!(
            set(&mut p, "xheader_width", Some("30")).as_deref(),
            Some("Expanded header width is 30.\n")
        );
        assert!(do_pset("xheader_width", Some("wide"), &mut p, false).is_err());
    }

    #[test]
    fn the_separators_and_strings_quote_back_into_pset_syntax() {
        let mut p = popt();
        set(&mut p, "fieldsep", Some("it's"));
        assert_eq!(pset_value_string("fieldsep", &p), "'it\\'s'");
        set(&mut p, "fieldsep_zero", None);
        assert_eq!(pset_value_string("fieldsep", &p), "''");
        assert_eq!(pset_value_string("fieldsep_zero", &p), "on");
        set(&mut p, "title", Some("T"));
        assert_eq!(pset_value_string("title", &p), "'T'");
        assert_eq!(set(&mut p, "C", None).as_deref(), Some("Title is unset.\n"));
        assert_eq!(pset_value_string("title", &p), "");
        assert_eq!(pset_value_string("nonesuch", &p), "ERROR");
    }

    #[test]
    fn the_unicode_line_styles_are_single_or_double() {
        let mut p = popt();
        assert_eq!(
            set(&mut p, "unicode_header_linestyle", Some("d")).as_deref(),
            Some("Unicode header line style is \"double\".\n")
        );
        assert_eq!(
            do_pset("unicode_column_linestyle", Some("triple"), &mut p, false),
            Err(PsetError::new(
                "\\pset: allowed Unicode column line styles are single, double"
            ))
        );
    }

    #[test]
    fn an_unknown_parameter_is_refused() {
        let mut p = popt();
        assert_eq!(
            do_pset("nosuch", Some("1"), &mut p, false),
            Err(PsetError::new("\\pset: unknown option: nosuch"))
        );
    }
}
