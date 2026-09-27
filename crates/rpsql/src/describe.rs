//! The `\d` family: `src/bin/psql/describe.c`, and the pattern machinery it
//! shares with the other client programs from `src/fe_utils/string_utils.c`.
//!
//! Each command is a query built from its flags and pattern, run through
//! `PSQLexec`, and printed with a title. The query text is kept identical to
//! upstream's, byte for byte, because `ECHO_HIDDEN` shows it and because the
//! server's answer is only the same if the question is.
//!
//! Scope (NAT-401): the dispatcher's whole `\d` switch
//! ([`DescribeCommand::parse`]) and name patterns ([`pattern_to_sql_regex`],
//! [`process_sql_name_pattern`], [`validate_sql_name_pattern`]); slice 1's
//! `listTables` ([`list_tables_query`]), which answers `\d` with no pattern
//! and `\dt`, `\di`, `\dv`, `\dm`, `\ds` and `\dE`; and slice 2's
//! `listPartitionedTables` ([`list_partitioned_tables_query`], `\dP`) and the
//! access-method listings ([`describe_access_methods_query`], `\dA`, and
//! [`OperatorListing`], `\dAc`, `\dAf`, `\dAo`, `\dAp`); and slice 3's
//! functions, operators and types ([`describe_aggregates_query`], `\da`;
//! [`describe_functions_query`], `\df`; [`describe_types_query`], `\dT`;
//! [`describe_operators_query`], `\do`) and
//! [`describe_configuration_parameters_query`], `\dconfig`. Every other
//! command the switch recognizes is refused by name until its slice lands.
//!
//! Queries are built as `String`: a pattern arrives as a slash option, which
//! the lexer has already made UTF-8. That is also why the multibyte steps of
//! `patternToSQLRegex` and `appendStringLiteral` are byte steps here — in
//! UTF-8 no byte of a multibyte character is ASCII, so none of them can be
//! mistaken for a quote, a dot or a wildcard (`docs/divergences.md`).

use std::fmt::Write as _;

/// One `\d…` command, as `exec_command_d()` (`command.c:1021`) dispatches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescribeCommand {
    /// `listTables()` (`describe.c:4011`), with the relation-type letters it
    /// is given: `"tvmsE"` for a bare `\d`, else the command after its `d`.
    ListTables(String),
    /// `describeTableDetails()` (`describe.c:1492`): `\d` with a pattern.
    TableDetails,
    /// `listPartitionedTables()` (`describe.c:4266`), with the letters after
    /// `\dP` (`&cmd[2]`).
    ListPartitionedTables(String),
    /// `describeAccessMethods()` (`describe.c:148`): `\dA`.
    AccessMethods,
    /// One of the `\dA` listings that take an access-method pattern and a
    /// second one (`command.c:1061`-`:1092`).
    OperatorListing(OperatorListing),
    /// `describeAggregates()` (`describe.c:78`): `\da`.
    Aggregates,
    /// `describeFunctions()` (`describe.c:295`), with the letters after
    /// `\df` (`&cmd[2]`).
    Functions(String),
    /// `describeTypes()` (`describe.c:639`): `\dT`.
    Types,
    /// `describeOperators()` (`describe.c:794`): `\do`.
    Operators,
    /// `describeConfigurationParameters()` (`describe.c:4715`): `\dconfig`.
    ConfigurationParameters,
    /// A command the switch recognizes whose port has not landed yet; the
    /// name is its `describe.c` function.
    NotYet(&'static str),
}

/// What `exec_command_d()` read off the command name besides the command
/// itself (`command.c:1037`-`:1048`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescribeFlags {
    /// `+`: `show_verbose`.
    pub verbose: bool,
    /// `S`: `show_system`.
    pub system: bool,
    /// `x` after the second character: expanded mode for this command only.
    pub expanded: bool,
}

impl DescribeFlags {
    /// `command.c:1037`-`:1048`.
    #[must_use]
    pub fn parse(cmd: &str) -> Self {
        Self {
            verbose: cmd.contains('+'),
            system: cmd.contains('S'),
            // "the 'x' option cannot appear immediately after \d".
            expanded: cmd.len() > 1 && cmd.get(2..).is_some_and(|rest| rest.contains('x')),
        }
    }
}

impl DescribeCommand {
    /// The `switch (cmd[1])` of `exec_command_d()` (`command.c:1050`-`:1287`)
    /// for a command that starts with `d`; `None` is `PSQL_CMD_UNKNOWN`.
    #[must_use]
    pub fn parse(cmd: &str, has_pattern: bool) -> Option<Self> {
        let b = cmd.as_bytes();
        let at = |i: usize| b.get(i).copied().unwrap_or(0);
        let not_yet = |name| Some(Self::NotYet(name));
        match at(1) {
            0 | b'+' | b'S' => Some(if has_pattern {
                Self::TableDetails
            } else {
                Self::ListTables("tvmsE".to_string())
            }),
            b'A' => match at(2) {
                0 | b'+' | b'x' => Some(Self::AccessMethods),
                b'c' => Some(Self::OperatorListing(OperatorListing::Classes)),
                b'f' => Some(Self::OperatorListing(OperatorListing::Families)),
                b'o' => Some(Self::OperatorListing(OperatorListing::Operators)),
                b'p' => Some(Self::OperatorListing(OperatorListing::Functions)),
                _ => None,
            },
            b'a' => Some(Self::Aggregates),
            b'b' => not_yet("describeTablespaces"),
            b'c' if cmd.starts_with("dconfig") => Some(Self::ConfigurationParameters),
            b'c' => not_yet("listConversions"),
            b'C' => not_yet("listCasts"),
            b'd' if cmd.starts_with("ddp") => not_yet("listDefaultACLs"),
            b'd' => not_yet("objectDescription"),
            b'D' => not_yet("listDomains"),
            b'f' => match at(2) {
                0 | b'+' | b'S' | b'a' | b'n' | b'p' | b't' | b'w' | b'x' => {
                    Some(Self::Functions(cmd[2..].to_string()))
                }
                _ => None,
            },
            b'g' | b'u' => not_yet("describeRoles"),
            b'l' => not_yet("listLargeObjects"),
            b'L' => not_yet("listLanguages"),
            b'n' => not_yet("listSchemas"),
            b'o' => Some(Self::Operators),
            b'O' => not_yet("listCollations"),
            b'p' => not_yet("permissionsList"),
            b'P' => match at(2) {
                0 | b'+' | b't' | b'i' | b'n' | b'x' => {
                    Some(Self::ListPartitionedTables(cmd[2..].to_string()))
                }
                _ => None,
            },
            b'T' => Some(Self::Types),
            b't' | b'v' | b'm' | b'i' | b's' | b'E' => Some(Self::ListTables(cmd[1..].to_string())),
            b'r' => match (at(2), at(3)) {
                (b'd', b's') => not_yet("listDbRoleSettings"),
                (b'g', _) => not_yet("describeRoleGrants"),
                _ => None,
            },
            b'R' => match at(2) {
                b'p' => not_yet("describePublications"),
                b's' => not_yet("describeSubscriptions"),
                _ => None,
            },
            b'F' => match at(2) {
                0 | b'+' | b'x' => not_yet("listTSConfigs"),
                b'p' => not_yet("listTSParsers"),
                b'd' => not_yet("listTSDictionaries"),
                b't' => not_yet("listTSTemplates"),
                _ => None,
            },
            b'e' => match at(2) {
                b's' => not_yet("listForeignServers"),
                b'u' => not_yet("listUserMappings"),
                b'w' => not_yet("listForeignDataWrappers"),
                b't' => not_yet("listForeignTables"),
                _ => None,
            },
            b'x' => not_yet("listExtensions"),
            b'X' => not_yet("listExtendedStats"),
            b'y' => not_yet("listEventTriggers"),
            _ => None,
        }
    }

    /// How many patterns `exec_command_d()` reads for `cmd`: a second one only
    /// for `\dAc`, `\dAf`, `\dAo` and `\dAp`, and only after a first
    /// (`command.c:1065`); for `\df` and `\do`, after a first, up to
    /// [`FUNC_MAX_ARGS`] argument types (`exec_command_dfo()`,
    /// `command.c:1313`). Any argument past them draws the "extra argument"
    /// warning.
    #[must_use]
    pub fn patterns_read(cmd: &str, has_pattern: bool) -> usize {
        match Self::parse(cmd, has_pattern) {
            Some(Self::OperatorListing(_)) if has_pattern => 2,
            Some(Self::Functions(_) | Self::Operators) if has_pattern => 1 + FUNC_MAX_ARGS,
            _ => 1,
        }
    }
}

/// The `\dA` listings with two patterns: an access method's, then a type's
/// (`\dAc`, `\dAf`) or an operator family's (`\dAo`, `\dAp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorListing {
    /// `listOperatorClasses()` (`describe.c:6898`): `\dAc`.
    Classes,
    /// `listOperatorFamilies()` (`describe.c:6999`): `\dAf`.
    Families,
    /// `listOpFamilyOperators()` (`describe.c:7088`): `\dAo`.
    Operators,
    /// `listOpFamilyFunctions()` (`describe.c:7195`): `\dAp`.
    Functions,
}

/// `patternToSQLRegex()`'s output (`string_utils.c:1335`): up to three
/// regular expressions, and how many separators the pattern had.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatternRegex {
    /// `dbnamebuf`: the part before the second-to-last separator, literal
    /// when `want_literal_dbname`. `None` when the pattern had no such part
    /// or none was asked for.
    pub dbname: Option<String>,
    /// `schemabuf`, likewise.
    pub schema: Option<String>,
    /// `namebuf`: always present, `^(…)$`-anchored.
    pub name: String,
    /// `dotcnt`: every separator, including those past the last buffer.
    pub dotcnt: usize,
}

/// How many dotted parts [`pattern_to_sql_regex`] may split a pattern into:
/// which of `dbnamebuf` and `schemabuf` the caller passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternParts {
    /// Only `namebuf`.
    Name,
    /// `schemabuf` and `namebuf`.
    SchemaName,
    /// All three.
    DbSchemaName,
}

/// `patternToSQLRegex()` (`string_utils.c:1335`): a shell-style, possibly
/// qualified pattern to anchored SQL regular expressions.
///
/// Unquoted letters are lower-cased, `*` becomes `.*`, `?` becomes `.`, `$`
/// is always escaped, and a double-quoted part is taken literally. Outside
/// quotes other regexp characters pass through unless `force_escape`, except
/// that `[]` is always escaped as the likely tail of an array type name.
#[must_use]
pub fn pattern_to_sql_regex(
    pattern: &str,
    parts: PatternParts,
    force_escape: bool,
    want_literal_dbname: bool,
) -> PatternRegex {
    let max = match parts {
        PatternParts::Name => 0,
        PatternParts::SchemaName => 1,
        PatternParts::DbSchemaName => 2,
    };
    // `curbuf`, and the buffers before it that a separator closed.
    let mut cur = String::from("^(");
    let mut done: Vec<String> = Vec::new();
    let mut left = want_literal_dbname;
    let mut left_literal = String::new();
    let mut inquotes = false;
    let mut dotcnt = 0;

    // Every character the loop tests is ASCII, so a multibyte character is
    // one step and is copied whole, as `PQmblenBounded` has it.
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        // What goes into `curbuf`, and what into `left_literal`.
        let (regex, literal): (&str, Option<char>) = match ch {
            '"' if inquotes && chars.peek() == Some(&'"') => {
                // Emit one quote, stay in inquotes mode.
                chars.next();
                ("\"", Some('"'))
            }
            '"' => {
                inquotes = !inquotes;
                ("", None)
            }
            'A'..='Z' if !inquotes => {
                let lower = ch.to_ascii_lowercase();
                cur.push(lower);
                ("", Some(lower))
            }
            '*' if !inquotes => (".*", Some('*')),
            '?' if !inquotes => (".", Some('?')),
            '.' if !inquotes => {
                left = false;
                dotcnt += 1;
                if done.len() < max {
                    cur.push_str(")$");
                    done.push(std::mem::replace(&mut cur, String::from("^(")));
                } else {
                    cur.push('.');
                }
                ("", None)
            }
            // Always quoted: legal in identifiers, and meaningless anchored.
            '$' => ("\\$", Some('$')),
            _ => {
                if (inquotes || force_escape) && "|*+?()[]{}.^$\\".contains(ch)
                    || ch == '[' && chars.peek() == Some(&']')
                {
                    cur.push('\\');
                }
                cur.push(ch);
                ("", Some(ch))
            }
        };
        cur.push_str(regex);
        if left && let Some(c) = literal {
            left_literal.push(c);
        }
    }
    cur.push_str(")$");

    // Hand the buffers out from the last: name, then schema, then dbname.
    let mut out = PatternRegex {
        name: cur,
        dotcnt,
        ..PatternRegex::default()
    };
    if max >= 1 {
        out.schema = done.pop();
    }
    if max >= 2
        && let Some(db) = done.pop()
    {
        out.dbname = Some(if want_literal_dbname {
            left_literal
        } else {
            db
        });
    }
    out
}

