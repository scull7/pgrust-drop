//! `fe-print.c` (PostgreSQL 18.6): `PQprint` (`:68`) and the two "really
//! old printing routines" (`:600`), `PQdisplayTuples` (`:604`) and
//! `PQprintTuples` (`:701`).
//!
//! Each renders a [`QueryResult`] into the writer it is given, byte for byte
//! as the C writes into its `FILE *`, quirks included: a caller compared
//! against C libpq sees them, so each is kept and named where it happens. A
//! field value is read as `PQgetvalue` hands it to C: a NULL is the empty
//! string, and the text stops at the first NUL while its length is still
//! `PQgetlength`'s (only a binary-format column can tell the two apart).
//!
//! What is not ported:
//! - `PQprint`'s pager (`:150`-`:204`). C pages only when `fout` is `stdout`
//!   and both `stdin` and `stdout` are terminals, and then asks the terminal
//!   its size and `popen`s `$PAGER` with `SIGPIPE` blocked. A [`Write`] is
//!   not known to be `stdout`, and the size and the signal mask need libc,
//!   which this crate does not call (`#![deny(unsafe_code)]`), so
//!   [`PrintOpt::pager`] is carried and never pages. Recorded in
//!   `docs/divergences.md`.
//! - The three `stderr` complaints: `overlong field separator` (`:114`),
//!   `out of memory` (`:124` and after) and `header size exceeds the maximum
//!   allowed` (`:556`). A separator longer than `INT_MAX` and a border that
//!   overflows `size_t` cannot be built in the first place, and a failed
//!   allocation aborts a Rust process.
//! - `res->client_encoding`, which `do_field` steps through each value with
//!   (`:376`): it cannot change the answer, see [`looks_numeric`].

use std::io::{self, Write};

use crate::result::QueryResult;

/// `PQprintOpt`, `libpq-fe.h:247`. Each string is a C string without its
/// terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // `pqbool`s, as upstream lays them out.
pub struct PrintOpt<'a> {
    /// Print output field headings and row count.
    pub header: bool,
    /// Fill align the fields.
    pub align: bool,
    /// "Old brain dead format": a `+`/`-` border around every row.
    pub standard: bool,
    /// Output HTML tables.
    pub html3: bool,
    /// Expand tables: one field per line.
    pub expanded: bool,
    /// Use a pager for output if needed. Never pages here; see the module
    /// comment.
    pub pager: bool,
    /// The field separator, `fieldSep`.
    pub field_sep: &'a [u8],
    /// Inserted into HTML `<table ...>`, `tableOpt`; `None` is C's `NULL`.
    pub table_opt: Option<&'a [u8]>,
    /// The HTML `<caption>`, `caption`; `None` is C's `NULL`.
    pub caption: Option<&'a [u8]>,
    /// Replacement field names, `fieldName` without its terminating `NULL`.
    /// An empty name, or a column past the end, keeps the result's own.
    pub field_name: &'a [&'a [u8]],
}

/// The bytes C's `%s` prints for a `char *`: up to the first NUL.
fn c_str(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|&b| b == 0)
        .map_or(bytes, |nul| &bytes[..nul])
}

/// `PQgetvalue` and `PQgetlength`: the value as a C string, and its length.
fn value(res: &QueryResult, row: usize, column: usize) -> (&[u8], usize) {
    let bytes = res.value(row, column).unwrap_or_default();
    (c_str(bytes), bytes.len())
}

/// `PQfname`, which is never `NULL` for a column in range.
fn fname(res: &QueryResult, column: usize) -> &[u8] {
    res.fname(column).map(c_str).unwrap_or_default()
}

/// What `fprintf`'s `%s` prints for a `NULL` `char *` in glibc, musl and
/// Apple's libc alike.
const PRINTF_NULL: &[u8] = b"(null)";

/// `%-*s` (`left`) or `%*s`: `s`, padded with spaces to `width` bytes.
fn pad(fout: &mut impl Write, s: &[u8], width: usize, left: bool) -> io::Result<()> {
    let fill = b" ".repeat(width.saturating_sub(s.len()));
    if left {
        fout.write_all(s)?;
        fout.write_all(&fill)
    } else {
        fout.write_all(&fill)?;
        fout.write_all(s)
    }
}

/// The `s` of `row%s`.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The HTML alignment of a non-numeric or a numeric column.
fn html_align(not_num: bool) -> &'static str {
    if not_num { "left" } else { "right" }
}

