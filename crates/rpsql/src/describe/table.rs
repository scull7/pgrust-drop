//! `\d` with a pattern: `describeTableDetails()` (`describe.c:1492`), and
//! the relation it finds described by `describeOneTableDetails()`
//! (`describe.c:1575`) and `add_tablespace_footer()` (`describe.c:3651`).
//!
//! Upstream assembles the printed table by hand from a dozen queries, each
//! depending on what the one before found. Here every query is built, and
//! every answer turned into cells or footers, by a pure function below; the
//! order they run in, and what stops them, is
//! `crate::command`'s `describe_table_details`.
//!
//! A cell or footer is bytes, as the server sent them. A title is text,
//! because the printer takes it as such, so a schema or relation name in
//! it that is not UTF-8 is converted lossily; under a UTF-8 client encoding,
//! which is every cluster `pgdrop initdb` makes, none can be.

use std::fmt::Write as _;

use super::{PatternError, PatternVars, ServerContext, validate_sql_name_pattern};

/// One row of a result, as `PQgetvalue` and `PQgetisnull` read it: `None`
/// for a null.
pub type Row<'a> = [Option<&'a [u8]>];

/// `PQgetvalue()`: a null reads as the empty string.
fn value<'a>(row: &Row<'a>, i: usize) -> &'a [u8] {
    row.get(i).copied().flatten().unwrap_or_default()
}

/// `PQgetisnull()`.
fn is_null(row: &Row<'_>, i: usize) -> bool {
    row.get(i).copied().flatten().is_none()
}

/// `strcmp(PQgetvalue(…), "t") == 0`.
fn is_t(row: &Row<'_>, i: usize) -> bool {
    value(row, i) == b"t"
}

/// `*PQgetvalue(…)`: the first byte, `'\0'` for an empty value.
fn first_byte(row: &Row<'_>, i: usize) -> u8 {
    value(row, i).first().copied().unwrap_or(0)
}

/// `pg_class.relkind` (`pg_class.h:167`-`:176`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelKind {
    /// `RELKIND_RELATION`, `'r'`.
    Relation,
    /// `RELKIND_INDEX`, `'i'`.
    Index,
    /// `RELKIND_SEQUENCE`, `'S'`.
    Sequence,
    /// `RELKIND_TOASTVALUE`, `'t'`.
    ToastValue,
    /// `RELKIND_VIEW`, `'v'`.
    View,
    /// `RELKIND_MATVIEW`, `'m'`.
    MatView,
    /// `RELKIND_COMPOSITE_TYPE`, `'c'`.
    CompositeType,
    /// `RELKIND_FOREIGN_TABLE`, `'f'`.
    ForeignTable,
    /// `RELKIND_PARTITIONED_TABLE`, `'p'`.
    PartitionedTable,
    /// `RELKIND_PARTITIONED_INDEX`, `'I'`.
    PartitionedIndex,
    /// Any other byte, which upstream titles `?%c?` (`describe.c:2061`).
    Other(u8),
}

impl RelKind {
    /// The kind `c` names.
    #[must_use]
    pub fn from_byte(c: u8) -> Self {
        match c {
            b'r' => Self::Relation,
            b'i' => Self::Index,
            b'S' => Self::Sequence,
            b't' => Self::ToastValue,
            b'v' => Self::View,
            b'm' => Self::MatView,
            b'c' => Self::CompositeType,
            b'f' => Self::ForeignTable,
            b'p' => Self::PartitionedTable,
            b'I' => Self::PartitionedIndex,
            other => Self::Other(other),
        }
    }

    /// The kinds whose columns show collation, nullability and default
    /// (`describe.c:1883`-`:1889`), and which may carry column comments
    /// (`:1983`-`:1988`).
    fn shows_column_details(self) -> bool {
        matches!(
            self,
            Self::Relation
                | Self::View
                | Self::MatView
                | Self::ForeignTable
                | Self::CompositeType
                | Self::PartitionedTable
        )
    }

    /// `RELKIND_INDEX` or `RELKIND_PARTITIONED_INDEX`.
    #[must_use]
    pub fn is_index(self) -> bool {
        matches!(self, Self::Index | Self::PartitionedIndex)
    }

    /// The kinds that get a table's footers (`describe.c:2414`-`:2419`) and
    /// its closing ones (`:3384`-`:3389`). `RELKIND_PARTITIONED_INDEX` is
    /// listed in both, but the index footer's `if` takes it first
    /// (`:2301`), so only the closing ones reach it.
    #[must_use]
    pub fn has_table_footers(self) -> bool {
        matches!(
            self,
            Self::Relation
                | Self::MatView
                | Self::ForeignTable
                | Self::PartitionedTable
                | Self::PartitionedIndex
                | Self::ToastValue
        )
    }

    /// The kinds `add_tablespace_footer()` shows a tablespace for
    /// (`describe.c:3655`-`:3660`).
    fn has_tablespace(self) -> bool {
        matches!(
            self,
            Self::Relation
                | Self::MatView
                | Self::Index
                | Self::PartitionedTable
                | Self::PartitionedIndex
                | Self::ToastValue
        )
    }

    /// A partitioned table or index: `is_partitioned` (`describe.c:3396`).
    #[must_use]
    pub fn is_partitioned(self) -> bool {
        matches!(self, Self::PartitionedTable | Self::PartitionedIndex)
    }
}

/// The query half of `describeTableDetails()` (`describe.c:1500`-`:1520`):
/// the oid, schema and name of each relation `pattern` matches.
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn describe_table_details_query(
    pattern: Option<&str>,
    system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT c.oid,\n",
        "  n.nspname,\n",
        "  c.relname\n",
        "FROM pg_catalog.pg_class c\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n",
    ));
    let have_where = !system && pattern.is_none();
    if have_where {
        buf.push_str(concat!(
            "WHERE n.nspname <> 'pg_catalog'\n",
            "      AND n.nspname <> 'information_schema'\n",
        ));
    }
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        have_where,
        false,
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("c.relname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 2, 3;");
    Ok(buf)
}

/// What `describeTableDetails()` logs, when not quiet, for nothing found
/// (`describe.c:1531`-`:1535`).
#[must_use]
pub fn relations_not_found(pattern: Option<&str>) -> String {
    match pattern {
        Some(pattern) => format!("Did not find any relation named \"{pattern}\"."),
        None => "Did not find any relations.".to_string(),
    }
}

/// What `describeOneTableDetails()` logs, when not quiet, when the relation
/// has gone (`describe.c:1731`).
#[must_use]
pub fn relation_oid_not_found(oid: &str) -> String {
    format!("Did not find any relation with OID {oid}.")
}

/// `describeOneTableDetails()`'s general query (`describe.c:1636`-`:1721`).
#[must_use]
pub fn table_info_query(oid: &str, verbose: bool, sversion: i32) -> String {
    let reloptions = if verbose {
        concat!(
            "pg_catalog.array_to_string(c.reloptions || ",
            "array(select 'toast.' || x from pg_catalog.unnest(tc.reloptions) x), ', ')\n",
        )
    } else {
        "''"
    };
    let (flags, tail, am_join) = if sversion >= 120_000 {
        (
            concat!(
                "c.relhastriggers, c.relrowsecurity, c.relforcerowsecurity, ",
                "false AS relhasoids, c.relispartition, ",
            ),
            "c.relpersistence, c.relreplident, am.amname\n",
            "LEFT JOIN pg_catalog.pg_am am ON (c.relam = am.oid)\n",
        )
    } else if sversion >= 100_000 {
        (
            concat!(
                "c.relhastriggers, c.relrowsecurity, c.relforcerowsecurity, ",
                "c.relhasoids, c.relispartition, ",
            ),
            "c.relpersistence, c.relreplident\n",
            "",
        )
    } else if sversion >= 90_500 {
        (
            concat!(
                "c.relhastriggers, c.relrowsecurity, c.relforcerowsecurity, ",
                "c.relhasoids, false as relispartition, ",
            ),
            "c.relpersistence, c.relreplident\n",
            "",
        )
    } else if sversion >= 90_400 {
        (
            concat!(
                "c.relhastriggers, false, false, c.relhasoids, ",
                "false as relispartition, ",
            ),
            "c.relpersistence, c.relreplident\n",
            "",
        )
    } else {
        (
            concat!(
                "c.relhastriggers, false, false, c.relhasoids, ",
                "false as relispartition, ",
            ),
            "c.relpersistence\n",
            "",
        )
    };
    format!(
        "SELECT c.relchecks, c.relkind, c.relhasindex, c.relhasrules, \
         {flags}{reloptions}, c.reltablespace, \
         CASE WHEN c.reloftype = 0 THEN '' ELSE c.reloftype::pg_catalog.regtype::pg_catalog.text END, \
         {tail}\
         FROM pg_catalog.pg_class c\n \
         LEFT JOIN pg_catalog.pg_class tc ON (c.reltoastrelid = tc.oid)\n\
         {am_join}\
         WHERE c.oid = '{oid}';"
    )
}

/// `tableinfo` (`describe.c:1607`-`:1624`), as read off [`table_info_query`]'s
/// row (`:1735`-`:1755`).
// One flag per catalog column, as upstream has them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    /// `relchecks`
    pub checks: i64,
    /// `relkind`
    pub relkind: RelKind,
    /// `relhasindex`
    pub hasindex: bool,
    /// `relhasrules`
    pub hasrules: bool,
    /// `relhastriggers`
    pub hastriggers: bool,
    /// `relrowsecurity`
    pub rowsecurity: bool,
    /// `relforcerowsecurity`
    pub forcerowsecurity: bool,
    /// `relhasoids`
    pub hasoids: bool,
    /// `relispartition`
    pub ispartition: bool,
    /// The options, with the TOAST table's as `toast.…`; empty without `+`.
    pub reloptions: Vec<u8>,
    /// `reltablespace`: 0 for the database's default.
    pub tablespace: u32,
    /// `reloftype` as a type name; `None` for an untyped table.
    pub reloftype: Option<Vec<u8>>,
    /// `relpersistence`
    pub relpersistence: u8,
    /// `relreplident`, `'d'` before 9.4.
    pub relreplident: u8,
    /// The table access method's name, from 12; `None` for none.
    pub relam: Option<Vec<u8>>,
}

impl TableInfo {
    /// `describe.c:1735`-`:1755`.
    #[must_use]
    pub fn parse(row: &Row<'_>, sversion: i32) -> Self {
        let reloftype = value(row, 11);
        Self {
            checks: super::atoi(value(row, 0)),
            relkind: RelKind::from_byte(first_byte(row, 1)),
            hasindex: is_t(row, 2),
            hasrules: is_t(row, 3),
            hastriggers: is_t(row, 4),
            rowsecurity: is_t(row, 5),
            forcerowsecurity: is_t(row, 6),
            hasoids: is_t(row, 7),
            ispartition: is_t(row, 8),
            reloptions: value(row, 9).to_vec(),
            tablespace: atooid(value(row, 10)),
            reloftype: (!reloftype.is_empty()).then(|| reloftype.to_vec()),
            relpersistence: first_byte(row, 12),
            relreplident: if sversion >= 90_400 {
                first_byte(row, 13)
            } else {
                b'd'
            },
            relam: if sversion >= 120_000 && !is_null(row, 14) {
                Some(value(row, 14).to_vec())
            } else {
                None
            },
        }
    }

    /// `relpersistence == RELPERSISTENCE_UNLOGGED`.
    fn unlogged(&self) -> bool {
        self.relpersistence == b'u'
    }
}

/// `atooid()` (`postgres_ext.h:44`): `strtoul(x, NULL, 10)`, 0 for no
/// digits, and as C's `unsigned int` cast wraps it.
fn atooid(bytes: &[u8]) -> u32 {
    let digits = bytes.trim_ascii_start();
    let digits = digits.strip_prefix(b"+").unwrap_or(digits);
    digits
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |n, &d| {
            n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
        })
}