/// `appendStringLiteralConn()` (`string_utils.c:446`) for a server of
/// version 8.1 or later: a string with a backslash is written as `E'…'`,
/// separated by a space from anything before it but a space, with both
/// quotes and backslashes doubled; any other string as `'…'` with quotes
/// doubled. `standard_conforming_strings` only decides whether a backslash
/// is doubled, and a string with one never reaches the second form.
pub fn append_string_literal_conn(buf: &mut String, s: &str) {
    if s.contains('\\') {
        if !buf.is_empty() && !buf.ends_with(' ') {
            buf.push(' ');
        }
        buf.push('E');
        buf.push('\'');
        for c in s.chars() {
            if c == '\'' || c == '\\' {
                buf.push(c);
            }
            buf.push(c);
        }
    } else {
        buf.push('\'');
        for c in s.chars() {
            if c == '\'' {
                buf.push(c);
            }
            buf.push(c);
        }
    }
    buf.push('\'');
}

/// The catalog columns and visibility rule `processSQLNamePattern()` matches
/// a pattern against.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PatternVars<'a> {
    /// `schemavar`: `None` when the object has no schema.
    pub schemavar: Option<&'a str>,
    /// `namevar`
    pub namevar: Option<&'a str>,
    /// `altnamevar`: a second column the name may match instead.
    pub altnamevar: Option<&'a str>,
    /// `visibilityrule`: the clause that restricts an unqualified pattern (or
    /// none) to visible objects.
    pub visibilityrule: Option<&'a str>,
}

/// What `processSQLNamePattern()` reports besides the text it appended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatternClause {
    /// Whether any clause was added.
    pub added: bool,
    /// `dbnamebuf`: the literal database part, empty if none.
    pub dbname: String,
    /// `dotcnt`
    pub dotcnt: usize,
}

/// `processSQLNamePattern()` (`string_utils.c:1163`): append the `WHERE` /
/// `AND` clauses that restrict a catalog query to `pattern`, or to visible
/// objects when there is none. `buf` should end with a newline, and so does
/// what is appended.
pub fn process_sql_name_pattern(
    buf: &mut String,
    pattern: Option<&str>,
    mut have_where: bool,
    force_escape: bool,
    vars: PatternVars<'_>,
    sversion: i32,
) -> PatternClause {
    let mut clause = PatternClause::default();
    let mut where_and = |buf: &mut String, clause: &mut PatternClause| {
        buf.push_str(if have_where { "  AND " } else { "WHERE " });
        have_where = true;
        clause.added = true;
    };
    let collate = |buf: &mut String| {
        if sversion >= 120_000 {
            buf.push_str(" COLLATE pg_catalog.default");
        }
    };

    let Some(pattern) = pattern else {
        // Default: select all visible objects.
        if let Some(rule) = vars.visibilityrule {
            where_and(buf, &mut clause);
            let _ = writeln!(buf, "{rule}");
        }
        return clause;
    };

    // `validateSQLNamePattern`, psql's one caller with a schema, always asks
    // for the database part too.
    let parts = if vars.schemavar.is_some() {
        PatternParts::DbSchemaName
    } else {
        PatternParts::Name
    };
    let regex = pattern_to_sql_regex(pattern, parts, force_escape, true);
    clause.dotcnt = regex.dotcnt;
    if vars.schemavar.is_some() {
        clause.dbname = regex.dbname.clone().unwrap_or_default();
    }
    let namebuf = regex.name;
    let schemabuf = regex.schema.unwrap_or_default();

    if let Some(namevar) = vars.namevar
        && namebuf.len() > 2
        // Optimize away a "*" pattern.
        && namebuf != "^(.*)$"
    {
        where_and(buf, &mut clause);
        if let Some(altnamevar) = vars.altnamevar {
            let _ = write!(buf, "({namevar} OPERATOR(pg_catalog.~) ");
            append_string_literal_conn(buf, &namebuf);
            collate(buf);
            let _ = write!(buf, "\n        OR {altnamevar} OPERATOR(pg_catalog.~) ");
            append_string_literal_conn(buf, &namebuf);
            collate(buf);
            buf.push_str(")\n");
        } else {
            let _ = write!(buf, "{namevar} OPERATOR(pg_catalog.~) ");
            append_string_literal_conn(buf, &namebuf);
            collate(buf);
            buf.push('\n');
        }
    }

    match vars.schemavar {
        Some(schemavar) if schemabuf.len() > 2 => {
            if schemabuf != "^(.*)$" {
                where_and(buf, &mut clause);
                let _ = write!(buf, "{schemavar} OPERATOR(pg_catalog.~) ");
                append_string_literal_conn(buf, &schemabuf);
                collate(buf);
                buf.push('\n');
            }
        }
        _ => {
            // No schema pattern given, so select only visible objects.
            if let Some(rule) = vars.visibilityrule {
                where_and(buf, &mut clause);
                let _ = writeln!(buf, "{rule}");
            }
        }
    }
    clause
}

/// Why `validateSQLNamePattern()` refused a pattern: the text it logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(pub String);

/// `validateSQLNamePattern()` (`describe.c:6347`): [`process_sql_name_pattern`],
/// then refuse a pattern with `maxparts` or more dotted parts, and one whose
/// database part names a database other than `db` (`PQdb`).
///
/// # Errors
/// The message upstream logs with `pg_log_error`.
#[allow(clippy::too_many_arguments)] // upstream's signature, one for one
pub fn validate_sql_name_pattern(
    buf: &mut String,
    pattern: Option<&str>,
    have_where: bool,
    force_escape: bool,
    vars: PatternVars<'_>,
    maxparts: usize,
    sversion: i32,
    db: Option<&str>,
) -> Result<bool, PatternError> {
    let clause = process_sql_name_pattern(buf, pattern, have_where, force_escape, vars, sversion);
    let pattern = pattern.unwrap_or_default();
    if clause.dotcnt >= maxparts {
        return Err(PatternError(format!(
            "improper qualified name (too many dotted names): {pattern}"
        )));
    }
    if maxparts > 1 && clause.dotcnt == maxparts - 1 {
        match db {
            None => {
                return Err(PatternError(
                    "You are currently not connected to a database.".to_string(),
                ));
            }
            Some(db) if db != clause.dbname => {
                return Err(PatternError(format!(
                    "cross-database references are not implemented: {pattern}"
                )));
            }
            Some(_) => {}
        }
    }
    Ok(clause.added)
}

/// The relation types `listTables()` was asked for (`describe.c:4013`-`:4018`).
// One flag per letter of `tabtypes`, as upstream has them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TableTypes {
    /// `t`
    pub tables: bool,
    /// `i`
    pub indexes: bool,
    /// `v`
    pub views: bool,
    /// `m`
    pub mat_views: bool,
    /// `s`
    pub sequences: bool,
    /// `E`
    pub foreign: bool,
    /// How many letters were given: 0 is the `\dtvmsE` default, and only 1
    /// gives a type its own title and messages.
    pub ntypes: usize,
}

impl TableTypes {
    /// `describe.c:4013`-`:4032`: each letter anywhere in `tabtypes`, and
    /// none meaning every type but indexes.
    #[must_use]
    pub fn parse(tabtypes: &str) -> Self {
        let has = |c| tabtypes.contains(c);
        let mut t = Self {
            tables: has('t'),
            indexes: has('i'),
            views: has('v'),
            mat_views: has('m'),
            sequences: has('s'),
            foreign: has('E'),
            ntypes: 0,
        };
        t.ntypes = [
            t.tables,
            t.indexes,
            t.views,
            t.mat_views,
            t.sequences,
            t.foreign,
        ]
        .into_iter()
        .filter(|&b| b)
        .count();
        if t.ntypes == 0 {
            t.tables = true;
            t.views = true;
            t.mat_views = true;
            t.sequences = true;
            t.foreign = true;
        }
        t
    }

    /// The printed title (`describe.c:4230`-`:4238`).
    #[must_use]
    pub fn title(self) -> &'static str {
        if self.ntypes != 1 {
            "List of relations"
        } else if self.tables {
            "List of tables"
        } else if self.indexes {
            "List of indexes"
        } else if self.views {
            "List of views"
        } else if self.mat_views {
            "List of materialized views"
        } else if self.sequences {
            "List of sequences"
        } else {
            "List of foreign tables"
        }
    }

    /// The error `listTables()` logs for an empty result when not quiet
    /// (`describe.c:4179`-`:4226`).
    #[must_use]
    pub fn not_found(self, pattern: Option<&str>) -> String {
        let what = if self.ntypes != 1 {
            "relations"
        } else if self.tables {
            "tables"
        } else if self.indexes {
            "indexes"
        } else if self.views {
            "views"
        } else if self.mat_views {
            "materialized views"
        } else if self.sequences {
            "sequences"
        } else {
            "foreign tables"
        };
        match pattern {
            Some(pattern) => format!("Did not find any {what} named \"{pattern}\"."),
            None => format!("Did not find any {what}."),
        }
    }
}

/// The settings `listTables()` reads besides its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerContext<'a> {
    /// `pset.sversion`
    pub sversion: i32,
    /// `pset.hide_tableam`
    pub hide_tableam: bool,
    /// `PQdb(pset.db)`
    pub db: Option<&'a str>,
}

