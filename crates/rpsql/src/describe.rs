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
//! [`describe_configuration_parameters_query`], `\dconfig`; and slice 4's
//! roles and privileges ([`permissions_list_query`], `\dp` and `\z`;
//! [`list_default_acls_query`], `\ddp`; [`describe_roles_query`], `\du` and
//! `\dg`; [`list_db_role_settings_query`], `\drds`;
//! [`describe_role_grants_query`], `\drg`) and [`list_domains_query`],
//! `\dD`; and slice 5's publications, subscriptions and extensions
//! ([`list_publications_query`], `\dRp`; [`describe_publications_query`],
//! `\dRp+`; [`describe_subscriptions_query`], `\dRs`;
//! [`list_extensions_query`], `\dx`; [`list_extension_contents_query`],
//! `\dx+`). Every other command the switch recognizes is refused by name
//! until its slice lands.
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
    /// `permissionsList()` (`describe.c:1054`): `\dp`, and `\z` from
    /// `exec_command_z()`.
    Permissions,
    /// `listDefaultACLs()` (`describe.c:1218`): `\ddp`.
    DefaultAcls,
    /// `describeRoles()` (`describe.c:3716`): `\du`, and `\dg`, "no longer
    /// distinct from `\du`".
    Roles,
    /// `listDbRoleSettings()` (`describe.c:3863`): `\drds`, with a second
    /// pattern.
    DbRoleSettings,
    /// `describeRoleGrants()` (`describe.c:3932`): `\drg`.
    RoleGrants,
    /// `listDomains()` (`describe.c:4552`): `\dD`.
    Domains,
    /// `listPublications()` (`describe.c:6400`): `\dRp`, and with `+`
    /// `describePublications()` (`describe.c:6531`).
    Publications,
    /// `describeSubscriptions()` (`describe.c:6746`): `\dRs`.
    Subscriptions,
    /// `listExtensions()` (`describe.c:6182`): `\dx`, and with `+`
    /// `listExtensionContents()` (`describe.c:6236`).
    Extensions,
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
            b'd' if cmd.starts_with("ddp") => Some(Self::DefaultAcls),
            b'd' => not_yet("objectDescription"),
            b'D' => Some(Self::Domains),
            b'f' => match at(2) {
                0 | b'+' | b'S' | b'a' | b'n' | b'p' | b't' | b'w' | b'x' => {
                    Some(Self::Functions(cmd[2..].to_string()))
                }
                _ => None,
            },
            b'g' | b'u' => Some(Self::Roles),
            b'l' => not_yet("listLargeObjects"),
            b'L' => not_yet("listLanguages"),
            b'n' => not_yet("listSchemas"),
            b'o' => Some(Self::Operators),
            b'O' => not_yet("listCollations"),
            b'p' => Some(Self::Permissions),
            b'P' => match at(2) {
                0 | b'+' | b't' | b'i' | b'n' | b'x' => {
                    Some(Self::ListPartitionedTables(cmd[2..].to_string()))
                }
                _ => None,
            },
            b'T' => Some(Self::Types),
            b't' | b'v' | b'm' | b'i' | b's' | b'E' => Some(Self::ListTables(cmd[1..].to_string())),
            b'r' => match (at(2), at(3)) {
                (b'd', b's') => Some(Self::DbRoleSettings),
                (b'g', _) => Some(Self::RoleGrants),
                _ => None,
            },
            b'R' => match at(2) {
                b'p' => Some(Self::Publications),
                b's' => Some(Self::Subscriptions),
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
            b'x' => Some(Self::Extensions),
            b'X' => not_yet("listExtendedStats"),
            b'y' => not_yet("listEventTriggers"),
            _ => None,
        }
    }

    /// How many patterns `exec_command_d()` reads for `cmd`: a second one only
    /// for `\dAc`, `\dAf`, `\dAo`, `\dAp` and `\drds`, and only after a
    /// first (`command.c:1065`, `:1200`); for `\df` and `\do`, after a first, up to
    /// [`FUNC_MAX_ARGS`] argument types (`exec_command_dfo()`,
    /// `command.c:1313`). Any argument past them draws the "extra argument"
    /// warning.
    #[must_use]
    pub fn patterns_read(cmd: &str, has_pattern: bool) -> usize {
        match Self::parse(cmd, has_pattern) {
            Some(Self::OperatorListing(_) | Self::DbRoleSettings) if has_pattern => 2,
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

/// The query half of `permissionsList()` (`describe.c:1054`-`:1188`), for
/// `\dp` and `\z`, whose title is "Access privileges". Indexes and TOAST
/// tables are left out, as they have no meaningful rights.
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
// One upstream function, kept in its order so it reads against its C.
#[allow(clippy::too_many_lines)]
pub fn permissions_list_query(
    pattern: Option<&str>,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "  c.relname as \"Name\",\n",
        "  CASE c.relkind",
        " WHEN 'r' THEN 'table'",
        " WHEN 'v' THEN 'view'",
        " WHEN 'm' THEN 'materialized view'",
        " WHEN 'S' THEN 'sequence'",
        " WHEN 'f' THEN 'foreign table'",
        " WHEN 'p' THEN 'partitioned table'",
        " END as \"Type\",\n",
        "  ",
    ));
    push_acl_column(&mut buf, "c.relacl");

    // Formatted as printACLColumn() does, but with no case for an empty
    // attacl: the backend always turns one back into NULL.
    buf.push_str(concat!(
        ",\n  pg_catalog.array_to_string(ARRAY(\n",
        "    SELECT attname || E':\\n  ' || pg_catalog.array_to_string(attacl, E'\\n  ')\n",
        "    FROM pg_catalog.pg_attribute a\n",
        "    WHERE attrelid = c.oid AND NOT attisdropped AND attacl IS NOT NULL\n",
        "  ), E'\\n') AS \"Column privileges\"",
    ));

    // Row security policies arrived in 9.5; RESTRICTIVE ones in 10.
    if server.sversion >= 90_500 {
        buf.push_str(concat!(
            ",\n  pg_catalog.array_to_string(ARRAY(\n",
            "    SELECT polname\n",
        ));
        if server.sversion >= 100_000 {
            buf.push_str(concat!(
                "    || CASE WHEN NOT polpermissive THEN\n",
                "       E' (RESTRICTIVE)'\n",
                "       ELSE '' END\n",
            ));
        }
        buf.push_str(concat!(
            "    || CASE WHEN polcmd != '*' THEN\n",
            "           E' (' || polcmd::pg_catalog.text || E'):'\n",
            "       ELSE E':'\n",
            "       END\n",
            "    || CASE WHEN polqual IS NOT NULL THEN\n",
            "           E'\\n  (u): ' || pg_catalog.pg_get_expr(polqual, polrelid)\n",
            "       ELSE E''\n",
            "       END\n",
            "    || CASE WHEN polwithcheck IS NOT NULL THEN\n",
            "           E'\\n  (c): ' || pg_catalog.pg_get_expr(polwithcheck, polrelid)\n",
            "       ELSE E''\n",
            "       END",
            "    || CASE WHEN polroles <> '{0}' THEN\n",
            "           E'\\n  to: ' || pg_catalog.array_to_string(\n",
            "               ARRAY(\n",
            "                   SELECT rolname\n",
            "                   FROM pg_catalog.pg_roles\n",
            "                   WHERE oid = ANY (polroles)\n",
            "                   ORDER BY 1\n",
            "               ), E', ')\n",
            "       ELSE E''\n",
            "       END\n",
            "    FROM pg_catalog.pg_policy pol\n",
            "    WHERE polrelid = c.oid), E'\\n')\n",
            "    AS \"Policies\"",
        ));
    }

    buf.push_str(concat!(
        "\nFROM pg_catalog.pg_class c\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n",
        "WHERE c.relkind IN ('r','v','m','S','f','p')\n",
    ));
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
            namevar: Some("c.relname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_table_is_visible(c.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// The query half of `listDefaultACLs()` (`describe.c:1218`-`:1263`), for
/// `\ddp`, whose title is "Default access privileges". The pattern matches
/// the schema's name or the owning role's.
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn list_default_acls_query(
    pattern: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT pg_catalog.pg_get_userbyid(d.defaclrole) AS \"Owner\",\n",
        "  n.nspname AS \"Schema\",\n",
        "  CASE d.defaclobjtype ",
        "    WHEN 'r' THEN 'table' WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'function'",
        "    WHEN 'T' THEN 'type' WHEN 'n' THEN 'schema' WHEN 'L' THEN 'large object' END AS \"Type\",\n",
        "  ",
    ));
    push_acl_column(&mut buf, "d.defaclacl");
    buf.push_str(concat!(
        "\nFROM pg_catalog.pg_default_acl d\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = d.defaclnamespace\n",
    ));
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            schemavar: None,
            namevar: Some("n.nspname"),
            altnamevar: Some("pg_catalog.pg_get_userbyid(d.defaclrole)"),
            visibilityrule: None,
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2, 3;");
    Ok(buf)
}