/// A sequence's query (`describe.c:1768`-`:1791`); `None` before 10, whose
/// query names the sequence through `fmtId()`, which this port does not
/// have (`docs/divergences.md`).
#[must_use]
pub fn sequence_query(oid: &str, sversion: i32) -> Option<String> {
    (sversion >= 100_000).then(|| {
        format!(
            "SELECT pg_catalog.format_type(seqtypid, NULL) AS \"Type\",\n       \
             seqstart AS \"Start\",\n       \
             seqmin AS \"Minimum\",\n       \
             seqmax AS \"Maximum\",\n       \
             seqincrement AS \"Increment\",\n       \
             CASE WHEN seqcycle THEN 'yes' ELSE 'no' END AS \"Cycles?\",\n       \
             seqcache AS \"Cache\"\n\
             FROM pg_catalog.pg_sequence\n\
             WHERE seqrelid = '{oid}';"
        )
    })
}

/// The query for the column that owns a sequence (`describe.c:1821`-`:1835`).
#[must_use]
pub fn sequence_owner_query(oid: &str) -> String {
    format!(
        "SELECT pg_catalog.quote_ident(nspname) || '.' ||\n   \
         pg_catalog.quote_ident(relname) || '.' ||\n   \
         pg_catalog.quote_ident(attname),\n   \
         d.deptype\n\
         FROM pg_catalog.pg_class c\n\
         INNER JOIN pg_catalog.pg_depend d ON c.oid=d.refobjid\n\
         INNER JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace\n\
         INNER JOIN pg_catalog.pg_attribute a ON (\n \
         a.attrelid=c.oid AND\n \
         a.attnum=d.refobjsubid)\n\
         WHERE d.classid='pg_catalog.pg_class'::pg_catalog.regclass\n \
         AND d.refclassid='pg_catalog.pg_class'::pg_catalog.regclass\n \
         AND d.objid='{oid}'\n \
         AND d.deptype IN ('a', 'i')"
    )
}

/// A sequence's footer (`describe.c:1846`-`:1859`): its owning column, when
/// exactly one row names it.
#[must_use]
pub fn sequence_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    let [row] = rows else {
        return Vec::new();
    };
    let label: &[u8] = match first_byte(row, 1) {
        b'a' => b"Owned by: ",
        b'i' => b"Sequence for identity column: ",
        _ => return Vec::new(),
    };
    vec![[label, value(row, 0)].concat()]
}

/// A sequence's title (`describe.c:1862`-`:1867`).
#[must_use]
pub fn sequence_title(info: &TableInfo, schemaname: &str, relationname: &str) -> String {
    if info.unlogged() {
        format!("Unlogged sequence \"{schemaname}.{relationname}\"")
    } else {
        format!("Sequence \"{schemaname}.{relationname}\"")
    }
}

/// Where each of the per-column query's optional columns landed
/// (`describe.c:1592`-`:1605`); `None` for one not fetched.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColumnLayout {
    /// `show_column_details`: the default, nullability, collation, identity
    /// and generation columns are 2 to 6.
    pub details: bool,
    /// `isindexkey_col`
    pub is_key: Option<usize>,
    /// `indexdef_col`
    pub indexdef: Option<usize>,
    /// `fdwopts_col`
    pub fdwopts: Option<usize>,
    /// `attstorage_col`
    pub storage: Option<usize>,
    /// `attcompression_col`
    pub compression: Option<usize>,
    /// `attstattarget_col`
    pub stattarget: Option<usize>,
    /// `attdescr_col`
    pub descr: Option<usize>,
}

/// The per-column query (`describe.c:1899`-`:1997`) and where its columns
/// landed. `hide_compression` is `HIDE_TOAST_COMPRESSION`.
#[must_use]
pub fn column_query(
    oid: &str,
    info: &TableInfo,
    verbose: bool,
    sversion: i32,
    hide_compression: bool,
) -> (String, ColumnLayout) {
    let relkind = info.relkind;
    let mut layout = ColumnLayout {
        details: relkind.shows_column_details(),
        ..ColumnLayout::default()
    };
    let mut cols = 2;
    let mut next = || {
        cols += 1;
        Some(cols - 1)
    };
    let mut buf =
        String::from("SELECT a.attname,\n  pg_catalog.format_type(a.atttypid, a.atttypmod)");
    if layout.details {
        // Use "pretty" mode for expression to avoid excessive parentheses.
        buf.push_str(concat!(
            ",\n  (SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid, true)",
            "\n   FROM pg_catalog.pg_attrdef d",
            "\n   WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum AND a.atthasdef)",
            ",\n  a.attnotnull",
            ",\n  (SELECT c.collname FROM pg_catalog.pg_collation c, pg_catalog.pg_type t\n",
            "   WHERE c.oid = a.attcollation AND t.oid = a.atttypid ",
            "AND a.attcollation <> t.typcollation) AS attcollation",
        ));
        buf.push_str(if sversion >= 100_000 {
            ",\n  a.attidentity"
        } else {
            ",\n  ''::pg_catalog.char AS attidentity"
        });
        buf.push_str(if sversion >= 120_000 {
            ",\n  a.attgenerated"
        } else {
            ",\n  ''::pg_catalog.char AS attgenerated"
        });
        for _ in 0..5 {
            next();
        }
    }
    if relkind.is_index() {
        if sversion >= 110_000 {
            let _ = write!(
                buf,
                ",\n  CASE WHEN a.attnum <= (SELECT i.indnkeyatts FROM pg_catalog.pg_index i \
                 WHERE i.indexrelid = '{oid}') THEN 'yes' ELSE 'no' END AS is_key"
            );
            layout.is_key = next();
        }
        buf.push_str(",\n  pg_catalog.pg_get_indexdef(a.attrelid, a.attnum, TRUE) AS indexdef");
        layout.indexdef = next();
    }
    // FDW options for foreign table column.
    if relkind == RelKind::ForeignTable {
        buf.push_str(concat!(
            ",\n  CASE WHEN attfdwoptions IS NULL THEN '' ELSE ",
            "  '(' || pg_catalog.array_to_string(ARRAY(SELECT ",
            "pg_catalog.quote_ident(option_name) || ' ' || ",
            "pg_catalog.quote_literal(option_value)  FROM ",
            "  pg_catalog.pg_options_to_table(attfdwoptions)), ', ') || ')' END AS attfdwoptions",
        ));
        layout.fdwopts = next();
    }
    if verbose {
        buf.push_str(",\n  a.attstorage");
        layout.storage = next();
        if sversion >= 140_000
            && !hide_compression
            && matches!(
                relkind,
                RelKind::Relation | RelKind::PartitionedTable | RelKind::MatView
            )
        {
            buf.push_str(",\n  a.attcompression AS attcompression");
            layout.compression = next();
        }
        if matches!(
            relkind,
            RelKind::Relation
                | RelKind::Index
                | RelKind::PartitionedIndex
                | RelKind::MatView
                | RelKind::ForeignTable
                | RelKind::PartitionedTable
        ) {
            buf.push_str(
                ",\n  CASE WHEN a.attstattarget=-1 THEN NULL ELSE a.attstattarget END \
                 AS attstattarget",
            );
            layout.stattarget = next();
        }
        if relkind.shows_column_details() {
            buf.push_str(",\n  pg_catalog.col_description(a.attrelid, a.attnum)");
            layout.descr = next();
        }
    }
    let _ = write!(
        buf,
        "\nFROM pg_catalog.pg_attribute a\
         \nWHERE a.attrelid = '{oid}' AND a.attnum > 0 AND NOT a.attisdropped\
         \nORDER BY a.attnum;"
    );
    (buf, layout)
}

/// The title (`describe.c:2005`-`:2064`).
#[must_use]
pub fn table_title(info: &TableInfo, schemaname: &str, relationname: &str) -> String {
    let unlogged = info.unlogged();
    let kind = match info.relkind {
        RelKind::Relation if unlogged => "Unlogged table",
        RelKind::Relation => "Table",
        RelKind::View => "View",
        RelKind::MatView => "Materialized view",
        RelKind::Index if unlogged => "Unlogged index",
        RelKind::Index => "Index",
        RelKind::PartitionedIndex if unlogged => "Unlogged partitioned index",
        RelKind::PartitionedIndex => "Partitioned index",
        RelKind::ToastValue => "TOAST table",
        RelKind::CompositeType => "Composite type",
        RelKind::ForeignTable => "Foreign table",
        RelKind::PartitionedTable if unlogged => "Unlogged partitioned table",
        RelKind::PartitionedTable => "Partitioned table",
        // A sequence never gets here (`describe.c:1762`).
        RelKind::Sequence => "?S?",
        RelKind::Other(c) => {
            // Untranslated unknown relkind.
            return format!("?{}? \"{schemaname}.{relationname}\"", char::from(c));
        }
    };
    format!("{kind} \"{schemaname}.{relationname}\"")
}

/// The headers (`describe.c:2067`-`:2089`), all left-aligned (`:2097`).
#[must_use]
pub fn column_headers(layout: &ColumnLayout) -> Vec<&'static str> {
    let mut headers = vec!["Column", "Type"];
    if layout.details {
        headers.extend(["Collation", "Nullable", "Default"]);
    }
    for (col, header) in [
        (layout.is_key, "Key?"),
        (layout.indexdef, "Definition"),
        (layout.fdwopts, "FDW options"),
        (layout.storage, "Storage"),
        (layout.compression, "Compression"),
        (layout.stattarget, "Stats target"),
        (layout.descr, "Description"),
    ] {
        if col.is_some() {
            headers.push(header);
        }
    }
    headers
}

/// One column's cells (`describe.c:2100`-`:2193`).
#[must_use]
pub fn column_cells(row: &Row<'_>, layout: &ColumnLayout) -> Vec<Vec<u8>> {
    let mut cells = vec![value(row, 0).to_vec(), value(row, 1).to_vec()];
    if layout.details {
        cells.push(value(row, 4).to_vec());
        cells.push(if is_t(row, 3) {
            b"not null".to_vec()
        } else {
            Vec::new()
        });
        let attrdef = value(row, 2);
        cells.push(match (first_byte(row, 5), first_byte(row, 6)) {
            (b'a', _) => b"generated always as identity".to_vec(),
            (b'd', _) => b"generated by default as identity".to_vec(),
            (_, b's') => [b"generated always as (", attrdef, b") stored"].concat(),
            (_, b'v') => [b"generated always as (", attrdef, b")"].concat(),
            _ => attrdef.to_vec(),
        });
    }
    for col in [layout.is_key, layout.indexdef, layout.fdwopts]
        .into_iter()
        .flatten()
    {
        cells.push(value(row, col).to_vec());
    }
    if let Some(col) = layout.storage {
        // These strings are literal in our syntax, so not translated.
        let storage: &[u8] = match first_byte(row, col) {
            b'p' => b"plain",
            b'm' => b"main",
            b'x' => b"extended",
            b'e' => b"external",
            _ => b"???",
        };
        cells.push(storage.to_vec());
    }
    if let Some(col) = layout.compression {
        let compression: &[u8] = match first_byte(row, col) {
            b'p' => b"pglz",
            b'l' => b"lz4",
            0 => b"",
            _ => b"???",
        };
        cells.push(compression.to_vec());
    }
    for col in [layout.stattarget, layout.descr].into_iter().flatten() {
        cells.push(value(row, col).to_vec());
    }
    cells
}

/// A partition's footer query (`describe.c:2202`-`:2218`).
#[must_use]
pub fn partition_of_query(oid: &str, verbose: bool, sversion: i32) -> String {
    format!(
        "SELECT inhparent::pg_catalog.regclass,\n  \
         pg_catalog.pg_get_expr(c.relpartbound, c.oid),\n  {}{}\
         \nFROM pg_catalog.pg_class c JOIN pg_catalog.pg_inherits i ON c.oid = inhrelid\
         \nWHERE c.oid = '{oid}';",
        if sversion >= 140_000 {
            "inhdetachpending"
        } else {
            "false as inhdetachpending"
        },
        if verbose {
            ",\n  pg_catalog.pg_get_partition_constraintdef(c.oid)"
        } else {
            ""
        },
    )
}