/// The query half of `listTables()` (`describe.c:4011`-`:4167`).
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn list_tables_query(
    types: TableTypes,
    pattern: Option<&str>,
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  \
         CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' \
         WHEN 'm' THEN 'materialized view' WHEN 'i' THEN 'index' \
         WHEN 'S' THEN 'sequence' WHEN 't' THEN 'TOAST table' \
         WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned table' \
         WHEN 'I' THEN 'partitioned index' END as \"Type\",\n  \
         pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"",
    );
    if types.indexes {
        buf.push_str(",\n  c2.relname as \"Table\"");
    }
    // Access methods exist for tables, materialized views and indexes, the
    // first since PostgreSQL 12.
    let show_am = server.sversion >= 120_000
        && !server.hide_tableam
        && (types.tables || types.mat_views || types.indexes);
    if verbose {
        // Whether a relation is permanent, temporary, or unlogged.
        buf.push_str(
            ",\n  CASE c.relpersistence WHEN 'p' THEN 'permanent' \
             WHEN 't' THEN 'temporary' WHEN 'u' THEN 'unlogged' END as \"Persistence\"",
        );
        if show_am {
            buf.push_str(",\n  am.amname as \"Access method\"");
        }
        buf.push_str(
            ",\n  pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\"\
             ,\n  pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"",
        );
    }

    buf.push_str(
        "\nFROM pg_catalog.pg_class c\
         \n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace",
    );
    if show_am {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_am am ON am.oid = c.relam");
    }
    if types.indexes {
        buf.push_str(
            "\n     LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\
             \n     LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid",
        );
    }

    let system_or_pattern = show_system || pattern.is_some();
    buf.push_str("\nWHERE c.relkind IN (");
    if types.tables {
        buf.push_str("'r','p',");
        // With 'S' or a pattern, allow 't' to match TOAST tables too.
        if system_or_pattern {
            buf.push_str("'t',");
        }
    }
    if types.views {
        buf.push_str("'v',");
    }
    if types.mat_views {
        buf.push_str("'m',");
    }
    if types.indexes {
        buf.push_str("'i','I',");
    }
    if types.sequences {
        buf.push_str("'S',");
    }
    if system_or_pattern {
        // Was RELKIND_SPECIAL.
        buf.push_str("'s',");
    }
    if types.foreign {
        buf.push_str("'f',");
    }
    buf.push_str("'')\n");

    if !system_or_pattern {
        buf.push_str(
            "      AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname !~ '^pg_toast'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }

    validate_sql_name_pattern(
        &mut buf,
        pattern,
        true,
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

    buf.push_str("ORDER BY 1,2;");
    Ok(buf)
}

/// `formatPGVersionNumber()` (`string_utils.c:313`): a `server_version_num`
/// as the release it names, `18` or `18.6` from 10 on, `9.6` or `9.6.24`
/// before.
#[must_use]
pub fn format_pg_version_number(version: i32, include_minor: bool) -> String {
    match (version >= 100_000, include_minor) {
        (true, true) => format!("{}.{}", version / 10000, version % 10000),
        (true, false) => format!("{}", version / 10000),
        (false, true) => format!(
            "{}.{}.{}",
            version / 10000,
            (version / 100) % 100,
            version % 100
        ),
        (false, false) => format!("{}.{}", version / 10000, (version / 100) % 100),
    }
}

/// What a `\d` command's query builder refuses with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `validateSQLNamePattern` failed: the command fails.
    Pattern(PatternError),
    /// The server predates the feature. Upstream logs the message and still
    /// reports success (`describe.c:155`-`:163`, `:4282`-`:4290`).
    ServerTooOld(String),
    /// The command's letters are not all ones it takes. Upstream logs the
    /// message and still reports success (`describe.c:314`-`:318`).
    InvalidOptions(String),
}

impl From<PatternError> for Refusal {
    fn from(err: PatternError) -> Self {
        Self::Pattern(err)
    }
}

/// The query half of `describeAccessMethods()` (`describe.c:148`-`:199`),
/// whose title is "List of access methods".
///
/// # Errors
/// A server before 9.6, or a pattern with a dot.
pub fn describe_access_methods_query(
    pattern: Option<&str>,
    verbose: bool,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    if server.sversion < 90_600 {
        return Err(Refusal::ServerTooOld(format!(
            "The server (version {}) does not support access methods.",
            format_pg_version_number(server.sversion, false)
        )));
    }
    let mut buf = String::from(
        "SELECT amname AS \"Name\",\n  \
         CASE amtype WHEN 'i' THEN 'Index' WHEN 't' THEN 'Table' END AS \"Type\"",
    );
    if verbose {
        buf.push_str(concat!(
            ",\n  amhandler AS \"Handler\",\n",
            "  pg_catalog.obj_description(oid, 'pg_am') AS \"Description\"",
        ));
    }
    buf.push_str("\nFROM pg_catalog.pg_am\n");
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("amname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

impl OperatorListing {
    /// The printed title.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Classes => "List of operator classes",
            Self::Families => "List of operator families",
            Self::Operators => "List of operators of operator families",
            Self::Functions => "List of support functions of operator families",
        }
    }

    /// The listing's target list and joins, which only `+` changes.
    // Four upstream query texts, one per arm, kept whole so each reads
    // against its C.
    #[allow(clippy::too_many_lines)]
    fn select_from(self, verbose: bool) -> String {
        let mut buf = String::from("SELECT\n  am.amname AS \"AM\",\n");
        match self {
            Self::Classes => {
                buf.push_str(concat!(
                    "  pg_catalog.format_type(c.opcintype, NULL) AS \"Input type\",\n",
                    "  CASE\n",
                    "    WHEN c.opckeytype <> 0 AND c.opckeytype <> c.opcintype\n",
                    "    THEN pg_catalog.format_type(c.opckeytype, NULL)\n",
                    "    ELSE NULL\n",
                    "  END AS \"Storage type\",\n",
                    "  CASE\n",
                    "    WHEN pg_catalog.pg_opclass_is_visible(c.oid)\n",
                    "    THEN pg_catalog.format('%I', c.opcname)\n",
                    "    ELSE pg_catalog.format('%I.%I', n.nspname, c.opcname)\n",
                    "  END AS \"Operator class\",\n",
                    "  (CASE WHEN c.opcdefault\n",
                    "    THEN 'yes'\n",
                    "    ELSE 'no'\n",
                    "  END) AS \"Default?\"",
                ));
                if verbose {
                    buf.push_str(concat!(
                        ",\n  CASE\n",
                        "    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n",
                        "    THEN pg_catalog.format('%I', of.opfname)\n",
                        "    ELSE pg_catalog.format('%I.%I', ofn.nspname, of.opfname)\n",
                        "  END AS \"Operator family\",\n",
                        " pg_catalog.pg_get_userbyid(c.opcowner) AS \"Owner\"\n",
                    ));
                }
                buf.push_str(concat!(
                    "\nFROM pg_catalog.pg_opclass c\n",
                    "  LEFT JOIN pg_catalog.pg_am am on am.oid = c.opcmethod\n",
                    "  LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.opcnamespace\n",
                    "  LEFT JOIN pg_catalog.pg_type t ON t.oid = c.opcintype\n",
                    "  LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n",
                ));
                if verbose {
                    buf.push_str(concat!(
                        "  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = c.opcfamily\n",
                        "  LEFT JOIN pg_catalog.pg_namespace ofn ON ofn.oid = of.opfnamespace\n",
                    ));
                }
            }
            Self::Families => {
                buf.push_str(concat!(
                    "  CASE\n",
                    "    WHEN pg_catalog.pg_opfamily_is_visible(f.oid)\n",
                    "    THEN pg_catalog.format('%I', f.opfname)\n",
                    "    ELSE pg_catalog.format('%I.%I', n.nspname, f.opfname)\n",
                    "  END AS \"Operator family\",\n",
                    "  (SELECT\n",
                    "     pg_catalog.string_agg(pg_catalog.format_type(oc.opcintype, NULL), ', ')\n",
                    "   FROM pg_catalog.pg_opclass oc\n",
                    "   WHERE oc.opcfamily = f.oid) \"Applicable types\"",
                ));
                if verbose {
                    buf.push_str(",\n  pg_catalog.pg_get_userbyid(f.opfowner) AS \"Owner\"\n");
                }
                buf.push_str(concat!(
                    "\nFROM pg_catalog.pg_opfamily f\n",
                    "  LEFT JOIN pg_catalog.pg_am am on am.oid = f.opfmethod\n",
                    "  LEFT JOIN pg_catalog.pg_namespace n ON n.oid = f.opfnamespace\n",
                ));
            }
            Self::Operators => {
                buf.push_str(concat!(
                    "  CASE\n",
                    "    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n",
                    "    THEN pg_catalog.format('%I', of.opfname)\n",
                    "    ELSE pg_catalog.format('%I.%I', nsf.nspname, of.opfname)\n",
                    "  END AS \"Operator family\",\n",
                    // Upstream's comma leads the next line here.
                    "  o.amopopr::pg_catalog.regoperator AS \"Operator\"\n,",
                    "  o.amopstrategy AS \"Strategy\",\n",
                    "  CASE o.amoppurpose\n",
                    "    WHEN 'o' THEN 'ordering'\n",
                    "    WHEN 's' THEN 'search'\n",
                    "  END AS \"Purpose\"\n",
                ));
                if verbose {
                    buf.push_str(concat!(
                        ", ofs.opfname AS \"Sort opfamily\",\n",
                        "  CASE\n",
                        "    WHEN p.proleakproof THEN 'yes'\n",
                        "    ELSE 'no'\n",
                        "  END AS \"Leakproof?\"\n",
                    ));
                }
                buf.push_str(concat!(
                    "FROM pg_catalog.pg_amop o\n",
                    "  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = o.amopfamily\n",
                    "  LEFT JOIN pg_catalog.pg_am am ON am.oid = of.opfmethod AND am.oid = o.amopmethod\n",
                    "  LEFT JOIN pg_catalog.pg_namespace nsf ON of.opfnamespace = nsf.oid\n",
                ));
                if verbose {
                    buf.push_str(concat!(
                        "  LEFT JOIN pg_catalog.pg_opfamily ofs ON ofs.oid = o.amopsortfamily\n",
                        "  LEFT JOIN pg_catalog.pg_operator op ON op.oid = o.amopopr\n",
                        "  LEFT JOIN pg_catalog.pg_proc p ON p.oid = op.oprcode\n",
                    ));
                }
            }
            Self::Functions => {
                buf.push_str(concat!(
                    "  CASE\n",
                    "    WHEN pg_catalog.pg_opfamily_is_visible(of.oid)\n",
                    "    THEN pg_catalog.format('%I', of.opfname)\n",
                    "    ELSE pg_catalog.format('%I.%I', ns.nspname, of.opfname)\n",
                    "  END AS \"Operator family\",\n",
                    "  pg_catalog.format_type(ap.amproclefttype, NULL) AS \"Registered left type\",\n",
                    "  pg_catalog.format_type(ap.amprocrighttype, NULL) AS \"Registered right type\",\n",
                    "  ap.amprocnum AS \"Number\"\n",
                ));
                buf.push_str(if verbose {
                    ", ap.amproc::pg_catalog.regprocedure AS \"Function\"\n"
                } else {
                    ", p.proname AS \"Function\"\n"
                });
                buf.push_str(concat!(
                    "FROM pg_catalog.pg_amproc ap\n",
                    "  LEFT JOIN pg_catalog.pg_opfamily of ON of.oid = ap.amprocfamily\n",
                    "  LEFT JOIN pg_catalog.pg_am am ON am.oid = of.opfmethod\n",
                    "  LEFT JOIN pg_catalog.pg_namespace ns ON of.opfnamespace = ns.oid\n",
                    "  LEFT JOIN pg_catalog.pg_proc p ON ap.amproc = p.oid\n",
                ));
            }
        }
        buf
    }

    /// The listing's query: `describe.c:6909`-`:6971` for `\dAc`,
    /// `:7010`-`:7059` for `\dAf`, `:7100`-`:7165` for `\dAo` and
    /// `:7206`-`:7257` for `\dAp`.
    ///
    /// # Errors
    /// The access-method pattern has a dot, or the second pattern failed
    /// `validateSQLNamePattern`.
    pub fn query(
        self,
        access_method_pattern: Option<&str>,
        second_pattern: Option<&str>,
        verbose: bool,
        server: ServerContext<'_>,
    ) -> Result<String, PatternError> {
        let mut buf = self.select_from(verbose);

        let mut have_where = false;
        if access_method_pattern.is_some() {
            have_where = validate_sql_name_pattern(
                &mut buf,
                access_method_pattern,
                false,
                false,
                PatternVars {
                    namevar: Some("am.amname"),
                    ..PatternVars::default()
                },
                1,
                server.sversion,
                server.db,
            )?;
        }
        if second_pattern.is_some() {
            // A type is matched by its internal or its external name; an
            // operator family by its name.
            let (schemavar, namevar, altnamevar, visibilityrule) = match self {
                Self::Classes | Self::Families => (
                    "tn.nspname",
                    "t.typname",
                    Some("pg_catalog.format_type(t.oid, NULL)"),
                    Some("pg_catalog.pg_type_is_visible(t.oid)"),
                ),
                Self::Operators => ("nsf.nspname", "of.opfname", None, None),
                Self::Functions => ("ns.nspname", "of.opfname", None, None),
            };
            if self == Self::Families {
                let _ = write!(
                    buf,
                    concat!(
                        "  {} EXISTS (\n",
                        "    SELECT 1\n",
                        "    FROM pg_catalog.pg_type t\n",
                        "    JOIN pg_catalog.pg_opclass oc ON oc.opcintype = t.oid\n",
                        "    LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n",
                        "    WHERE oc.opcfamily = f.oid\n",
                    ),
                    if have_where { "AND" } else { "WHERE" }
                );
                have_where = true;
            }
            validate_sql_name_pattern(
                &mut buf,
                second_pattern,
                have_where,
                false,
                PatternVars {
                    schemavar: Some(schemavar),
                    namevar: Some(namevar),
                    altnamevar,
                    visibilityrule,
                },
                3,
                server.sversion,
                server.db,
            )?;
            if self == Self::Families {
                buf.push_str("  )\n");
            }
        }

        buf.push_str(match self {
            Self::Classes => "ORDER BY 1, 2, 4;",
            Self::Families => "ORDER BY 1, 2;",
            Self::Operators => concat!(
                "ORDER BY 1, 2,\n",
                "  o.amoplefttype = o.amoprighttype DESC,\n",
                "  pg_catalog.format_type(o.amoplefttype, NULL),\n",
                "  pg_catalog.format_type(o.amoprighttype, NULL),\n",
                "  o.amopstrategy;",
            ),
            Self::Functions => concat!(
                "ORDER BY 1, 2,\n",
                "  ap.amproclefttype = ap.amprocrighttype DESC,\n",
                "  3, 4, 5;",
            ),
        });
        Ok(buf)
    }
}

/// The relation kinds `listPartitionedTables()` was asked for
/// (`describe.c:4268`-`:4270`, `:4293`-`:4294`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionTypes {
    /// `t`: partitioned tables.
    pub tables: bool,
    /// `i`: partitioned indexes.
    pub indexes: bool,
    /// `n`: non-root partitioned relations too, with their parents.
    pub nested: bool,
}

impl PartitionTypes {
    /// Each letter anywhere in `reltypes`, and neither `t` nor `i` meaning
    /// both.
    #[must_use]
    pub fn parse(reltypes: &str) -> Self {
        let (tables, indexes) = (reltypes.contains('t'), reltypes.contains('i'));
        Self {
            tables: tables || !indexes,
            indexes: indexes || !tables,
            nested: reltypes.contains('n'),
        }
    }

    /// Both kinds, which adds a "Type" column (`mixed_output`).
    #[must_use]
    pub fn mixed(self) -> bool {
        self.tables && self.indexes
    }

    /// The printed title (`describe.c:4296`-`:4305`).
    #[must_use]
    pub fn title(self) -> &'static str {
        if self.mixed() {
            "List of partitioned relations"
        } else if self.indexes {
            "List of partitioned indexes"
        } else {
            "List of partitioned tables"
        }
    }
}