/// Whether `do_field` leaves a column numeric after seeing `pval`, a
/// non-empty C string (`fe-print.c:371`-`:398`): every character is a digit,
/// `.`, `E`, `e`, space or `-`, the first is not `E`/`e`, and the last is a
/// digit.
///
/// C steps through `pval` with `PQmblenBounded` in the result's client
/// encoding and tests each character's first byte. That cannot differ from
/// testing every byte: the loop stops at the first byte outside the set, and
/// every byte inside it is ASCII, a one-byte character in every client
/// encoding, so no byte is skipped before the loop stops. The encoding is
/// therefore not an argument.
fn looks_numeric(pval: &[u8]) -> bool {
    let in_set = |ch: &u8| ch.is_ascii_digit() || matches!(ch, b'.' | b'E' | b'e' | b' ' | b'-');
    pval.iter().all(in_set)
        && !matches!(pval.first(), Some(b'E' | b'e'))
        && pval.last().is_some_and(u8::is_ascii_digit)
}

/// `PQprint`, `fe-print.c:68`: format a result for printing, as `po` says.
/// Nothing is printed for a result without fields (`:75`).
///
/// # Errors
///
/// Whatever writing to `fout` returns; C ignores its `FILE *`'s errors.
pub fn print(fout: &mut impl Write, res: &QueryResult, po: &PrintOpt<'_>) -> io::Result<()> {
    if res.nfields() == 0 {
        return Ok(());
    }
    Printer::new(res, po).print(fout)
}

/// What `PQprint` keeps across `do_field`, `do_header` and `output_row`.
struct Printer<'a> {
    res: &'a QueryResult,
    po: &'a PrintOpt<'a>,
    n_fields: usize,
    n_tups: usize,
    /// `fieldNames`: `po.field_name`'s replacement, or `PQfname`.
    field_names: Vec<&'a [u8]>,
    /// `fieldNotNum`.
    field_not_num: Vec<bool>,
    /// `fieldMax`.
    field_max: Vec<usize>,
    /// `fieldMaxLen`: the longest field name, plus the separator.
    field_max_len: usize,
    /// `fields`, allocated only for an aligned or HTML table (`:206`); a
    /// value never stored stays `NULL`.
    fields: Option<Vec<Option<&'a [u8]>>>,
}

impl<'a> Printer<'a> {
    fn new(res: &'a QueryResult, po: &'a PrintOpt<'a>) -> Self {
        let n_fields = res.nfields();
        let n_tups = res.ntuples();
        let fs_len = po.field_sep.len();
        // fe-print.c:131
        let field_names: Vec<&[u8]> = (0..n_fields)
            .map(|j| match po.field_name.get(j).map(|name| c_str(name)) {
                Some(name) if !name.is_empty() => name,
                _ => fname(res, j),
            })
            .collect();
        let field_max: Vec<usize> = field_names.iter().map(|name| name.len()).collect();
        let field_max_len = field_max.iter().map(|len| len + fs_len).max().unwrap_or(0);
        // fe-print.c:206
        let fields =
            (!po.expanded && (po.align || po.html3)).then(|| vec![None; n_tups * n_fields]);
        Printer {
            res,
            po,
            n_fields,
            n_tups,
            field_names,
            field_not_num: vec![false; n_fields],
            field_max,
            field_max_len,
            fields,
        }
    }