/// A partition's footers (`describe.c:2223`-`:2248`): its parent and bound,
/// and with `+` its constraint.
#[must_use]
pub fn partition_of_footers(rows: &[Vec<Option<&[u8]>>], verbose: bool) -> Vec<Vec<u8>> {
    let Some(row) = rows.first() else {
        return Vec::new();
    };
    let detached: &[u8] = if is_t(row, 2) {
        b" DETACH PENDING"
    } else {
        b""
    };
    let mut footers = vec![
        [
            b"Partition of: ",
            value(row, 0),
            b" ",
            value(row, 1),
            detached,
        ]
        .concat(),
    ];
    if verbose {
        // If there isn't any constraint, show that explicitly.
        let constraint = value(row, 3);
        footers.push(if constraint.is_empty() {
            b"No partition constraint".to_vec()
        } else {
            [b"Partition constraint: ", constraint].concat()
        });
    }
    footers
}

/// A partitioned table's key query (`describe.c:2257`-`:2259`).
#[must_use]
pub fn partition_key_query(oid: &str) -> String {
    format!("SELECT pg_catalog.pg_get_partkeydef('{oid}'::pg_catalog.oid);")
}

/// A partitioned table's key footer (`describe.c:2264`-`:2270`).
#[must_use]
pub fn partition_key_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    match rows {
        [row] => vec![[b"Partition key: ", value(row, 0)].concat()],
        _ => Vec::new(),
    }
}

/// A TOAST table's owner query (`describe.c:2279`-`:2284`).
#[must_use]
pub fn owning_table_query(oid: &str) -> String {
    format!(
        "SELECT n.nspname, c.relname\n\
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
         WHERE reltoastrelid = '{oid}';"
    )
}

/// A TOAST table's owner footer (`describe.c:2289`-`:2297`).
#[must_use]
pub fn owning_table_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    match rows {
        [row] => vec![
            [
                b"Owning table: \"",
                value(row, 0),
                b".",
                value(row, 1),
                b"\"",
            ]
            .concat(),
        ],
        _ => Vec::new(),
    }
}

/// The subquery for whether an index's constraint is deferrable or
/// deferred (`describe.c:2310`-`:2325`).
fn index_constraint_flag(column: &str) -> String {
    format!(
        "  (NOT i.indimmediate) AND EXISTS (SELECT 1 FROM pg_catalog.pg_constraint \
         WHERE conrelid = i.indrelid AND conindid = i.indexrelid AND \
         contype IN ('p','u','x') AND {column}) AS {column},\n"
    )
}

/// An index's footer query (`describe.c:2307`-`:2342`).
#[must_use]
pub fn index_footer_query(oid: &str, sversion: i32) -> String {
    format!(
        "SELECT i.indisunique, i.indisprimary, i.indisclustered, i.indisvalid,\n{}{}{}{}  \
         a.amname, c2.relname, pg_catalog.pg_get_expr(i.indpred, i.indrelid, true)\n\
         FROM pg_catalog.pg_index i, pg_catalog.pg_class c, pg_catalog.pg_class c2, \
         pg_catalog.pg_am a\n\
         WHERE i.indexrelid = c.oid AND c.oid = '{oid}' AND c.relam = a.oid\n\
         AND i.indrelid = c2.oid;",
        index_constraint_flag("condeferrable"),
        index_constraint_flag("condeferred"),
        if sversion >= 90_400 {
            "i.indisreplident,\n"
        } else {
            "false AS indisreplident,\n"
        },
        if sversion >= 150_000 {
            "i.indnullsnotdistinct,\n"
        } else {
            "false AS indnullsnotdistinct,\n"
        },
    )
}

/// An index's footer (`describe.c:2354`-`:2401`), from the one row of
/// [`index_footer_query`]. `schemaname` is the index's, which upstream
/// assumes is its table's too.
#[must_use]
pub fn index_footer(row: &Row<'_>, schemaname: &str) -> Vec<u8> {
    let mut buf: Vec<u8> = if is_t(row, 1) {
        b"primary key, ".to_vec()
    } else if is_t(row, 0) {
        let mut buf = b"unique".to_vec();
        if is_t(row, 7) {
            buf.extend_from_slice(b" nulls not distinct");
        }
        buf.extend_from_slice(b", ");
        buf
    } else {
        Vec::new()
    };
    buf.extend_from_slice(value(row, 8));
    buf.extend_from_slice(b", for table \"");
    buf.extend_from_slice(schemaname.as_bytes());
    buf.push(b'.');
    buf.extend_from_slice(value(row, 9));
    buf.push(b'"');
    let indpred = value(row, 10);
    if !indpred.is_empty() {
        buf.extend_from_slice(b", predicate (");
        buf.extend_from_slice(indpred);
        buf.push(b')');
    }
    for (col, set, text) in [
        (2, true, b", clustered".as_slice()),
        (3, false, b", invalid"),
        (4, true, b", deferrable"),
        (5, true, b", initially deferred"),
        (6, true, b", replica identity"),
    ] {
        if is_t(row, col) == set {
            buf.extend_from_slice(text);
        }
    }
    buf
}

/// A table's index query (`describe.c:2428`-`:2451`).
#[must_use]
pub fn table_indexes_query(oid: &str, sversion: i32) -> String {
    format!(
        "SELECT c2.relname, i.indisprimary, i.indisunique, i.indisclustered, i.indisvalid, \
         pg_catalog.pg_get_indexdef(i.indexrelid, 0, true),\n  \
         pg_catalog.pg_get_constraintdef(con.oid, true), contype, condeferrable, condeferred\
         {}, c2.reltablespace{}\n\
         FROM pg_catalog.pg_class c, pg_catalog.pg_class c2, pg_catalog.pg_index i\n  \
         LEFT JOIN pg_catalog.pg_constraint con ON (conrelid = i.indrelid AND \
         conindid = i.indexrelid AND contype IN ('p','u','x'))\n\
         WHERE c.oid = '{oid}' AND c.oid = i.indrelid AND i.indexrelid = c2.oid\n\
         ORDER BY i.indisprimary DESC, c2.relname;",
        if sversion >= 90_400 {
            ", i.indisreplident"
        } else {
            ", false AS indisreplident"
        },
        if sversion >= 180_000 {
            ", con.conperiod"
        } else {
            ", false AS conperiod"
        },
    )
}

/// One index's line under "Indexes:" (`describe.c:2463`-`:2516`), and its
/// tablespace, which [`add_tablespace_footer`]'s caller then appends.
#[must_use]
pub fn table_index_line(row: &Row<'_>) -> (Vec<u8>, u32) {
    // Untranslated index name.
    let mut buf = [b"    \"", value(row, 0), b"\""].concat();
    // If exclusion constraint or PK/UNIQUE constraint WITHOUT OVERLAPS,
    // print the constraintdef.
    if value(row, 7) == b"x" || is_t(row, 12) {
        buf.push(b' ');
        buf.extend_from_slice(value(row, 6));
    } else {
        // Label as primary key or unique (but not both).
        if is_t(row, 1) {
            buf.extend_from_slice(b" PRIMARY KEY,");
        } else if is_t(row, 2) {
            buf.extend_from_slice(if value(row, 7) == b"u" {
                b" UNIQUE CONSTRAINT,".as_slice()
            } else {
                b" UNIQUE,"
            });
        }
        // Everything after "USING" is echoed verbatim.
        let indexdef = value(row, 5);
        let indexdef = find(indexdef, b" USING ").map_or(indexdef, |at| &indexdef[at + 7..]);
        buf.push(b' ');
        buf.extend_from_slice(indexdef);
        // Need these for deferrable PK/UNIQUE indexes.
        if is_t(row, 8) {
            buf.extend_from_slice(b" DEFERRABLE");
        }
        if is_t(row, 9) {
            buf.extend_from_slice(b" INITIALLY DEFERRED");
        }
    }
    // Add these for all cases.
    if is_t(row, 3) {
        buf.extend_from_slice(b" CLUSTER");
    }
    if !is_t(row, 4) {
        buf.extend_from_slice(b" INVALID");
    }
    if is_t(row, 10) {
        buf.extend_from_slice(b" REPLICA IDENTITY");
    }
    (buf, atooid(value(row, 11)))
}

/// `strstr()`: where `needle` first starts in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// `add_tablespace_footer()`'s query (`describe.c:3672`-`:3674`), when
/// `relkind` shows a tablespace and it is not the database's default.
#[must_use]
pub fn tablespace_query(relkind: RelKind, tablespace: u32) -> Option<String> {
    (relkind.has_tablespace() && tablespace != 0).then(|| {
        format!("SELECT spcname FROM pg_catalog.pg_tablespace\nWHERE oid = '{tablespace}';")
    })
}

/// `add_tablespace_footer()`'s footer (`describe.c:3682`-`:3703`): a new
/// one when `newline`, else the last one with the tablespace appended
/// (`printTableSetFooter`).
pub fn add_tablespace_footer(
    footers: &mut Vec<Vec<u8>>,
    rows: &[Vec<Option<&[u8]>>],
    newline: bool,
) {
    // Should always be the case, but....
    let Some(row) = rows.first() else {
        return;
    };
    let spcname = value(row, 0);
    if newline {
        footers.push([b"Tablespace: \"", spcname, b"\""].concat());
    } else if let Some(last) = footers.last_mut() {
        last.extend_from_slice(b", tablespace \"");
        last.extend_from_slice(spcname);
        last.push(b'"');
    }
}

/// A heading, then one footer per row, or nothing for no rows.
fn headed(
    heading: &str,
    rows: &[Vec<Option<&[u8]>>],
    line: impl Fn(&Row<'_>) -> Vec<u8>,
) -> Vec<Vec<u8>> {
    if rows.is_empty() {
        return Vec::new();
    }
    std::iter::once(heading.as_bytes().to_vec())
        .chain(rows.iter().map(|row| line(row)))
        .collect()
}

/// A table's check constraint query (`describe.c:2532`-`:2539`).
#[must_use]
pub fn check_constraints_query(oid: &str) -> String {
    format!(
        "SELECT r.conname, pg_catalog.pg_get_constraintdef(r.oid, true)\n\
         FROM pg_catalog.pg_constraint r\n\
         WHERE r.conrelid = '{oid}' AND r.contype = 'c'\n\
         ORDER BY 1;"
    )
}

/// A table's check constraints (`describe.c:2546`-`:2558`).
#[must_use]
pub fn check_constraint_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    // Untranslated constraint name and def.
    headed("Check constraints:", rows, |row| {
        [b"    \"", value(row, 0), b"\" ", value(row, 1)].concat()
    })
}

/// A table's foreign-key query (`describe.c:2563`-`:2594`): from 12, for a
/// partition or partitioned table, its own constraints first, then those it
/// inherits from its ancestors.
#[must_use]
pub fn foreign_keys_query(oid: &str, info: &TableInfo, sversion: i32) -> String {
    if sversion >= 120_000 && (info.ispartition || info.relkind == RelKind::PartitionedTable) {
        format!(
            "SELECT conrelid = '{oid}'::pg_catalog.regclass AS sametable,\n       \
             conname,\n       \
             pg_catalog.pg_get_constraintdef(oid, true) AS condef,\n       \
             conrelid::pg_catalog.regclass AS ontable\n  \
             FROM pg_catalog.pg_constraint,\n       \
             pg_catalog.pg_partition_ancestors('{oid}')\n \
             WHERE conrelid = relid AND contype = 'f' AND conparentid = 0\n\
             ORDER BY sametable DESC, conname;"
        )
    } else {
        format!(
            "SELECT true as sametable, conname,\n  \
             pg_catalog.pg_get_constraintdef(r.oid, true) as condef,\n  \
             conrelid::pg_catalog.regclass AS ontable\n\
             FROM pg_catalog.pg_constraint r\n\
             WHERE r.conrelid = '{oid}' AND r.contype = 'f'\n{}\
             ORDER BY conname",
            if sversion >= 120_000 {
                "     AND conparentid = 0\n"
            } else {
                ""
            }
        )
    }
}

/// A table's foreign keys (`describe.c:2602`-`:2629`), named with the table
/// that defines them when that is an ancestor. Both queries put `sametable`,
/// `conname`, `condef` and `ontable` in that order, which is what
/// `PQfnumber` finds.
#[must_use]
pub fn foreign_key_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    headed("Foreign-key constraints:", rows, |row| {
        if value(row, 0) == b"f" {
            [
                b"    TABLE \"",
                value(row, 3),
                b"\" CONSTRAINT \"",
                value(row, 1),
                b"\" ",
                value(row, 2),
            ]
            .concat()
        } else {
            [b"    \"", value(row, 1), b"\" ", value(row, 2)].concat()
        }
    })
}