/// The `LATERAL` subquery `listPartitionedTables()` sums partition sizes
/// with under `+` (`describe.c:4388`-`:4417`).
fn partition_sizes(sversion: i32) -> &'static str {
    if sversion < 120_000 {
        concat!(
            ",\n     LATERAL (WITH RECURSIVE d\n",
            "                AS (SELECT inhrelid AS oid, 1 AS level\n",
            "                      FROM pg_catalog.pg_inherits\n",
            "                     WHERE inhparent = c.oid\n",
            "                    UNION ALL\n",
            "                    SELECT inhrelid, level + 1\n",
            "                      FROM pg_catalog.pg_inherits i\n",
            "                           JOIN d ON i.inhparent = d.oid)\n",
            "                SELECT pg_catalog.pg_size_pretty(sum(pg_catalog.pg_table_size(",
            "d.oid))) AS tps,\n",
            "                       pg_catalog.pg_size_pretty(sum(",
            "\n             CASE WHEN d.level = 1",
            " THEN pg_catalog.pg_table_size(d.oid) ELSE 0 END)) AS dps\n",
            "               FROM d) s",
        )
    } else {
        // PostgreSQL 12 has pg_partition_tree.
        concat!(
            ",\n     LATERAL (SELECT pg_catalog.pg_size_pretty(sum(",
            "\n                 CASE WHEN ppt.isleaf AND ppt.level = 1",
            "\n                      THEN pg_catalog.pg_table_size(ppt.relid)",
            " ELSE 0 END)) AS dps",
            ",\n                     pg_catalog.pg_size_pretty(sum(",
            "pg_catalog.pg_table_size(ppt.relid))) AS tps",
            "\n              FROM pg_catalog.pg_partition_tree(c.oid) ppt) s",
        )
    }
}

