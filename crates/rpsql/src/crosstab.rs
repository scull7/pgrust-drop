//! `\crosstabview`: `src/bin/psql/crosstabview.c`.
//!
//! The command itself only records its arguments (`command.c:997`); the next
//! `SendQuery` hands its last result to [`print_result_in_crosstab`] instead
//! of the plain printer (`common.c:1060`). That function is a calculation:
//! [`pivot`] turns the result into a [`Table`], and
//! [`crate::print::print_table`] renders it with the same code every other
//! result goes through.
//!
//! Upstream collects the distinct header values in a hand-rolled AVL tree
//! (`crosstabview.c:431`-`:585`) and looks them up again with `bsearch`. Both
//! are only a sorted set under `pivotFieldCompare`; here that is a
//! `BTreeMap` and a binary search over the same order, which yields the same
//! sorted arrays and the same ranks.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use rlibpq::{ExecStatus, QueryResult};

use crate::print::{PrintError, Table, column_type_alignment, print_table};
use crate::settings::PrintQueryOpt;
use crate::slash::dequote_downcase_identifier;

/// `CROSSTABVIEW_MAX_COLUMNS` (`crosstabview.h:24`).
pub const CROSSTABVIEW_MAX_COLUMNS: usize = 1600;

/// `pset.ctv_args` (`settings.h:133`): the vertical header, horizontal
/// header, data and sort columns `\crosstabview` was given, each a column
/// number or a (possibly quoted) name, `None` when omitted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CtvArgs(pub [Option<String>; 4]);

/// Why a result could not be shown as a crosstab. Each is one
/// `pg_log_error` of `crosstabview.c`; [`CrosstabError::message`] is its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrosstabError {
    /// `crosstabview.c:124`.
    NotTuples,
    /// `crosstabview.c:130`.
    TooFewColumns,
    /// `crosstabview.c:157`.
    SameHeaderColumns,
    /// `crosstabview.c:173`.
    DataColumnRequired,
    /// `crosstabview.c:229`.
    TooManyColumns,
    /// `crosstabview.c:398`: two rows land in one cell. The row and column
    /// names are as C prints them: the value, else the null string, else
    /// `(null)`.
    MultipleValues {
        /// The vertical header value.
        row: Vec<u8>,
        /// The horizontal header value.
        column: Vec<u8>,
    },
    /// `crosstabview.c:646`: `number` is what C prints, `atoi(arg)`.
    ColumnNumberOutOfRange {
        /// The column number as given.
        number: i32,
        /// `PQnfields`.
        nfields: usize,
    },
    /// `crosstabview.c:671`, with the dequoted name.
    AmbiguousColumnName(Vec<u8>),
    /// `crosstabview.c:679`, with the dequoted name.
    ColumnNameNotFound(Vec<u8>),
    /// The pivoted table uses a print option this port does not render yet.
    Print(PrintError),
}

impl CrosstabError {
    /// The message `pg_log_error` is handed, as bytes: a column name or a
    /// value reaches stderr as the server sent it.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        let quoted = |what: &str, name: &[u8]| {
            let mut out = format!("\\crosstabview: {what}: \"").into_bytes();
            out.extend_from_slice(name);
            out.push(b'"');
            out
        };
        match self {
            Self::NotTuples => b"\\crosstabview: statement did not return a result set".to_vec(),
            Self::TooFewColumns => {
                b"\\crosstabview: query must return at least three columns".to_vec()
            }
            Self::SameHeaderColumns => {
                b"\\crosstabview: vertical and horizontal headers must be different columns"
                    .to_vec()
            }
            Self::DataColumnRequired => b"\\crosstabview: data column must be specified when \
                query returns more than three columns"
                .to_vec(),
            Self::TooManyColumns => format!(
                "\\crosstabview: maximum number of columns ({CROSSTABVIEW_MAX_COLUMNS}) exceeded"
            )
            .into_bytes(),
            Self::MultipleValues { row, column } => {
                let mut out =
                    b"\\crosstabview: query result contains multiple data values for row \""
                        .to_vec();
                out.extend_from_slice(row);
                out.extend_from_slice(b"\", column \"");
                out.extend_from_slice(column);
                out.push(b'"');
                out
            }
            Self::ColumnNumberOutOfRange { number, nfields } => {
                format!("\\crosstabview: column number {number} is out of range 1..{nfields}")
                    .into_bytes()
            }
            Self::AmbiguousColumnName(name) => quoted("ambiguous column name", name),
            Self::ColumnNameNotFound(name) => quoted("column name not found", name),
            Self::Print(err) => err.to_string().into_bytes(),
        }
    }
}

