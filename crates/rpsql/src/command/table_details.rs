//! `describeTableDetails()` (`describe.c:1492`) and
//! `describeOneTableDetails()` (`describe.c:1575`): the queries of
//! [`crate::describe::table`], run in upstream's order, each one stopping
//! the command when it fails.

use std::io::Write;

use rlibpq::QueryResult;

use super::{print_with, refuse, rows_with_nulls};
use crate::common::{Executor, LogLevel, log_prefix, psql_exec};
use crate::describe::table::{
    RelKind, TableInfo, access_method_footers, add_tablespace_footer, check_constraint_footers,
    check_constraints_query, child_table_footers, child_tables_query, column_cells, column_headers,
    column_query, describe_table_details_query, foreign_key_footers, foreign_keys_query,
    foreign_server_footers, foreign_server_query, index_footer, index_footer_query,
    inherits_footers, inherits_query, not_null_constraint_footers, not_null_constraints_query,
    options_footers, owning_table_footers, owning_table_query, partition_key_footers,
    partition_key_query, partition_of_footers, partition_of_query, policies_query, policy_footers,
    publication_footers, publications_query, referenced_by_footers, referenced_by_query,
    relation_oid_not_found, relations_not_found, rule_footers, rules_query, sequence_footers,
    sequence_owner_query, sequence_query, sequence_title, statistics_footers, statistics_query,
    table_index_line, table_indexes_query, table_info_query, table_title, tablespace_query,
    trigger_footers, triggers_query, typed_table_and_identity_footers, view_definition_query,
    view_rule_footers, view_rules_query,
};
use crate::describe::{DescribeFlags, Refusal, ServerContext};
use crate::print::{Align, print_table};
use crate::settings::{Expanded, PsqlSettings};

/// Where the queries go and their output and errors are written: what every
/// step of `describeOneTableDetails()` shares.
struct Session<'a> {
    pset: &'a PsqlSettings,
    executor: &'a mut dyn Executor,
    stdout: &'a mut dyn Write,
    stderr: &'a mut dyn Write,
}

impl Session<'_> {
    /// `PSQLexec()`: `None` stops the command.
    fn exec(&mut self, query: &str) -> Option<QueryResult> {
        psql_exec(self.executor, query, self.pset, self.stdout, self.stderr)
    }

    /// Log `message` as an error.
    fn error(&mut self, message: &str) {
        let _ = writeln!(
            self.stderr,
            "{}{message}",
            log_prefix(self.pset, LogLevel::Error)
        );
    }
}

/// `describeTableDetails()` (`describe.c:1492`): find the relations
/// `pattern` names and describe each; nothing found fails the command, with
/// a message unless quiet.
pub(super) fn describe_table_details(
    pattern: Option<&str>,
    flags: DescribeFlags,
    server: ServerContext<'_>,
    pset: &PsqlSettings,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let query = match describe_table_details_query(pattern, flags.system, server) {
        Ok(query) => query,
        Err(err) => return refuse(Refusal::Pattern(err), pset, stderr),
    };
    let mut session = Session {
        pset,
        executor,
        stdout,
        stderr,
    };
    let Some(result) = session.exec(&query) else {
        return false;
    };
    if result.ntuples() == 0 {
        if !pset.quiet {
            session.error(&relations_not_found(pattern));
        }
        return false;
    }
    let text = |r: usize, c: usize| String::from_utf8_lossy(result.value(r, c).unwrap_or_default());
    (0..result.ntuples()).all(|r| {
        describe_one_table_details(
            &mut session,
            &text(r, 1),
            &text(r, 2),
            &text(r, 0),
            flags.verbose,
            server,
        )
    })
}