/// The query half of `describeRoles()` (`describe.c:3716`-`:3763`), for `\du`
/// and `\dg`. Only a role's name is matched: any dot makes the pattern
/// "improper". The result is not printed as it comes but folded into
/// "Attributes" cells ([`role_attributes`]).
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn describe_roles_query(
    pattern: Option<&str>,
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT r.rolname, r.rolsuper, r.rolinherit,\n",
        "  r.rolcreaterole, r.rolcreatedb, r.rolcanlogin,\n",
        "  r.rolconnlimit, r.rolvaliduntil",
    ));
    if verbose {
        buf.push_str("\n, pg_catalog.shobj_description(r.oid, 'pg_authid') AS description");
    }
    buf.push_str("\n, r.rolreplication");
    if server.sversion >= 90_500 {
        buf.push_str("\n, r.rolbypassrls");
    }
    buf.push_str("\nFROM pg_catalog.pg_roles r\n");
    if !show_system && pattern.is_none() {
        buf.push_str("WHERE r.rolname !~ '^pg_'\n");
    }
    // `have_where` is false even after the WHERE above, which is only added
    // when there is no pattern, and so no clause to join to it.
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("r.rolname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// The column headers `describeRoles()` prints (`describe.c:3774`-`:3778`),
/// all left-aligned.
#[must_use]
pub fn describe_roles_headers(verbose: bool) -> &'static [&'static str] {
    if verbose {
        &["Role name", "Attributes", "Description"]
    } else {
        &["Role name", "Attributes"]
    }
}

/// One row of `describeRoles()`'s table (`describe.c:3780`-`:3835`): the
/// role's name, its attributes folded into one cell — flags joined with `, `
/// ([`add_role_attribute`], `:3851`), then the connection limit and the
/// password's expiry each on a line of its own — and, when `verbose`, its
/// description.
///
/// `row` is the query's row as text, a NULL as the empty string, the way
/// `PQgetvalue` hands it out.
#[must_use]
pub fn describe_roles_row(row: &[&[u8]], verbose: bool, sversion: i32) -> Vec<Vec<u8>> {
    let col = |i: usize| row.get(i).copied().unwrap_or_default();
    let is_true = |i: usize| col(i) == b"t";
    let mut buf: Vec<u8> = Vec::new();
    if is_true(1) {
        add_role_attribute(&mut buf, b"Superuser");
    }
    if !is_true(2) {
        add_role_attribute(&mut buf, b"No inheritance");
    }
    if is_true(3) {
        add_role_attribute(&mut buf, b"Create role");
    }
    if is_true(4) {
        add_role_attribute(&mut buf, b"Create DB");
    }
    if !is_true(5) {
        add_role_attribute(&mut buf, b"Cannot login");
    }
    if is_true(if verbose { 9 } else { 8 }) {
        add_role_attribute(&mut buf, b"Replication");
    }
    if sversion >= 90_500 && is_true(if verbose { 10 } else { 9 }) {
        add_role_attribute(&mut buf, b"Bypass RLS");
    }

    let conns = atoi(col(6));
    if conns >= 0 {
        if !buf.is_empty() {
            buf.push(b'\n');
        }
        if conns == 0 {
            buf.extend_from_slice(b"No connections");
        } else if conns == 1 {
            buf.extend_from_slice(b"1 connection");
        } else {
            buf.extend_from_slice(format!("{conns} connections").as_bytes());
        }
    }

    if !col(7).is_empty() {
        if !buf.is_empty() {
            buf.push(b'\n');
        }
        buf.extend_from_slice(b"Password valid until ");
        buf.extend_from_slice(col(7));
    }

    let mut cells = vec![col(0).to_vec(), buf];
    if verbose {
        cells.push(col(8).to_vec());
    }
    cells
}

/// `add_role_attribute()` (`describe.c:3851`).
fn add_role_attribute(buf: &mut Vec<u8>, attribute: &[u8]) {
    if !buf.is_empty() {
        buf.extend_from_slice(b", ");
    }
    buf.extend_from_slice(attribute);
}

/// `atoi()`: optional leading whitespace and sign, then as many digits as
/// there are; 0 when there are none. (Overflow is undefined in C; a
/// `rolconnlimit` is an `int4`, so it never arises.)
fn atoi(bytes: &[u8]) -> i64 {
    let s = bytes.trim_ascii_start();
    let (negative, digits) = match s.first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let n = digits
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0_i64, |n, &d| {
            n.saturating_mul(10).saturating_add(i64::from(d - b'0'))
        });
    if negative { -n } else { n }
}