    /// The body of `PQprint` after its setup, `fe-print.c:206`-`:308`.
    fn print(mut self, fout: &mut impl Write) -> io::Result<()> {
        let po = self.po;
        let fs = po.field_sep;
        if self.fields.is_none() && po.header && !po.html3 {
            if po.expanded {
                if po.align {
                    pad(fout, b"Field", self.field_max_len - fs.len(), true)?;
                    fout.write_all(fs)?;
                    fout.write_all(b" Value\n")?;
                } else {
                    fout.write_all(b"Field")?;
                    fout.write_all(fs)?;
                    fout.write_all(b"Value\n")?;
                }
            } else {
                // fe-print.c:228: the names, then as many '-' as they and
                // the separators between them took.
                let mut len = 0;
                for (j, name) in self.field_names.iter().enumerate() {
                    fout.write_all(name)?;
                    len += name.len() + fs.len();
                    if j + 1 < self.n_fields {
                        fout.write_all(fs)?;
                    }
                }
                fout.write_all(b"\n")?;
                fout.write_all(&b"-".repeat(len - fs.len()))?;
                fout.write_all(b"\n")?;
            }
        }
        if po.expanded && po.html3 {
            match po.caption {
                Some(caption) => {
                    fout.write_all(b"<center><h2>")?;
                    fout.write_all(caption)?;
                    fout.write_all(b"</h2></center>\n")?;
                }
                None => writeln!(
                    fout,
                    "<center><h2>Query retrieved {} rows * {} fields</h2></center>",
                    self.n_tups, self.n_fields
                )?,
            }
        }
        let table_opt = po.table_opt.unwrap_or_default();
        for i in 0..self.n_tups {
            if po.expanded {
                if po.html3 {
                    fout.write_all(b"<table ")?;
                    fout.write_all(table_opt)?;
                    writeln!(fout, "><caption align=\"top\">{i}</caption>")?;
                } else {
                    writeln!(fout, "-- RECORD {i} --")?;
                }
            }
            for j in 0..self.n_fields {
                self.do_field(fout, i, j)?;
            }
            if po.html3 && po.expanded {
                fout.write_all(b"</table>\n")?;
            }
        }
        if self.fields.is_some() {
            if po.html3 {
                fout.write_all(b"<table ")?;
                fout.write_all(table_opt)?;
                if po.header {
                    match po.caption {
                        Some(caption) => {
                            fout.write_all(b"><caption align=\"top\">")?;
                            fout.write_all(caption)?;
                            fout.write_all(b"</caption>\n")?;
                        }
                        None => writeln!(
                            fout,
                            "><caption align=\"top\">Retrieved {} rows * {} fields</caption>",
                            self.n_tups, self.n_fields
                        )?,
                    }
                } else {
                    // fe-print.c:295: no newline.
                    fout.write_all(b">")?;
                }
            }
            let border = if po.header {
                self.do_header(fout)?
            } else {
                None
            };
            for i in 0..self.n_tups {
                self.output_row(fout, border.as_deref(), i)?;
            }
        }
        if po.header && !po.html3 {
            let n = self.res.ntuples();
            write!(fout, "({n} row{})\n\n", plural(n))?;
        }
        if po.html3 && !po.expanded {
            fout.write_all(b"</table>\n")?;
        }
        Ok(())
    }

    /// `efield`, `fe-print.c:442`: the separator, or the end of the line
    /// after the last field.
    fn end_field(&self, fout: &mut impl Write, j: usize) -> io::Result<()> {
        if j + 1 < self.n_fields {
            fout.write_all(self.po.field_sep)
        } else {
            fout.write_all(b"\n")
        }
    }

    /// `do_field`, `fe-print.c:341`.
    fn do_field(&mut self, fout: &mut impl Write, i: usize, j: usize) -> io::Result<()> {
        let po = self.po;
        let (pval, plen) = value(self.res, i, j);
        if pval.is_empty() {
            // fe-print.c:356. An aligned or expanded field prints nothing at
            // all, not even its name. Otherwise the jump to `efield` lands
            // inside `if (!po->html3)` and so prints the separator even into
            // an HTML table.
            if po.align || po.expanded {
                return Ok(());
            }
            return self.end_field(fout, j);
        }
        if po.align && !self.field_not_num[j] && !looks_numeric(pval) {
            self.field_not_num[j] = true;
        }
        if let Some(fields) = &mut self.fields {
            self.field_max[j] = self.field_max[j].max(plen);
            fields[i * self.n_fields + j] = Some(pval);
        } else if po.expanded {
            let name = self.field_names[j];
            if po.html3 {
                fout.write_all(b"<tr><td align=\"left\"><b>")?;
                fout.write_all(name)?;
                write!(
                    fout,
                    "</b></td><td align=\"{}\">",
                    html_align(self.field_not_num[j])
                )?;
                fout.write_all(pval)?;
                fout.write_all(b"</td></tr>\n")?;
            } else {
                if po.align {
                    pad(fout, name, self.field_max_len - po.field_sep.len(), true)?;
                    fout.write_all(po.field_sep)?;
                    fout.write_all(b" ")?;
                } else {
                    fout.write_all(name)?;
                    fout.write_all(po.field_sep)?;
                }
                fout.write_all(pval)?;
                fout.write_all(b"\n")?;
            }
        } else {
            // Not expanded and no `fields`, so neither aligned nor HTML.
            fout.write_all(pval)?;
            self.end_field(fout, j)?;
        }
        Ok(())
    }