/// `describeOneTableDetails()` (`describe.c:1575`).
fn describe_one_table_details(
    session: &mut Session<'_>,
    schemaname: &str,
    relationname: &str,
    oid: &str,
    verbose: bool,
    server: ServerContext<'_>,
) -> bool {
    let sversion = server.sversion;
    let pset = session.pset;

    // Get general table info.
    let Some(result) = session.exec(&table_info_query(oid, verbose, sversion)) else {
        return false;
    };
    let rows = rows_with_nulls(&result);
    let Some(row) = rows.first() else {
        if !pset.quiet {
            session.error(&relation_oid_not_found(oid));
        }
        return false;
    };
    let info = TableInfo::parse(row, sversion);

    // If it's a sequence, deal with it here separately.
    if info.relkind == RelKind::Sequence {
        return describe_sequence(session, &info, schemaname, relationname, oid);
    }

    // Get per-column info.
    let (query, layout) = column_query(oid, &info, verbose, sversion, pset.hide_compression);
    let Some(result) = session.exec(&query) else {
        return false;
    };
    let columns = rows_with_nulls(&result);
    let title = table_title(&info, schemaname, relationname);
    let headers: Vec<(&str, Align)> = column_headers(&layout)
        .into_iter()
        .map(|h| (h, Align::Left))
        .collect();
    let cells: Vec<Vec<Vec<u8>>> = columns
        .iter()
        .map(|row| column_cells(row, &layout))
        .collect();

    let Some(footers) = footers(session, &info, schemaname, oid, verbose, server) else {
        return false;
    };

    let mut topt = pset.popt.topt.clone();
    topt.default_footer = false;
    // This output looks confusing in expanded mode.
    topt.expanded = Expanded::Off;
    match print_table(&topt, Some(&title), &headers, cells, footers) {
        Ok(text) => {
            let _ = session.stdout.write_all(&text);
            true
        }
        Err(err) => {
            session.error(&err.to_string());
            false
        }
    }
}

/// A sequence (`describe.c:1762`-`:1880`): its parameters printed as a query
/// result, in the settings' own expanded mode, with its owning column as the
/// footer.
fn describe_sequence(
    session: &mut Session<'_>,
    info: &TableInfo,
    schemaname: &str,
    relationname: &str,
    oid: &str,
) -> bool {
    let pset = session.pset;
    let Some(query) = sequence_query(oid, pset.sversion) else {
        session.error(
            "\\d of a sequence on a server before 10 is not implemented: \
             it needs fmtId (Linear NAT-401)",
        );
        return false;
    };
    let Some(result) = session.exec(&query) else {
        return false;
    };
    let Some(owner) = session.exec(&sequence_owner_query(oid)) else {
        return false;
    };
    let mut opt = pset.popt.clone();
    opt.footers = sequence_footers(&rows_with_nulls(&owner));
    opt.topt.default_footer = false;
    opt.title = Some(sequence_title(info, schemaname, relationname));
    print_with(&result, &opt, pset, session.stdout, session.stderr)
}

/// Every footer, in upstream's order (`describe.c:2195`-`:3623`); `None`
/// when a query failed, or an index or foreign table lost its row, which
/// stops the command without a message (`:2347`-`:2351`, `:3418`-`:3422`).
fn footers(
    session: &mut Session<'_>,
    info: &TableInfo,
    schemaname: &str,
    oid: &str,
    verbose: bool,
    server: ServerContext<'_>,
) -> Option<Vec<Vec<u8>>> {
    let sversion = server.sversion;
    let mut footers = Vec::new();

    if info.ispartition {
        let result = session.exec(&partition_of_query(oid, verbose, sversion))?;
        footers.extend(partition_of_footers(&rows_with_nulls(&result), verbose));
    }
    if info.relkind == RelKind::PartitionedTable {
        let result = session.exec(&partition_key_query(oid))?;
        footers.extend(partition_key_footers(&rows_with_nulls(&result)));
    }
    if info.relkind == RelKind::ToastValue {
        let result = session.exec(&owning_table_query(oid))?;
        footers.extend(owning_table_footers(&rows_with_nulls(&result)));
    }

    if info.relkind.is_index() {
        let result = session.exec(&index_footer_query(oid, sversion))?;
        let rows = rows_with_nulls(&result);
        let [row] = rows.as_slice() else {
            return None;
        };
        footers.push(index_footer(row, schemaname));
        // A partitioned index's tablespace is printed below.
        if info.relkind == RelKind::Index {
            tablespace(session, &mut footers, info.relkind, info.tablespace, true);
        }
    } else if info.relkind.has_table_footers() {
        table_footers(session, &mut footers, info, oid, verbose, sversion)?;
    }

    // Get view_def if table is a view or materialized view.
    if matches!(info.relkind, RelKind::View | RelKind::MatView) && verbose {
        let result = session.exec(&view_definition_query(oid))?;
        if let Some(view_def) = rows_with_nulls(&result).first() {
            footers.push(b"View definition:".to_vec());
            footers.push(
                view_def
                    .first()
                    .copied()
                    .flatten()
                    .unwrap_or_default()
                    .to_vec(),
            );
            if info.hasrules {
                let result = session.exec(&view_rules_query(oid))?;
                footers.extend(view_rule_footers(&rows_with_nulls(&result)));
            }
        }
    }

    // Print triggers next, if any (but only user-defined triggers). This
    // could apply to either a table or a view.
    if info.hastriggers {
        let result = session.exec(&triggers_query(oid, sversion))?;
        footers.extend(trigger_footers(&rows_with_nulls(&result)));
    }

    // Finish printing the footer information about a table.
    if info.relkind.has_table_footers() {
        if info.relkind == RelKind::ForeignTable {
            let result = session.exec(&foreign_server_query(oid))?;
            let rows = rows_with_nulls(&result);
            let [row] = rows.as_slice() else {
                return None;
            };
            footers.extend(foreign_server_footers(row));
        }
        let result = session.exec(&inherits_query(oid))?;
        footers.extend(inherits_footers(&rows_with_nulls(&result)));
        let result = session.exec(&child_tables_query(oid, sversion))?;
        footers.extend(child_table_footers(
            &rows_with_nulls(&result),
            info.relkind.is_partitioned(),
            verbose,
        ));
        footers.extend(typed_table_and_identity_footers(info, schemaname, verbose));
        tablespace(session, &mut footers, info.relkind, info.tablespace, true);
        footers.extend(access_method_footers(info, verbose, server.hide_tableam));
    }

    footers.extend(options_footers(info, verbose));
    Some(footers)
}