/// The query half of `listDbRoleSettings()` (`describe.c:3863`-`:3887`), for
/// `\drds`, whose title is "List of settings": a role pattern, then a
/// database pattern, each a bare name.
///
/// # Errors
/// Either pattern failed `validateSQLNamePattern`.
pub fn list_db_role_settings_query(
    pattern: Option<&str>,
    pattern2: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT rolname AS \"Role\", datname AS \"Database\",\n",
        "pg_catalog.array_to_string(setconfig, E'\\n') AS \"Settings\"\n",
        "FROM pg_catalog.pg_db_role_setting s\n",
        "LEFT JOIN pg_catalog.pg_database d ON d.oid = setdatabase\n",
        "LEFT JOIN pg_catalog.pg_roles r ON r.oid = setrole\n",
    ));
    let havewhere = validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("r.rolname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    validate_sql_name_pattern(
        &mut buf,
        pattern2,
        havewhere,
        false,
        PatternVars {
            namevar: Some("d.datname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// What `listDbRoleSettings()` logs instead of an empty table when not quiet
/// (`describe.c:3900`-`:3909`), since the user may have mixed up what its
/// two patterns mean.
#[must_use]
pub fn db_role_settings_not_found(pattern: Option<&str>, pattern2: Option<&str>) -> String {
    match (pattern, pattern2) {
        (Some(role), Some(db)) => {
            format!("Did not find any settings for role \"{role}\" and database \"{db}\".")
        }
        (Some(role), None) => format!("Did not find any settings for role \"{role}\"."),
        (None, _) => "Did not find any settings.".to_string(),
    }
}

/// The query half of `describeRoleGrants()` (`describe.c:3932`-`:3979`), for
/// `\drg`, whose title is "List of role grants". Before 16 a grant had no
/// INHERIT or SET option of its own, so the member's `rolinherit` and a
/// constant `SET` stand in.
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn describe_role_grants_query(
    pattern: Option<&str>,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT m.rolname AS \"Role name\", r.rolname AS \"Member of\",\n",
        "  pg_catalog.concat_ws(', ',\n",
    ));
    buf.push_str(if server.sversion >= 160_000 {
        concat!(
            "    CASE WHEN pam.admin_option THEN 'ADMIN' END,\n",
            "    CASE WHEN pam.inherit_option THEN 'INHERIT' END,\n",
            "    CASE WHEN pam.set_option THEN 'SET' END\n",
        )
    } else {
        concat!(
            "    CASE WHEN pam.admin_option THEN 'ADMIN' END,\n",
            "    CASE WHEN m.rolinherit THEN 'INHERIT' END,\n",
            "    'SET'\n",
        )
    });
    buf.push_str(concat!(
        "  ) AS \"Options\",\n",
        "  g.rolname AS \"Grantor\"\n",
        "FROM pg_catalog.pg_roles m\n",
        "     JOIN pg_catalog.pg_auth_members pam ON (pam.member = m.oid)\n",
        "     LEFT JOIN pg_catalog.pg_roles r ON (pam.roleid = r.oid)\n",
        "     LEFT JOIN pg_catalog.pg_roles g ON (pam.grantor = g.oid)\n",
    ));
    if !show_system && pattern.is_none() {
        buf.push_str("WHERE m.rolname !~ '^pg_'\n");
    }
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("m.rolname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2, 4;\n");
    Ok(buf)
}

/// The query half of `listDomains()` (`describe.c:4552`-`:4613`), for `\dD`,
/// whose title is "List of domains".
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn list_domains_query(
    pattern: Option<&str>,
    verbose: bool,
    show_system: bool,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT n.nspname as \"Schema\",\n",
        "       t.typname as \"Name\",\n",
        "       pg_catalog.format_type(t.typbasetype, t.typtypmod) as \"Type\",\n",
        "       (SELECT c.collname FROM pg_catalog.pg_collation c, pg_catalog.pg_type bt\n",
        "        WHERE c.oid = t.typcollation AND bt.oid = t.typbasetype AND t.typcollation <> bt.typcollation) as \"Collation\",\n",
        "       CASE WHEN t.typnotnull THEN 'not null' END as \"Nullable\",\n",
        "       t.typdefault as \"Default\",\n",
        "       pg_catalog.array_to_string(ARRAY(\n",
        "         SELECT pg_catalog.pg_get_constraintdef(r.oid, true) FROM pg_catalog.pg_constraint r WHERE t.oid = r.contypid AND r.contype = 'c' ORDER BY r.conname\n",
        "       ), ' ') as \"Check\"",
    ));
    if verbose {
        buf.push_str(",\n  ");
        push_acl_column(&mut buf, "t.typacl");
        buf.push_str(",\n       d.description as \"Description\"");
    }
    buf.push_str(concat!(
        "\nFROM pg_catalog.pg_type t\n",
        "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace\n",
    ));
    if verbose {
        buf.push_str(
            "     LEFT JOIN pg_catalog.pg_description d \
             ON d.classoid = t.tableoid AND d.objoid = t.oid \
             AND d.objsubid = 0\n",
        );
    }
    buf.push_str("WHERE t.typtype = 'd'\n");
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
            namevar: Some("t.typname"),
            altnamevar: None,
            visibilityrule: Some("pg_catalog.pg_type_is_visible(t.oid)"),
        },
        3,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1, 2;");
    Ok(buf)
}