impl std::fmt::Display for CrosstabError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for CrosstabError {}

/// `PrintResultInCrosstab()` (`crosstabview.c:104`), minus the writing:
/// the bytes `printTable` would put on `queryFout`.
///
/// # Errors
/// Every `goto error_return` of upstream's, as a [`CrosstabError`].
pub fn print_result_in_crosstab(
    res: &QueryResult,
    args: &CtvArgs,
    popt: &PrintQueryOpt,
) -> Result<Vec<u8>, CrosstabError> {
    let table = pivot(res, args, popt.null_print.as_deref())?;
    print_table(table, popt).map_err(CrosstabError::Print)
}

/// `pivot_field` (`crosstabview.c:22`): one distinct header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PivotField<'a> {
    /// The value, `None` for a NULL.
    name: Option<&'a [u8]>,
    /// The sort column's value in the row where `name` first appeared.
    sort_value: Option<&'a [u8]>,
    /// First-appearance order, then, with a sort column, display order.
    rank: usize,
}

/// `pivotFieldCompare()` (`crosstabview.c:695`): NULLs are equal to each
/// other and sort after every value; values compare as `strcmp` does, byte
/// by unsigned byte.
fn pivot_field_compare(a: Option<&[u8]>, b: Option<&[u8]>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (Some(a), Some(b)) => a.cmp(b),
    }
}

/// A header value as a key ordered by [`pivot_field_compare`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PivotKey<'a>(Option<&'a [u8]>);

impl Ord for PivotKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        pivot_field_compare(self.0, other.0)
    }
}

impl PartialOrd for PivotKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `avl_tree` (`crosstabview.c:73`): the distinct values seen so far.
#[derive(Default)]
struct Distinct<'a>(BTreeMap<PivotKey<'a>, PivotField<'a>>);

impl<'a> Distinct<'a> {
    /// `avlMergeValue()` (`crosstabview.c:560`): add `name` unless present,
    /// ranked by how many values came before it.
    fn merge(&mut self, name: Option<&'a [u8]>, sort_value: Option<&'a [u8]>) {
        let rank = self.0.len();
        self.0.entry(PivotKey(name)).or_insert(PivotField {
            name,
            sort_value,
            rank,
        });
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    /// `avlCollectFields()` (`crosstabview.c:577`): the values in
    /// [`pivot_field_compare`] order.
    fn into_sorted(self) -> Vec<PivotField<'a>> {
        self.0.into_values().collect()
    }
}

/// The `bsearch` of `printCrosstab` (`crosstabview.c:363`, `:377`). Every
/// value of the result was merged, so it is always found.
fn find<'f, 'a>(fields: &'f [PivotField<'a>], name: Option<&[u8]>) -> &'f PivotField<'a> {
    let at = fields
        .binary_search_by(|f| pivot_field_compare(f.name, name))
        .expect("every value of the result was merged into the header set");
    &fields[at]
}