/// The foreign keys that reference a table (`describe.c:2633`-`:2654`).
#[must_use]
pub fn referenced_by_query(oid: &str, sversion: i32) -> String {
    if sversion >= 120_000 {
        format!(
            "SELECT conname, conrelid::pg_catalog.regclass AS ontable,\n       \
             pg_catalog.pg_get_constraintdef(oid, true) AS condef\n  \
             FROM pg_catalog.pg_constraint c\n \
             WHERE confrelid IN (SELECT pg_catalog.pg_partition_ancestors('{oid}')\n                     \
             UNION ALL VALUES ('{oid}'::pg_catalog.regclass))\n       \
             AND contype = 'f' AND conparentid = 0\n\
             ORDER BY conname;"
        )
    } else {
        format!(
            "SELECT conname, conrelid::pg_catalog.regclass AS ontable,\n       \
             pg_catalog.pg_get_constraintdef(oid, true) AS condef\n  \
             FROM pg_catalog.pg_constraint\n \
             WHERE confrelid = {oid} AND contype = 'f'\n\
             ORDER BY conname;"
        )
    }
}

/// "Referenced by:" (`describe.c:2662`-`:2678`), from `conname`, `ontable`,
/// `condef`.
#[must_use]
pub fn referenced_by_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    headed("Referenced by:", rows, |row| {
        [
            b"    TABLE \"",
            value(row, 1),
            b"\" CONSTRAINT \"",
            value(row, 0),
            b"\" ",
            value(row, 2),
        ]
        .concat()
    })
}

/// A table's row-level policy query (`describe.c:2682`-`:2703`); `None`
/// before 9.5.
#[must_use]
pub fn policies_query(oid: &str, sversion: i32) -> Option<String> {
    (sversion >= 90_500).then(|| {
        format!(
            "SELECT pol.polname,{}  \
             CASE WHEN pol.polroles = '{{0}}' THEN NULL ELSE \
             pg_catalog.array_to_string(array(select rolname from pg_catalog.pg_roles \
             where oid = any (pol.polroles) order by 1),',') END,\n  \
             pg_catalog.pg_get_expr(pol.polqual, pol.polrelid),\n  \
             pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid),\n  \
             CASE pol.polcmd\n    \
             WHEN 'r' THEN 'SELECT'\n    \
             WHEN 'a' THEN 'INSERT'\n    \
             WHEN 'w' THEN 'UPDATE'\n    \
             WHEN 'd' THEN 'DELETE'\n    \
             END AS cmd\n\
             FROM pg_catalog.pg_policy pol\n\
             WHERE pol.polrelid = '{oid}' ORDER BY 1;",
            if sversion >= 100_000 {
                " pol.polpermissive,\n"
            } else {
                " 't' as polpermissive,\n"
            }
        )
    })
}

/// A table's policies (`describe.c:2716`-`:2759`), under a heading that
/// says whether row security is on and forced, which is there even with no
/// policy when it is on.
#[must_use]
pub fn policy_footers(rows: &[Vec<Option<&[u8]>>], info: &TableInfo) -> Vec<Vec<u8>> {
    let any = !rows.is_empty();
    let heading = match (info.rowsecurity, info.forcerowsecurity, any) {
        (true, false, true) => Some("Policies:"),
        (true, true, true) => Some("Policies (forced row security enabled):"),
        (true, false, false) => Some("Policies (row security enabled): (none)"),
        (true, true, false) => Some("Policies (forced row security enabled): (none)"),
        (false, _, true) => Some("Policies (row security disabled):"),
        (false, _, false) => None,
    };
    let mut footers: Vec<Vec<u8>> = heading.map(|h| h.as_bytes().to_vec()).into_iter().collect();
    for row in rows {
        let mut buf = [b"    POLICY \"", value(row, 0), b"\""].concat();
        if first_byte(row, 1) == b'f' {
            buf.extend_from_slice(b" AS RESTRICTIVE");
        }
        for (col, prefix, suffix) in [
            (5, b" FOR ".as_slice(), b"".as_slice()),
            (2, b"\n      TO ", b""),
            (3, b"\n      USING (", b")"),
            (4, b"\n      WITH CHECK (", b")"),
        ] {
            if !is_null(row, col) {
                buf.extend_from_slice(prefix);
                buf.extend_from_slice(value(row, col));
                buf.extend_from_slice(suffix);
            }
        }
        footers.push(buf);
    }
    footers
}

/// A table's extended statistics query (`describe.c:2764`-`:2884`); `None`
/// before 10.
#[must_use]
pub fn statistics_query(oid: &str, sversion: i32) -> Option<String> {
    if sversion >= 140_000 {
        Some(format!(
            "SELECT oid, stxrelid::pg_catalog.regclass, \
             stxnamespace::pg_catalog.regnamespace::pg_catalog.text AS nsp, stxname,\n\
             pg_catalog.pg_get_statisticsobjdef_columns(oid) AS columns,\n  \
             'd' = any(stxkind) AS ndist_enabled,\n  \
             'f' = any(stxkind) AS deps_enabled,\n  \
             'm' = any(stxkind) AS mcv_enabled,\n\
             stxstattarget\n\
             FROM pg_catalog.pg_statistic_ext\n\
             WHERE stxrelid = '{oid}'\n\
             ORDER BY nsp, stxname;"
        ))
    } else if sversion >= 100_000 {
        Some(format!(
            "SELECT oid, stxrelid::pg_catalog.regclass, \
             stxnamespace::pg_catalog.regnamespace AS nsp, stxname,\n  \
             (SELECT pg_catalog.string_agg(pg_catalog.quote_ident(attname),', ')\n   \
             FROM pg_catalog.unnest(stxkeys) s(attnum)\n   \
             JOIN pg_catalog.pg_attribute a ON (stxrelid = a.attrelid AND\n        \
             a.attnum = s.attnum AND NOT attisdropped)) AS columns,\n  \
             'd' = any(stxkind) AS ndist_enabled,\n  \
             'f' = any(stxkind) AS deps_enabled,\n  \
             'm' = any(stxkind) AS mcv_enabled,\n{}\
             FROM pg_catalog.pg_statistic_ext\n\
             WHERE stxrelid = '{oid}'\n\
             ORDER BY 1;",
            if sversion >= 130_000 {
                "  stxstattarget\n"
            } else {
                "  -1 AS stxstattarget\n"
            }
        ))
    } else {
        None
    }
}

/// A table's extended statistics (`describe.c:2787`-`:2859`, and before 14
/// `:2892`-`:2936`). From 14 the kinds are shown only when some but not all
/// are on: none means statistics on a single expression, all means `CREATE
/// STATISTICS` expanded them.
#[must_use]
pub fn statistics_footers(rows: &[Vec<Option<&[u8]>>], sversion: i32) -> Vec<Vec<u8>> {
    headed("Statistics objects:", rows, |row| {
        let kinds: Vec<&[u8]> = [
            (5, b"ndistinct".as_slice()),
            (6, b"dependencies"),
            (7, b"mcv"),
        ]
        .into_iter()
        .filter(|&(col, _)| is_t(row, col))
        .map(|(_, kind)| kind)
        .collect();
        // Statistics object name (qualified with namespace).
        let mut buf = [b"    \"", value(row, 2), b".", value(row, 3), b"\""].concat();
        let show_kinds = if sversion >= 140_000 {
            !kinds.is_empty() && kinds.len() < 3
        } else {
            true
        };
        if show_kinds {
            buf.extend_from_slice(b" (");
            buf.extend_from_slice(&kinds.join(b", ".as_slice()));
            buf.push(b')');
        }
        buf.extend_from_slice(b" ON ");
        buf.extend_from_slice(value(row, 4));
        buf.extend_from_slice(b" FROM ");
        buf.extend_from_slice(value(row, 1));
        // Show the stats target if it's not default.
        let target = value(row, 8);
        if (sversion < 140_000 || !is_null(row, 8)) && target != b"-1" {
            buf.extend_from_slice(b"; STATISTICS ");
            buf.extend_from_slice(target);
        }
        buf
    })
}

/// A table's rule query (`describe.c:2943`-`:2948`).
#[must_use]
pub fn rules_query(oid: &str) -> String {
    format!(
        "SELECT r.rulename, trim(trailing ';' from pg_catalog.pg_get_ruledef(r.oid, true)), \
         ev_enabled\n\
         FROM pg_catalog.pg_rewrite r\n\
         WHERE r.ev_class = '{oid}' ORDER BY 1;"
    )
}

/// Everything after "CREATE RULE " (`describe.c:3013`-`:3014`).
fn after_create_rule(ruledef: &[u8]) -> &[u8] {
    ruledef.get(12..).unwrap_or_default()
}

/// A table's rules (`describe.c:2955`-`:3019`), by `ev_enabled`: enabled,
/// disabled, always, replica only, each under its heading.
#[must_use]
pub fn rule_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    let mut footers = Vec::new();
    for (enabled, heading) in [
        (b'O', "Rules:"),
        (b'D', "Disabled rules:"),
        (b'A', "Rules firing always:"),
        (b'R', "Rules firing on replica only:"),
    ] {
        let listed: Vec<&Vec<Option<&[u8]>>> = rows
            .iter()
            .filter(|row| first_byte(row, 2) == enabled)
            .collect();
        if !listed.is_empty() {
            footers.push(heading.as_bytes().to_vec());
        }
        for row in listed {
            footers.push([b"    ", after_create_rule(value(row, 1))].concat());
        }
    }
    footers
}

/// A table's publication query (`describe.c:3024`-`:3075`); `None` before
/// 10.
#[must_use]
pub fn publications_query(oid: &str, sversion: i32) -> Option<String> {
    if sversion >= 150_000 {
        Some(format!(
            "SELECT pubname\n     \
             , NULL\n     \
             , NULL\n\
             FROM pg_catalog.pg_publication p\n     \
             JOIN pg_catalog.pg_publication_namespace pn ON p.oid = pn.pnpubid\n     \
             JOIN pg_catalog.pg_class pc ON pc.relnamespace = pn.pnnspid\n\
             WHERE pc.oid ='{oid}' and pg_catalog.pg_relation_is_publishable('{oid}')\n\
             UNION\n\
             SELECT pubname\n     \
             , pg_get_expr(pr.prqual, c.oid)\n     \
             , (CASE WHEN pr.prattrs IS NOT NULL THEN\n         \
             (SELECT string_agg(attname, ', ')\n           \
             FROM pg_catalog.generate_series(0, \
             pg_catalog.array_upper(pr.prattrs::pg_catalog.int2[], 1)) s,\n                \
             pg_catalog.pg_attribute\n          \
             WHERE attrelid = pr.prrelid AND attnum = prattrs[s])\n        \
             ELSE NULL END) \
             FROM pg_catalog.pg_publication p\n     \
             JOIN pg_catalog.pg_publication_rel pr ON p.oid = pr.prpubid\n     \
             JOIN pg_catalog.pg_class c ON c.oid = pr.prrelid\n\
             WHERE pr.prrelid = '{oid}'\n\
             UNION\n\
             SELECT pubname\n     \
             , NULL\n     \
             , NULL\n\
             FROM pg_catalog.pg_publication p\n\
             WHERE p.puballtables AND pg_catalog.pg_relation_is_publishable('{oid}')\n\
             ORDER BY 1;"
        ))
    } else if sversion >= 100_000 {
        Some(format!(
            "SELECT pubname\n     \
             , NULL\n     \
             , NULL\n\
             FROM pg_catalog.pg_publication p\n\
             JOIN pg_catalog.pg_publication_rel pr ON p.oid = pr.prpubid\n\
             WHERE pr.prrelid = '{oid}'\n\
             UNION ALL\n\
             SELECT pubname\n     \
             , NULL\n     \
             , NULL\n\
             FROM pg_catalog.pg_publication p\n\
             WHERE p.puballtables AND pg_catalog.pg_relation_is_publishable('{oid}')\n\
             ORDER BY 1;"
        ))
    } else {
        None
    }
}