/// The query half of `listExtensions()` (`describe.c:6182`-`:6214`), for
/// `\dx`, whose title is "List of installed extensions".
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn list_extensions_query(
    pattern: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from(concat!(
        "SELECT e.extname AS \"Name\", ",
        "e.extversion AS \"Version\", ae.default_version AS \"Default version\",",
        "n.nspname AS \"Schema\", d.description AS \"Description\"\n",
        "FROM pg_catalog.pg_extension e ",
        "LEFT JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace ",
        "LEFT JOIN pg_catalog.pg_description d ON d.objoid = e.oid ",
        "AND d.classoid = 'pg_catalog.pg_extension'::pg_catalog.regclass ",
        "LEFT JOIN pg_catalog.pg_available_extensions() ae(name, default_version, comment) ",
        "ON ae.name = e.extname\n",
    ));
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("e.extname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// The query half of `listExtensionContents()` (`describe.c:6236`-`:6257`),
/// for `\dx+`: each matching extension's name and OID, whose contents
/// [`list_one_extension_contents_query`] then lists.
///
/// # Errors
/// The pattern failed `validateSQLNamePattern`.
pub fn list_extension_contents_query(
    pattern: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, PatternError> {
    let mut buf = String::from("SELECT e.extname, e.oid\nFROM pg_catalog.pg_extension e\n");
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("e.extname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// What `listExtensionContents()` logs, when not quiet, for no extension at
/// all (`describe.c:6264`-`:6273`).
#[must_use]
pub fn extensions_not_found(pattern: Option<&str>) -> String {
    match pattern {
        Some(pattern) => format!("Did not find any extension named \"{pattern}\"."),
        None => "Did not find any extensions.".to_string(),
    }
}

/// The query of `listOneExtensionContents()` (`describe.c:6303`):
/// the objects that depend on extension `oid` as its members. `oid` is
/// pasted in as the server sent it, as upstream does.
#[must_use]
pub fn list_one_extension_contents_query(oid: &str) -> String {
    format!(
        "SELECT pg_catalog.pg_describe_object(classid, objid, 0) AS \"Object description\"\n\
         FROM pg_catalog.pg_depend\n\
         WHERE refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass \
         AND refobjid = '{oid}' AND deptype = 'e'\n\
         ORDER BY 1;"
    )
}

/// `listOneExtensionContents()`'s title (`describe.c:6325`).
#[must_use]
pub fn extension_contents_title(extname: &str) -> String {
    format!("Objects in extension \"{extname}\"")
}

/// `PUBLISH_GENCOLS_NONE` and `PUBLISH_GENCOLS_STORED`
/// (`pg_publication.h:118`, `:121`), as `listPublications()` and
/// `describePublications()` spell them into their queries.
const PUBLISH_GENCOLS_CASE: &str = "(CASE pubgencols\n    \
                                    WHEN 'n' THEN 'none'\n    \
                                    WHEN 's' THEN 'stored'\n   \
                                    END) AS \"Generated columns\"";

/// The message `listPublications()` and `describePublications()` log for a
/// server before 10 (`describe.c:6411`, `:6547`), which `\dRs` shares with
/// "subscriptions" (`:6759`).
fn does_not_support(sversion: i32, what: &str) -> Refusal {
    Refusal::ServerTooOld(format!(
        "The server (version {}) does not support {what}.",
        format_pg_version_number(sversion, false)
    ))
}

/// The query half of `listPublications()` (`describe.c:6400`-`:6462`), for
/// `\dRp`, whose title is "List of publications".
///
/// # Errors
/// A server before 10, or a pattern that failed `validateSQLNamePattern`.
pub fn list_publications_query(
    pattern: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    if server.sversion < 100_000 {
        return Err(does_not_support(server.sversion, "publications"));
    }
    let mut buf = String::from(concat!(
        "SELECT pubname AS \"Name\",\n",
        "  pg_catalog.pg_get_userbyid(pubowner) AS \"Owner\",\n",
        "  puballtables AS \"All tables\",\n",
        "  pubinsert AS \"Inserts\",\n",
        "  pubupdate AS \"Updates\",\n",
        "  pubdelete AS \"Deletes\"",
    ));
    if server.sversion >= 110_000 {
        buf.push_str(",\n  pubtruncate AS \"Truncates\"");
    }
    if server.sversion >= 180_000 {
        buf.push_str(",\n ");
        buf.push_str(PUBLISH_GENCOLS_CASE);
    }
    if server.sversion >= 130_000 {
        buf.push_str(",\n  pubviaroot AS \"Via root\"");
    }
    buf.push_str("\nFROM pg_catalog.pg_publication\n");
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("pubname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
    Ok(buf)
}

/// The query half of `describePublications()` (`describe.c:6531`-`:6602`),
/// for `\dRp+`: one row per publication, whose columns
/// [`describe_publication_table`] reads by position. A server that predates
/// a column gets a constant in its place, so the positions never move.
///
/// # Errors
/// A server before 10, or a pattern that failed `validateSQLNamePattern`.
pub fn describe_publications_query(
    pattern: Option<&str>,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    if server.sversion < 100_000 {
        return Err(does_not_support(server.sversion, "publications"));
    }
    let mut buf = String::from(concat!(
        "SELECT oid, pubname,\n",
        "  pg_catalog.pg_get_userbyid(pubowner) AS owner,\n",
        "  puballtables, pubinsert, pubupdate, pubdelete",
    ));
    buf.push_str(if server.sversion >= 110_000 {
        ", pubtruncate"
    } else {
        ", false AS pubtruncate"
    });
    if server.sversion >= 180_000 {
        buf.push_str(", ");
        buf.push_str(PUBLISH_GENCOLS_CASE);
        buf.push('\n');
    } else {
        buf.push_str(", 'none' AS pubgencols");
    }
    buf.push_str(if server.sversion >= 130_000 {
        ", pubviaroot"
    } else {
        ", false AS pubviaroot"
    });
    buf.push_str("\nFROM pg_catalog.pg_publication\n");
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        false,
        false,
        PatternVars {
            namevar: Some("pubname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 2;");
    Ok(buf)
}

/// What `describePublications()` logs, when not quiet, for no publication
/// at all (`describe.c:6610`-`:6619`).
#[must_use]
pub fn publications_not_found(pattern: Option<&str>) -> String {
    match pattern {
        Some(pattern) => format!("Did not find any publication named \"{pattern}\"."),
        None => "Did not find any publications.".to_string(),
    }
}

/// One publication as `describePublications()` lays it out
/// (`describe.c:6626`-`:6670`): the title, the headers and the one row of
/// cells, taken from a row of [`describe_publications_query`]'s result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationTable {
    /// `pubid`: pasted into the footer queries.
    pub oid: String,
    /// "Publication %s".
    pub title: String,
    /// Left-aligned, one per cell.
    pub headers: Vec<&'static str>,
    /// The one row.
    pub cells: Vec<Vec<u8>>,
    /// `puballtables`: such a publication has no footers.
    pub all_tables: bool,
}

/// `describePublications()`'s loop body up to the footers
/// (`describe.c:6626`-`:6670`), over one row of
/// [`describe_publications_query`]'s result. The truncate, generated-columns
/// and via-root columns are only shown for a server that has them.
#[must_use]
pub fn describe_publication_table(row: &[&[u8]], sversion: i32) -> PublicationTable {
    let cell = |i: usize| row.get(i).copied().unwrap_or_default();
    let mut headers = vec!["Owner", "All tables", "Inserts", "Updates", "Deletes"];
    let mut columns = vec![2, 3, 4, 5, 6];
    for (since, header, column) in [
        (110_000, "Truncates", 7),
        (180_000, "Generated columns", 8),
        (130_000, "Via root", 9),
    ] {
        if sversion >= since {
            headers.push(header);
            columns.push(column);
        }
    }
    PublicationTable {
        oid: String::from_utf8_lossy(cell(0)).into_owned(),
        title: format!("Publication {}", String::from_utf8_lossy(cell(1))),
        headers,
        cells: columns.into_iter().map(|c| cell(c).to_vec()).collect(),
        all_tables: cell(3) == b"t",
    }
}

/// The query for a publication's "Tables:" footer (`describe.c:6674`-
/// `:6701`): each table's schema and name, and from 15 its row filter and
/// column list.
#[must_use]
pub fn publication_tables_query(pubid: &str, sversion: i32) -> String {
    let mut buf = String::from("SELECT n.nspname, c.relname");
    if sversion >= 150_000 {
        buf.push_str(", pg_get_expr(pr.prqual, c.oid)");
        buf.push_str(concat!(
            ", (CASE WHEN pr.prattrs IS NOT NULL THEN\n",
            "     pg_catalog.array_to_string(",
            "      ARRAY(SELECT attname\n",
            "              FROM\n",
            "                pg_catalog.generate_series(0, ",
            "pg_catalog.array_upper(pr.prattrs::pg_catalog.int2[], 1)) s,\n",
            "                pg_catalog.pg_attribute\n",
            "        WHERE attrelid = c.oid AND attnum = prattrs[s]), ', ')\n",
            "       ELSE NULL END)",
        ));
    } else {
        buf.push_str(", NULL, NULL");
    }
    let _ = write!(
        buf,
        "\nFROM pg_catalog.pg_class c,\n     \
         pg_catalog.pg_namespace n,\n     \
         pg_catalog.pg_publication_rel pr\n\
         WHERE c.relnamespace = n.oid\n  \
         AND c.oid = pr.prrelid\n  \
         AND pr.prpubid = '{pubid}'\n\
         ORDER BY 1,2"
    );
    buf
}

/// The query for a publication's "Tables from schemas:" footer, from 15
/// (`describe.c:6707`-`:6713`).
#[must_use]
pub fn publication_schemas_query(pubid: &str) -> String {
    format!(
        "SELECT n.nspname\n\
         FROM pg_catalog.pg_namespace n\n     \
         JOIN pg_catalog.pg_publication_namespace pn ON n.oid = pn.pnnspid\n\
         WHERE pn.pnpubid = '{pubid}'\n\
         ORDER BY 1"
    )
}

/// `addFooterToPublicationDesc()` (`describe.c:6485`) past its query: the
/// footers one result adds, `footermsg` first, or none for no rows. A row is
/// `"schema"` when `as_schema`, else `"schema.table"`, then ` (columns)` and
/// ` WHERE filter` for whichever of columns 3 and 2 is not null.
#[must_use]
pub fn publication_footers(
    footermsg: &str,
    as_schema: bool,
    rows: &[Vec<Option<&[u8]>>],
) -> Vec<Vec<u8>> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut footers = vec![footermsg.as_bytes().to_vec()];
    for row in rows {
        let cell = |i: usize| row.get(i).copied().flatten();
        let mut footer = b"    \"".to_vec();
        footer.extend_from_slice(cell(0).unwrap_or_default());
        if !as_schema {
            footer.push(b'.');
            footer.extend_from_slice(cell(1).unwrap_or_default());
        }
        footer.push(b'"');
        if !as_schema {
            if let Some(columns) = cell(3) {
                footer.extend_from_slice(b" (");
                footer.extend_from_slice(columns);
                footer.push(b')');
            }
            if let Some(filter) = cell(2) {
                footer.extend_from_slice(b" WHERE ");
                footer.extend_from_slice(filter);
            }
        }
        footers.push(footer);
    }
    footers
}

/// The query half of `describeSubscriptions()` (`describe.c:6746`-`:6851`),
/// for `\dRs`, whose title is "List of subscriptions". Only the current
/// database's subscriptions are listed; `+` adds each option the server has.
///
/// # Errors
/// A server before 10, or a pattern that failed `validateSQLNamePattern`.
pub fn describe_subscriptions_query(
    pattern: Option<&str>,
    verbose: bool,
    server: ServerContext<'_>,
) -> Result<String, Refusal> {
    let sversion = server.sversion;
    if sversion < 100_000 {
        return Err(does_not_support(sversion, "subscriptions"));
    }
    let mut buf = String::from(concat!(
        "SELECT subname AS \"Name\"\n",
        ",  pg_catalog.pg_get_userbyid(subowner) AS \"Owner\"\n",
        ",  subenabled AS \"Enabled\"\n",
        ",  subpublications AS \"Publication\"\n",
    ));
    if verbose {
        // Binary mode and streaming are only supported in v14 and higher.
        if sversion >= 140_000 {
            buf.push_str(", subbinary AS \"Binary\"\n");
            // `LOGICALREP_STREAM_OFF`, `_ON` and `_PARALLEL`
            // (`pg_subscription.h:165`-`:177`).
            buf.push_str(if sversion >= 160_000 {
                concat!(
                    ", (CASE substream\n",
                    "    WHEN 'f' THEN 'off'\n",
                    "    WHEN 't' THEN 'on'\n",
                    "    WHEN 'p' THEN 'parallel'\n",
                    "   END) AS \"Streaming\"\n",
                )
            } else {
                ", substream AS \"Streaming\"\n"
            });
        }
        // Two_phase and disable_on_error are only supported in v15 and higher.
        if sversion >= 150_000 {
            buf.push_str(concat!(
                ", subtwophasestate AS \"Two-phase commit\"\n",
                ", subdisableonerr AS \"Disable on error\"\n",
            ));
        }
        if sversion >= 160_000 {
            buf.push_str(concat!(
                ", suborigin AS \"Origin\"\n",
                ", subpasswordrequired AS \"Password required\"\n",
                ", subrunasowner AS \"Run as owner?\"\n",
            ));
        }
        if sversion >= 170_000 {
            buf.push_str(", subfailover AS \"Failover\"\n");
        }
        buf.push_str(concat!(
            ",  subsynccommit AS \"Synchronous commit\"\n",
            ",  subconninfo AS \"Conninfo\"\n",
        ));
        // Skip LSN is only supported in v15 and higher.
        if sversion >= 150_000 {
            buf.push_str(", subskiplsn AS \"Skip LSN\"\n");
        }
    }
    // Only display subscriptions in current database.
    buf.push_str(concat!(
        "FROM pg_catalog.pg_subscription\n",
        "WHERE subdbid = (SELECT oid\n",
        "                 FROM pg_catalog.pg_database\n",
        "                 WHERE datname = pg_catalog.current_database())",
    ));
    validate_sql_name_pattern(
        &mut buf,
        pattern,
        true,
        false,
        PatternVars {
            namevar: Some("subname"),
            ..PatternVars::default()
        },
        1,
        server.sversion,
        server.db,
    )?;
    buf.push_str("ORDER BY 1;");
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
            p("dy", false),
            Some(DescribeCommand::NotYet("listEventTriggers"))
        );
        assert_eq!(p("dx+", false), Some(DescribeCommand::Extensions));
        assert_eq!(p("dRp+x", true), Some(DescribeCommand::Publications));
        assert_eq!(p("dRs", false), Some(DescribeCommand::Subscriptions));
        assert_eq!(p("dR", false), None);
        assert_eq!(p("dRx", false), None);
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

    #[test]
    fn the_roles_and_privileges_commands_parse_to_their_functions() {
        for (cmd, expected) in [
            ("dp", DescribeCommand::Permissions),
            ("dpS", DescribeCommand::Permissions),
            ("ddp", DescribeCommand::DefaultAcls),
            ("du", DescribeCommand::Roles),
            ("dg+", DescribeCommand::Roles),
            ("duSx", DescribeCommand::Roles),
            ("drds", DescribeCommand::DbRoleSettings),
            ("drg", DescribeCommand::RoleGrants),
            ("drgS", DescribeCommand::RoleGrants),
            ("dD", DescribeCommand::Domains),
            ("dD+", DescribeCommand::Domains),
        ] {
            assert_eq!(DescribeCommand::parse(cmd, true), Some(expected), "{cmd}");
        }
        // `\dd` without `p` is `objectDescription`; `\dr` needs `ds` or `g`.
        assert_eq!(
            DescribeCommand::parse("dd", false),
            Some(DescribeCommand::NotYet("objectDescription"))
        );
        assert_eq!(DescribeCommand::parse("drd", true), None);
        assert_eq!(DescribeCommand::parse("dr", true), None);
        // `\drds` reads a second pattern, and only after a first.
        assert_eq!(DescribeCommand::patterns_read("drds", true), 2);
        assert_eq!(DescribeCommand::patterns_read("drds", false), 1);
        assert_eq!(DescribeCommand::patterns_read("drg", true), 1);
    }

    #[test]
    fn permissions_list_has_policies_from_9_5_and_restrictive_ones_from_10() {
        let at = |sversion| ServerContext { sversion, ..PG18 };
        let q18 = permissions_list_query(None, false, PG18).unwrap();
        assert!(
            q18.starts_with(
                "SELECT n.nspname as \"Schema\",\n  c.relname as \"Name\",\n  CASE c.relkind \
                 WHEN 'r' THEN 'table' WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view' \
                 WHEN 'S' THEN 'sequence' WHEN 'f' THEN 'foreign table' \
                 WHEN 'p' THEN 'partitioned table' END as \"Type\",\n  \
                 CASE WHEN pg_catalog.array_length(c.relacl, 1) = 0 THEN '(none)' \
                 ELSE pg_catalog.array_to_string(c.relacl, E'\\n') END AS \"Access privileges\",\n"
            ),
            "{q18}"
        );
        assert!(
            q18.contains("    SELECT polname\n    || CASE WHEN NOT polpermissive THEN\n"),
            "{q18}"
        );
        // Upstream has no newline between these two lines.
        assert!(
            q18.contains("       END    || CASE WHEN polroles <> '{0}' THEN\n"),
            "{q18}"
        );
        assert!(
            q18.ends_with(
                "    AS \"Policies\"\nFROM pg_catalog.pg_class c\n     \
                 LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace\n\
                 WHERE c.relkind IN ('r','v','m','S','f','p')\n      \
                 AND n.nspname <> 'pg_catalog'\n      \
                 AND n.nspname <> 'information_schema'\n  \
                 AND pg_catalog.pg_table_is_visible(c.oid)\nORDER BY 1, 2;"
            ),
            "{q18}"
        );

        let q95 = permissions_list_query(None, false, at(90_526)).unwrap();
        assert!(
            q95.contains("    SELECT polname\n    || CASE WHEN polcmd != '*' THEN\n"),
            "{q95}"
        );
        assert!(!q95.contains("RESTRICTIVE"), "{q95}");
        // Otherwise the two are the same text.
        assert_eq!(
            q18.replace(
                "    || CASE WHEN NOT polpermissive THEN\n       E' (RESTRICTIVE)'\n       ELSE '' END\n",
                ""
            ),
            q95
        );

        let q94 = permissions_list_query(None, false, at(90_424)).unwrap();
        assert!(!q94.contains("Policies"), "{q94}");
        assert!(
            q94.contains("AS \"Column privileges\"\nFROM pg_catalog.pg_class c\n"),
            "{q94}"
        );
    }

    #[test]
    fn permissions_list_with_s_or_a_pattern_keeps_the_system_schemas() {
        let q = permissions_list_query(None, true, PG18).unwrap();
        assert!(!q.contains("<> 'pg_catalog'"), "{q}");
        let q = permissions_list_query(Some("public.t*"), false, PG18).unwrap();
        assert!(
            q.ends_with(
                "WHERE c.relkind IN ('r','v','m','S','f','p')\n  \
                 AND c.relname OPERATOR(pg_catalog.~) '^(t.*)$' COLLATE pg_catalog.default\n  \
                 AND n.nspname OPERATOR(pg_catalog.~) '^(public)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1, 2;"
            ),
            "{q}"
        );
        // `psql.sql:1768`-`:1770`.
        assert_eq!(
            permissions_list_query(Some("host.regression.public.a_star"), false, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): host.regression.public.a_star"
                    .to_string()
            ))
        );
        assert!(permissions_list_query(Some("regression.public.a_star"), false, PG18).is_ok());
    }

    #[test]
    fn default_acls_match_the_schema_or_the_owner_and_never_split_the_name() {
        let q = list_default_acls_query(None, PG18).unwrap();
        assert!(
            q.contains(
                "  CASE d.defaclobjtype     WHEN 'r' THEN 'table' WHEN 'S' THEN 'sequence' \
                 WHEN 'f' THEN 'function'    WHEN 'T' THEN 'type' WHEN 'n' THEN 'schema' \
                 WHEN 'L' THEN 'large object' END AS \"Type\",\n  CASE WHEN"
            ),
            "{q}"
        );
        assert!(
            q.ends_with(
                "     LEFT JOIN pg_catalog.pg_namespace n ON n.oid = d.defaclnamespace\n\
                 ORDER BY 1, 2, 3;"
            ),
            "{q}"
        );
        let q = list_default_acls_query(Some("Me"), PG18).unwrap();
        assert!(
            q.contains(
                "WHERE (n.nspname OPERATOR(pg_catalog.~) '^(me)$' COLLATE pg_catalog.default\n        \
                 OR pg_catalog.pg_get_userbyid(d.defaclrole) OPERATOR(pg_catalog.~) '^(me)$' \
                 COLLATE pg_catalog.default)\nORDER BY"
            ),
            "{q}"
        );
        // One dot stays in the name; two draw the database check with no
        // database part to match, and three are too many (`psql.sql:1710`).
        let q = list_default_acls_query(Some("a.b"), PG18).unwrap();
        assert!(q.contains("'^(a.b)$'"), "{q}");
        assert_eq!(
            list_default_acls_query(Some("{.pg_catalog.pg_class"), PG18),
            Err(PatternError(
                "cross-database references are not implemented: {.pg_catalog.pg_class".to_string()
            ))
        );
        assert_eq!(
            list_default_acls_query(Some("host.regression.pg_catalog.pg_class"), PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): host.regression.pg_catalog.pg_class"
                    .to_string()
            ))
        );
    }

    #[test]
    fn describe_roles_hides_pg_roles_unless_asked_and_takes_no_dot() {
        let q = describe_roles_query(None, false, false, PG18).unwrap();
        assert_eq!(
            q,
            "SELECT r.rolname, r.rolsuper, r.rolinherit,\n  \
             r.rolcreaterole, r.rolcreatedb, r.rolcanlogin,\n  \
             r.rolconnlimit, r.rolvaliduntil\n, r.rolreplication\n, r.rolbypassrls\n\
             FROM pg_catalog.pg_roles r\nWHERE r.rolname !~ '^pg_'\nORDER BY 1;"
        );
        let q = describe_roles_query(Some("regress_*"), true, false, PG18).unwrap();
        assert!(
            q.contains(
                "r.rolvaliduntil\n, pg_catalog.shobj_description(r.oid, 'pg_authid') AS description\n\
                 , r.rolreplication"
            ),
            "{q}"
        );
        assert!(
            q.ends_with(
                "FROM pg_catalog.pg_roles r\n\
                 WHERE r.rolname OPERATOR(pg_catalog.~) '^(regress_.*)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1;"
            ),
            "{q}"
        );
        let q = describe_roles_query(None, false, true, PG18).unwrap();
        assert!(
            q.ends_with("FROM pg_catalog.pg_roles r\nORDER BY 1;"),
            "{q}"
        );
        // No `rolbypassrls` before 9.5.
        let v94 = ServerContext {
            sversion: 90_424,
            ..PG18
        };
        let q = describe_roles_query(None, false, false, v94).unwrap();
        assert!(!q.contains("rolbypassrls"), "{q}");
        // `psql.sql:1755`.
        assert_eq!(
            describe_roles_query(Some("regression.pg_database_owner"), false, false, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): regression.pg_database_owner"
                    .to_string()
            ))
        );
    }

    /// A `describeRoles()` row: name, super, inherit, createrole, createdb,
    /// canlogin, connlimit, validuntil, [description,] replication, bypassrls.
    fn role(fields: &[&'static str]) -> Vec<&'static [u8]> {
        fields.iter().map(|f| f.as_bytes()).collect()
    }

    #[test]
    fn a_role_s_attributes_fold_into_one_cell() {
        let cells = |fields: &[&'static str], verbose| {
            describe_roles_row(&role(fields), verbose, 180_006)
                .into_iter()
                .map(|c| String::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
        };
        // `psql.out`'s `\du regress_du_role*`.
        assert_eq!(
            cells(&["r0", "f", "t", "f", "f", "f", "-1", "", "f", "f"], false),
            ["r0", "Cannot login"]
        );
        // Everything at once, in upstream's order.
        assert_eq!(
            cells(
                &[
                    "su",
                    "t",
                    "f",
                    "t",
                    "t",
                    "f",
                    "1",
                    "2030-01-01 00:00:00+00",
                    "t",
                    "t"
                ],
                false
            ),
            [
                "su",
                "Superuser, No inheritance, Create role, Create DB, Cannot login, \
                 Replication, Bypass RLS\n1 connection\n\
                 Password valid until 2030-01-01 00:00:00+00"
            ]
        );
        // No flag at all: the limit starts the cell; `+` adds the description
        // and moves replication and bypassrls one column on.
        assert_eq!(
            cells(
                &["u", "f", "t", "f", "f", "t", "0", "", "a user", "t", "f"],
                true
            ),
            ["u", "Replication\nNo connections", "a user"]
        );
        assert_eq!(
            cells(&["u", "f", "t", "f", "f", "t", "3", "", "", "f", "f"], true),
            ["u", "3 connections", ""]
        );
        assert_eq!(
            cells(&["u", "f", "t", "f", "f", "t", "-1", "", "f", "f"], false),
            ["u", ""]
        );
        // Before 9.5 there is no bypassrls column to read.
        assert_eq!(
            describe_roles_row(
                &role(&["u", "f", "t", "f", "f", "t", "-1", "", "t"]),
                false,
                90_424
            )[1],
            b"Replication"
        );
    }

    #[test]
    fn atoi_reads_a_leading_integer() {
        assert_eq!(atoi(b"-1"), -1);
        assert_eq!(atoi(b"  +42x"), 42);
        assert_eq!(atoi(b""), 0);
        assert_eq!(atoi(b"x1"), 0);
    }

    #[test]
    fn db_role_settings_take_a_role_pattern_then_a_database_one() {
        let q = list_db_role_settings_query(Some("r*"), Some("d*"), PG18).unwrap();
        assert!(
            q.ends_with(
                "LEFT JOIN pg_catalog.pg_roles r ON r.oid = setrole\n\
                 WHERE r.rolname OPERATOR(pg_catalog.~) '^(r.*)$' COLLATE pg_catalog.default\n  \
                 AND d.datname OPERATOR(pg_catalog.~) '^(d.*)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1, 2;"
            ),
            "{q}"
        );
        // A `*` role pattern adds nothing, so the database one opens the WHERE.
        let q = list_db_role_settings_query(Some("*"), Some("d"), PG18).unwrap();
        assert!(
            q.contains("setrole\nWHERE d.datname OPERATOR(pg_catalog.~) '^(d)$'"),
            "{q}"
        );
        // `psql.sql:1775`: either pattern takes no dot.
        assert_eq!(
            list_db_role_settings_query(Some("regression.lc_messages"), None, PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): regression.lc_messages"
                    .to_string()
            ))
        );
        assert!(list_db_role_settings_query(Some("r"), Some("a.b"), PG18).is_err());
        assert_eq!(
            db_role_settings_not_found(Some("r"), Some("d")),
            "Did not find any settings for role \"r\" and database \"d\"."
        );
        assert_eq!(
            db_role_settings_not_found(Some("r"), None),
            "Did not find any settings for role \"r\"."
        );
        assert_eq!(
            db_role_settings_not_found(None, None),
            "Did not find any settings."
        );
    }

    #[test]
    fn role_grants_before_16_stand_in_for_the_inherit_and_set_options() {
        let q = describe_role_grants_query(None, false, PG18).unwrap();
        assert!(
            q.contains("    CASE WHEN pam.set_option THEN 'SET' END\n  ) AS \"Options\",\n"),
            "{q}"
        );
        assert!(
            q.ends_with("WHERE m.rolname !~ '^pg_'\nORDER BY 1, 2, 4;\n"),
            "{q}"
        );
        let v15 = ServerContext {
            sversion: 150_010,
            ..PG18
        };
        let q = describe_role_grants_query(None, true, v15).unwrap();
        assert!(
            q.contains(
                "    CASE WHEN m.rolinherit THEN 'INHERIT' END,\n    'SET'\n  ) AS \"Options\",\n"
            ),
            "{q}"
        );
        assert!(
            q.ends_with("(pam.grantor = g.oid)\nORDER BY 1, 2, 4;\n"),
            "{q}"
        );
    }

    #[test]
    fn domains_show_their_privileges_and_description_with_plus() {
        let q = list_domains_query(None, false, false, PG18).unwrap();
        assert!(!q.contains("pg_description"), "{q}");
        assert!(
            q.contains(
                "r.contype = 'c' ORDER BY r.conname\n       ), ' ') as \"Check\"\n\
                 FROM pg_catalog.pg_type t\n"
            ),
            "{q}"
        );
        let q = list_domains_query(Some("s.d"), true, false, PG18).unwrap();
        assert!(
            q.contains(
                "as \"Check\",\n  CASE WHEN pg_catalog.array_length(t.typacl, 1) = 0 THEN '(none)' \
                 ELSE pg_catalog.array_to_string(t.typacl, E'\\n') END AS \"Access privileges\",\n       \
                 d.description as \"Description\"\n"
            ),
            "{q}"
        );
        assert!(
            q.ends_with(
                "     LEFT JOIN pg_catalog.pg_description d ON d.classoid = t.tableoid \
                 AND d.objoid = t.oid AND d.objsubid = 0\n\
                 WHERE t.typtype = 'd'\n  \
                 AND t.typname OPERATOR(pg_catalog.~) '^(d)$' COLLATE pg_catalog.default\n  \
                 AND n.nspname OPERATOR(pg_catalog.~) '^(s)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1, 2;"
            ),
            "{q}"
        );
    }

    #[test]
    fn publications_and_subscriptions_need_a_server_of_10() {
        let v96 = ServerContext {
            sversion: 90_624,
            ..PG18
        };
        let refused = |what: &str| {
            Err(Refusal::ServerTooOld(format!(
                "The server (version 9.6) does not support {what}."
            )))
        };
        assert_eq!(list_publications_query(None, v96), refused("publications"));
        assert_eq!(
            describe_publications_query(None, v96),
            refused("publications")
        );
        assert_eq!(
            describe_subscriptions_query(None, true, v96),
            refused("subscriptions")
        );
    }

    #[test]
    fn a_publication_listing_shows_the_columns_its_server_has() {
        let at = |sversion| ServerContext { sversion, ..PG18 };
        let q = list_publications_query(None, at(100_000)).unwrap();
        assert!(
            q.ends_with("  pubdelete AS \"Deletes\"\nFROM pg_catalog.pg_publication\nORDER BY 1;"),
            "{q}"
        );
        let q = list_publications_query(Some("p"), at(130_000)).unwrap();
        assert!(
            q.ends_with(
                "  pubdelete AS \"Deletes\",\n  pubtruncate AS \"Truncates\",\n  \
                 pubviaroot AS \"Via root\"\nFROM pg_catalog.pg_publication\n\
                 WHERE pubname OPERATOR(pg_catalog.~) '^(p)$' COLLATE pg_catalog.default\n\
                 ORDER BY 1;"
            ),
            "{q}"
        );
        let q = list_publications_query(None, PG18).unwrap();
        assert!(
            q.contains(
                "  pubtruncate AS \"Truncates\",\n (CASE pubgencols\n    \
                 WHEN 'n' THEN 'none'\n    WHEN 's' THEN 'stored'\n   \
                 END) AS \"Generated columns\",\n  pubviaroot AS \"Via root\"\n"
            ),
            "{q}"
        );
    }

    #[test]
    fn a_publication_description_keeps_its_column_positions_on_any_server() {
        let at = |sversion| ServerContext { sversion, ..PG18 };
        assert_eq!(
            describe_publications_query(None, at(100_000)).unwrap(),
            "SELECT oid, pubname,\n  pg_catalog.pg_get_userbyid(pubowner) AS owner,\n  \
             puballtables, pubinsert, pubupdate, pubdelete, false AS pubtruncate, \
             'none' AS pubgencols, false AS pubviaroot\n\
             FROM pg_catalog.pg_publication\nORDER BY 2;"
        );
        let q = describe_publications_query(None, PG18).unwrap();
        assert!(
            q.contains(
                "pubdelete, pubtruncate, (CASE pubgencols\n    WHEN 'n' THEN 'none'\n    \
                 WHEN 's' THEN 'stored'\n   END) AS \"Generated columns\"\n, pubviaroot\n\
                 FROM pg_catalog.pg_publication\n"
            ),
            "{q}"
        );

        let row: Vec<&[u8]> = vec![
            b"16390", b"p", b"alice", b"f", b"t", b"t", b"f", b"t", b"none", b"f",
        ];
        let t10 = describe_publication_table(&row, 100_000);
        assert_eq!(
            t10.headers,
            ["Owner", "All tables", "Inserts", "Updates", "Deletes"]
        );
        assert_eq!(t10.cells, [&b"alice"[..], b"f", b"t", b"t", b"f"]);
        assert_eq!(
            (t10.oid.as_str(), t10.title.as_str()),
            ("16390", "Publication p")
        );
        assert!(!t10.all_tables);
        let t13 = describe_publication_table(&row, 130_000);
        assert_eq!(
            t13.headers,
            [
                "Owner",
                "All tables",
                "Inserts",
                "Updates",
                "Deletes",
                "Truncates",
                "Via root"
            ]
        );
        assert_eq!(t13.cells[5..], [b"t".to_vec(), b"f".to_vec()]);
        let t18 = describe_publication_table(&row, 180_000);
        assert_eq!(
            t18.headers[5..],
            ["Truncates", "Generated columns", "Via root"]
        );
        assert_eq!(t18.cells[6], b"none");
    }

    #[test]
    fn a_publication_before_15_has_no_filters_columns_or_schemas_to_show() {
        let q = publication_tables_query("16390", 140_000);
        assert!(
            q.starts_with("SELECT n.nspname, c.relname, NULL, NULL\nFROM pg_catalog.pg_class c,\n"),
            "{q}"
        );
        assert!(
            q.ends_with("  AND pr.prpubid = '16390'\nORDER BY 1,2"),
            "{q}"
        );
        assert!(
            publication_tables_query("16390", 150_000)
                .starts_with("SELECT n.nspname, c.relname, pg_get_expr(pr.prqual, c.oid), (CASE")
        );
    }

    #[test]
    fn publication_footers_quote_each_name_then_add_columns_then_the_filter() {
        assert!(publication_footers("Tables:", false, &[]).is_empty());
        let rows: Vec<Vec<Option<&[u8]>>> = vec![
            vec![Some(b"s"), Some(b"t"), Some(b"(a > 1)"), Some(b"a, c")],
            vec![Some(b"s"), Some(b"u"), None, None],
        ];
        assert_eq!(
            publication_footers("Tables:", false, &rows),
            [
                b"Tables:".to_vec(),
                b"    \"s.t\" (a, c) WHERE (a > 1)".to_vec(),
                b"    \"s.u\"".to_vec(),
            ]
        );
        let schemas: Vec<Vec<Option<&[u8]>>> = vec![vec![Some(b"s5")]];
        assert_eq!(
            publication_footers("Tables from schemas:", true, &schemas),
            [b"Tables from schemas:".to_vec(), b"    \"s5\"".to_vec()]
        );
    }

    #[test]
    fn a_verbose_subscription_listing_shows_the_options_its_server_has() {
        let at = |sversion| ServerContext { sversion, ..PG18 };
        let columns = |sversion| {
            describe_subscriptions_query(None, true, at(sversion))
                .unwrap()
                .lines()
                .filter_map(|l| {
                    l.rsplit_once(" AS \"")
                        .map(|(_, c)| c.trim_end_matches('"').to_string())
                })
                .collect::<Vec<_>>()
        };
        let base = ["Name", "Owner", "Enabled", "Publication"];
        assert_eq!(
            columns(100_000),
            [&base[..], &["Synchronous commit", "Conninfo"]].concat()
        );
        assert_eq!(
            columns(140_000),
            [
                &base[..],
                &["Binary", "Streaming", "Synchronous commit", "Conninfo"]
            ]
            .concat()
        );
        assert!(
            describe_subscriptions_query(None, true, at(150_000))
                .unwrap()
                .contains(", substream AS \"Streaming\"\n")
        );
        assert_eq!(
            columns(170_000),
            [
                &base[..],
                &[
                    "Binary",
                    "Streaming",
                    "Two-phase commit",
                    "Disable on error",
                    "Origin",
                    "Password required",
                    "Run as owner?",
                    "Failover",
                    "Synchronous commit",
                    "Conninfo",
                    "Skip LSN",
                ]
            ]
            .concat()
        );
        // No newline after the database clause: a pattern's `  AND` follows it
        // on the same line, and so does the `ORDER BY` without one.
        assert!(
            describe_subscriptions_query(None, false, PG18)
                .unwrap()
                .ends_with("WHERE datname = pg_catalog.current_database())ORDER BY 1;")
        );
    }

    #[test]
    fn extension_listings_take_a_bare_name_and_say_what_was_not_found() {
        assert_eq!(
            list_extension_contents_query(Some("plpgsql"), PG18).unwrap(),
            "SELECT e.extname, e.oid\nFROM pg_catalog.pg_extension e\n\
             WHERE e.extname OPERATOR(pg_catalog.~) '^(plpgsql)$' COLLATE pg_catalog.default\n\
             ORDER BY 1;"
        );
        assert_eq!(
            list_extensions_query(Some("a.b"), PG18),
            Err(PatternError(
                "improper qualified name (too many dotted names): a.b".to_string()
            ))
        );
        assert_eq!(
            list_one_extension_contents_query("13000"),
            "SELECT pg_catalog.pg_describe_object(classid, objid, 0) AS \"Object description\"\n\
             FROM pg_catalog.pg_depend\n\
             WHERE refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass \
             AND refobjid = '13000' AND deptype = 'e'\nORDER BY 1;"
        );
        assert_eq!(
            extension_contents_title("plpgsql"),
            "Objects in extension \"plpgsql\""
        );
        assert_eq!(extensions_not_found(None), "Did not find any extensions.");
        assert_eq!(
            publications_not_found(Some("p")),
            "Did not find any publication named \"p\"."
        );
    }
}