    /// `do_header`, `fe-print.c:456`: the heading of an aligned or HTML
    /// table, and the border a `standard` table repeats after every row
    /// (`None` for HTML).
    fn do_header(&mut self, fout: &mut impl Write) -> io::Result<Option<Vec<u8>>> {
        let po = self.po;
        let fs = po.field_sep;
        let mut border = None;
        if po.html3 {
            fout.write_all(b"<tr>")?;
        } else {
            // fe-print.c:497. The border is measured before the loop below
            // widens a column to its `PQfname`, so a replacement name shorter
            // than the real one leaves the border short.
            let mut line = Vec::new();
            let plus = b"+".repeat(fs.len());
            if po.standard {
                line.extend_from_slice(&plus);
            }
            for j in 0..self.n_fields {
                let dashes = self.field_max[j] + if po.standard { 2 } else { 0 };
                line.extend(std::iter::repeat_n(b'-', dashes));
                if po.standard || j + 1 < self.n_fields {
                    line.extend_from_slice(&plus);
                }
            }
            if po.standard {
                fout.write_all(&line)?;
                fout.write_all(b"\n")?;
            }
            border = Some(line);
        }
        if po.standard {
            fout.write_all(fs)?;
        }
        for j in 0..self.n_fields {
            if po.html3 {
                write!(fout, "<th align=\"{}\">", html_align(self.field_not_num[j]))?;
                fout.write_all(self.field_names[j])?;
                fout.write_all(b"</th>")?;
            } else {
                // `PQfname`, not `fieldNames`: an aligned heading ignores the
                // replacement names (fe-print.c:526).
                let s = fname(self.res, j);
                self.field_max[j] = self.field_max[j].max(s.len());
                let left = self.field_not_num[j];
                if po.standard {
                    fout.write_all(b" ")?;
                    pad(fout, s, self.field_max[j], left)?;
                    fout.write_all(b" ")?;
                } else {
                    pad(fout, s, self.field_max[j], left)?;
                }
                if po.standard || j + 1 < self.n_fields {
                    fout.write_all(fs)?;
                }
            }
        }
        if let Some(line) = &border {
            fout.write_all(b"\n")?;
            fout.write_all(line)?;
            fout.write_all(b"\n")?;
        } else {
            fout.write_all(b"</tr>\n")?;
        }
        Ok(border)
    }

    /// `output_row`, `fe-print.c:562`.
    fn output_row(&self, fout: &mut impl Write, border: Option<&[u8]>, i: usize) -> io::Result<()> {
        let po = self.po;
        let fields = self.fields.as_deref().unwrap_or_default();
        if po.html3 {
            fout.write_all(b"<tr>")?;
        } else if po.standard {
            fout.write_all(po.field_sep)?;
        }
        for j in 0..self.n_fields {
            let p = fields[i * self.n_fields + j].unwrap_or_default();
            let not_num = self.field_not_num[j];
            if po.html3 {
                write!(fout, "<td align=\"{}\">", html_align(not_num))?;
                fout.write_all(p)?;
                fout.write_all(b"</td>")?;
            } else {
                if po.standard {
                    fout.write_all(b" ")?;
                    pad(fout, p, self.field_max[j], not_num)?;
                    fout.write_all(b" ")?;
                } else {
                    pad(fout, p, self.field_max[j], not_num)?;
                }
                if po.standard || j + 1 < self.n_fields {
                    fout.write_all(po.field_sep)?;
                }
            }
        }
        if po.html3 {
            fout.write_all(b"</tr>")?;
        } else if po.standard {
            // Without a header there is no border, and C prints its NULL
            // with `%s`.
            fout.write_all(b"\n")?;
            fout.write_all(border.unwrap_or(PRINTF_NULL))?;
        }
        fout.write_all(b"\n")
    }
}

/// `fill`, `fe-print.c:786`: one more `filler` than `max - length`, or none
/// when `length` exceeds `max`.
fn fill(fp: &mut impl Write, length: usize, max: usize, filler: u8) -> io::Result<()> {
    let count = if length > max { 0 } else { max - length + 1 };
    fp.write_all(&vec![filler; count])
}