/// A table's publications (`describe.c:3083`-`:3103`), each with its column
/// list and row filter, if any.
#[must_use]
pub fn publication_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    headed("Publications:", rows, |row| {
        let mut buf = [b"    \"", value(row, 0), b"\""].concat();
        // Column list (if any).
        if !is_null(row, 2) {
            buf.extend_from_slice(b" (");
            buf.extend_from_slice(value(row, 2));
            buf.push(b')');
        }
        // Row filter (if any).
        if !is_null(row, 1) {
            buf.extend_from_slice(b" WHERE ");
            buf.extend_from_slice(value(row, 1));
        }
        buf
    })
}

/// A table's not-null constraint query, for `+` (`describe.c:3112`-`:3122`).
#[must_use]
pub fn not_null_constraints_query(oid: &str) -> String {
    format!(
        "SELECT c.conname, a.attname, c.connoinherit,\n  \
         c.conislocal, c.coninhcount <> 0,\n  \
         c.convalidated\n\
         FROM pg_catalog.pg_constraint c JOIN\n  \
         pg_catalog.pg_attribute a ON\n    \
         (a.attrelid = c.conrelid AND a.attnum = c.conkey[1])\n\
         WHERE c.contype = 'n' AND\n  \
         c.conrelid = '{oid}'::pg_catalog.regclass\n\
         ORDER BY a.attnum"
    )
}

/// A table's not-null constraints (`describe.c:3130`-`:3150`).
#[must_use]
pub fn not_null_constraint_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    headed("Not-null constraints:", rows, |row| {
        let islocal = first_byte(row, 3) == b't';
        let inherited = first_byte(row, 4) == b't';
        let validated = first_byte(row, 5) == b't';
        let how: &[u8] = if first_byte(row, 2) == b't' {
            b" NO INHERIT"
        } else if islocal && inherited {
            b" (local, inherited)"
        } else if inherited {
            b" (inherited)"
        } else {
            b""
        };
        let not_valid: &[u8] = if validated { b"" } else { b" NOT VALID" };
        [
            b"    \"",
            value(row, 0),
            b"\" NOT NULL \"",
            value(row, 1),
            b"\"",
            how,
            not_valid,
        ]
        .concat()
    })
}

/// A view's definition query, for `+` (`describe.c:3161`-`:3163`).
#[must_use]
pub fn view_definition_query(oid: &str) -> String {
    format!("SELECT pg_catalog.pg_get_viewdef('{oid}'::pg_catalog.oid, true);")
}

/// A view's rule query (`describe.c:3185`-`:3189`), all but `_RETURN`.
#[must_use]
pub fn view_rules_query(oid: &str) -> String {
    format!(
        "SELECT r.rulename, trim(trailing ';' from pg_catalog.pg_get_ruledef(r.oid, true))\n\
         FROM pg_catalog.pg_rewrite r\n\
         WHERE r.ev_class = '{oid}' AND r.rulename != '_RETURN' ORDER BY 1;"
    )
}

/// A view's rules (`describe.c:3194`-`:3208`), indented by one space where a
/// table's are by four.
#[must_use]
pub fn view_rule_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    headed("Rules:", rows, |row| {
        [b" ", after_create_rule(value(row, 1))].concat()
    })
}

/// A relation's trigger query (`describe.c:3222`-`:3273`): user-defined
/// triggers, and internal ones that are disabled.
#[must_use]
pub fn triggers_query(oid: &str, sversion: i32) -> String {
    let parent = if sversion >= 130_000 {
        concat!(
            "  CASE WHEN t.tgparentid != 0 THEN\n",
            "    (SELECT u.tgrelid::pg_catalog.regclass\n",
            "     FROM pg_catalog.pg_trigger AS u,\n",
            "          pg_catalog.pg_partition_ancestors(t.tgrelid) WITH ORDINALITY AS a(relid, depth)\n",
            "     WHERE u.tgname = t.tgname AND u.tgrelid = a.relid\n",
            "           AND u.tgparentid = 0\n",
            "     ORDER BY a.depth LIMIT 1)\n",
            "  END AS parent\n",
        )
    } else {
        "  NULL AS parent\n"
    };
    // `tgisinternal` is set for the inherited triggers of partitions from 11
    // to 14, which must still be shown: they depend on another trigger.
    let which = if (110_000..150_000).contains(&sversion) {
        concat!(
            "(NOT t.tgisinternal OR (t.tgisinternal AND t.tgenabled = 'D') \n",
            "    OR EXISTS (SELECT 1 FROM pg_catalog.pg_depend WHERE objid = t.oid \n",
            "        AND refclassid = 'pg_catalog.pg_trigger'::pg_catalog.regclass))",
        )
    } else {
        // Display/warn about disabled internal triggers.
        "(NOT t.tgisinternal OR (t.tgisinternal AND t.tgenabled = 'D'))"
    };
    format!(
        "SELECT t.tgname, pg_catalog.pg_get_triggerdef(t.oid, true), t.tgenabled, \
         t.tgisinternal,\n{parent}\
         FROM pg_catalog.pg_trigger t\n\
         WHERE t.tgrelid = '{oid}' AND {which}\
         \nORDER BY 1;"
    )
}

/// The headings a relation's triggers fall under, in order
/// (`describe.c:3339`-`:3356`).
const TRIGGER_HEADINGS: [&str; 5] = [
    "Triggers:",
    "Disabled user triggers:",
    "Disabled internal triggers:",
    "Triggers firing always:",
    "Triggers firing on replica only:",
];

/// Which of [`TRIGGER_HEADINGS`] a trigger falls under, by `tgenabled` and
/// `tgisinternal` (`describe.c:3308`-`:3332`); `None` for none.
fn trigger_category(row: &Row<'_>) -> Option<usize> {
    match (first_byte(row, 2), first_byte(row, 3)) {
        (b'O' | b't', _) => Some(0),
        (b'D' | b'f', b'f') => Some(1),
        (b'D' | b'f', b't') => Some(2),
        (b'A', _) => Some(3),
        (b'R', _) => Some(4),
        _ => None,
    }
}

/// A relation's triggers (`describe.c:3281`-`:3377`), each under its
/// heading, and marked with the table they are inherited from.
#[must_use]
pub fn trigger_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    let mut footers = Vec::new();
    for (category, heading) in TRIGGER_HEADINGS.iter().enumerate() {
        let mut have_heading = false;
        for row in rows
            .iter()
            .filter(|row| trigger_category(row) == Some(category))
        {
            if !have_heading {
                footers.push(heading.as_bytes().to_vec());
                have_heading = true;
            }
            // Everything after "TRIGGER" is echoed verbatim.
            let tgdef = value(row, 1);
            let tgdef = find(tgdef, b" TRIGGER ").map_or(tgdef, |at| &tgdef[at + 9..]);
            let mut buf = [b"    ", tgdef].concat();
            // Visually distinguish inherited triggers.
            if !is_null(row, 4) {
                buf.extend_from_slice(b", ON TABLE ");
                buf.extend_from_slice(value(row, 4));
            }
            footers.push(buf);
        }
    }
    footers
}

/// A foreign table's server query (`describe.c:3405`-`:3414`).
#[must_use]
pub fn foreign_server_query(oid: &str) -> String {
    format!(
        "SELECT s.srvname,\n  \
         pg_catalog.array_to_string(ARRAY(\n    \
         SELECT pg_catalog.quote_ident(option_name) || ' ' || \
         pg_catalog.quote_literal(option_value)\n    \
         FROM pg_catalog.pg_options_to_table(ftoptions)),  ', ')\n\
         FROM pg_catalog.pg_foreign_table f,\n     \
         pg_catalog.pg_foreign_server s\n\
         WHERE f.ftrelid = '{oid}' AND s.oid = f.ftserver;"
    )
}

/// A foreign table's server and options (`describe.c:3424`-`:3435`), from
/// the one row of [`foreign_server_query`].
#[must_use]
pub fn foreign_server_footers(row: &Row<'_>) -> Vec<Vec<u8>> {
    let mut footers = vec![[b"Server: ", value(row, 0)].concat()];
    // Print per-table FDW options, if any.
    let ftoptions = value(row, 1);
    if !ftoptions.is_empty() {
        footers.push([b"FDW options: (", ftoptions, b")"].concat());
    }
    footers
}

/// A table's parents query (`describe.c:3440`-`:3447`), partitioned ones
/// left out.
#[must_use]
pub fn inherits_query(oid: &str) -> String {
    format!(
        "SELECT c.oid::pg_catalog.regclass\n\
         FROM pg_catalog.pg_class c, pg_catalog.pg_inherits i\n\
         WHERE c.oid = i.inhparent AND i.inhrelid = '{oid}'\n  \
         AND c.relkind != 'p' AND c.relkind != 'I'\n\
         ORDER BY inhseqno;"
    )
}

/// A list of relations under a label, one per footer, the first after
/// `label: ` and the rest lined up under it, all but the last followed by a
/// comma (`describe.c:3459`-`:3471`, `:3540`-`:3563`). The label is
/// untranslated ASCII, so its display width, `pg_wcswidth`, is its length.
fn lined_up(label: &str, items: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let count = items.len();
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let mut buf = if i == 0 {
                format!("{label}: ").into_bytes()
            } else {
                format!("{:w$}  ", "", w = label.len()).into_bytes()
            };
            buf.extend_from_slice(&item);
            if i + 1 < count {
                buf.push(b',');
            }
            buf
        })
        .collect()
}

/// A table's parents (`describe.c:3454`-`:3471`).
#[must_use]
pub fn inherits_footers(rows: &[Vec<Option<&[u8]>>]) -> Vec<Vec<u8>> {
    lined_up(
        "Inherits",
        rows.iter().map(|row| value(row, 0).to_vec()).collect(),
    )
}

/// A table's children query (`describe.c:3477`-`:3504`), partitions with
/// their bounds, the default one last.
#[must_use]
pub fn child_tables_query(oid: &str, sversion: i32) -> String {
    if sversion >= 100_000 {
        format!(
            "SELECT c.oid::pg_catalog.regclass, c.relkind, {} \
             pg_catalog.pg_get_expr(c.relpartbound, c.oid)\n\
             FROM pg_catalog.pg_class c, pg_catalog.pg_inherits i\n\
             WHERE c.oid = i.inhrelid AND i.inhparent = '{oid}'\n\
             ORDER BY pg_catalog.pg_get_expr(c.relpartbound, c.oid) = 'DEFAULT', \
             c.oid::pg_catalog.regclass::pg_catalog.text;",
            if sversion >= 140_000 {
                "inhdetachpending,"
            } else {
                "false AS inhdetachpending,"
            }
        )
    } else {
        format!(
            "SELECT c.oid::pg_catalog.regclass, c.relkind, false AS inhdetachpending, NULL\n\
             FROM pg_catalog.pg_class c, pg_catalog.pg_inherits i\n\
             WHERE c.oid = i.inhrelid AND i.inhparent = '{oid}'\n\
             ORDER BY c.oid::pg_catalog.regclass::pg_catalog.text;"
        )
    }
}

/// A table's children (`describe.c:3517`-`:3564`): without `+` only how
/// many, with it each one. A partitioned table or index with none always
/// says so.
#[must_use]
pub fn child_table_footers(
    rows: &[Vec<Option<&[u8]>>],
    is_partitioned: bool,
    verbose: bool,
) -> Vec<Vec<u8>> {
    let tuples = rows.len();
    if is_partitioned && tuples == 0 {
        return vec![b"Number of partitions: 0".to_vec()];
    }
    if !verbose {
        return match (tuples, is_partitioned) {
            (0, _) => Vec::new(),
            (n, true) => {
                vec![format!("Number of partitions: {n} (Use \\d+ to list them.)").into_bytes()]
            }
            (n, false) => {
                vec![format!("Number of child tables: {n} (Use \\d+ to list them.)").into_bytes()]
            }
        };
    }
    let items = rows
        .iter()
        .map(|row| {
            let mut buf = value(row, 0).to_vec();
            if !is_null(row, 3) {
                buf.push(b' ');
                buf.extend_from_slice(value(row, 3));
            }
            match RelKind::from_byte(first_byte(row, 1)) {
                RelKind::PartitionedTable | RelKind::PartitionedIndex => {
                    buf.extend_from_slice(b", PARTITIONED");
                }
                RelKind::ForeignTable => buf.extend_from_slice(b", FOREIGN"),
                _ => {}
            }
            if is_t(row, 2) {
                buf.extend_from_slice(b" (DETACH PENDING)");
            }
            buf
        })
        .collect();
    lined_up(
        if is_partitioned {
            "Partitions"
        } else {
            "Child tables"
        },
        items,
    )
}

