//! The `\d` family: `src/bin/psql/describe.c`, and the pattern machinery it
//! shares with the other client programs from `src/fe_utils/string_utils.c`.
//!
//! Each command is a query built from its flags and pattern, run through
//! `PSQLexec`, and printed with a title. The query text is kept identical to
//! upstream's, byte for byte, because `ECHO_HIDDEN` shows it and because the
//! server's answer is only the same if the question is.
//!
//! Scope (NAT-401, slice 1): the dispatcher's whole `\d` switch
//! ([`DescribeCommand::parse`]), name patterns ([`pattern_to_sql_regex`],
//! [`process_sql_name_pattern`], [`validate_sql_name_pattern`]) and
//! `listTables` ([`list_tables_query`]), which answers `\d` with no pattern
//! and `\dt`, `\di`, `\dv`, `\dm`, `\ds` and `\dE`. Every other command the
//! switch recognizes is refused by name until its slice lands.
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
                0 | b'+' | b'x' => not_yet("describeAccessMethods"),
                b'c' => not_yet("listOperatorClasses"),
                b'f' => not_yet("listOperatorFamilies"),
                b'o' => not_yet("listOpFamilyOperators"),
                b'p' => not_yet("listOpFamilyFunctions"),
                _ => None,
            },
            b'a' => not_yet("describeAggregates"),
            b'b' => not_yet("describeTablespaces"),
            b'c' if cmd.starts_with("dconfig") => not_yet("describeConfigurationParameters"),
            b'c' => not_yet("listConversions"),
            b'C' => not_yet("listCasts"),
            b'd' if cmd.starts_with("ddp") => not_yet("listDefaultACLs"),
            b'd' => not_yet("objectDescription"),
            b'D' => not_yet("listDomains"),
            b'f' => match at(2) {
                0 | b'+' | b'S' | b'a' | b'n' | b'p' | b't' | b'w' | b'x' => {
                    not_yet("describeFunctions")
                }
                _ => None,
            },
            b'g' | b'u' => not_yet("describeRoles"),
            b'l' => not_yet("listLargeObjects"),
            b'L' => not_yet("listLanguages"),
            b'n' => not_yet("listSchemas"),
            b'o' => not_yet("describeOperators"),
            b'O' => not_yet("listCollations"),
            b'p' => not_yet("permissionsList"),
            b'P' => match at(2) {
                0 | b'+' | b't' | b'i' | b'n' | b'x' => not_yet("listPartitionedTables"),
                _ => None,
            },
            b'T' => not_yet("describeTypes"),
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
            p("dconfig", false),
            Some(DescribeCommand::NotYet("describeConfigurationParameters"))
        );
        assert_eq!(
            p("dc", false),
            Some(DescribeCommand::NotYet("listConversions"))
        );
        assert_eq!(
            p("dx", false),
            Some(DescribeCommand::NotYet("listExtensions"))
        );
        assert_eq!(p("dAz", false), None);
        assert_eq!(p("dfz", false), None);
        assert_eq!(p("drx", false), None);
        assert_eq!(p("dz", false), None);
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
}