/// `PQdisplayTuples`, `fe-print.c:604`. A `None` separator is C's `NULL`,
/// which means a space (`DEFAULT_FIELD_SEP`, `:613`).
///
/// # Errors
///
/// Whatever writing to `fp` returns.
pub fn display_tuples(
    res: &QueryResult,
    fp: &mut impl Write,
    fill_align: bool,
    field_sep: Option<&[u8]>,
    print_header: bool,
    quiet: bool,
) -> io::Result<()> {
    let field_sep = field_sep.unwrap_or(b" ");
    let n_fields = res.nfields();
    let n_tuples = res.ntuples();

    // fe-print.c:633: the widths are `PQgetlength`s, the filling `strlen`s.
    let f_length: Vec<usize> = if fill_align {
        (0..n_fields)
            .map(|j| {
                (0..n_tuples)
                    .map(|i| value(res, i, j).1)
                    .fold(fname(res, j).len(), usize::max)
            })
            .collect()
    } else {
        Vec::new()
    };

    if print_header {
        for (i, width) in (0..n_fields).map(|i| (i, f_length.get(i))) {
            let name = fname(res, i);
            fp.write_all(name)?;
            if let Some(&width) = width {
                fill(fp, name.len(), width, b' ')?;
            }
            fp.write_all(field_sep)?;
        }
        fp.write_all(b"\n")?;
        for i in 0..n_fields {
            if let Some(&width) = f_length.get(i) {
                fill(fp, 0, width, b'-')?;
            }
            fp.write_all(field_sep)?;
        }
        fp.write_all(b"\n")?;
    }

    for i in 0..n_tuples {
        for j in 0..n_fields {
            let (pval, _) = value(res, i, j);
            fp.write_all(pval)?;
            if let Some(&width) = f_length.get(j) {
                fill(fp, pval.len(), width, b' ')?;
            }
            fp.write_all(field_sep)?;
        }
        fp.write_all(b"\n")?;
    }

    if !quiet {
        let n = res.ntuples();
        write!(fp, "\nQuery returned {n} row{}.\n", plural(n))?;
    }
    fp.flush()
}