/// The closing footers before the tablespace (`describe.c:3568`-`:3601`):
/// the type of a typed table, and with `+` a replica identity that is not
/// the default and whether the table has OIDs.
#[must_use]
pub fn typed_table_and_identity_footers(
    info: &TableInfo,
    schemaname: &str,
    verbose: bool,
) -> Vec<Vec<u8>> {
    let mut footers = Vec::new();
    if let Some(reloftype) = &info.reloftype {
        footers.push([b"Typed table of type: ", reloftype.as_slice()].concat());
    }
    let catalog = schemaname == "pg_catalog";
    // No need to display default values; we already display a REPLICA
    // IDENTITY marker on indexes.
    if verbose
        && matches!(info.relkind, RelKind::Relation | RelKind::MatView)
        && info.relreplident != b'i'
        && ((!catalog && info.relreplident != b'd') || (catalog && info.relreplident != b'n'))
    {
        let identity = match info.relreplident {
            b'f' => "FULL",
            b'd' => "NOTHING",
            _ => "???",
        };
        footers.push(format!("Replica Identity: {identity}").into_bytes());
    }
    // OIDs, if verbose and not a materialized view.
    if verbose && info.relkind != RelKind::MatView && info.hasoids {
        footers.push(b"Has OIDs: yes".to_vec());
    }
    footers
}

/// The access method footer, with `+` unless `HIDE_TABLEAM`
/// (`describe.c:3608`-`:3612`).
#[must_use]
pub fn access_method_footers(info: &TableInfo, verbose: bool, hide_tableam: bool) -> Vec<Vec<u8>> {
    match &info.relam {
        Some(relam) if verbose && !hide_tableam => {
            vec![[b"Access method: ", relam.as_slice()].concat()]
        }
        _ => Vec::new(),
    }
}