/// `atoi()`, which C defines as `(int) strtol(s, NULL, 10)`: an optional
/// sign and the digits after it, saturated to the range of a 64-bit `long`
/// and then truncated to `int`, as a 64-bit Unix build does. Callers have
/// already checked that `s` is a sign and digits, so there is no leading
/// whitespace to skip.
fn atoi(s: &[u8]) -> i32 {
    let (negative, digits) = match s.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, s),
    };
    let mut value: i64 = 0;
    for &d in digits.iter().take_while(|d| d.is_ascii_digit()) {
        let d = i64::from(d - b'0');
        value = if negative {
            value.saturating_mul(10).saturating_sub(d)
        } else {
            value.saturating_mul(10).saturating_add(d)
        };
    }
    // The `(int)` conversion keeps the low 32 bits.
    #[allow(clippy::cast_possible_truncation)]
    let truncated = value as i32;
    truncated
}

/// `rankSort()` (`crosstabview.c:588`): re-rank the horizontal header by
/// each value's sort-column value, when that matches `/^-?\d*$/`, else 0.
///
/// The sort is stable. C's `qsort` promises nothing about equal keys, so
/// two header values with the same sort value come out in whichever order
/// the platform's `qsort` leaves them; here they keep the order
/// `pivotFieldCompare` sorted them into (`docs/divergences.md`).
fn rank_sort(columns: &mut [PivotField<'_>]) {
    let rank_of = |value: Option<&[u8]>| match value {
        Some(v) => {
            let digits = v.strip_prefix(b"-").unwrap_or(v);
            if digits.iter().all(u8::is_ascii_digit) {
                atoi(v)
            } else {
                0
            }
        }
        None => 0,
    };
    let mut hmap: Vec<(i32, usize)> = columns
        .iter()
        .enumerate()
        .map(|(i, f)| (rank_of(f.sort_value), i))
        .collect();
    hmap.sort_by_key(|&(rank, _)| rank);
    for (rank, &(_, i)) in hmap.iter().enumerate() {
        columns[i].rank = rank;
    }
}

/// `indexOfColumn()` (`crosstabview.c:636`): a 1-based column number, or a
/// name matched after `dequote_downcase_identifier`.
fn index_of_column(arg: &str, res: &QueryResult) -> Result<usize, CrosstabError> {
    let nfields = res.nfields();
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        let number = atoi(arg.as_bytes());
        return match usize::try_from(number) {
            Ok(n) if (1..=nfields).contains(&n) => Ok(n - 1),
            _ => Err(CrosstabError::ColumnNumberOutOfRange { number, nfields }),
        };
    }

    let name = dequote_downcase_identifier(arg.as_bytes(), true);
    let mut found = None;
    for i in 0..nfields {
        if res.fname(i) == Some(name.as_slice()) {
            if found.is_some() {
                return Err(CrosstabError::AmbiguousColumnName(name));
            }
            found = Some(i);
        }
    }
    found.ok_or(CrosstabError::ColumnNameNotFound(name))
}

/// A cell of `res`, `None` for a NULL.
fn cell(res: &QueryResult, row: usize, column: usize) -> Option<&[u8]> {
    if res.is_null(row, column) {
        None
    } else {
        Some(res.value(row, column).unwrap_or(b""))
    }
}

/// The alignment `column_type_alignment(PQftype(res, column))` gives.
fn alignment(res: &QueryResult, column: usize) -> crate::print::Align {
    column_type_alignment(res.ftype(column).unwrap_or(0))
}