/// The query half of `listPartitionedTables()` (`describe.c:4266`-`:4447`).
///
/// # Errors
/// A server before 10, or a pattern that failed `validateSQLNamePattern`.
pub fn list_partitioned_tables_query(
    types: PartitionTypes,
    pattern: Option<&str>,
    verbose: bool,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    if server.sversion < 100_000 {
        return Err(Refusal::ServerTooOld(format!(
            "The server (version {}) does not support declarative table partitioning.",
            format_pg_version_number(server.sversion, false)
        )));
    }
    // With a pattern a partition can match, so its parent is shown too.
    let with_parent = types.nested || pattern.is_some();

    let mut buf = String::from(
        "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  \
         pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"",
    );
    if types.mixed() {
        buf.push_str(
            ",\n  CASE c.relkind WHEN 'p' THEN 'partitioned table' \
             WHEN 'I' THEN 'partitioned index' END as \"Type\"",
        );
    }
    if with_parent {
        buf.push_str(",\n  inh.inhparent::pg_catalog.regclass as \"Parent name\"");
    }
    if types.indexes {
        // One space of indent, as upstream has it.
        buf.push_str(",\n c2.oid::pg_catalog.regclass as \"Table\"");
    }
    if verbose {
        buf.push_str(",\n  am.amname as \"Access method\"");
        if types.nested {
            buf.push_str(",\n  s.dps as \"Leaf partition size\"");
        }
        // Without `n`, the sizes of all partitions are summed.
        buf.push_str(
            ",\n  s.tps as \"Total size\"\
             ,\n  pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"",
        );
    }

    buf.push_str(
        "\nFROM pg_catalog.pg_class c\
         \n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace",
    );
    if types.indexes {
        buf.push_str(
            "\n     LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\
             \n     LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid",
        );
    }
    if with_parent {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_inherits inh ON c.oid = inh.inhrelid");
    }
    if verbose {
        buf.push_str("\n     LEFT JOIN pg_catalog.pg_am am ON c.relam = am.oid");
        buf.push_str(partition_sizes(server.sversion));
    }

    buf.push_str("\nWHERE c.relkind IN (");
    if types.tables {
        buf.push_str("'p',");
    }
    if types.indexes {
        buf.push_str("'I',");
    }
    buf.push_str("'')\n");
    if !with_parent {
        buf.push_str(" AND NOT c.relispartition\n");
    }
    if pattern.is_none() {
        buf.push_str(
            "      AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname !~ '^pg_toast'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }

    validate_sql_name_pattern(
        &mut buf,
        pattern,
        true,
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

    let _ = write!(
        buf,
        "ORDER BY \"Schema\", {}{}\"Name\";",
        if types.mixed() { "\"Type\" DESC, " } else { "" },
        if with_parent {
            "\"Parent name\" NULLS FIRST, "
        } else {
            ""
        }
    );
    Ok(buf)
}

/// `printACLColumn()` (`describe.c:6880`): an ACL column as one grant per
/// line, `(none)` for an empty one.
fn push_acl_column(buf: &mut String, colname: &str) {
    let _ = write!(
        buf,
        "CASE WHEN pg_catalog.array_length({colname}, 1) = 0 THEN '(none)' \
         ELSE pg_catalog.array_to_string({colname}, E'\\n') END AS \"Access privileges\""
    );
}

/// The query half of `describeAggregates()` (`describe.c:78`-`:127`), whose
/// title is "List of aggregate functions".
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn describe_aggregates_query(
    pattern: Option<&str>,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "  p.proname AS \"Name\",\n",
        "  pg_catalog.format_type(p.prorettype, NULL) AS \"Result data type\",\n",
        "  CASE WHEN p.pronargs = 0\n",
        "    THEN CAST('*' AS pg_catalog.text)\n",
        "    ELSE pg_catalog.pg_get_function_arguments(p.oid)\n",
        "  END AS \"Argument data types\",\n",
        "  pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"\n",
        "FROM pg_catalog.pg_proc p\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\n",
    ));
    // `prokind` replaced `proisagg` in PostgreSQL 11.
    buf.push_str(if server.sversion >= 110_000 {
        "WHERE p.prokind = 'a'\n"
    } else {
        "WHERE p.proisagg\n"
    });
    if !show_system && pattern.is_none() {
        buf.push_str(
            "      AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        true,
        false,
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("p.proname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_function_is_visible(p.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2, 4;");
    Ok(buf)
}

/// The `\df` letters `describeFunctions()` accepts after `\df`
/// (`describe.c:299`).
pub const DF_OPTIONS: &str = "anptwSx+";

/// `FUNC_MAX_ARGS` (`pg_config_manual.h:43`): how many argument-type
/// patterns `exec_command_dfo()` reads after a `\df` or `\do` pattern
/// (`command.c:1313`-`:1325`).
pub const FUNC_MAX_ARGS: usize = 100;

/// The function kinds `describeFunctions()` was asked for
/// (`describe.c:300`-`:304`, `:331`-`:336`).
// One flag per letter of `functypes`, as upstream has them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionTypes {
    /// `a`
    pub aggregate: bool,
    /// `n`
    pub normal: bool,
    /// `p`
    pub procedure: bool,
    /// `t`
    pub trigger: bool,
    /// `w`
    pub window: bool,
}

impl FunctionTypes {
    /// Each letter anywhere in `functypes`, and none meaning all of them —
    /// procedures only from PostgreSQL 11, which has them.
    ///
    /// # Errors
    /// The messages `describeFunctions()` logs before it returns `true`
    /// (`describe.c:314`-`:329`): a letter outside [`DF_OPTIONS`], or `p`
    /// for a server before 11.
    pub fn parse(functypes: &str, sversion: i32) -> Result<Self, Refusal> {
        if !functypes.chars().all(|c| DF_OPTIONS.contains(c)) {
            return Err(Refusal::InvalidOptions(format!(
                "\\df only takes [{DF_OPTIONS}] as options"
            )));
        }
        let has = |c| functypes.contains(c);
        let mut t = Self {
            aggregate: has('a'),
            normal: has('n'),
            procedure: has('p'),
            trigger: has('t'),
            window: has('w'),
        };
        if t.procedure && sversion < 110_000 {
            return Err(Refusal::ServerTooOld(format!(
                "\\df does not take a \"p\" option with server version {}",
                format_pg_version_number(sversion, false)
            )));
        }
        if !(t.aggregate || t.normal || t.procedure || t.trigger || t.window) {
            t = Self {
                aggregate: true,
                normal: true,
                procedure: sversion >= 110_000,
                trigger: true,
                window: true,
            };
        }
        Ok(t)
    }

    /// Every kind: no filter at all.
    fn all(self) -> bool {
        self.aggregate && self.normal && self.procedure && self.trigger && self.window
    }
}

/// The `WHERE` / `AND` clauses that match the argument-type patterns of
/// `\df` and `\do` (`describe.c:564`-`:596`, `:883`-`:915`): each against
/// the type's internal or external name, as `\dT` does, or `-` for "no such
/// argument".
fn push_arg_type_patterns(
    buf: &mut String,
    arg_patterns: &[&str],
    server: ServerContext<'_>,
) -> Result<(), PatternError> {
    for (i, &arg) in arg_patterns.iter().enumerate() {
        if arg == "-" {
            let _ = writeln!(buf, "  AND t{i}.typname IS NULL");
            continue;
        }
        let nspname = format!("nt{i}.nspname");
        let typname = format!("t{i}.typname");
        let ft = format!("pg_catalog.format_type(t{i}.oid, NULL)");
        let tiv = format!("pg_catalog.pg_type_is_visible(t{i}.oid)");
        validate_sql_name_pattern(
            buf,
            map_typename_pattern(Some(arg)),
            true,
            false,
            PatternVars {
                schemavar: Some(&nspname),
                namevar: Some(&typname),
                altnamevar: Some(&ft),
                visibilityrule: Some(&tiv),
            },
            3,
            server.sversion,
            server.db,
        )?;
    }
    Ok(())
}

/// Append `clause` after `WHERE` or `AND`, as each of `describeFunctions()`'s
/// kind filters does (`describe.c:459`-`:512`).
fn push_where_and(buf: &mut String, have_where: &mut bool, clause: &str) {
    buf.push_str(if *have_where { "      AND " } else { "WHERE " });
    *have_where = true;
    buf.push_str(clause);
}

/// The query half of `describeFunctions()` (`describe.c:295`-`:602`), whose
/// title is "List of functions".
///
/// # Errors
/// [`FunctionTypes::parse`]'s refusals, or a pattern that failed
/// `validateSQLNamePattern`.
// One upstream function, kept in its order so it reads against its C.
#[allow(clippy::too_many_lines)]
pub fn describe_functions_query(
    functypes: &str,
    pattern: Option<&str>,
    arg_patterns: &[&str],
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    let types = FunctionTypes::parse(functypes, server.sversion)?;
    let v11 = server.sversion >= 110_000;

    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "  p.proname as \"Name\",\n",
        "  pg_catalog.pg_get_function_result(p.oid) as \"Result data type\",\n",
        "  pg_catalog.pg_get_function_arguments(p.oid) as \"Argument data types\",\n",
    ));
    buf.push_str(if v11 {
        concat!(
            " CASE p.prokind\n",
            "  WHEN 'a' THEN 'agg'\n",
            "  WHEN 'w' THEN 'window'\n",
            "  WHEN 'p' THEN 'proc'\n",
            "  ELSE 'func'\n",
            " END as \"Type\"",
        )
    } else {
        concat!(
            " CASE\n",
            "  WHEN p.proisagg THEN 'agg'\n",
            "  WHEN p.proiswindow THEN 'window'\n",
            "  WHEN p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype THEN 'trigger'\n",
            "  ELSE 'func'\n",
            " END as \"Type\"",
        )
    });
    if verbose {
        buf.push_str(concat!(
            ",\n CASE\n",
            "  WHEN p.provolatile = 'i' THEN 'immutable'\n",
            "  WHEN p.provolatile = 's' THEN 'stable'\n",
            "  WHEN p.provolatile = 'v' THEN 'volatile'\n",
            " END as \"Volatility\"",
        ));
        // No "Parallel" column before 9.6.
        if server.sversion >= 90_600 {
            buf.push_str(concat!(
                ",\n CASE\n",
                "  WHEN p.proparallel = 'r' THEN 'restricted'\n",
                "  WHEN p.proparallel = 's' THEN 'safe'\n",
                "  WHEN p.proparallel = 'u' THEN 'unsafe'\n",
                " END as \"Parallel\"",
            ));
        }
        buf.push_str(concat!(
            ",\n pg_catalog.pg_get_userbyid(p.proowner) as \"Owner\"",
            ",\n CASE WHEN prosecdef THEN 'definer' ELSE 'invoker' END AS \"Security\"",
            ",\n CASE WHEN p.proleakproof THEN 'yes' ELSE 'no' END as \"Leakproof?\"",
            ",\n ",
        ));
        push_acl_column(&mut buf, "p.proacl");
        buf.push_str(concat!(
            ",\n l.lanname as \"Language\"",
            ",\n CASE WHEN l.lanname IN ('internal', 'c') THEN p.prosrc END as \"Internal name\"",
            ",\n pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"",
        ));
    }

    buf.push_str(
        "\nFROM pg_catalog.pg_proc p\
         \n     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\n",
    );
    for i in 0..arg_patterns.len() {
        let _ = writeln!(
            buf,
            "     LEFT JOIN pg_catalog.pg_type t{i} ON t{i}.oid = p.proargtypes[{i}]"
        );
        let _ = writeln!(
            buf,
            "     LEFT JOIN pg_catalog.pg_namespace nt{i} ON nt{i}.oid = t{i}.typnamespace"
        );
    }
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_language l ON l.oid = p.prolang\n");
    }

    // Filter by function type, if requested.
    let mut have_where = false;
    if types.all() {
        // Do nothing.
    } else if types.normal {
        if !types.aggregate {
            push_where_and(
                &mut buf,
                &mut have_where,
                if v11 {
                    "p.prokind <> 'a'\n"
                } else {
                    "NOT p.proisagg\n"
                },
            );
        }
        if !types.procedure && v11 {
            push_where_and(&mut buf, &mut have_where, "p.prokind <> 'p'\n");
        }
        if !types.trigger {
            push_where_and(
                &mut buf,
                &mut have_where,
                "p.prorettype <> 'pg_catalog.trigger'::pg_catalog.regtype\n",
            );
        }
        if !types.window {
            push_where_and(
                &mut buf,
                &mut have_where,
                if v11 {
                    "p.prokind <> 'w'\n"
                } else {
                    "NOT p.proiswindow\n"
                },
            );
        }
    } else {
        // At least one of these is true.
        buf.push_str("WHERE (\n       ");
        have_where = true;
        let mut needs_or = false;
        let mut or = |buf: &mut String, clause: &str| {
            if needs_or {
                buf.push_str("       OR ");
            }
            buf.push_str(clause);
            needs_or = true;
        };
        if types.aggregate {
            or(
                &mut buf,
                if v11 {
                    "p.prokind = 'a'\n"
                } else {
                    "p.proisagg\n"
                },
            );
        }
        if types.trigger {
            or(
                &mut buf,
                "p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype\n",
            );
        }
        if types.procedure {
            or(&mut buf, "p.prokind = 'p'\n");
        }
        if types.window {
            or(
                &mut buf,
                if v11 {
                    "p.prokind = 'w'\n"
                } else {
                    "p.proiswindow\n"
                },
            );
        }
        buf.push_str("      )\n");
    }

    validate_sql_name_pattern(
        &mut buf,
        pattern,
        have_where,
        false,
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("p.proname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_function_is_visible(p.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    push_arg_type_patterns(&mut buf, arg_patterns, server)?;

    if !show_system && pattern.is_none() {
        buf.push_str(
            "      AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }
    buf.push_str("ORDER BY 1, 2, 4;");
    Ok(buf)
}

/// `map_typename_pattern()` (`describe.c:744`): a type name the grammar
/// accepts but neither `pg_type` nor `format_type()` uses, as the canonical
/// name, compared case-insensitively; any other pattern as it is.
#[must_use]
pub fn map_typename_pattern(pattern: Option<&str>) -> Option<&str> {
    const TYPENAME_MAP: [(&str, &str); 18] = [
        // Accepted by gram.y, though neither the "real" name seen in pg_type
        // nor the canonical name printed by format_type().
        ("decimal", "numeric"),
        ("float", "double precision"),
        ("int", "integer"),
        // Array names whose canonical name differs from what pg_type says.
        ("bool[]", "boolean[]"),
        ("decimal[]", "numeric[]"),
        ("float[]", "double precision[]"),
        ("float4[]", "real[]"),
        ("float8[]", "double precision[]"),
        ("int[]", "integer[]"),
        ("int2[]", "smallint[]"),
        ("int4[]", "integer[]"),
        ("int8[]", "bigint[]"),
        ("time[]", "time without time zone[]"),
        ("timetz[]", "time with time zone[]"),
        ("timestamp[]", "timestamp without time zone[]"),
        ("timestamptz[]", "timestamp with time zone[]"),
        ("varbit[]", "bit varying[]"),
        ("varchar[]", "character varying[]"),
    ];
    let pattern = pattern?;
    Some(
        TYPENAME_MAP
            .iter()
            .find(|(from, _)| pattern.eq_ignore_ascii_case(from))
            .map_or(pattern, |&(_, to)| to),
    )
}

/// The query half of `describeTypes()` (`describe.c:639`-`:718`), whose
/// title is "List of data types".
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn describe_types_query(
    pattern: Option<&str>,
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "  pg_catalog.format_type(t.oid, NULL) AS \"Name\",\n",
    ));
    if verbose {
        buf.push_str(concat!(
            "  t.typname AS \"Internal name\",\n",
            "  CASE WHEN t.typrelid != 0\n",
            "      THEN CAST('tuple' AS pg_catalog.text)\n",
            "    WHEN t.typlen < 0\n",
            "      THEN CAST('var' AS pg_catalog.text)\n",
            "    ELSE CAST(t.typlen AS pg_catalog.text)\n",
            "  END AS \"Size\",\n",
            "  pg_catalog.array_to_string(\n",
            "      ARRAY(\n",
            "          SELECT e.enumlabel\n",
            "          FROM pg_catalog.pg_enum e\n",
            "          WHERE e.enumtypid = t.oid\n",
            "          ORDER BY e.enumsortorder\n",
            "      ),\n",
            "      E'\\n'\n",
            "  ) AS \"Elements\",\n",
            "  pg_catalog.pg_get_userbyid(t.typowner) AS \"Owner\",\n",
        ));
        push_acl_column(&mut buf, "t.typacl");
        buf.push_str(",\n  ");
    }
    buf.push_str(concat!(
        "  pg_catalog.obj_description(t.oid, 'pg_type') as \"Description\"\n",
        "FROM pg_catalog.pg_type t\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace\n",
        // Complex types only when they are standalone composite types.
        "WHERE (t.typrelid = 0 ",
        "OR (SELECT c.relkind = 'c' FROM pg_catalog.pg_class c ",
        "WHERE c.oid = t.typrelid))\n",
    ));
    // Array types only when the pattern asks for them.
    if !pattern.is_some_and(|p| p.contains("[]")) {
        buf.push_str(
            "  AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_type el \
             WHERE el.oid = t.typelem AND el.typarray = t.oid)\n",
        );
    }
    if !show_system && pattern.is_none() {
        buf.push_str(
            "      AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }
    // Match the name pattern against either the internal or external name.
    validate_sql_name_pattern(
        &mut buf,
        map_typename_pattern(pattern),
        true,
        false,
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("t.typname"),
            altnamevar: Some("pg_catalog.format_type(t.oid, NULL)"),
            visibilityrule: Some("pg_catalog.pg_type_is_visible(t.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// The query half of `describeOperators()` (`describe.c:794`-`:917`), whose
/// title is "List of operators". One argument-type pattern matches the right
/// argument of a prefix operator; two match the left and right ones; any
/// more are read and ignored.
///
/// # Errors
/// A pattern failed `validateSQLNamePattern`.
pub fn describe_operators_query(
    pattern: Option<&str>,
    arg_patterns: &[&str],
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    // The support for postfix operators is dead code as of PostgreSQL 14,
    // kept for older servers; the coalesce() for third-party operators whose
    // comment is on their function.
    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "  o.oprname AS \"Name\",\n",
        "  CASE WHEN o.oprkind='l' THEN NULL ELSE pg_catalog.format_type(o.oprleft, NULL) END AS \"Left arg type\",\n",
        "  CASE WHEN o.oprkind='r' THEN NULL ELSE pg_catalog.format_type(o.oprright, NULL) END AS \"Right arg type\",\n",
        "  pg_catalog.format_type(o.oprresult, NULL) AS \"Result type\",\n",
    ));
    if verbose {
        buf.push_str(concat!(
            "  o.oprcode AS \"Function\",\n",
            "  CASE WHEN p.proleakproof THEN 'yes' ELSE 'no' END AS \"Leakproof?\",\n",
        ));
    }
    buf.push_str(concat!(
        "  coalesce(pg_catalog.obj_description(o.oid, 'pg_operator'),\n",
        "           pg_catalog.obj_description(o.oprcode, 'pg_proc')) AS \"Description\"\n",
        "FROM pg_catalog.pg_operator o\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = o.oprnamespace\n",
    ));

    let arg_patterns = &arg_patterns[..arg_patterns.len().min(2)];
    match arg_patterns.len() {
        2 => buf.push_str(concat!(
            "     LEFT JOIN pg_catalog.pg_type t0 ON t0.oid = o.oprleft\n",
            "     LEFT JOIN pg_catalog.pg_namespace nt0 ON nt0.oid = t0.typnamespace\n",
            "     LEFT JOIN pg_catalog.pg_type t1 ON t1.oid = o.oprright\n",
            "     LEFT JOIN pg_catalog.pg_namespace nt1 ON nt1.oid = t1.typnamespace\n",
        )),
        1 => buf.push_str(concat!(
            "     LEFT JOIN pg_catalog.pg_type t0 ON t0.oid = o.oprright\n",
            "     LEFT JOIN pg_catalog.pg_namespace nt0 ON nt0.oid = t0.typnamespace\n",
        )),
        _ => {}
    }
    if verbose {
        buf.push_str("     LEFT JOIN pg_catalog.pg_proc p ON p.oid = o.oprcode\n");
    }

    let not_system = !show_system && pattern.is_none();
    if not_system {
        buf.push_str(
            "WHERE n.nspname <> 'pg_catalog'\n      \
             AND n.nspname <> 'information_schema'\n",
        );
    }
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        not_system,
        true,
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("o.oprname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_operator_is_visible(o.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    if arg_patterns.len() == 1 {
        buf.push_str("  AND o.oprleft = 0\n");
    }
    push_arg_type_patterns(&mut buf, arg_patterns, server)?;
    buf.push_str("ORDER BY 1, 2, 3, 4;");
    Ok(buf)
}

/// The query half of `describeConfigurationParameters()`
/// (`describe.c:4715`-`:4758`), and its title (`:4765`-`:4768`). A pattern
/// is matched against the lower-cased name and never refused; without one,
/// only parameters set away from their defaults are listed.
#[must_use]
pub fn describe_configuration_parameters_query(
    pattern: Option<&str>,
    verbose: bool,
    server: ServerContext<'_>,
) -> (String, &'static str) {
    let mut buf = String::from(
        "SELECT s.name AS \"Parameter\", pg_catalog.current_setting(s.name) AS \"Value\"",
    );
    // pg_parameter_acl arrived in PostgreSQL 15.
    let v15 = server.sversion >= 150_000;
    if verbose {
        buf.push_str(", s.vartype AS \"Type\", s.context AS \"Context\", ");
        if v15 {
            push_acl_column(&mut buf, "p.paracl");
        } else {
            buf.push_str("NULL AS \"Access privileges\"");
        }
    }
    buf.push_str("\nFROM pg_catalog.pg_settings s\n");
    if verbose && v15 {
        buf.push_str(concat!(
            "  LEFT JOIN pg_catalog.pg_parameter_acl p\n",
            "  ON pg_catalog.lower(s.name) = p.parname\n",
        ));
    }
    if pattern.is_some() {
        process_sql_name_pattern(
            &mut buf,
            pattern,
            false,
            false,
            PatternVars {
                namevar: Some("pg_catalog.lower(s.name)"),
                ..PatternVars::default()
            },
            server.sversion,
        );
    } else {
        buf.push_str(concat!(
            "WHERE s.source <> 'default' AND\n",
            "      s.setting IS DISTINCT FROM s.boot_val\n",
        ));
    }
    buf.push_str("ORDER BY 1;");
    let title = if pattern.is_some() {
        "List of configuration parameters"
    } else {
        "List of non-default configuration parameters"
    };
    (buf, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PG18: ServerContext<'static> = ServerContext {
        sversion: 180_006,
        hide_tableam: false,
        db: Some("regression"),
    };

    fn regex(pattern: &str, parts: PatternParts) -> PatternRegex {
        pattern_to_sql_regex(pattern, parts, false, true)
    }

    #[test]
    fn a_bare_name_is_lower_cased_and_anchored() {
        let r = regex("Foo", PatternParts::SchemaName);
        assert_eq!(r.name, "^(foo)$");
        assert_eq!(r.schema, None);
        assert_eq!(r.dotcnt, 0);
    }

    #[test]
    fn shell_wildcards_become_regexp_ones() {
        assert_eq!(regex("f*o?", PatternParts::Name).name, "^(f.*o.)$");
        // `$` is always escaped; `[]` too, as an array type name's tail.
        assert_eq!(regex("a$b", PatternParts::Name).name, "^(a\\$b)$");
        assert_eq!(regex("int4[]", PatternParts::Name).name, "^(int4\\[])$");
        // Other regexp characters pass through outside quotes...
        assert_eq!(regex("a|b+", PatternParts::Name).name, "^(a|b+)$");
        // ...unless force_escape.
        assert_eq!(
            pattern_to_sql_regex("a|b+", PatternParts::Name, true, true).name,
            "^(a\\|b\\+)$"
        );
    }

    #[test]
    fn double_quotes_keep_case_and_escape_regexp_characters() {
        assert_eq!(regex("\"Foo*\"", PatternParts::Name).name, "^(Foo\\*)$");
        // A doubled quote inside quotes is one quote.
        assert_eq!(regex("\"a\"\"b\"", PatternParts::Name).name, "^(a\"b)$");
        // A quoted dot does not separate.
        let r = regex("\"a.b\"", PatternParts::SchemaName);
        assert_eq!((r.name.as_str(), r.dotcnt), ("^(a\\.b)$", 0));
    }

    #[test]
    fn dots_separate_as_many_parts_as_there_are_buffers() {
        let r = regex("s.t", PatternParts::SchemaName);
        assert_eq!(r.schema.as_deref(), Some("^(s)$"));
        assert_eq!(r.name, "^(t)$");
        let r = regex("d.s.t", PatternParts::DbSchemaName);
        assert_eq!(r.dbname.as_deref(), Some("d"));
        assert_eq!(r.schema.as_deref(), Some("^(s)$"));
        assert_eq!((r.name.as_str(), r.dotcnt), ("^(t)$", 2));
        // A dot past the last buffer is literal, but still counted.
        let r = regex("a.b.c.d", PatternParts::DbSchemaName);
        assert_eq!((r.name.as_str(), r.dotcnt), ("^(c.d)$", 3));
        let r = regex("a.b", PatternParts::Name);
        assert_eq!((r.name.as_str(), r.dotcnt), ("^(a.b)$", 1));
    }

    #[test]
    fn the_literal_database_part_keeps_its_wildcards_as_typed() {
        let r = regex("D*b?.s.t", PatternParts::DbSchemaName);
        assert_eq!(r.dbname.as_deref(), Some("d*b?"));
        let r = pattern_to_sql_regex("D*.s.t", PatternParts::DbSchemaName, false, false);
        assert_eq!(r.dbname.as_deref(), Some("^(d.*)$"));
    }

    #[test]
    fn a_multibyte_character_is_copied_whole() {
        assert_eq!(regex("Ä*é", PatternParts::Name).name, "^(Ä.*é)$");
    }

    #[test]
    fn a_literal_with_a_backslash_is_an_escape_string() {
        let mut buf = String::from("x ~");
        append_string_literal_conn(&mut buf, "a\\b'c");
        assert_eq!(buf, "x ~ E'a\\\\b''c'");
        let mut buf = String::from("x ~ ");
        append_string_literal_conn(&mut buf, "it's");
        assert_eq!(buf, "x ~ 'it''s'");
    }

    fn table_vars() -> PatternVars<'static> {
        PatternVars {
            schemavar: Some("n.nspname"),
            namevar: Some("c.relname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        }
    }

    #[test]
    fn no_pattern_restricts_to_visible_objects() {
        let mut buf = String::new();
        let clause = process_sql_name_pattern(&mut buf, None, true, false, table_vars(), 180_006);
        assert!(clause.added);
        assert_eq!(buf, "  AND pg_catalog.pg_table_is_visible(c.oid)\n");
    }

    #[test]
    fn a_name_pattern_is_matched_under_the_default_collation() {
        let mut buf = String::new();
        process_sql_name_pattern(&mut buf, Some("foo*"), false, false, table_vars(), 180_006);
        assert_eq!(
            buf,
            "WHERE c.relname OPERATOR(pg_catalog.~) '^(foo.*)$' COLLATE pg_catalog.default\n  \
             AND pg_catalog.pg_table_is_visible(c.oid)\n"
        );
        // Before 12 there is no COLLATE.
        let mut buf = String::new();
        process_sql_name_pattern(&mut buf, Some("foo"), true, false, table_vars(), 110_000);
        assert!(buf.starts_with("  AND c.relname OPERATOR(pg_catalog.~) '^(foo)$'\n"));
    }

    #[test]
    fn a_schema_pattern_replaces_the_visibility_rule() {
        let mut buf = String::new();
        process_sql_name_pattern(&mut buf, Some("s.t"), true, false, table_vars(), 180_006);
        assert_eq!(
            buf,
            "  AND c.relname OPERATOR(pg_catalog.~) '^(t)$' COLLATE pg_catalog.default\n  \
             AND n.nspname OPERATOR(pg_catalog.~) '^(s)$' COLLATE pg_catalog.default\n"
        );
        // `s.*`: the name part is optimized away, the schema part is not.
        let mut buf = String::new();
        process_sql_name_pattern(&mut buf, Some("s.*"), true, false, table_vars(), 180_006);
        assert_eq!(
            buf,
            "  AND n.nspname OPERATOR(pg_catalog.~) '^(s)$' COLLATE pg_catalog.default\n"
        );
        // `*.*`: both are optimized away, and nothing restricts visibility.
        let mut buf = String::new();
        let clause =
            process_sql_name_pattern(&mut buf, Some("*.*"), true, false, table_vars(), 180_006);
        assert_eq!((buf.as_str(), clause.added), ("", false));
    }

    #[test]
    fn an_alternative_name_column_is_ored_in() {
        let mut buf = String::new();
        let vars = PatternVars {
            altnamevar: Some("t.alt"),
            ..table_vars()
        };
        process_sql_name_pattern(&mut buf, Some("x"), false, false, vars, 180_006);
        assert!(buf.starts_with(
            "WHERE (c.relname OPERATOR(pg_catalog.~) '^(x)$' COLLATE pg_catalog.default\n        \
             OR t.alt OPERATOR(pg_catalog.~) '^(x)$' COLLATE pg_catalog.default)\n"
        ));
    }

    #[test]
    fn too_many_dots_and_another_database_are_refused() {
        let check = |pattern: &str, db: Option<&str>| {
            let mut buf = String::new();
            validate_sql_name_pattern(
                &mut buf,
                Some(pattern),
                true,
                false,
                table_vars(),
                3,
                180_006,
                db,
            )
        };
        assert_eq!(
            check("a.b.c.d", Some("a")),
            Err(PatternError(
                "improper qualified name (too many dotted names): a.b.c.d".to_string()
            ))
        );
        assert_eq!(
            check("other.s.t", Some("regression")),
            Err(PatternError(
                "cross-database references are not implemented: other.s.t".to_string()
            ))
        );
        assert_eq!(check("regression.s.t", Some("regression")), Ok(true));
        assert_eq!(
            check("regression.s.t", None),
            Err(PatternError(
                "You are currently not connected to a database.".to_string()
            ))
        );
    }

    #[test]
    fn the_d_switch_routes_listings_and_refuses_the_rest_by_name() {
        let p = DescribeCommand::parse;
        assert_eq!(
            p("d", false),
            Some(DescribeCommand::ListTables("tvmsE".into()))
        );
        assert_eq!(p("d+", true), Some(DescribeCommand::TableDetails));
        assert_eq!(p("dS", true), Some(DescribeCommand::TableDetails));
        assert_eq!(
            p("dt+", true),
            Some(DescribeCommand::ListTables("t+".into()))
        );
        assert_eq!(
            p("dE", false),
            Some(DescribeCommand::ListTables("E".into()))
        );
        assert_eq!(
            p("dconfig+", false),
            Some(DescribeCommand::ConfigurationParameters)
        );
        assert_eq!(p("daS", true), Some(DescribeCommand::Aggregates));
        assert_eq!(
            p("dfn+", true),
            Some(DescribeCommand::Functions("n+".into()))
        );
        assert_eq!(
            p("df", false),
            Some(DescribeCommand::Functions(String::new()))
        );
        // Only `cmd[2]` is checked here; `describeFunctions` refuses the rest.
        assert_eq!(
            p("dfnq", false),
            Some(DescribeCommand::Functions("nq".into()))
        );
        assert_eq!(p("dT+", true), Some(DescribeCommand::Types));
        assert_eq!(p("doS", true), Some(DescribeCommand::Operators));
        assert_eq!(
            p("dc", false),
            Some(DescribeCommand::NotYet("listConversions"))
        );
        assert_eq!(
            p("dx", false),
            Some(DescribeCommand::NotYet("listExtensions"))
        );
        assert_eq!(p("dA+", true), Some(DescribeCommand::AccessMethods));
        assert_eq!(p("dAx", false), Some(DescribeCommand::AccessMethods));
        assert_eq!(
            p("dApx+", true),
            Some(DescribeCommand::OperatorListing(OperatorListing::Functions))
        );
        assert_eq!(
            p("dAc", false),
            Some(DescribeCommand::OperatorListing(OperatorListing::Classes))
        );
        assert_eq!(
            p("dPtn+", false),
            Some(DescribeCommand::ListPartitionedTables("tn+".into()))
        );
        assert_eq!(
            p("dP", true),
            Some(DescribeCommand::ListPartitionedTables(String::new()))
        );
        assert_eq!(p("dPS", false), None);
        assert_eq!(p("dAz", false), None);
        assert_eq!(p("dfz", false), None);
        assert_eq!(p("drx", false), None);
        assert_eq!(p("dz", false), None);
    }

    #[test]
    fn only_a_da_listing_after_a_first_pattern_reads_a_second() {
        // `command.c:1065`: `cmd[2]` is not '\0', '+' or 'x', and there was a
        // first pattern.
        let n = DescribeCommand::patterns_read;
        assert_eq!(n("dAc", true), 2);
        assert_eq!(n("dAo+", true), 2);
        assert_eq!(n("dAc", false), 1);
        assert_eq!(n("dA", true), 1);
        assert_eq!(n("dA+", true), 1);
        assert_eq!(n("dAx", true), 1);
        assert_eq!(n("dt", true), 1);
        assert_eq!(n("dAz", true), 1);
    }

    #[test]
    fn df_and_do_read_up_to_func_max_args_argument_types_after_a_pattern() {
        // `command.c:1313`-`:1325`.
        let n = DescribeCommand::patterns_read;
        assert_eq!(n("df", true), 101);
        assert_eq!(n("dfa+", true), 101);
        assert_eq!(n("do", true), 101);
        assert_eq!(n("df", false), 1);
        assert_eq!(n("da", true), 1);
        assert_eq!(n("dT", true), 1);
    }

    #[test]
    fn the_da_query_is_upstreams_on_both_sides_of_11() {
        assert_eq!(
            describe_aggregates_query(None, false, PG18).unwrap(),
            "SELECT n.nspname as \"Schema\",\n  \
             p.proname AS \"Name\",\n  \
             pg_catalog.format_type(p.prorettype, NULL) AS \"Result data type\",\n  \
             CASE WHEN p.pronargs = 0\n    \
             THEN CAST('*' AS pg_catalog.text)\n    \
             ELSE pg_catalog.pg_get_function_arguments(p.oid)\n  \
             END AS \"Argument data types\",\n  \
             pg_catalog.obj_description(p.oid, 'pg_proc') as \"Description\"\n\
             FROM pg_catalog.pg_proc p\n     \
             LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\n\
             WHERE p.prokind = 'a'\n      \
             AND n.nspname <> 'pg_catalog'\n      \
             AND n.nspname <> 'information_schema'\n  \
             AND pg_catalog.pg_function_is_visible(p.oid)\n\
             ORDER BY 1, 2, 4;"
        );
        let v10 = ServerContext {
            sversion: 100_023,
            ..PG18
        };
        let q = describe_aggregates_query(Some("sum"), true, v10).unwrap();
        assert!(q.contains("WHERE p.proisagg\n  AND p.proname"), "{q}");
        assert!(!q.contains("COLLATE"), "{q}");
    }

    #[test]
    fn df_refuses_letters_it_does_not_take_and_p_before_11() {
        assert_eq!(
            FunctionTypes::parse("nq", 180_006),
            Err(Refusal::InvalidOptions(
                "\\df only takes [anptwSx+] as options".into()
            ))
        );
        assert_eq!(
            FunctionTypes::parse("p", 100_023),
            Err(Refusal::ServerTooOld(
                "\\df does not take a \"p\" option with server version 10".into()
            ))
        );
        // No kind letter is every kind, procedures only from 11.
        let all = FunctionTypes::parse("S+x", 180_006).unwrap();
        assert!(all.all());
        let old = FunctionTypes::parse("", 100_023).unwrap();
        assert!(!old.procedure && old.normal && old.aggregate);
    }

    #[test]
    fn df_kind_letters_become_upstreams_filters() {
        let q =
            |types| describe_functions_query(types, Some("f"), &[], false, false, PG18).unwrap();
        // `n` alone excludes the others, each on its own line.
        assert!(
            q("n").contains(
                "WHERE p.prokind <> 'a'\n      \
                 AND p.prokind <> 'p'\n      \
                 AND p.prorettype <> 'pg_catalog.trigger'::pg_catalog.regtype\n      \
                 AND p.prokind <> 'w'\n  \
                 AND p.proname"
            ),
            "{}",
            q("n")
        );
        // Without `n`, the kinds asked for are ORed, in upstream's order.
        assert!(
            q("wat").contains(
                "WHERE (\n       p.prokind = 'a'\n       \
                 OR p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype\n       \
                 OR p.prokind = 'w'\n      )\n  \
                 AND p.proname"
            ),
            "{}",
            q("wat")
        );
        // Every kind is no filter at all.
        assert!(q("anptw").contains(
            "LEFT JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace\nWHERE p.proname"
        ));
        // Before 11 there is no prokind, and no procedure filter.
        let v10 = ServerContext {
            sversion: 100_023,
            ..PG18
        };
        let old = describe_functions_query("n", None, &[], false, false, v10).unwrap();
        assert!(
            old.contains("WHERE NOT p.proisagg\n      AND p.prorettype <> "),
            "{old}"
        );
        assert!(
            old.contains("  WHEN p.proiswindow THEN 'window'\n"),
            "{old}"
        );
    }

    #[test]
    fn df_argument_patterns_join_a_type_each_and_dash_means_none() {
        let q = describe_functions_query("", Some("f"), &["int", "-"], false, false, PG18).unwrap();
        assert!(
            q.contains(
                "     LEFT JOIN pg_catalog.pg_type t1 ON t1.oid = p.proargtypes[1]\n     \
                 LEFT JOIN pg_catalog.pg_namespace nt1 ON nt1.oid = t1.typnamespace\n"
            ),
            "{q}"
        );
        // `int` is looked up as `integer`, by either name.
        assert!(
            q.contains(
                "  AND (t0.typname OPERATOR(pg_catalog.~) '^(integer)$' COLLATE pg_catalog.default\n        \
                 OR pg_catalog.format_type(t0.oid, NULL) OPERATOR(pg_catalog.~) '^(integer)$' COLLATE pg_catalog.default)\n  \
                 AND pg_catalog.pg_type_is_visible(t0.oid)\n  \
                 AND t1.typname IS NULL\n\
                 ORDER BY 1, 2, 4;"
            ),
            "{q}"
        );
        let refused = describe_functions_query("", Some("f"), &["a.b.c.d"], false, false, PG18);
        assert_eq!(
            refused,
            Err(Refusal::Pattern(PatternError(
                "improper qualified name (too many dotted names): a.b.c.d".into()
            )))
        );
    }

    #[test]
    fn df_plus_adds_upstreams_columns_and_the_language_join() {
        let q = describe_functions_query("", None, &[], true, false, PG18).unwrap();
        assert!(
            q.contains(
                ",\n CASE WHEN p.proleakproof THEN 'yes' ELSE 'no' END as \"Leakproof?\",\n \
                 CASE WHEN pg_catalog.array_length(p.proacl, 1) = 0 THEN '(none)' \
                 ELSE pg_catalog.array_to_string(p.proacl, E'\\n') END AS \"Access privileges\",\n \
                 l.lanname as \"Language\""
            ),
            "{q}"
        );
        assert!(
            q.contains("     LEFT JOIN pg_catalog.pg_language l ON l.oid = p.prolang\n"),
            "{q}"
        );
        // No "Parallel" column before 9.6.
        let v95 = ServerContext {
            sversion: 90_524,
            ..PG18
        };
        let old = describe_functions_query("", None, &[], true, false, v95).unwrap();
        assert!(!old.contains("Parallel"), "{old}");
        assert!(
            old.contains(" END as \"Volatility\",\n pg_catalog.pg_get_userbyid"),
            "{old}"
        );
    }

    #[test]
    fn typename_patterns_map_grammar_names_case_insensitively() {
        assert_eq!(map_typename_pattern(Some("DECIMAL")), Some("numeric"));
        assert_eq!(map_typename_pattern(Some("int4[]")), Some("integer[]"));
        assert_eq!(
            map_typename_pattern(Some("varchar[]")),
            Some("character varying[]")
        );
        assert_eq!(map_typename_pattern(Some("int4")), Some("int4"));
        assert_eq!(map_typename_pattern(Some("int*")), Some("int*"));
        assert_eq!(map_typename_pattern(None), None);
    }

    #[test]
    fn dt_hides_arrays_unless_the_pattern_asks_for_them() {
        let hides = "  AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_type el \
                     WHERE el.oid = t.typelem AND el.typarray = t.oid)\n";
        let q = describe_types_query(None, false, false, PG18).unwrap();
        assert!(
            q.contains(&format!(
                "WHERE (t.typrelid = 0 OR (SELECT c.relkind = 'c' FROM pg_catalog.pg_class c \
                 WHERE c.oid = t.typrelid))\n{hides}      AND n.nspname <> 'pg_catalog'\n"
            )),
            "{q}"
        );
        let arrays = describe_types_query(Some("int4[]"), false, false, PG18).unwrap();
        assert!(!arrays.contains(hides), "{arrays}");
        // The pattern is mapped, and `[]` escaped as an array tail.
        assert!(arrays.contains("E'^(integer\\\\[])$'"), "{arrays}");
    }

    #[test]
    fn dt_plus_lists_enum_elements_one_per_line() {
        let q = describe_types_query(None, true, false, PG18).unwrap();
        assert!(
            q.contains(
                "          ORDER BY e.enumsortorder\n      ),\n      E'\\n'\n  ) AS \"Elements\",\n"
            ),
            "{q}"
        );
        // `printACLColumn`, then upstream's `",\n  "` before the description.
        assert!(
            q.contains(
                "END AS \"Access privileges\",\n    pg_catalog.obj_description(t.oid, 'pg_type')"
            ),
            "{q}"
        );
    }

    #[test]
    fn do_argument_patterns_match_the_right_argument_or_both() {
        let q =
            |args: &[&str]| describe_operators_query(Some("+"), args, false, false, PG18).unwrap();
        let one = q(&["int4"]);
        assert!(
            one.contains("pg_catalog.pg_type t0 ON t0.oid = o.oprright\n"),
            "{one}"
        );
        assert!(
            one.contains("  AND o.oprleft = 0\n  AND (t0.typname"),
            "{one}"
        );
        // `force_escape`: `+` is an operator character, not a quantifier.
        assert!(
            one.contains("o.oprname OPERATOR(pg_catalog.~) E'^(\\\\+)$'"),
            "{one}"
        );
        let three = q(&["int4", "-", "text"]);
        assert!(three.contains("t0 ON t0.oid = o.oprleft\n"), "{three}");
        assert!(three.contains("t1 ON t1.oid = o.oprright\n"), "{three}");
        assert!(
            three.contains("  AND t1.typname IS NULL\nORDER BY 1, 2, 3, 4;"),
            "{three}"
        );
        assert!(!three.contains("t2") && !three.contains("text"), "{three}");
    }

    #[test]
    fn do_without_a_pattern_starts_the_where_with_the_system_filter() {
        let q = describe_operators_query(None, &[], true, false, PG18).unwrap();
        assert!(
            q.contains(
                "     LEFT JOIN pg_catalog.pg_proc p ON p.oid = o.oprcode\n\
                 WHERE n.nspname <> 'pg_catalog'\n      \
                 AND n.nspname <> 'information_schema'\n  \
                 AND pg_catalog.pg_operator_is_visible(o.oid)\n\
                 ORDER BY 1, 2, 3, 4;"
            ),
            "{q}"
        );
        let system = describe_operators_query(None, &[], false, true, PG18).unwrap();
        assert!(
            system.contains("\nWHERE pg_catalog.pg_operator_is_visible(o.oid)\n"),
            "{system}"
        );
    }

    #[test]
    fn dconfig_lists_non_defaults_or_matches_the_lower_cased_name() {
        let (q, title) = describe_configuration_parameters_query(None, false, PG18);
        assert_eq!(title, "List of non-default configuration parameters");
        assert_eq!(
            q,
            "SELECT s.name AS \"Parameter\", pg_catalog.current_setting(s.name) AS \"Value\"\n\
             FROM pg_catalog.pg_settings s\n\
             WHERE s.source <> 'default' AND\n      \
             s.setting IS DISTINCT FROM s.boot_val\n\
             ORDER BY 1;"
        );
        let (q, title) = describe_configuration_parameters_query(Some("Work*"), true, PG18);
        assert_eq!(title, "List of configuration parameters");
        assert!(
            q.contains(
                ", s.vartype AS \"Type\", s.context AS \"Context\", CASE WHEN pg_catalog.array_length(p.paracl, 1)"
            ),
            "{q}"
        );
        assert!(
            q.ends_with(
                "  LEFT JOIN pg_catalog.pg_parameter_acl p\n  \
                 ON pg_catalog.lower(s.name) = p.parname\n\
                 WHERE pg_catalog.lower(s.name) OPERATOR(pg_catalog.~) '^(work.*)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1;"
            ),
            "{q}"
        );
        // Before 15 there is no pg_parameter_acl.
        let v14 = ServerContext {
            sversion: 140_019,
            ..PG18
        };
        let (old, _) = describe_configuration_parameters_query(Some("a.b"), true, v14);
        assert!(
            old.contains(", NULL AS \"Access privileges\"\nFROM"),
            "{old}"
        );
        // No schema: a dot is part of the name, never refused.
        assert!(old.contains("'^(a.b)$'"), "{old}");
    }

    #[test]
    fn a_version_number_reads_as_its_release() {
        assert_eq!(format_pg_version_number(180_006, false), "18");
        assert_eq!(format_pg_version_number(180_006, true), "18.6");
        assert_eq!(format_pg_version_number(90_524, false), "9.5");
        assert_eq!(format_pg_version_number(90_524, true), "9.5.24");
    }

    #[test]
    fn x_means_expanded_only_after_the_second_character() {
        // `command.c:1047`: `\dx` is extensions, not expanded `\d`.
        assert!(!DescribeFlags::parse("dx").expanded);
        assert!(DescribeFlags::parse("d+x").expanded);
        assert!(DescribeFlags::parse("dtx").expanded);
        let f = DescribeFlags::parse("dtS+");
        assert!(f.verbose && f.system && !f.expanded);
    }

    #[test]
    fn table_types_default_to_everything_but_indexes() {
        let t = TableTypes::parse("");
        assert!(t.tables && t.views && t.mat_views && t.sequences && t.foreign && !t.indexes);
        assert_eq!(t.ntypes, 0);
        assert_eq!(t.title(), "List of relations");
        assert_eq!(TableTypes::parse("t+").title(), "List of tables");
        assert_eq!(TableTypes::parse("ti").title(), "List of relations");
        assert_eq!(TableTypes::parse("m").title(), "List of materialized views");
    }

    #[test]
    fn not_found_names_the_single_type_and_the_pattern() {
        assert_eq!(
            TableTypes::parse("i").not_found(Some("foo")),
            "Did not find any indexes named \"foo\"."
        );
        assert_eq!(
            TableTypes::parse("tvmsE").not_found(None),
            "Did not find any relations."
        );
        assert_eq!(
            TableTypes::parse("E").not_found(None),
            "Did not find any foreign tables."
        );
    }

    #[test]
    fn the_bare_d_query_is_upstreams() {
        // `\d` in a fresh database: `describe.c:4036`-`:4167` with no type
        // letters, not verbose, no system objects, no pattern.
        let q = list_tables_query(TableTypes::parse("tvmsE"), None, false, false, PG18).unwrap();
        assert_eq!(
            q,
            "SELECT n.nspname as \"Schema\",\n\
             \x20 c.relname as \"Name\",\n\
             \x20 CASE c.relkind WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' WHEN 'i' THEN 'index' WHEN 'S' THEN 'sequence' WHEN 't' THEN 'TOAST table' WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned table' WHEN 'I' THEN 'partitioned index' END as \"Type\",\n\
             \x20 pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\"\n\
             FROM pg_catalog.pg_class c\n\
             \x20    LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
             \x20    LEFT JOIN pg_catalog.pg_am am ON am.oid = c.relam\n\
             WHERE c.relkind IN ('r','p','v','m','S','f','')\n\
             \x20     AND n.nspname <> 'pg_catalog'\n\
             \x20     AND n.nspname !~ '^pg_toast'\n\
             \x20     AND n.nspname <> 'information_schema'\n\
             \x20 AND pg_catalog.pg_table_is_visible(c.oid)\n\
             ORDER BY 1,2;"
        );
    }

    #[test]
    fn the_verbose_index_query_adds_its_columns_and_joins() {
        let q = list_tables_query(TableTypes::parse("i+"), Some("x"), true, false, PG18).unwrap();
        assert!(q.contains(",\n  c2.relname as \"Table\",\n  CASE c.relpersistence WHEN 'p' THEN 'permanent' WHEN 't' THEN 'temporary' WHEN 'u' THEN 'unlogged' END as \"Persistence\",\n  am.amname as \"Access method\",\n  pg_catalog.pg_size_pretty(pg_catalog.pg_table_size(c.oid)) as \"Size\",\n  pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"\nFROM"), "{q}");
        assert!(q.contains("\n     LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\n     LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid\nWHERE c.relkind IN ('i','I','s','')\n  AND c.relname OPERATOR(pg_catalog.~) '^(x)$' COLLATE pg_catalog.default\n  AND pg_catalog.pg_table_is_visible(c.oid)\nORDER BY 1,2;"), "{q}");
    }

    #[test]
    fn hide_tableam_drops_the_access_method_column_and_join() {
        let server = ServerContext {
            hide_tableam: true,
            ..PG18
        };
        let q = list_tables_query(TableTypes::parse("t"), None, true, true, server).unwrap();
        assert!(!q.contains("am."), "{q}");
        // 'S' lets TOAST and the old special relkind in.
        assert!(
            q.contains("IN ('r','p','t','s','')\n  AND pg_catalog"),
            "{q}"
        );
    }

    #[test]
    fn a_bad_pattern_is_refused_before_any_query() {
        assert_eq!(
            list_tables_query(TableTypes::parse("t"), Some("a.b.c.d"), false, false, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): a.b.c.d".to_string()
            ))
        );
    }

    #[test]
    fn the_da_query_is_upstreams() {
        assert_eq!(
            describe_access_methods_query(None, false, PG18).unwrap(),
            "SELECT amname AS \"Name\",\n\
             \x20 CASE amtype WHEN 'i' THEN 'Index' WHEN 't' THEN 'Table' END AS \"Type\"\n\
             FROM pg_catalog.pg_am\n\
             ORDER BY 1;"
        );
        assert_eq!(
            describe_access_methods_query(Some("h*"), true, PG18).unwrap(),
            "SELECT amname AS \"Name\",\n\
             \x20 CASE amtype WHEN 'i' THEN 'Index' WHEN 't' THEN 'Table' END AS \"Type\",\n\
             \x20 amhandler AS \"Handler\",\n\
             \x20 pg_catalog.obj_description(oid, 'pg_am') AS \"Description\"\n\
             FROM pg_catalog.pg_am\n\
             WHERE amname OPERATOR(pg_catalog.~) '^(h.*)$' COLLATE pg_catalog.default\n\
             ORDER BY 1;"
        );
    }

    #[test]
    fn an_access_method_has_no_schema_so_a_dot_is_refused() {
        assert_eq!(
            describe_access_methods_query(Some("regression.heap"), false, PG18),
            Err(Refusal::Pattern(PatternError(
                "improper qualified name (too many dotted names): regression.heap".to_string()
            )))
        );
        assert_eq!(
            OperatorListing::Classes.query(Some("regression.brin"), None, false, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): regression.brin".to_string()
            ))
        );
    }

    #[test]
    fn a_server_too_old_is_told_so() {
        let old = ServerContext {
            sversion: 90_524,
            ..PG18
        };
        assert_eq!(
            describe_access_methods_query(None, false, old),
            Err(Refusal::ServerTooOld(
                "The server (version 9.5) does not support access methods.".to_string()
            ))
        );
        assert_eq!(
            list_partitioned_tables_query(PartitionTypes::parse(""), None, false, old),
            Err(Refusal::ServerTooOld(
                "The server (version 9.5) does not support declarative table partitioning."
                    .to_string()
            ))
        );
    }

    #[test]
    fn the_dac_query_matches_the_type_by_either_name() {
        // `\dAc brin pg*.oid*` (`psql.sql:1341`).
        let q = OperatorListing::Classes
            .query(Some("brin"), Some("pg*.oid*"), false, PG18)
            .unwrap();
        assert!(
            q.ends_with(
                "  LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n\
                 WHERE am.amname OPERATOR(pg_catalog.~) '^(brin)$' COLLATE pg_catalog.default\n\
                 \x20 AND (t.typname OPERATOR(pg_catalog.~) '^(oid.*)$' COLLATE pg_catalog.default\n\
                 \x20       OR pg_catalog.format_type(t.oid, NULL) OPERATOR(pg_catalog.~) '^(oid.*)$' COLLATE pg_catalog.default)\n\
                 \x20 AND tn.nspname OPERATOR(pg_catalog.~) '^(pg.*)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1, 2, 4;"
            ),
            "{q}"
        );
        // Verbose adds the family and owner, and their joins.
        let q = OperatorListing::Classes
            .query(None, None, true, PG18)
            .unwrap();
        assert!(
            q.contains(
                " pg_catalog.pg_get_userbyid(c.opcowner) AS \"Owner\"\n\n\
                 FROM pg_catalog.pg_opclass c\n"
            ),
            "{q}"
        );
        assert!(
            q.ends_with(
                "  LEFT JOIN pg_catalog.pg_namespace ofn ON ofn.oid = of.opfnamespace\n\
                 ORDER BY 1, 2, 4;"
            ),
            "{q}"
        );
    }

    #[test]
    fn the_daf_type_pattern_is_an_exists_subquery() {
        // `*` as the access method adds no clause, so the subquery opens the
        // WHERE.
        let q = OperatorListing::Families
            .query(Some("*"), Some("int4"), false, PG18)
            .unwrap();
        assert_eq!(
            q,
            "SELECT\n\
             \x20 am.amname AS \"AM\",\n\
             \x20 CASE\n\
             \x20   WHEN pg_catalog.pg_opfamily_is_visible(f.oid)\n\
             \x20   THEN pg_catalog.format('%I', f.opfname)\n\
             \x20   ELSE pg_catalog.format('%I.%I', n.nspname, f.opfname)\n\
             \x20 END AS \"Operator family\",\n\
             \x20 (SELECT\n\
             \x20    pg_catalog.string_agg(pg_catalog.format_type(oc.opcintype, NULL), ', ')\n\
             \x20  FROM pg_catalog.pg_opclass oc\n\
             \x20  WHERE oc.opcfamily = f.oid) \"Applicable types\"\n\
             FROM pg_catalog.pg_opfamily f\n\
             \x20 LEFT JOIN pg_catalog.pg_am am on am.oid = f.opfmethod\n\
             \x20 LEFT JOIN pg_catalog.pg_namespace n ON n.oid = f.opfnamespace\n\
             \x20 WHERE EXISTS (\n\
             \x20   SELECT 1\n\
             \x20   FROM pg_catalog.pg_type t\n\
             \x20   JOIN pg_catalog.pg_opclass oc ON oc.opcintype = t.oid\n\
             \x20   LEFT JOIN pg_catalog.pg_namespace tn ON tn.oid = t.typnamespace\n\
             \x20   WHERE oc.opcfamily = f.oid\n\
             \x20 AND (t.typname OPERATOR(pg_catalog.~) '^(int4)$' COLLATE pg_catalog.default\n\
             \x20       OR pg_catalog.format_type(t.oid, NULL) OPERATOR(pg_catalog.~) '^(int4)$' COLLATE pg_catalog.default)\n\
             \x20 AND pg_catalog.pg_type_is_visible(t.oid)\n\
             \x20 )\n\
             ORDER BY 1, 2;"
        );
        let q = OperatorListing::Families
            .query(Some("btree"), Some("int4"), false, PG18)
            .unwrap();
        assert!(
            q.contains("'^(btree)$' COLLATE pg_catalog.default\n  AND EXISTS (\n"),
            "{q}"
        );
    }

    #[test]
    fn the_dao_and_dap_queries_order_same_type_entries_first() {
        let q = OperatorListing::Operators
            .query(Some("btree"), Some("array_ops|float_ops"), true, PG18)
            .unwrap();
        // Upstream's comma leads the line after "Operator".
        assert!(
            q.contains("AS \"Operator\"\n,  o.amopstrategy AS \"Strategy\",\n"),
            "{q}"
        );
        assert!(
            q.contains("  END AS \"Purpose\"\n, ofs.opfname AS \"Sort opfamily\",\n"),
            "{q}"
        );
        assert!(
            q.ends_with(
                "  LEFT JOIN pg_catalog.pg_proc p ON p.oid = op.oprcode\n\
                 WHERE am.amname OPERATOR(pg_catalog.~) '^(btree)$' COLLATE pg_catalog.default\n\
                 \x20 AND of.opfname OPERATOR(pg_catalog.~) '^(array_ops|float_ops)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1, 2,\n\
                 \x20 o.amoplefttype = o.amoprighttype DESC,\n\
                 \x20 pg_catalog.format_type(o.amoplefttype, NULL),\n\
                 \x20 pg_catalog.format_type(o.amoprighttype, NULL),\n\
                 \x20 o.amopstrategy;"
            ),
            "{q}"
        );
        let q = OperatorListing::Functions
            .query(None, None, false, PG18)
            .unwrap();
        assert!(q.contains(", p.proname AS \"Function\"\nFROM"), "{q}");
        assert!(
            q.ends_with(
                "  LEFT JOIN pg_catalog.pg_proc p ON ap.amproc = p.oid\n\
                 ORDER BY 1, 2,\n\
                 \x20 ap.amproclefttype = ap.amprocrighttype DESC,\n\
                 \x20 3, 4, 5;"
            ),
            "{q}"
        );
        let q = OperatorListing::Functions
            .query(Some("*"), Some("pg_catalog.uuid_ops"), true, PG18)
            .unwrap();
        assert!(
            q.contains(", ap.amproc::pg_catalog.regprocedure AS \"Function\"\nFROM"),
            "{q}"
        );
        assert!(
            q.contains(
                "WHERE of.opfname OPERATOR(pg_catalog.~) '^(uuid_ops)$' COLLATE pg_catalog.default\n  \
                 AND ns.nspname OPERATOR(pg_catalog.~) '^(pg_catalog)$' COLLATE pg_catalog.default\n"
            ),
            "{q}"
        );
    }

    #[test]
    fn partition_types_default_to_both_kinds() {
        let t = PartitionTypes::parse("");
        assert!(t.tables && t.indexes && !t.nested && t.mixed());
        assert_eq!(t.title(), "List of partitioned relations");
        assert_eq!(
            PartitionTypes::parse("t+").title(),
            "List of partitioned tables"
        );
        assert_eq!(
            PartitionTypes::parse("in").title(),
            "List of partitioned indexes"
        );
        assert_eq!(
            PartitionTypes::parse("tix").title(),
            "List of partitioned relations"
        );
        assert!(PartitionTypes::parse("n").nested);
    }

    #[test]
    fn the_bare_dp_query_is_upstreams() {
        let q =
            list_partitioned_tables_query(PartitionTypes::parse(""), None, false, PG18).unwrap();
        assert_eq!(
            q,
            "SELECT n.nspname as \"Schema\",\n\
             \x20 c.relname as \"Name\",\n\
             \x20 pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",\n\
             \x20 CASE c.relkind WHEN 'p' THEN 'partitioned table' WHEN 'I' THEN 'partitioned index' END as \"Type\",\n\
             \x20c2.oid::pg_catalog.regclass as \"Table\"\n\
             FROM pg_catalog.pg_class c\n\
             \x20    LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
             \x20    LEFT JOIN pg_catalog.pg_index i ON i.indexrelid = c.oid\n\
             \x20    LEFT JOIN pg_catalog.pg_class c2 ON i.indrelid = c2.oid\n\
             WHERE c.relkind IN ('p','I','')\n\
             \x20AND NOT c.relispartition\n\
             \x20     AND n.nspname <> 'pg_catalog'\n\
             \x20     AND n.nspname !~ '^pg_toast'\n\
             \x20     AND n.nspname <> 'information_schema'\n\
             \x20 AND pg_catalog.pg_table_is_visible(c.oid)\n\
             ORDER BY \"Schema\", \"Type\" DESC, \"Name\";"
        );
    }

    #[test]
    fn a_dp_pattern_or_n_shows_parents_and_partitions() {
        let q = list_partitioned_tables_query(
            PartitionTypes::parse("t+"),
            Some("testpart.*"),
            true,
            PG18,
        )
        .unwrap();
        assert!(
            q.starts_with(
                "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  \
                 pg_catalog.pg_get_userbyid(c.relowner) as \"Owner\",\n  \
                 inh.inhparent::pg_catalog.regclass as \"Parent name\",\n  \
                 am.amname as \"Access method\",\n  s.tps as \"Total size\",\n  \
                 pg_catalog.obj_description(c.oid, 'pg_class') as \"Description\"\n"
            ),
            "{q}"
        );
        assert!(
            q.contains(
                "\n     LEFT JOIN pg_catalog.pg_inherits inh ON c.oid = inh.inhrelid\
                 \n     LEFT JOIN pg_catalog.pg_am am ON c.relam = am.oid,\n     \
                 LATERAL (SELECT pg_catalog.pg_size_pretty(sum(\n                 \
                 CASE WHEN ppt.isleaf AND ppt.level = 1\n                      \
                 THEN pg_catalog.pg_table_size(ppt.relid) ELSE 0 END)) AS dps,\n                     \
                 pg_catalog.pg_size_pretty(sum(pg_catalog.pg_table_size(ppt.relid))) AS tps\n              \
                 FROM pg_catalog.pg_partition_tree(c.oid) ppt) s\n\
                 WHERE c.relkind IN ('p','')\n  \
                 AND n.nspname OPERATOR(pg_catalog.~) '^(testpart)$' COLLATE pg_catalog.default\n\
                 ORDER BY \"Schema\", \"Parent name\" NULLS FIRST, \"Name\";"
            ),
            "{q}"
        );
        // `n` alone keeps the schema filter, and verbose adds the leaf size.
        let q =
            list_partitioned_tables_query(PartitionTypes::parse("in+"), None, true, PG18).unwrap();
        assert!(
            q.contains("  s.dps as \"Leaf partition size\",\n  s.tps as \"Total size\""),
            "{q}"
        );
        assert!(
            q.contains("WHERE c.relkind IN ('I','')\n      AND n.nspname <> 'pg_catalog'\n"),
            "{q}"
        );
    }

    #[test]
    fn before_12_partition_sizes_come_from_a_recursive_query() {
        let v11 = ServerContext {
            sversion: 110_022,
            ..PG18
        };
        let q = list_partitioned_tables_query(PartitionTypes::parse("n"), None, true, v11).unwrap();
        assert!(
            q.contains(
                ",\n     LATERAL (WITH RECURSIVE d\n\
                 \x20               AS (SELECT inhrelid AS oid, 1 AS level\n"
            ),
            "{q}"
        );
        assert!(
            q.contains(
                "SELECT pg_catalog.pg_size_pretty(sum(pg_catalog.pg_table_size(d.oid))) AS tps,\n\
                 \x20                      pg_catalog.pg_size_pretty(sum(\n\
                 \x20            CASE WHEN d.level = 1 THEN pg_catalog.pg_table_size(d.oid) ELSE 0 END)) AS dps\n\
                 \x20              FROM d) s\n"
            ),
            "{q}"
        );
    }
}