/// The options footer, with `+` (`describe.c:3616`-`:3623`).
#[must_use]
pub fn options_footers(info: &TableInfo, verbose: bool) -> Vec<Vec<u8>> {
    if verbose && !info.reloptions.is_empty() {
        vec![[b"Options: ", info.reloptions.as_slice()].concat()]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG18: ServerContext<'static> = ServerContext {
        sversion: 180_006,
        hide_tableam: false,
        db: Some("regression"),
    };

    /// Rows of text cells, `None` for `NULL`.
    fn rows<'a>(rows: &[&[Option<&'a str>]]) -> Vec<Vec<Option<&'a [u8]>>> {
        rows.iter()
            .map(|row| row.iter().map(|c| c.map(str::as_bytes)).collect())
            .collect()
    }

    fn text(footers: &[Vec<u8>]) -> Vec<String> {
        footers
            .iter()
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect()
    }

    fn info(relkind: u8) -> TableInfo {
        let kind = [relkind];
        let kind = std::str::from_utf8(&kind).unwrap();
        let row = rows(&[&[
            Some("0"),
            Some(kind),
            Some("f"),
            Some("f"),
            Some("f"),
            Some("f"),
            Some("f"),
            Some("f"),
            Some("f"),
            Some(""),
            Some("0"),
            Some(""),
            Some("p"),
            Some("d"),
            Some("heap"),
        ]]);
        TableInfo::parse(&row[0], 180_006)
    }

    #[test]
    fn the_lookup_matches_schema_and_name_and_orders_by_both() {
        assert_eq!(
            describe_table_details_query(Some("s.t*"), false, PG18).unwrap(),
            "SELECT c.oid,\n  n.nspname,\n  c.relname\n\
             FROM pg_catalog.pg_class c\n     \
             LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
             WHERE c.relname OPERATOR(pg_catalog.~) '^(t.*)$' COLLATE pg_catalog.default\n  \
             AND n.nspname OPERATOR(pg_catalog.~) '^(s)$' COLLATE pg_catalog.default\n\
             ORDER BY 2, 3;"
        );
        // Without a pattern (never from `\d`, which lists instead), the
        // system schemas are left out unless `S`.
        let all = describe_table_details_query(None, false, PG18).unwrap();
        assert!(
            all.contains(
                "WHERE n.nspname <> 'pg_catalog'\n      AND n.nspname <> 'information_schema'\n  \
                 AND pg_catalog.pg_table_is_visible(c.oid)\n"
            ),
            "{all}"
        );
        let system = describe_table_details_query(None, true, PG18).unwrap();
        assert!(system.contains("\nWHERE pg_catalog.pg_table_is_visible(c.oid)\n"));
    }

    #[test]
    fn the_lookup_takes_three_parts_and_only_this_database() {
        assert_eq!(
            describe_table_details_query(Some("a.b.c.d"), false, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): a.b.c.d".to_string()
            ))
        );
        assert_eq!(
            describe_table_details_query(Some("other.s.t"), false, PG18),
            Err(PatternError(
                "cross-database references are not implemented: other.s.t".to_string()
            ))
        );
        assert!(describe_table_details_query(Some("regression.s.t"), false, PG18).is_ok());
        assert_eq!(
            relations_not_found(Some("x")),
            "Did not find any relation named \"x\"."
        );
        assert_eq!(relations_not_found(None), "Did not find any relations.");
        assert_eq!(
            relation_oid_not_found("42"),
            "Did not find any relation with OID 42."
        );
    }

    #[test]
    fn the_general_query_asks_for_options_only_with_plus_and_the_am_from_12() {
        assert_eq!(
            table_info_query("42", false, 180_006),
            "SELECT c.relchecks, c.relkind, c.relhasindex, c.relhasrules, \
             c.relhastriggers, c.relrowsecurity, c.relforcerowsecurity, \
             false AS relhasoids, c.relispartition, '', c.reltablespace, \
             CASE WHEN c.reloftype = 0 THEN '' ELSE \
             c.reloftype::pg_catalog.regtype::pg_catalog.text END, \
             c.relpersistence, c.relreplident, am.amname\n\
             FROM pg_catalog.pg_class c\n \
             LEFT JOIN pg_catalog.pg_class tc ON (c.reltoastrelid = tc.oid)\n\
             LEFT JOIN pg_catalog.pg_am am ON (c.relam = am.oid)\n\
             WHERE c.oid = '42';"
        );
        let verbose = table_info_query("42", true, 180_006);
        assert!(verbose.contains(
            "c.relispartition, pg_catalog.array_to_string(c.reloptions || \
             array(select 'toast.' || x from pg_catalog.unnest(tc.reloptions) x), ', ')\n\
             , c.reltablespace, "
        ));
        let old = table_info_query("42", false, 90_300);
        assert!(
            old.contains(
                "c.relhastriggers, false, false, c.relhasoids, false as relispartition, ''"
            )
        );
        assert!(old.contains("c.relpersistence\nFROM pg_catalog.pg_class c\n LEFT JOIN"));
        assert!(!old.contains("pg_am"));
    }

    #[test]
    fn table_info_reads_each_flag_and_defaults_what_old_servers_lack() {
        let row = rows(&[&[
            Some("2"),
            Some("r"),
            Some("t"),
            Some("f"),
            Some("t"),
            Some("t"),
            Some("f"),
            Some("f"),
            Some("t"),
            Some("fillfactor=70"),
            Some("16385"),
            Some("s.ct"),
            Some("u"),
            Some("f"),
            None,
        ]]);
        let info = TableInfo::parse(&row[0], 180_006);
        assert_eq!(
            info,
            TableInfo {
                checks: 2,
                relkind: RelKind::Relation,
                hasindex: true,
                hasrules: false,
                hastriggers: true,
                rowsecurity: true,
                forcerowsecurity: false,
                hasoids: false,
                ispartition: true,
                reloptions: b"fillfactor=70".to_vec(),
                tablespace: 16385,
                reloftype: Some(b"s.ct".to_vec()),
                relpersistence: b'u',
                relreplident: b'f',
                relam: None,
            }
        );
        let old = TableInfo::parse(&row[0][..13], 90_300);
        assert_eq!((old.relreplident, old.relam), (b'd', None));
        assert_eq!(atooid(b"4294967295"), u32::MAX);
        assert_eq!(atooid(b" 12x"), 12);
        assert_eq!(atooid(b""), 0);
    }

    #[test]
    fn the_column_query_fetches_what_the_kind_shows() {
        let (query, layout) = column_query("42", &info(b'r'), false, 180_006, false);
        assert_eq!(
            query,
            "SELECT a.attname,\n  pg_catalog.format_type(a.atttypid, a.atttypmod),\n  \
             (SELECT pg_catalog.pg_get_expr(d.adbin, d.adrelid, true)\n   \
             FROM pg_catalog.pg_attrdef d\n   \
             WHERE d.adrelid = a.attrelid AND d.adnum = a.attnum AND a.atthasdef),\n  \
             a.attnotnull,\n  \
             (SELECT c.collname FROM pg_catalog.pg_collation c, pg_catalog.pg_type t\n   \
             WHERE c.oid = a.attcollation AND t.oid = a.atttypid AND \
             a.attcollation <> t.typcollation) AS attcollation,\n  \
             a.attidentity,\n  a.attgenerated\n\
             FROM pg_catalog.pg_attribute a\n\
             WHERE a.attrelid = '42' AND a.attnum > 0 AND NOT a.attisdropped\n\
             ORDER BY a.attnum;"
        );
        assert_eq!(
            column_headers(&layout),
            ["Column", "Type", "Collation", "Nullable", "Default"]
        );

        let (query, layout) = column_query("42", &info(b'r'), true, 180_006, false);
        assert!(query.contains(
            ",\n  a.attstorage,\n  a.attcompression AS attcompression,\n  \
             CASE WHEN a.attstattarget=-1 THEN NULL ELSE a.attstattarget END AS attstattarget,\n  \
             pg_catalog.col_description(a.attrelid, a.attnum)\nFROM"
        ));
        assert_eq!(
            (
                layout.storage,
                layout.compression,
                layout.stattarget,
                layout.descr
            ),
            (Some(7), Some(8), Some(9), Some(10))
        );
        // `HIDE_TOAST_COMPRESSION` drops the column, and the rest move up.
        let (_, layout) = column_query("42", &info(b'r'), true, 180_006, true);
        assert_eq!((layout.compression, layout.stattarget), (None, Some(8)));
        assert_eq!(
            column_headers(&layout),
            [
                "Column",
                "Type",
                "Collation",
                "Nullable",
                "Default",
                "Storage",
                "Stats target",
                "Description"
            ]
        );

        let (query, layout) = column_query("42", &info(b'i'), true, 180_006, false);
        assert!(query.contains(
            "SELECT i.indnkeyatts FROM pg_catalog.pg_index i WHERE i.indexrelid = '42') \
             THEN 'yes' ELSE 'no' END AS is_key,\n  \
             pg_catalog.pg_get_indexdef(a.attrelid, a.attnum, TRUE) AS indexdef"
        ));
        assert_eq!(
            column_headers(&layout),
            [
                "Column",
                "Type",
                "Key?",
                "Definition",
                "Storage",
                "Stats target"
            ]
        );
        let (_, layout) = column_query("42", &info(b'i'), false, 100_000, false);
        assert_eq!((layout.is_key, layout.indexdef), (None, Some(2)));

        let (query, layout) = column_query("42", &info(b'f'), true, 180_006, false);
        assert!(query.contains("END AS attfdwoptions,\n  a.attstorage,\n  CASE"));
        assert_eq!(layout.fdwopts, Some(7));
        assert_eq!(layout.compression, None);

        // A composite type has comments but no statistics target.
        let (_, layout) = column_query("42", &info(b'c'), true, 180_006, false);
        assert_eq!(
            column_headers(&layout),
            [
                "Column",
                "Type",
                "Collation",
                "Nullable",
                "Default",
                "Storage",
                "Description"
            ]
        );
        let (query, _) = column_query("42", &info(b'r'), false, 90_600, false);
        assert!(query.contains(
            ",\n  ''::pg_catalog.char AS attidentity,\n  ''::pg_catalog.char AS attgenerated"
        ));
    }

    #[test]
    fn a_columns_default_cell_says_how_it_is_generated() {
        let (_, layout) = column_query("42", &info(b'r'), true, 180_006, false);
        let cells = |row: &[Option<&str>]| {
            let row = rows(&[row]);
            text(&column_cells(&row[0], &layout))
        };
        let row = |def: Option<&str>,
                   identity: &str,
                   generated: &str,
                   storage: &str,
                   compression: &str| {
            cells(&[
                Some("a"),
                Some("integer"),
                def,
                Some("t"),
                Some("C"),
                Some(identity),
                Some(generated),
                Some(storage),
                Some(compression),
                None,
                Some("a comment"),
            ])
        };
        assert_eq!(
            row(None, "a", "", "p", ""),
            [
                "a",
                "integer",
                "C",
                "not null",
                "generated always as identity",
                "plain",
                "",
                "",
                "a comment"
            ]
        );
        assert_eq!(
            row(None, "d", "", "m", "p")[4..6],
            ["generated by default as identity", "main"]
        );
        assert_eq!(
            row(Some("a * 2"), "", "s", "x", "l")[4..7],
            ["generated always as (a * 2) stored", "extended", "lz4"]
        );
        assert_eq!(
            row(Some("a + 1"), "", "v", "e", "?")[4..7],
            ["generated always as (a + 1)", "external", "???"]
        );
        assert_eq!(row(Some("1"), "", "", "?", "")[4..6], ["1", "???"]);
    }

    #[test]
    fn the_title_names_the_kind_and_whether_it_is_logged() {
        let title = |relkind: u8, persistence: u8| {
            let mut info = info(relkind);
            info.relpersistence = persistence;
            table_title(&info, "s", "x")
        };
        assert_eq!(title(b'r', b'p'), "Table \"s.x\"");
        assert_eq!(title(b'r', b'u'), "Unlogged table \"s.x\"");
        assert_eq!(title(b'v', b'u'), "View \"s.x\"");
        assert_eq!(title(b'm', b'p'), "Materialized view \"s.x\"");
        assert_eq!(title(b'i', b'u'), "Unlogged index \"s.x\"");
        assert_eq!(title(b'I', b'p'), "Partitioned index \"s.x\"");
        assert_eq!(title(b'I', b'u'), "Unlogged partitioned index \"s.x\"");
        assert_eq!(title(b't', b'p'), "TOAST table \"s.x\"");
        assert_eq!(title(b'c', b'p'), "Composite type \"s.x\"");
        assert_eq!(title(b'f', b'p'), "Foreign table \"s.x\"");
        assert_eq!(title(b'p', b'u'), "Unlogged partitioned table \"s.x\"");
        assert_eq!(title(b'z', b'p'), "?z? \"s.x\"");
        let mut seq = info(b'S');
        assert_eq!(sequence_title(&seq, "s", "q"), "Sequence \"s.q\"");
        seq.relpersistence = b'u';
        assert_eq!(sequence_title(&seq, "s", "q"), "Unlogged sequence \"s.q\"");
    }

    #[test]
    fn a_sequence_names_its_owner_only_when_exactly_one_row_does() {
        assert_eq!(
            text(&sequence_footers(&rows(&[&[Some("s.t.id"), Some("a")]]))),
            ["Owned by: s.t.id"]
        );
        assert_eq!(
            text(&sequence_footers(&rows(&[&[Some("s.t.id"), Some("i")]]))),
            ["Sequence for identity column: s.t.id"]
        );
        let two = rows(&[&[Some("x"), Some("a")], &[Some("y"), Some("a")]]);
        assert!(sequence_footers(&two).is_empty());
        assert!(sequence_footers(&[]).is_empty());
        assert!(sequence_query("42", 90_600).is_none());
        assert!(sequence_owner_query("42").contains(
            "INNER JOIN pg_catalog.pg_attribute a ON (\n a.attrelid=c.oid AND\n \
             a.attnum=d.refobjsubid)\n"
        ));
    }

    #[test]
    fn an_index_footer_lists_what_is_true_of_it() {
        let footer = |flags: [&str; 8], pred: &str| {
            let mut row: Vec<Option<&str>> = flags.iter().map(|&f| Some(f)).collect();
            row.extend([Some("btree"), Some("t"), Some(pred)]);
            String::from_utf8(index_footer(&rows(&[&row])[0], "s")).unwrap()
        };
        assert_eq!(
            footer(["f", "t", "t", "t", "f", "f", "f", "f"], ""),
            "primary key, btree, for table \"s.t\", clustered"
        );
        assert_eq!(
            footer(["t", "f", "f", "f", "t", "t", "t", "t"], "a > 1"),
            "unique nulls not distinct, btree, for table \"s.t\", predicate (a > 1), \
             invalid, deferrable, initially deferred, replica identity"
        );
        assert_eq!(
            footer(["f", "f", "f", "t", "f", "f", "f", "t"], ""),
            "btree, for table \"s.t\""
        );
    }

    #[test]
    fn a_tables_index_line_echoes_the_definition_after_using() {
        let line = |cells: [Option<&str>; 13]| {
            let (line, spc) = table_index_line(&rows(&[&cells])[0]);
            (String::from_utf8(line).unwrap(), spc)
        };
        let t = Some("t");
        let f = Some("f");
        assert_eq!(
            line([
                Some("t_pkey"),
                t,
                t,
                t,
                t,
                Some("CREATE UNIQUE INDEX t_pkey ON s.t USING btree (a)"),
                Some("PRIMARY KEY (a)"),
                Some("p"),
                f,
                f,
                t,
                Some("1663"),
                f
            ]),
            (
                "    \"t_pkey\" PRIMARY KEY, btree (a) CLUSTER REPLICA IDENTITY".to_string(),
                1663
            )
        );
        assert_eq!(
            line([
                Some("t_c"),
                f,
                t,
                f,
                f,
                Some("CREATE UNIQUE INDEX t_c ON s.t USING btree (c)"),
                Some("UNIQUE (c) DEFERRABLE"),
                Some("u"),
                t,
                t,
                f,
                Some("0"),
                f
            ])
            .0,
            "    \"t_c\" UNIQUE CONSTRAINT, btree (c) DEFERRABLE INITIALLY DEFERRED INVALID"
        );
        // An index with no constraint: no label, and no "USING" to cut at.
        assert_eq!(
            line([
                Some("i"),
                f,
                t,
                f,
                t,
                Some("odd"),
                None,
                None,
                None,
                None,
                f,
                Some("0"),
                f
            ])
            .0,
            "    \"i\" UNIQUE, odd"
        );
        // Exclusion and WITHOUT OVERLAPS print the constraint instead.
        for (contype, period) in [(Some("x"), f), (Some("p"), t)] {
            assert_eq!(
                line([
                    Some("e"),
                    t,
                    t,
                    f,
                    t,
                    Some("CREATE INDEX e ON s.t USING gist (r)"),
                    Some("EXCLUDE USING gist (r WITH &&)"),
                    contype,
                    t,
                    f,
                    f,
                    Some("0"),
                    period
                ])
                .0,
                "    \"e\" EXCLUDE USING gist (r WITH &&)"
            );
        }
    }

    #[test]
    fn a_tablespace_is_a_new_footer_or_the_end_of_the_last() {
        assert_eq!(tablespace_query(RelKind::Relation, 0), None);
        assert_eq!(tablespace_query(RelKind::View, 1663), None);
        assert_eq!(tablespace_query(RelKind::ForeignTable, 1663), None);
        assert_eq!(
            tablespace_query(RelKind::Index, 1663).unwrap(),
            "SELECT spcname FROM pg_catalog.pg_tablespace\nWHERE oid = '1663';"
        );
        let spc = rows(&[&[Some("spc")]]);
        let mut footers = vec![b"    \"i\" btree (a)".to_vec()];
        add_tablespace_footer(&mut footers, &spc, false);
        add_tablespace_footer(&mut footers, &spc, true);
        add_tablespace_footer(&mut footers, &[], true);
        assert_eq!(
            text(&footers),
            [
                "    \"i\" btree (a), tablespace \"spc\"",
                "Tablespace: \"spc\""
            ]
        );
    }

    #[test]
    fn the_policy_heading_says_whether_row_security_is_on_and_forced() {
        let policy = rows(&[&[
            Some("p"),
            Some("f"),
            Some("r1,r2"),
            Some("(a > 0)"),
            Some("(a < 9)"),
            Some("UPDATE"),
        ]]);
        let heading = |on: bool, forced: bool, rows: &[Vec<Option<&[u8]>>]| {
            let mut info = info(b'r');
            info.rowsecurity = on;
            info.forcerowsecurity = forced;
            text(&policy_footers(rows, &info))
        };
        assert_eq!(
            heading(true, false, &policy),
            [
                "Policies:",
                "    POLICY \"p\" AS RESTRICTIVE FOR UPDATE\n      TO r1,r2\n      \
                 USING ((a > 0))\n      WITH CHECK ((a < 9))"
            ]
        );
        assert_eq!(
            heading(true, true, &policy)[0],
            "Policies (forced row security enabled):"
        );
        assert_eq!(
            heading(false, true, &policy)[0],
            "Policies (row security disabled):"
        );
        assert_eq!(
            heading(true, false, &[]),
            ["Policies (row security enabled): (none)"]
        );
        assert_eq!(
            heading(true, true, &[]),
            ["Policies (forced row security enabled): (none)"]
        );
        assert!(heading(false, false, &[]).is_empty());
        let plain = rows(&[&[Some("p"), Some("t"), None, None, None, None]]);
        assert_eq!(heading(false, false, &plain)[1], "    POLICY \"p\"");
        assert!(policies_query("42", 90_400).is_none());
        assert!(policies_query("42", 90_600).unwrap().starts_with(
            "SELECT pol.polname, 't' as polpermissive,\n  CASE WHEN pol.polroles = '{0}'"
        ));
    }

    #[test]
    fn statistics_show_their_kinds_only_when_some_but_not_all_are_on() {
        let stat = |kinds: [&'static str; 3], target: Option<&'static str>| {
            vec![
                Some(b"1".as_slice()),
                Some(b"s.t"),
                Some(b"s"),
                Some(b"st"),
                Some(b"a, b"),
                Some(kinds[0].as_bytes()),
                Some(kinds[1].as_bytes()),
                Some(kinds[2].as_bytes()),
                target.map(str::as_bytes),
            ]
        };
        let footers = statistics_footers(
            &[
                stat(["t", "f", "t"], None),
                stat(["t", "t", "t"], Some("100")),
                stat(["f", "f", "f"], Some("-1")),
            ],
            180_006,
        );
        assert_eq!(
            text(&footers),
            [
                "Statistics objects:",
                "    \"s.st\" (ndistinct, mcv) ON a, b FROM s.t",
                "    \"s.st\" ON a, b FROM s.t; STATISTICS 100",
                "    \"s.st\" ON a, b FROM s.t",
            ]
        );
        // Before 14 the kinds are always listed, even none.
        let footers = statistics_footers(
            &[
                stat(["f", "t", "f"], Some("-1")),
                stat(["f", "f", "f"], Some("5")),
            ],
            130_000,
        );
        assert_eq!(
            text(&footers)[1..],
            [
                "    \"s.st\" (dependencies) ON a, b FROM s.t",
                "    \"s.st\" () ON a, b FROM s.t; STATISTICS 5"
            ]
        );
        assert!(statistics_query("42", 90_600).is_none());
        assert!(
            statistics_query("42", 120_000)
                .unwrap()
                .contains("  -1 AS stxstattarget\n")
        );
        assert!(statistics_footers(&[], 180_006).is_empty());
    }

    #[test]
    fn rules_and_triggers_are_grouped_by_how_they_fire() {
        let rule = |name: &str, enabled: &'static str| {
            vec![
                Some(name.as_bytes().to_vec()),
                Some(format!("CREATE RULE {name} AS ON INSERT TO t DO NOTHING").into_bytes()),
                Some(enabled.as_bytes().to_vec()),
            ]
        };
        let owned = [
            rule("r", "R"),
            rule("a", "A"),
            rule("o", "O"),
            rule("d", "D"),
            rule("o2", "O"),
        ];
        let borrowed: Vec<Vec<Option<&[u8]>>> = owned
            .iter()
            .map(|r| r.iter().map(|c| c.as_deref()).collect())
            .collect();
        assert_eq!(
            text(&rule_footers(&borrowed)),
            [
                "Rules:",
                "    o AS ON INSERT TO t DO NOTHING",
                "    o2 AS ON INSERT TO t DO NOTHING",
                "Disabled rules:",
                "    d AS ON INSERT TO t DO NOTHING",
                "Rules firing always:",
                "    a AS ON INSERT TO t DO NOTHING",
                "Rules firing on replica only:",
                "    r AS ON INSERT TO t DO NOTHING",
            ]
        );
        assert_eq!(
            text(&view_rule_footers(&borrowed[2..3])),
            ["Rules:", " o AS ON INSERT TO t DO NOTHING"]
        );

        let def = "CREATE TRIGGER g BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()";
        let trigger =
            |enabled: &'static str, internal: &'static str, parent: Option<&'static str>| {
                vec![
                    Some(b"g".as_slice()),
                    Some(def.as_bytes()),
                    Some(enabled.as_bytes()),
                    Some(internal.as_bytes()),
                    parent.map(str::as_bytes),
                ]
            };
        let footers = trigger_footers(&[
            trigger("R", "f", None),
            trigger("D", "t", None),
            trigger("O", "f", Some("s.p")),
            trigger("A", "f", None),
            trigger("D", "f", None),
            trigger("t", "f", None),
        ]);
        let tail = "g BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()";
        assert_eq!(
            text(&footers),
            [
                "Triggers:".to_string(),
                format!("    {tail}, ON TABLE s.p"),
                format!("    {tail}"),
                "Disabled user triggers:".to_string(),
                format!("    {tail}"),
                "Disabled internal triggers:".to_string(),
                format!("    {tail}"),
                "Triggers firing always:".to_string(),
                format!("    {tail}"),
                "Triggers firing on replica only:".to_string(),
                format!("    {tail}"),
            ]
        );
        let query = triggers_query("42", 140_000);
        assert!(query.contains("  CASE WHEN t.tgparentid != 0 THEN\n"));
        assert!(query.contains("OR EXISTS (SELECT 1 FROM pg_catalog.pg_depend"));
        let query = triggers_query("42", 180_006);
        assert!(query.ends_with(
            "WHERE t.tgrelid = '42' AND \
             (NOT t.tgisinternal OR (t.tgisinternal AND t.tgenabled = 'D'))\nORDER BY 1;"
        ));
        assert!(triggers_query("42", 120_000).contains("  NULL AS parent\n"));
    }

    #[test]
    fn parents_and_children_line_up_under_their_label() {
        let parents = rows(&[&[Some("s.a")], &[Some("s.b")], &[Some("s.c")]]);
        assert_eq!(
            text(&inherits_footers(&parents)),
            ["Inherits: s.a,", "          s.b,", "          s.c"]
        );
        let children = rows(&[
            &[
                Some("s.p1"),
                Some("r"),
                Some("f"),
                Some("FOR VALUES IN (1)"),
            ],
            &[
                Some("s.p2"),
                Some("p"),
                Some("t"),
                Some("FOR VALUES IN (2)"),
            ],
            &[Some("s.p3"), Some("f"), Some("f"), Some("DEFAULT")],
        ]);
        assert_eq!(
            text(&child_table_footers(&children, true, true)),
            [
                "Partitions: s.p1 FOR VALUES IN (1),",
                "            s.p2 FOR VALUES IN (2), PARTITIONED (DETACH PENDING),",
                "            s.p3 DEFAULT, FOREIGN",
            ]
        );
        assert_eq!(
            text(&child_table_footers(&children, true, false)),
            ["Number of partitions: 3 (Use \\d+ to list them.)"]
        );
        let inherited = rows(&[&[Some("s.c"), Some("r"), Some("f"), None]]);
        assert_eq!(
            text(&child_table_footers(&inherited, false, false)),
            ["Number of child tables: 1 (Use \\d+ to list them.)"]
        );
        assert_eq!(
            text(&child_table_footers(&inherited, false, true)),
            ["Child tables: s.c"]
        );
        assert_eq!(
            text(&child_table_footers(&[], true, true)),
            ["Number of partitions: 0"]
        );
        assert!(child_table_footers(&[], false, false).is_empty());
    }

    #[test]
    fn the_closing_footers_show_only_what_is_not_the_default() {
        let mut info = info(b'r');
        info.reloftype = Some(b"s.ct".to_vec());
        info.relreplident = b'f';
        info.hasoids = true;
        info.reloptions = b"fillfactor=70".to_vec();
        assert_eq!(
            text(&typed_table_and_identity_footers(&info, "s", true)),
            [
                "Typed table of type: s.ct",
                "Replica Identity: FULL",
                "Has OIDs: yes"
            ]
        );
        assert_eq!(
            text(&typed_table_and_identity_footers(&info, "s", false)),
            ["Typed table of type: s.ct"]
        );
        info.reloftype = None;
        info.hasoids = false;
        // Upstream's own labels: NOTHING outside the catalog is "???", and
        // the default inside it is "NOTHING".
        for (schema, replident, shown) in [
            ("s", b'n', vec!["Replica Identity: ???"]),
            ("s", b'd', vec![]),
            ("s", b'i', vec![]),
            ("pg_catalog", b'd', vec!["Replica Identity: NOTHING"]),
            ("pg_catalog", b'n', vec![]),
        ] {
            info.relreplident = replident;
            assert_eq!(
                text(&typed_table_and_identity_footers(&info, schema, true)),
                shown
            );
        }
        assert_eq!(
            text(&access_method_footers(&info, true, false)),
            ["Access method: heap"]
        );
        assert!(access_method_footers(&info, true, true).is_empty());
        assert!(access_method_footers(&info, false, false).is_empty());
        assert_eq!(
            text(&options_footers(&info, true)),
            ["Options: fillfactor=70"]
        );
        assert!(options_footers(&info, false).is_empty());
    }

    #[test]
    fn constraints_references_and_publications_are_one_footer_each() {
        assert_eq!(
            text(&check_constraint_footers(&rows(&[&[
                Some("c"),
                Some("CHECK (a > 0)")
            ]]))),
            ["Check constraints:", "    \"c\" CHECK (a > 0)"]
        );
        let fks = rows(&[
            &[
                Some("t"),
                Some("fk"),
                Some("FOREIGN KEY (a) REFERENCES u(a)"),
                Some("s.t"),
            ],
            &[
                Some("f"),
                Some("pfk"),
                Some("FOREIGN KEY (b) REFERENCES u(b)"),
                Some("s.p"),
            ],
        ]);
        assert_eq!(
            text(&foreign_key_footers(&fks)),
            [
                "Foreign-key constraints:",
                "    \"fk\" FOREIGN KEY (a) REFERENCES u(a)",
                "    TABLE \"s.p\" CONSTRAINT \"pfk\" FOREIGN KEY (b) REFERENCES u(b)",
            ]
        );
        assert_eq!(
            text(&referenced_by_footers(&rows(&[&[
                Some("fk"),
                Some("s.r"),
                Some("FOREIGN KEY (x) REFERENCES s.t(a)")
            ]]))),
            [
                "Referenced by:",
                "    TABLE \"s.r\" CONSTRAINT \"fk\" FOREIGN KEY (x) REFERENCES s.t(a)"
            ]
        );
        assert_eq!(
            text(&publication_footers(&rows(&[
                &[Some("all"), None, None],
                &[Some("cols"), Some("(a > 5)"), Some("a, b")],
            ]))),
            [
                "Publications:",
                "    \"all\"",
                "    \"cols\" (a, b) WHERE (a > 5)"
            ]
        );
        assert!(publication_footers(&[]).is_empty());
    }

    #[test]
    fn not_null_constraints_say_how_they_are_inherited_and_keys_where_they_come_from() {
        let nn = rows(&[
            &[
                Some("n1"),
                Some("a"),
                Some("t"),
                Some("t"),
                Some("f"),
                Some("t"),
            ],
            &[
                Some("n2"),
                Some("b"),
                Some("f"),
                Some("t"),
                Some("t"),
                Some("f"),
            ],
            &[
                Some("n3"),
                Some("c"),
                Some("f"),
                Some("f"),
                Some("t"),
                Some("t"),
            ],
            &[
                Some("n4"),
                Some("d"),
                Some("f"),
                Some("t"),
                Some("f"),
                Some("t"),
            ],
        ]);
        assert_eq!(
            text(&not_null_constraint_footers(&nn)),
            [
                "Not-null constraints:",
                "    \"n1\" NOT NULL \"a\" NO INHERIT",
                "    \"n2\" NOT NULL \"b\" (local, inherited) NOT VALID",
                "    \"n3\" NOT NULL \"c\" (inherited)",
                "    \"n4\" NOT NULL \"d\"",
            ]
        );
        // A partition's foreign keys include its ancestors', from 12.
        let mut part = info(b'r');
        part.ispartition = true;
        assert!(
            foreign_keys_query("42", &part, 180_006)
                .contains("pg_catalog.pg_partition_ancestors('42')")
        );
        let plain = foreign_keys_query("42", &info(b'r'), 180_006);
        assert!(plain.ends_with("r.contype = 'f'\n     AND conparentid = 0\nORDER BY conname"));
        assert!(
            foreign_keys_query("42", &part, 110_000).ends_with("r.contype = 'f'\nORDER BY conname")
        );
        assert!(
            referenced_by_query("42", 110_000)
                .contains(" WHERE confrelid = 42 AND contype = 'f'\n")
        );
    }

    #[test]
    fn a_partition_shows_its_parent_bound_and_with_plus_its_constraint() {
        let row = rows(&[&[Some("s.p"), Some("FOR VALUES IN (1)"), Some("t"), None]]);
        assert_eq!(
            text(&partition_of_footers(&row, true)),
            [
                "Partition of: s.p FOR VALUES IN (1) DETACH PENDING",
                "No partition constraint"
            ]
        );
        let row = rows(&[&[Some("s.p"), Some("DEFAULT"), Some("f"), Some("(a = 1)")]]);
        assert_eq!(
            text(&partition_of_footers(&row, true)),
            ["Partition of: s.p DEFAULT", "Partition constraint: (a = 1)"]
        );
        assert_eq!(
            text(&partition_of_footers(&row, false)),
            ["Partition of: s.p DEFAULT"]
        );
        assert_eq!(
            partition_of_query("42", true, 130_000),
            "SELECT inhparent::pg_catalog.regclass,\n  \
             pg_catalog.pg_get_expr(c.relpartbound, c.oid),\n  false as inhdetachpending,\n  \
             pg_catalog.pg_get_partition_constraintdef(c.oid)\n\
             FROM pg_catalog.pg_class c JOIN pg_catalog.pg_inherits i ON c.oid = inhrelid\n\
             WHERE c.oid = '42';"
        );
        assert_eq!(
            text(&partition_key_footers(&rows(&[&[Some("RANGE (a)")]]))),
            ["Partition key: RANGE (a)"]
        );
        assert_eq!(
            text(&owning_table_footers(&rows(&[&[Some("s"), Some("t")]]))),
            ["Owning table: \"s.t\""]
        );
        let ft = rows(&[&[Some("srv"), Some("a '1'")]]);
        assert_eq!(
            text(&foreign_server_footers(&ft[0])),
            ["Server: srv", "FDW options: (a '1')"]
        );
        let ft = rows(&[&[Some("srv"), Some("")]]);
        assert_eq!(text(&foreign_server_footers(&ft[0])), ["Server: srv"]);
    }
}