/// The footers of a table, a materialized view, a foreign table, a
/// partitioned table or a TOAST table (`describe.c:2421`-`:3152`): its
/// indexes, constraints, references, policies, statistics, rules,
/// publications and, with `+`, not-null constraints.
fn table_footers(
    session: &mut Session<'_>,
    footers: &mut Vec<Vec<u8>>,
    info: &TableInfo,
    oid: &str,
    verbose: bool,
    sversion: i32,
) -> Option<()> {
    if info.hasindex {
        let result = session.exec(&table_indexes_query(oid, sversion))?;
        let rows = rows_with_nulls(&result);
        if !rows.is_empty() {
            footers.push(b"Indexes:".to_vec());
        }
        for row in &rows {
            let (line, spc) = table_index_line(row);
            footers.push(line);
            // Print tablespace of the index on the same line.
            tablespace(session, footers, RelKind::Index, spc, false);
        }
    }
    if info.checks != 0 {
        let result = session.exec(&check_constraints_query(oid))?;
        footers.extend(check_constraint_footers(&rows_with_nulls(&result)));
    }
    let result = session.exec(&foreign_keys_query(oid, info, sversion))?;
    footers.extend(foreign_key_footers(&rows_with_nulls(&result)));
    let result = session.exec(&referenced_by_query(oid, sversion))?;
    footers.extend(referenced_by_footers(&rows_with_nulls(&result)));
    if let Some(query) = policies_query(oid, sversion) {
        let result = session.exec(&query)?;
        footers.extend(policy_footers(&rows_with_nulls(&result), info));
    }
    if let Some(query) = statistics_query(oid, sversion) {
        let result = session.exec(&query)?;
        footers.extend(statistics_footers(&rows_with_nulls(&result), sversion));
    }
    if info.hasrules && info.relkind != RelKind::MatView {
        let result = session.exec(&rules_query(oid))?;
        footers.extend(rule_footers(&rows_with_nulls(&result)));
    }
    if let Some(query) = publications_query(oid, sversion) {
        let result = session.exec(&query)?;
        footers.extend(publication_footers(&rows_with_nulls(&result)));
    }
    if verbose {
        let result = session.exec(&not_null_constraints_query(oid))?;
        footers.extend(not_null_constraint_footers(&rows_with_nulls(&result)));
    }
    Some(())
}

/// `add_tablespace_footer()` (`describe.c:3651`): a failed query adds
/// nothing and does not stop the command.
fn tablespace(
    session: &mut Session<'_>,
    footers: &mut Vec<Vec<u8>>,
    relkind: RelKind,
    spc: u32,
    newline: bool,
) {
    if let Some(query) = tablespace_query(relkind, spc)
        && let Some(result) = session.exec(&query)
    {
        add_tablespace_footer(footers, &rows_with_nulls(&result), newline);
    }
}