/// The table `PrintResultInCrosstab` (`crosstabview.c:104`) and
/// `printCrosstab` (`crosstabview.c:286`) build from `res`: the first column
/// is the vertical header, one column follows per distinct horizontal header
/// value, and each data value lands in the cell its two header values name.
///
/// `null_print` is `popt.nullPrint`.
///
/// # Errors
/// Every `goto error_return` and `goto error` of upstream's.
pub fn pivot(
    res: &QueryResult,
    args: &CtvArgs,
    null_print: Option<&str>,
) -> Result<Table, CrosstabError> {
    if res.status() != ExecStatus::TuplesOk {
        return Err(CrosstabError::NotTuples);
    }
    let nfields = res.nfields();
    if nfields < 3 {
        return Err(CrosstabError::TooFewColumns);
    }
    let [rows_arg, columns_arg, data_arg, sort_arg] = &args.0;
    let field_for_rows = rows_arg
        .as_deref()
        .map_or(Ok(0), |a| index_of_column(a, res))?;
    let field_for_columns = columns_arg
        .as_deref()
        .map_or(Ok(1), |a| index_of_column(a, res))?;
    if field_for_columns == field_for_rows {
        return Err(CrosstabError::SameHeaderColumns);
    }
    let field_for_data = match data_arg.as_deref() {
        // The one column that is neither header; with more than three there
        // is no single one.
        None if nfields != 3 => return Err(CrosstabError::DataColumnRequired),
        // Both headers are among the three columns, so the data column is
        // the index they leave: 0 + 1 + 2 less theirs.
        None => 3 - field_for_rows - field_for_columns,
        Some(a) => index_of_column(a, res)?,
    };
    let sort_field_for_columns = sort_arg
        .as_deref()
        .map(|a| index_of_column(a, res))
        .transpose()?;

    // First part: the distinct values of both headers
    // (`crosstabview.c:211`).
    let mut columns = Distinct::default();
    let mut rows = Distinct::default();
    for rn in 0..res.ntuples() {
        let sort_value = sort_field_for_columns.and_then(|f| cell(res, rn, f));
        columns.merge(cell(res, rn, field_for_columns), sort_value);
        if columns.len() > CROSSTABVIEW_MAX_COLUMNS {
            return Err(CrosstabError::TooManyColumns);
        }
        rows.merge(cell(res, rn, field_for_rows), None);
    }

    // Second and third parts: sorted arrays, re-ranked by the sort column.
    let mut columns = columns.into_sorted();
    let rows = rows.into_sorted();
    if sort_field_for_columns.is_some() {
        rank_sort(&mut columns);
    }

    // Fourth part, `printCrosstab`.
    let null = null_print.unwrap_or("").as_bytes();
    let num_columns = columns.len();

    // Step 1: the horizontal header, in rank order (`crosstabview.c:304`).
    let mut headers = Vec::with_capacity(num_columns + 1);
    let mut aligns = Vec::with_capacity(num_columns + 1);
    headers.push(res.fname(field_for_rows).unwrap_or(b"").to_vec());
    aligns.push(alignment(res, field_for_rows));
    let mut horiz_map = vec![0; num_columns];
    for (i, f) in columns.iter().enumerate() {
        horiz_map[f.rank] = i;
    }
    let col_align = alignment(res, field_for_data);
    for &i in &horiz_map {
        headers.push(columns[i].name.unwrap_or(null).to_vec());
        aligns.push(col_align);
    }

    // Step 2: the vertical header in the first column (`crosstabview.c:337`).
    let mut cells: Vec<Vec<Option<&[u8]>>> = vec![vec![None; num_columns + 1]; rows.len()];
    for f in &rows {
        cells[f.rank][0] = Some(f.name.unwrap_or(null));
    }

    // Step 3: the content cells (`crosstabview.c:350`).
    for rn in 0..res.ntuples() {
        let rp = find(&rows, cell(res, rn, field_for_rows));
        let cp = find(&columns, cell(res, rn, field_for_columns));
        let slot = &mut cells[rp.rank][1 + cp.rank];
        if slot.is_some() {
            let shown = |name: Option<&[u8]>| {
                name.or(null_print.map(str::as_bytes))
                    .unwrap_or(b"(null)")
                    .to_vec()
            };
            return Err(CrosstabError::MultipleValues {
                row: shown(rp.name),
                column: shown(cp.name),
            });
        }
        *slot = Some(cell(res, rn, field_for_data).unwrap_or(null));
    }

    // Cells no row filled print as empty strings (`crosstabview.c:416`).
    let cells = cells
        .into_iter()
        .map(|row| row.into_iter().map(|c| c.unwrap_or(b"").to_vec()).collect())
        .collect();
    Ok(Table {
        headers,
        cells,
        aligns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::Align::{Left, Right};
    use rlibpq::{Backend, FieldDescription, QueryRunner, TransactionStatus};

    const TEXT: u32 = 25;
    const INT4: u32 = 23;

    fn field(name: &str, typid: u32) -> FieldDescription {
        FieldDescription {
            name: name.as_bytes().to_vec(),
            tableid: 0,
            columnid: 0,
            typid,
            typlen: -1,
            atttypmod: -1,
            format: 0,
        }
    }

    fn result(fields: &[(&str, u32)], rows: &[&[Option<&str>]]) -> QueryResult {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(
                fields.iter().map(|&(n, t)| field(n, t)).collect(),
            ))
            .unwrap();
        for row in rows {
            runner
                .push(Backend::DataRow(
                    row.iter()
                        .map(|c| c.map(|s| s.as_bytes().to_vec()))
                        .collect(),
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

    fn args(a: &[&str]) -> CtvArgs {
        let mut out = CtvArgs::default();
        for (slot, a) in out.0.iter_mut().zip(a) {
            *slot = Some((*a).to_string());
        }
        out
    }

    fn render(res: &QueryResult, a: &[&str]) -> Result<String, String> {
        print_result_in_crosstab(res, &args(a), &PrintQueryOpt::default())
            .map(|b| String::from_utf8(b).unwrap())
            .map_err(|e| e.to_string())
    }

    /// `psql_crosstab.sql:19`'s query: `v`, the year and a count.
    fn years() -> QueryResult {
        result(
            &[("v", TEXT), ("extract", 1700), ("count", 20)],
            &[
                &[Some("v0"), Some("2014"), Some("2")],
                &[Some("v0"), Some("2015"), Some("1")],
                &[Some("v1"), Some("2015"), Some("3")],
                &[Some("v2"), Some("2015"), Some("1")],
            ],
        )
    }

    #[test]
    fn basic_usage_with_three_columns() {
        // `psql_crosstab.out:30`-`:36`.
        assert_eq!(
            render(&years(), &[]).unwrap(),
            " v  | 2014 | 2015 \n\
             ----+------+------\n \
             v0 |    2 |    1\n \
             v1 |      |    3\n \
             v2 |      |    1\n\
             (3 rows)\n\n"
        );
    }

    #[test]
    fn a_sort_column_orders_the_horizontal_header() {
        // Months by their number, whatever order the rows bring them in.
        let res = result(
            &[("v", TEXT), ("m", TEXT), ("n", 1700), ("c", 20)],
            &[
                &[Some("v0"), Some("Jul"), Some("7"), Some("2")],
                &[Some("v0"), Some("Dec"), Some("12"), Some("1")],
                &[Some("v1"), Some("Apr"), Some("4"), Some("2")],
                &[Some("v2"), Some("Jan"), Some("1"), Some("1")],
            ],
        );
        let out = render(&res, &["v", "m", "4", "n"]).unwrap();
        assert!(out.starts_with(" v  | Jan | Apr | Jul | Dec \n"), "{out}");
        // Without it, header order is order of first appearance.
        let out = render(&res, &["v", "m", "4"]).unwrap();
        assert!(out.starts_with(" v  | Jul | Dec | Apr | Jan \n"), "{out}");
    }

    #[test]
    fn a_non_integer_sort_value_ranks_as_zero() {
        // `crosstabview.c:599`: only `/^-?\d+$/` counts; anything else is 0.
        let res = result(
            &[("v", TEXT), ("h", TEXT), ("d", TEXT), ("s", TEXT)],
            &[
                &[Some("a"), Some("x"), Some("1"), Some("5")],
                &[Some("a"), Some("y"), Some("2"), Some("-1")],
                &[Some("a"), Some("z"), Some("3"), Some("two")],
            ],
        );
        let out = render(&res, &["v", "h", "d", "s"]).unwrap();
        assert!(out.starts_with(" v | y | z | x \n"), "{out}");
    }

    #[test]
    fn equal_sort_values_keep_their_header_order() {
        // The stable sort `docs/divergences.md` records: `y` and `x` tie at
        // 1 and stay in the order `pivotFieldCompare` put them, `x` first.
        let res = result(
            &[("v", TEXT), ("h", TEXT), ("d", TEXT), ("s", TEXT)],
            &[
                &[Some("a"), Some("y"), Some("1"), Some("1")],
                &[Some("a"), Some("z"), Some("2"), Some("0")],
                &[Some("a"), Some("x"), Some("3"), Some("1")],
            ],
        );
        let out = render(&res, &["v", "h", "d", "s"]).unwrap();
        assert!(out.starts_with(" v | z | x | y \n"), "{out}");
    }

    #[test]
    fn nulls_head_a_column_of_their_own_after_every_value() {
        // `pivotFieldCompare`: NULL sorts last and equals NULL; the header
        // and a NULL data value print as the null string.
        let res = result(
            &[("v", TEXT), ("h", TEXT), ("i", TEXT)],
            &[
                &[Some("v1"), None, None],
                &[Some("v1"), Some("h0"), Some("3")],
            ],
        );
        let popt = PrintQueryOpt {
            null_print: Some("#null#".to_string()),
            ..PrintQueryOpt::default()
        };
        let table = pivot(&res, &CtvArgs::default(), popt.null_print.as_deref()).unwrap();
        assert_eq!(
            table.headers,
            [b"v".to_vec(), b"#null#".to_vec(), b"h0".to_vec()]
        );
        assert_eq!(
            table.cells,
            [[b"v1".to_vec(), b"#null#".to_vec(), b"3".to_vec()]]
        );
    }

    #[test]
    fn the_data_column_takes_its_alignment_from_its_type() {
        let res = result(
            &[("a", INT4), ("b", INT4), ("c", TEXT)],
            &[&[Some("1"), Some("2"), Some("3")]],
        );
        let table = pivot(&res, &CtvArgs::default(), None).unwrap();
        assert_eq!(table.aligns, [Right, Left]);
    }

    #[test]
    fn columns_are_named_by_number_or_by_dequoted_downcased_name() {
        let res = result(
            &[("22", INT4), ("b", INT4), ("Foo", INT4)],
            &[&[Some("1"), Some("2"), Some("3")]],
        );
        // `psql_crosstab.sql:84`.
        assert!(render(&res, &["\"22\"", "B", "\"Foo\""]).is_ok());
        assert_eq!(
            render(&res, &["1", "2", "Foo"]).unwrap_err(),
            "\\crosstabview: column name not found: \"foo\""
        );
        assert_eq!(
            render(&res, &["1", "\"B\"", "\"Foo\""]).unwrap_err(),
            "\\crosstabview: column name not found: \"B\""
        );
        assert_eq!(
            render(&res, &["2", "1", "5"]).unwrap_err(),
            "\\crosstabview: column number 5 is out of range 1..3"
        );
        assert_eq!(
            render(&res, &["0", "1"]).unwrap_err(),
            "\\crosstabview: column number 0 is out of range 1..3"
        );
    }

    #[test]
    fn a_name_two_columns_share_is_ambiguous() {
        let res = result(
            &[("a", INT4), ("a", INT4), ("c", INT4)],
            &[&[Some("1"), Some("2"), Some("3")]],
        );
        assert_eq!(
            render(&res, &["a", "c"]).unwrap_err(),
            "\\crosstabview: ambiguous column name: \"a\""
        );
    }

    #[test]
    fn the_argument_errors_are_upstreams() {
        let two = result(&[("a", INT4), ("b", INT4)], &[]);
        assert_eq!(
            render(&two, &[]).unwrap_err(),
            "\\crosstabview: query must return at least three columns"
        );
        let four = result(&[("v", TEXT), ("h", TEXT), ("i", INT4), ("c", TEXT)], &[]);
        assert_eq!(
            render(&four, &["2", "h", "4"]).unwrap_err(),
            "\\crosstabview: vertical and horizontal headers must be different columns"
        );
        assert_eq!(
            render(&four, &[]).unwrap_err(),
            "\\crosstabview: data column must be specified when query returns more than three columns"
        );
    }

    #[test]
    fn more_than_1600_header_values_is_refused() {
        let values: Vec<String> = (0..=CROSSTABVIEW_MAX_COLUMNS)
            .map(|n| n.to_string())
            .collect();
        let rows: Vec<Vec<Option<&str>>> = values
            .iter()
            .map(|v| vec![Some(v.as_str()), Some(v.as_str()), Some("1")])
            .collect();
        let rows: Vec<&[Option<&str>]> = rows.iter().map(Vec::as_slice).collect();
        let res = result(&[("a", INT4), ("a", INT4), ("n", INT4)], &rows);
        assert_eq!(
            render(&res, &[]).unwrap_err(),
            "\\crosstabview: maximum number of columns (1600) exceeded"
        );
        // Exactly 1600 is allowed.
        let res = result(
            &[("a", INT4), ("a", INT4), ("n", INT4)],
            &rows[..CROSSTABVIEW_MAX_COLUMNS],
        );
        assert!(pivot(&res, &CtvArgs::default(), None).is_ok());
    }

    #[test]
    fn two_values_for_one_cell_name_the_row_and_the_column() {
        // `psql_crosstab.sql:121` (bug #14476).
        let res = result(
            &[("x", INT4), ("y", INT4), ("v", TEXT)],
            &[
                &[Some("1"), Some("10"), Some("*10")],
                &[Some("1"), Some("10"), Some("*")],
            ],
        );
        assert_eq!(
            render(&res, &[]).unwrap_err(),
            "\\crosstabview: query result contains multiple data values for row \"1\", column \"10\""
        );
        // A NULL header shows as the null string, or `(null)` without one.
        let res = result(
            &[("x", INT4), ("y", INT4), ("v", TEXT)],
            &[&[None, Some("1"), None], &[None, Some("1"), None]],
        );
        let err = pivot(&res, &CtvArgs::default(), None).unwrap_err();
        assert_eq!(
            err,
            CrosstabError::MultipleValues {
                row: b"(null)".to_vec(),
                column: b"1".to_vec()
            }
        );
        let err = pivot(&res, &CtvArgs::default(), Some("")).unwrap_err();
        assert_eq!(
            err,
            CrosstabError::MultipleValues {
                row: Vec::new(),
                column: b"1".to_vec()
            }
        );
    }

    #[test]
    fn a_result_without_tuples_is_refused() {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::CommandComplete(b"CREATE TABLE".to_vec()))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        let res = runner.into_results().remove(0);
        assert_eq!(
            render(&res, &[]).unwrap_err(),
            "\\crosstabview: statement did not return a result set"
        );
    }

    #[test]
    fn atoi_saturates_as_strtol_then_truncates_to_int() {
        assert_eq!(atoi(b"42"), 42);
        assert_eq!(atoi(b"-3"), -3);
        assert_eq!(atoi(b"-"), 0);
        assert_eq!(atoi(b""), 0);
        assert_eq!(atoi(b"007"), 7);
        // 2^32 + 5 keeps its low 32 bits.
        assert_eq!(atoi(b"4294967301"), 5);
        // strtol saturates at LONG_MAX, whose low 32 bits are -1.
        assert_eq!(atoi(b"99999999999999999999"), -1);
        assert_eq!(atoi(b"-99999999999999999999"), 0);
    }
}