/// `PQprintTuples`, `fe-print.c:701`. A `col_width` of 0 means variable
/// width, as any C value not above 0 does.
///
/// # Errors
///
/// Whatever writing to `fout` returns.
pub fn print_tuples(
    res: &QueryResult,
    fout: &mut impl Write,
    print_att_names: bool,
    terse_output: bool,
    col_width: usize,
) -> io::Result<()> {
    let n_fields = res.nfields();
    let n_tups = res.ntuples();
    if n_fields == 0 {
        return Ok(());
    }
    // formatString, fe-print.c:719: "%s %-<colWidth>s" or "%s %s".
    let bar: &[u8] = if terse_output { b"" } else { b"|" };
    let format = |fout: &mut dyn Write, s: &[u8]| -> io::Result<()> {
        fout.write_all(bar)?;
        fout.write_all(b" ")?;
        fout.write_all(s)?;
        fout.write_all(&b" ".repeat(col_width.saturating_sub(s.len())))
    };
    let tborder = b"-".repeat(n_fields * 14);
    let end_line = |fout: &mut dyn Write| -> io::Result<()> {
        if terse_output {
            fout.write_all(b"\n")
        } else {
            fout.write_all(b"|\n")?;
            fout.write_all(&tborder)?;
            fout.write_all(b"\n")
        }
    };

    if !terse_output {
        fout.write_all(&tborder)?;
        fout.write_all(b"\n")?;
    }
    if print_att_names {
        for i in 0..n_fields {
            format(fout, fname(res, i))?;
        }
        end_line(fout)?;
    }
    for i in 0..n_tups {
        for j in 0..n_fields {
            format(fout, value(res, i, j).0)?;
        }
        end_line(fout)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{ExecStatus, FieldDescription};

    fn field(name: &str) -> FieldDescription {
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

    /// A `PGRES_TUPLES_OK` result with these columns and rows; `None` is
    /// SQL NULL.
    fn result(names: &[&str], rows: &[&[Option<&str>]]) -> QueryResult {
        let mut res = QueryResult::new(ExecStatus::TuplesOk);
        res.set_fields(names.iter().map(|name| field(name)).collect());
        for row in rows {
            res.push_row(
                row.iter()
                    .map(|v| v.map(|v| v.as_bytes().to_vec()))
                    .collect(),
            );
        }
        res
    }

    const PLAIN: PrintOpt<'static> = PrintOpt {
        header: true,
        align: true,
        standard: false,
        html3: false,
        expanded: false,
        pager: false,
        field_sep: b"|",
        table_opt: None,
        caption: None,
        field_name: &[],
    };

    fn render(res: &QueryResult, po: &PrintOpt<'_>) -> String {
        let mut out = Vec::new();
        print(&mut out, res, po).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_numeric_column_is_right_aligned_and_a_text_one_left() {
        let res = result(
            &["n", "t"],
            &[&[Some("1"), Some("a")], &[Some("123"), Some("bcd")]],
        );
        assert_eq!(
            render(&res, &PLAIN),
            "  n|t  \n---+---\n  1|a  \n123|bcd\n(2 rows)\n\n"
        );
    }

    /// `printResultSet`, `src/test/isolation/isolationtester.c:1113`: the
    /// one upstream caller, whose output the isolation suite's expected
    /// files hold.
    const PRINT_RESULT_SET: PrintOpt<'static> = PLAIN;

    #[test]
    fn isolationtester_print_result_set_eval_plan_qual_read() {
        // src/test/isolation/expected/eval-plan-qual.out:20-25, step `read`
        // of the first permutation.
        let res = result(
            &["accountid", "balance", "balance2"],
            &[
                &[Some("checking"), Some("850"), Some("1700")],
                &[Some("savings"), Some("600"), Some("1200")],
            ],
        );
        assert_eq!(
            render(&res, &PRINT_RESULT_SET),
            "accountid|balance|balance2\n\
             ---------+-------+--------\n\
             checking |    850|    1700\n\
             savings  |    600|    1200\n\
             (2 rows)\n\
             \n"
        );
    }

    #[test]
    fn isolationtester_print_result_set_eval_plan_qual_no_rows() {
        // src/test/isolation/expected/eval-plan-qual.out:37-40, step `wy2`
        // of the second permutation.
        let res = result(&["balance"], &[]);
        assert_eq!(
            render(&res, &PRINT_RESULT_SET),
            "balance\n-------\n(0 rows)\n\n"
        );
    }

    #[test]
    fn looks_numeric_follows_do_field() {
        for numeric in ["1", "-1.5", "1e5", "1 2", "0.5", "1E-3"] {
            assert!(looks_numeric(numeric.as_bytes()), "{numeric}");
        }
        for text in ["E1", "e1", "1.", "1e", "x", "1-", "١", "1\u{e9}1"] {
            assert!(!looks_numeric(text.as_bytes()), "{text}");
        }
    }

    #[test]
    fn a_standard_table_without_a_header_prints_the_null_border() {
        let res = result(&["a"], &[&[Some("x")]]);
        let po = PrintOpt {
            header: false,
            standard: true,
            ..PLAIN
        };
        assert_eq!(render(&res, &po), "| x |\n(null)\n");
    }

    #[test]
    fn an_empty_field_still_separates_in_an_unaligned_html_table() {
        let res = result(&["a", "b"], &[&[None, Some("y")]]);
        let po = PrintOpt {
            header: false,
            align: false,
            html3: true,
            ..PLAIN
        };
        assert_eq!(
            render(&res, &po),
            "|<table ><tr><td align=\"right\"></td><td align=\"right\">y</td></tr>\n</table>\n"
        );
    }

    #[test]
    fn the_border_is_measured_before_the_heading_widens_a_column() {
        let res = result(&["long_name"], &[&[Some("v")]]);
        let po = PrintOpt {
            standard: true,
            field_name: &[b"x"],
            ..PLAIN
        };
        assert_eq!(
            render(&res, &po),
            "+---+\n| long_name |\n+---+\n| v         |\n+---+\n(1 row)\n\n"
        );
    }

    #[test]
    fn the_pager_never_pages() {
        let res = result(
            &["a"],
            &(0..100).map(|_| &[Some("x")][..]).collect::<Vec<_>>(),
        );
        let paged = PrintOpt {
            pager: true,
            ..PLAIN
        };
        assert_eq!(render(&res, &paged), render(&res, &PLAIN));
    }

    #[test]
    fn a_result_without_fields_prints_nothing() {
        assert_eq!(render(&QueryResult::new(ExecStatus::CommandOk), &PLAIN), "");
    }

    #[test]
    fn display_tuples_fills_one_past_the_widest() {
        let res = result(&["ab", "c"], &[&[Some("x"), None]]);
        let mut out = Vec::new();
        display_tuples(&res, &mut out, true, None, true, false).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "ab  c  \n--- -- \nx   \u{20}  \n\nQuery returned 1 row.\n"
        );
    }

    #[test]
    fn print_tuples_pads_to_the_column_width() {
        let res = result(&["a"], &[&[Some("xy")]]);
        let mut out = Vec::new();
        print_tuples(&res, &mut out, true, false, 4).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "--------------\n| a   |\n--------------\n| xy  |\n--------------\n"
        );
    }
}
