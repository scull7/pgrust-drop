//! The backslash-command dispatcher: `src/bin/psql/command.c`.
//!
//! `HandleSlashCmds` (`command.c:231`) parses the command name, dispatches,
//! then eats whatever arguments are left over with a warning. This issue
//! implements the four commands it names — `\q`, `\c`, `\echo` and `\set` —
//! plus the `\unset`, `\qecho` and `\warn` that share their code, NAT-400
//! adds `\pset`, and NAT-401 the `\d` family ([`crate::describe`]).
//! Everything else is [`CommandResult::Unknown`], which renders upstream's
//! `invalid command \%s`; NAT-401 … NAT-403 fill the table in.

use std::io::Write;

use rlibpq::QueryResult;

use crate::common::{Executor, LogLevel, log_prefix, psql_exec};
use crate::describe::{
    DescribeCommand, DescribeFlags, FUNC_MAX_ARGS, PartitionTypes, Refusal, ServerContext,
    TableTypes, db_role_settings_not_found, describe_access_methods_query,
    describe_aggregates_query, describe_configuration_parameters_query, describe_functions_query,
    describe_operators_query, describe_role_grants_query, describe_roles_headers,
    describe_roles_query, describe_roles_row, describe_types_query, list_db_role_settings_query,
    list_default_acls_query, list_domains_query, list_partitioned_tables_query, list_tables_query,
    permissions_list_query,
};
use crate::print::{Align, print_query, print_table};
use crate::scan::{Scanner, VariableSource};
use crate::settings::{Expanded, PsqlSettings};
use crate::slash::SlashOption;
use crate::variables::{VarView, VariableSpace};

/// `backslashResult` (`command.h:15`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// `PSQL_CMD_UNKNOWN`: not a recognized command.
    Unknown,
    /// `PSQL_CMD_SEND`: query buffer is complete, send it.
    Send,
    /// `PSQL_CMD_SKIP_LINE`: the command did its work.
    SkipLine,
    /// `PSQL_CMD_TERMINATE`: end the session.
    Terminate,
    /// `PSQL_CMD_ERROR`: the command failed.
    Error,
    /// `\c`'s reconnection, `do_connect`, which the caller performs (`command.c:3919`).
    Connect(Box<ConnectRequest>),
}

/// The four arguments `\connect` takes (`command.c:645`-`:648`).
///
/// `None` means "keep what the current connection uses"; `Some("-")` is
/// upstream's explicit "use the default" (`command.c:3660`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectRequest {
    /// `dbname`
    pub dbname: Option<String>,
    /// `user`
    pub user: Option<String>,
    /// `host`
    pub host: Option<String>,
    /// `port`
    pub port: Option<String>,
}

impl ConnectRequest {
    /// `read_connect_arg()` (`command.c:3638`): a literal `-` means "unset".
    fn from_options(options: &[SlashOption]) -> Self {
        let arg = |i: usize| options.get(i).map(|o| o.value.clone()).filter(|v| v != "-");
        Self {
            dbname: arg(0),
            user: arg(1),
            host: arg(2),
            port: arg(3),
        }
    }
}

/// Everything a backslash command may read or write, so the dispatcher stays
/// one function of its inputs.
pub struct CommandContext<'a> {
    /// `pset`, for `quiet` and the print options `\pset` sets.
    pub pset: &'a mut PsqlSettings,
    /// `pset.vars`.
    pub vars: &'a mut VariableSpace,
    /// `pset.db`, for the commands that query the server.
    pub executor: &'a mut dyn Executor,
}

/// One whole backslash command, from the variable snapshot the lexer reads to
/// the settings a `\set` may have changed.
///
/// Every caller that reaches [`crate::scan::ScanResult::Backslash`] owes the
/// same sequence, and upstream gets it for free because `pset` is a global.
/// Here it is one function so the sequence — and the message that refuses
/// `\connect` — has a single spelling.
///
/// Reconnection is an action no caller performs yet, so [`CommandResult`]
/// never comes back as [`CommandResult::Connect`]: it is reported and turned
/// into [`CommandResult::Error`] here. NAT-405 replaces that with the real
/// thing.
pub fn dispatch_slash(
    scanner: &mut Scanner,
    pset: &mut PsqlSettings,
    vars: &mut VariableSpace,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    // The lexer reads the variable space while the command writes to it;
    // upstream aliases one global for both, so the read side works from a
    // snapshot taken before dispatch — the state the C lexer would have seen.
    let snapshot = vars.clone();
    let mut working = pset.clone();
    let status = {
        let mut ctx = CommandContext {
            pset: &mut working,
            vars,
            executor,
        };
        handle_slash_cmds(scanner, &mut ctx, &VarView(&snapshot), stdout, stderr)
    };
    // `\pset` wrote `working.popt`; a `\set` of a hooked variable wrote the
    // variable space, whose settings are re-derived on top.
    *pset = vars.settings(&working);

    if matches!(status, CommandResult::Connect(_)) {
        let _ = writeln!(
            stderr,
            "{}\\connect is not implemented yet (Linear NAT-405)",
            log_prefix(pset, LogLevel::Error)
        );
        return CommandResult::Error;
    }
    status
}

/// `HandleSlashCmds()` (`command.c:231`).
///
/// The scanner must be positioned just past the backslash, which is where
/// [`crate::scan::ScanResult::Backslash`] leaves it.
pub fn handle_slash_cmds(
    scanner: &mut Scanner,
    ctx: &mut CommandContext<'_>,
    vars_view: &dyn VariableSource,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let cmd = scanner.slash_command();
    let options = scanner.slash_options(vars_view);
    let status = exec_command(&cmd, &options, ctx, stdout, stderr);

    let status = if status == CommandResult::Unknown {
        let _ = writeln!(
            stderr,
            "{}invalid command \\{cmd}",
            log_prefix(ctx.pset, LogLevel::Error)
        );
        CommandResult::Error
    } else {
        status
    };

    // Eat any remaining arguments after a valid command (`command.c:271`).
    // `slash_options` above already consumed them, so what is left is the
    // warning upstream prints for the ones a command did not use.
    if status != CommandResult::Error {
        for extra in extra_arguments(&cmd, &options) {
            let _ = writeln!(
                stderr,
                "{}\\{cmd}: extra argument \"{extra}\" ignored",
                log_prefix(ctx.pset, LogLevel::Warning)
            );
        }
    }

    scanner.slash_command_end();
    status
}

/// How many arguments each implemented command consumes; the rest draw
/// upstream's "extra argument … ignored" warning.
fn extra_arguments<'a>(cmd: &str, options: &'a [SlashOption]) -> Vec<&'a str> {
    let takes = match cmd {
        // `\echo` and friends take everything.
        "echo" | "qecho" | "warn" | "set" => return Vec::new(),
        "c" | "connect" => 4,
        "pset" => 2,
        "unset" | "z" | "zS" | "zx" | "zSx" | "zxS" => 1,
        // `exec_command_d` reads one pattern, or two for some `\dA`s.
        d if d.starts_with('d') => DescribeCommand::patterns_read(d, !options.is_empty()),
        _ => 0,
    };
    options[options.len().min(takes)..]
        .iter()
        .map(|o| o.value.as_str())
        .collect()
}

/// `exec_command()` (`command.c:315`), for the commands this issue ports.
fn exec_command(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    match cmd {
        // `exec_command_quit()` (`command.c:2750`).
        "q" | "quit" => CommandResult::Terminate,
        // `exec_command_connect()` (`command.c:638`).
        "c" | "connect" => CommandResult::Connect(Box::new(ConnectRequest::from_options(options))),
        // `exec_command_echo()` (`command.c:1559`).
        "echo" | "qecho" | "warn" => {
            let sink: &mut dyn Write = if cmd == "warn" { stderr } else { stdout };
            let _ = sink.write_all(&echo_text(options));
            CommandResult::SkipLine
        }
        // `exec_command_pset()` (`command.c:2695`).
        "pset" => exec_command_pset(options, ctx, stdout, stderr),
        // `exec_command_set()` (`command.c:2881`).
        "set" => exec_command_set(options, ctx, stdout, stderr),
        // `exec_command_d()` (`command.c:1021`).
        d if d.starts_with('d') => exec_command_d(d, options, ctx, stdout, stderr),
        // `exec_command_z()` (`command.c:3548`), for exactly these spellings
        // (`command.c:472`).
        "z" | "zS" | "zx" | "zSx" | "zxS" => exec_command_z(cmd, options, ctx, stdout, stderr),
        // `exec_command_unset()` (`command.c:3238`).
        "unset" => {
            let Some(name) = options.first() else {
                let _ = writeln!(
                    stderr,
                    "{}\\unset: missing required argument",
                    log_prefix(ctx.pset, LogLevel::Error)
                );
                return CommandResult::Error;
            };
            match ctx.vars.delete(&name.value) {
                Ok(()) => CommandResult::SkipLine,
                Err(err) => {
                    let _ = writeln!(
                        stderr,
                        "{}{}",
                        log_prefix(ctx.pset, LogLevel::Error),
                        err.message
                    );
                    CommandResult::Error
                }
            }
        }
        _ => CommandResult::Unknown,
    }
}

/// `exec_command_d()` (`command.c:1021`): the `\d` family.
///
/// The pattern is the first option with its unquoted trailing semicolons
/// stripped (`psql_scan_slash_option(…, true)`). An `x` after the second
/// character turns expanded mode on for this command alone.
fn exec_command_d(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let pattern = options
        .first()
        .map(SlashOption::without_trailing_semicolons);
    let flags = DescribeFlags::parse(cmd);
    let Some(command) = DescribeCommand::parse(cmd, pattern.is_some()) else {
        return CommandResult::Unknown;
    };

    // `save_expanded` / restore (`command.c:1046`, `:1290`): the command sees
    // a copy of the settings, so there is nothing to restore.
    let mut pset = ctx.pset.clone();
    if flags.expanded {
        pset.popt.topt.expanded = Expanded::On;
    }

    let pattern = pattern.as_deref();
    let db = ctx.executor.db().map(str::to_owned);
    let server = ServerContext {
        sversion: pset.sversion,
        hide_tableam: pset.hide_tableam,
        db: db.as_deref(),
    };
    let success = match command {
        DescribeCommand::ListTables(tabtypes) => list_tables(
            &tabtypes,
            pattern,
            flags,
            server,
            &pset,
            ctx.executor,
            stdout,
            stderr,
        ),
        DescribeCommand::TableDetails => {
            not_yet(&pset, cmd, "describeTableDetails", stderr);
            false
        }
        DescribeCommand::NotYet(function) => {
            not_yet(&pset, cmd, function, stderr);
            false
        }
        DescribeCommand::Roles => {
            describe_roles(pattern, flags, server, &pset, ctx.executor, stdout, stderr)
        }
        DescribeCommand::DbRoleSettings => {
            // The second pattern is only read after a first (`command.c:1200`).
            let pattern2 = options
                .get(1)
                .filter(|_| pattern.is_some())
                .map(SlashOption::without_trailing_semicolons);
            list_db_role_settings(
                pattern,
                pattern2.as_deref(),
                server,
                &pset,
                ctx.executor,
                stdout,
                stderr,
            )
        }
        listing => {
            let (query, title) = listing_query(&listing, pattern, options, flags, server);
            run_listing(query, title, &pset, ctx.executor, stdout, stderr)
        }
    };
    if success {
        CommandResult::SkipLine
    } else {
        CommandResult::Error
    }
}

/// The query and title of a `\d` command that is a plain listing: one query,
/// printed under its title, with no message of its own for an empty result.
fn listing_query(
    command: &DescribeCommand,
    pattern: Option<&str>,
    options: &[SlashOption],
    flags: DescribeFlags,
    server: ServerContext<'_>,
) -> (Result<String, Refusal>, &'static str) {
    match command {
        DescribeCommand::ListPartitionedTables(reltypes) => {
            let types = PartitionTypes::parse(reltypes);
            (
                list_partitioned_tables_query(types, pattern, flags.verbose, server),
                types.title(),
            )
        }
        DescribeCommand::AccessMethods => (
            describe_access_methods_query(pattern, flags.verbose, server),
            "List of access methods",
        ),
        DescribeCommand::OperatorListing(which) => {
            // The second pattern is only read after a first (`command.c:1065`).
            let second = options
                .get(1)
                .filter(|_| pattern.is_some())
                .map(SlashOption::without_trailing_semicolons);
            (
                which
                    .query(pattern, second.as_deref(), flags.verbose, server)
                    .map_err(Refusal::from),
                which.title(),
            )
        }
        DescribeCommand::Aggregates => (
            describe_aggregates_query(pattern, flags.system, server).map_err(Refusal::from),
            "List of aggregate functions",
        ),
        DescribeCommand::Functions(functypes) => {
            let args = arg_patterns(pattern, options);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            (
                describe_functions_query(
                    functypes,
                    pattern,
                    &args,
                    flags.verbose,
                    flags.system,
                    server,
                ),
                "List of functions",
            )
        }
        DescribeCommand::Types => (
            describe_types_query(pattern, flags.verbose, flags.system, server)
                .map_err(Refusal::from),
            "List of data types",
        ),
        DescribeCommand::Operators => {
            let args = arg_patterns(pattern, options);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            (
                describe_operators_query(pattern, &args, flags.verbose, flags.system, server)
                    .map_err(Refusal::from),
                "List of operators",
            )
        }
        DescribeCommand::ConfigurationParameters => {
            let (query, title) =
                describe_configuration_parameters_query(pattern, flags.verbose, server);
            (Ok(query), title)
        }
        DescribeCommand::Permissions => (
            permissions_list_query(pattern, flags.system, server).map_err(Refusal::from),
            "Access privileges",
        ),
        DescribeCommand::DefaultAcls => (
            list_default_acls_query(pattern, server).map_err(Refusal::from),
            "Default access privileges",
        ),
        DescribeCommand::RoleGrants => (
            describe_role_grants_query(pattern, flags.system, server).map_err(Refusal::from),
            "List of role grants",
        ),
        DescribeCommand::Domains => (
            list_domains_query(pattern, flags.verbose, flags.system, server).map_err(Refusal::from),
            "List of domains",
        ),
        DescribeCommand::ListTables(_)
        | DescribeCommand::TableDetails
        | DescribeCommand::Roles
        | DescribeCommand::DbRoleSettings
        | DescribeCommand::NotYet(_) => {
            unreachable!("exec_command_d handles {command:?} itself")
        }
    }
}

/// `exec_command_dfo()`'s argument-type patterns (`command.c:1313`-`:1325`):
/// only after a first pattern, each without its unquoted trailing
/// semicolons, and at most [`FUNC_MAX_ARGS`] of them.
fn arg_patterns(pattern: Option<&str>, options: &[SlashOption]) -> Vec<String> {
    if pattern.is_none() {
        return Vec::new();
    }
    options
        .iter()
        .skip(1)
        .take(FUNC_MAX_ARGS)
        .map(SlashOption::without_trailing_semicolons)
        .collect()
}

/// Refuse a `\d` command whose `describe.c` function has not been ported,
/// naming both, rather than print something that only looks right.
fn not_yet(pset: &PsqlSettings, cmd: &str, function: &str, stderr: &mut dyn Write) {
    let _ = writeln!(
        stderr,
        "{}\\{cmd}: {function} is not implemented yet (Linear NAT-401)",
        log_prefix(pset, LogLevel::Error)
    );
}

/// The shape most `describe.c` listings share: build the query, run it
/// through `PSQLexec`, and print the result under `title`.
///
/// A refused pattern fails the command. A server too old for the feature,
/// or letters the command does not take, are logged as an error, yet the
/// command succeeds, as upstream returns `true` there (`describe.c:155`-`:163`,
/// `:314`-`:318`).
fn run_listing(
    query: Result<String, Refusal>,
    title: &str,
    pset: &PsqlSettings,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let query = match query {
        Ok(query) => query,
        Err(refusal) => {
            let (message, success) = match refusal {
                Refusal::Pattern(err) => (err.0, false),
                Refusal::ServerTooOld(message) | Refusal::InvalidOptions(message) => {
                    (message, true)
                }
            };
            let _ = writeln!(stderr, "{}{message}", log_prefix(pset, LogLevel::Error));
            return success;
        }
    };
    let Some(result) = psql_exec(executor, &query, pset, stdout, stderr) else {
        return false;
    };
    print_titled(&result, title, pset, stdout, stderr)
}

/// `printQuery()` of a listing under its title.
fn print_titled(
    result: &QueryResult,
    title: &str,
    pset: &PsqlSettings,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let mut opt = pset.popt.clone();
    opt.title = Some(title.to_string());
    match print_query(result, &opt) {
        Ok(text) => {
            let _ = stdout.write_all(&text);
            true
        }
        Err(err) => {
            let _ = writeln!(stderr, "{}{err}", log_prefix(pset, LogLevel::Error));
            false
        }
    }
}

/// `listTables()` (`describe.c:4011`): build the query, run it through
/// `PSQLexec`, and print the result under its title — or, when nothing
/// matched and psql is not quiet, say so instead (`describe.c:4179`).
#[allow(clippy::too_many_arguments)]
fn list_tables(
    tabtypes: &str,
    pattern: Option<&str>,
    flags: DescribeFlags,
    server: ServerContext<'_>,
    pset: &PsqlSettings,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let types = TableTypes::parse(tabtypes);
    let query = match list_tables_query(types, pattern, flags.verbose, flags.system, server) {
        Ok(query) => query,
        Err(err) => {
            let _ = writeln!(stderr, "{}{}", log_prefix(pset, LogLevel::Error), err.0);
            return false;
        }
    };
    let Some(result) = psql_exec(executor, &query, pset, stdout, stderr) else {
        return false;
    };

    if result.ntuples() == 0 && !pset.quiet {
        let _ = writeln!(
            stderr,
            "{}{}",
            log_prefix(pset, LogLevel::Error),
            types.not_found(pattern)
        );
        return true;
    }
    print_titled(&result, types.title(), pset, stdout, stderr)
}

/// `describeRoles()` (`describe.c:3716`): run the query, then fold each row
/// into a name, an "Attributes" cell and, with `+`, a description, and print
/// them as a table of its own, with no footer (`describe.c:3729`).
fn describe_roles(
    pattern: Option<&str>,
    flags: DescribeFlags,
    server: ServerContext<'_>,
    pset: &PsqlSettings,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let query = match describe_roles_query(pattern, flags.verbose, flags.system, server) {
        Ok(query) => query,
        Err(err) => {
            let _ = writeln!(stderr, "{}{}", log_prefix(pset, LogLevel::Error), err.0);
            return false;
        }
    };
    let Some(result) = psql_exec(executor, &query, pset, stdout, stderr) else {
        return false;
    };
    let cells = (0..result.ntuples())
        .map(|r| {
            let row: Vec<&[u8]> = (0..result.nfields())
                .map(|c| result.value(r, c).unwrap_or_default())
                .collect();
            describe_roles_row(&row, flags.verbose, server.sversion)
        })
        .collect();
    let headers: Vec<(&str, Align)> = describe_roles_headers(flags.verbose)
        .iter()
        .map(|&h| (h, Align::Left))
        .collect();
    let mut opt = pset.popt.topt.clone();
    opt.default_footer = false;
    match print_table(&opt, Some("List of roles"), &headers, cells) {
        Ok(text) => {
            let _ = stdout.write_all(&text);
            true
        }
        Err(err) => {
            let _ = writeln!(stderr, "{}{err}", log_prefix(pset, LogLevel::Error));
            false
        }
    }
}

/// `listDbRoleSettings()` (`describe.c:3863`): print the settings under
/// their title — or, when nothing matched and psql is not quiet, say so
/// instead (`describe.c:3900`).
#[allow(clippy::too_many_arguments)]
fn list_db_role_settings(
    pattern: Option<&str>,
    pattern2: Option<&str>,
    server: ServerContext<'_>,
    pset: &PsqlSettings,
    executor: &mut dyn Executor,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> bool {
    let query = match list_db_role_settings_query(pattern, pattern2, server) {
        Ok(query) => query,
        Err(err) => {
            let _ = writeln!(stderr, "{}{}", log_prefix(pset, LogLevel::Error), err.0);
            return false;
        }
    };
    let Some(result) = psql_exec(executor, &query, pset, stdout, stderr) else {
        return false;
    };
    if result.ntuples() == 0 && !pset.quiet {
        let _ = writeln!(
            stderr,
            "{}{}",
            log_prefix(pset, LogLevel::Error),
            db_role_settings_not_found(pattern, pattern2)
        );
        return true;
    }
    print_titled(&result, "List of settings", pset, stdout, stderr)
}

/// `exec_command_z()` (`command.c:3548`): `permissionsList()`, as `\dp`,
/// with `S` for system objects and `x` for expanded output this once.
fn exec_command_z(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let pattern = options
        .first()
        .map(SlashOption::without_trailing_semicolons);
    let mut pset = ctx.pset.clone();
    if cmd.contains('x') {
        pset.popt.topt.expanded = Expanded::On;
    }
    let db = ctx.executor.db().map(str::to_owned);
    let server = ServerContext {
        sversion: pset.sversion,
        hide_tableam: pset.hide_tableam,
        db: db.as_deref(),
    };
    let query = permissions_list_query(pattern.as_deref(), cmd.contains('S'), server)
        .map_err(Refusal::from);
    if run_listing(
        query,
        "Access privileges",
        &pset,
        ctx.executor,
        stdout,
        stderr,
    ) {
        CommandResult::SkipLine
    } else {
        CommandResult::Error
    }
}

/// The pure half of `exec_command_echo` (`command.c:1559`): the bytes `\echo`
/// writes, including the `-n` handling and the trailing newline.
#[must_use]
pub fn echo_text(options: &[SlashOption]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut no_newline = false;
    let mut first = true;
    for option in options {
        // `-n` only counts as the first, unquoted argument.
        if first && !no_newline && option.quote.is_none() && option.value == "-n" {
            no_newline = true;
            continue;
        }
        if first {
            first = false;
        } else {
            out.push(b' ');
        }
        out.extend_from_slice(option.value.as_bytes());
    }
    if !no_newline {
        out.push(b'\n');
    }
    out
}

/// `exec_command_pset()` (`command.c:2695`): list every print option, or
/// `do_pset` the first argument to the second.
fn exec_command_pset(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let Some(param) = options.first() else {
        let _ = stdout.write_all(crate::pset::list_all(&ctx.pset.popt).as_bytes());
        return CommandResult::SkipLine;
    };
    let value = options.get(1).map(|o| o.value.as_str());
    let quiet = ctx.pset.quiet;
    match crate::pset::do_pset(&param.value, value, &mut ctx.pset.popt, quiet) {
        Ok(info) => {
            if let Some(info) = info {
                let _ = stdout.write_all(info.as_bytes());
            }
            CommandResult::SkipLine
        }
        Err(err) => {
            let _ = writeln!(stderr, "{}{err}", log_prefix(ctx.pset, LogLevel::Error));
            CommandResult::Error
        }
    }
}

fn exec_command_set(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let Some(name) = options.first() else {
        // No arguments: list all variables (`command.c:2892`).
        let _ = stdout.write_all(ctx.vars.print().as_bytes());
        return CommandResult::SkipLine;
    };
    // The value is the concatenation of the remaining arguments
    // (`command.c:2899`).
    let value: String = options[1..].iter().map(|o| o.value.as_str()).collect();
    match ctx.vars.set(&name.value, Some(&value)) {
        Ok(()) => CommandResult::SkipLine,
        Err(err) => {
            let _ = writeln!(
                stderr,
                "{}{}",
                log_prefix(ctx.pset, LogLevel::Error),
                err.message
            );
            CommandResult::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ErrorMessage;
    use crate::scan::{NoVariables, ScanResult};
    use crate::settings::EchoHidden;
    use rlibpq::{Backend, FieldDescription, QueryResult, QueryRunner, TransactionStatus};

    struct Run {
        result: CommandResult,
        stdout: String,
        stderr: String,
        vars: VariableSpace,
    }

    /// An executor that records each query and answers from a queue, or
    /// reports no connection when the queue is `None`.
    struct Canned {
        answers: Option<Vec<Vec<QueryResult>>>,
        seen: Vec<String>,
    }

    impl Executor for Canned {
        fn exec(&mut self, query: &[u8]) -> Result<Vec<QueryResult>, ErrorMessage> {
            self.seen.push(String::from_utf8(query.to_vec()).unwrap());
            Ok(self.answers.as_mut().expect("connected").remove(0))
        }
        fn connected(&self) -> bool {
            self.answers.is_some()
        }
        fn db(&self) -> Option<&str> {
            Some("regression")
        }
    }

    fn run(line: &str) -> Run {
        run_with(line, PsqlSettings::default(), None).0
    }

    /// [`run`], with the settings and the executor's answers given.
    fn run_with(
        line: &str,
        mut pset: PsqlSettings,
        answers: Option<Vec<Vec<QueryResult>>>,
    ) -> (Run, Vec<String>) {
        let mut executor = Canned {
            answers,
            seen: Vec::new(),
        };
        let mut vars = VariableSpace::new();
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        let (res, _) = scanner.scan(&mut buf, &NoVariables);
        assert_eq!(res, ScanResult::Backslash);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = {
            let mut ctx = CommandContext {
                pset: &mut pset,
                vars: &mut vars,
                executor: &mut executor,
            };
            handle_slash_cmds(
                &mut scanner,
                &mut ctx,
                &NoVariables,
                &mut stdout,
                &mut stderr,
            )
        };
        (
            Run {
                result,
                stdout: String::from_utf8(stdout).unwrap(),
                stderr: String::from_utf8(stderr).unwrap(),
                vars,
            },
            executor.seen,
        )
    }

    #[test]
    fn quit_terminates() {
        assert_eq!(run("\\q").result, CommandResult::Terminate);
        assert_eq!(run("\\quit").result, CommandResult::Terminate);
    }

    #[test]
    fn echo_writes_its_arguments_separated_by_one_space() {
        let run = run("\\echo a b   c");
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(run.stdout, "a b c\n");
    }

    #[test]
    fn echo_with_no_arguments_writes_a_newline() {
        assert_eq!(run("\\echo").stdout, "\n");
    }

    #[test]
    fn echo_minus_n_suppresses_the_newline_only_when_unquoted_and_first() {
        assert_eq!(run("\\echo -n hi").stdout, "hi");
        // Quoted, it is just text (`command.c:1579`).
        assert_eq!(run("\\echo '-n' hi").stdout, "-n hi\n");
        // Not first, it is just text.
        assert_eq!(run("\\echo hi -n").stdout, "hi -n\n");
    }

    #[test]
    fn warn_writes_to_stderr_instead() {
        let run = run("\\warn oops");
        assert_eq!(run.stdout, "");
        assert_eq!(run.stderr, "oops\n");
    }

    #[test]
    fn set_stores_the_concatenation_of_the_remaining_arguments() {
        // `command.c:2899`.
        let run = run("\\set x a b");
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(run.vars.get("x"), Some("ab"));
    }

    #[test]
    fn set_with_no_arguments_lists_the_variables() {
        let run = run("\\set");
        assert!(run.stdout.contains("AUTOCOMMIT = "), "{}", run.stdout);
    }

    #[test]
    fn set_of_a_hooked_variable_is_validated() {
        let run = run("\\set ECHO sideways");
        assert_eq!(run.result, CommandResult::Error);
        assert!(
            run.stderr
                .starts_with("psql: error: unrecognized value \"sideways\""),
            "{}",
            run.stderr
        );
    }

    #[test]
    fn unset_removes_a_variable() {
        assert_eq!(run("\\set x 1").vars.get("x"), Some("1"));
        let unset = run("\\unset AUTOCOMMIT");
        assert_eq!(unset.result, CommandResult::SkipLine);
        assert_eq!(unset.vars.get("AUTOCOMMIT"), Some("off"));
    }

    #[test]
    fn connect_collects_up_to_four_arguments_with_dash_meaning_default() {
        let run = run("\\c mydb bob - 5433");
        assert_eq!(
            run.result,
            CommandResult::Connect(Box::new(ConnectRequest {
                dbname: Some("mydb".into()),
                user: Some("bob".into()),
                host: None,
                port: Some("5433".into()),
            }))
        );
    }

    #[test]
    fn an_unknown_command_is_reported_the_way_upstream_reports_it() {
        let run = run("\\nosuch");
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(run.stderr, "psql: error: invalid command \\nosuch\n");
    }

    #[test]
    fn extra_arguments_draw_a_warning() {
        // `command.c:282`.
        let run = run("\\q one");
        assert_eq!(
            run.stderr,
            "psql: warning: \\q: extra argument \"one\" ignored\n"
        );
    }

    /// Run one backslash command through the whole dispatch sequence, the way
    /// both `MainLoop` and the `-c \…` action do.
    fn dispatch(line: &str) -> (CommandResult, PsqlSettings, String) {
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings::default();
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let mut executor = Canned {
            answers: None,
            seen: Vec::new(),
        };
        let status = dispatch_slash(
            &mut scanner,
            &mut pset,
            &mut vars,
            &mut executor,
            &mut stdout,
            &mut stderr,
        );

        (status, pset, String::from_utf8(stderr).unwrap())
    }

    #[test]
    fn pset_sets_a_print_option_and_the_dispatcher_keeps_it() {
        let (status, pset, stderr) = dispatch("\\pset border 2");
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(stderr, "");
        assert_eq!(pset.popt.topt.border, 2);
    }

    #[test]
    fn pset_reports_the_new_state_and_lists_everything_without_arguments() {
        assert_eq!(
            run("\\pset format unaligned").stdout,
            "Output format is unaligned.\n"
        );
        let listing = run("\\pset").stdout;
        assert!(
            listing.starts_with("border                   1\n"),
            "{listing}"
        );
        assert_eq!(listing.lines().count(), crate::pset::PSET_LIST.len());
    }

    #[test]
    fn a_refused_pset_is_an_error_and_a_third_argument_is_ignored_with_a_warning() {
        let refused = run("\\pset nosuch");
        assert_eq!(refused.result, CommandResult::Error);
        assert_eq!(
            refused.stderr,
            "psql: error: \\pset: unknown option: nosuch\n"
        );
        let extra = run("\\pset border 0 extra");
        assert_eq!(extra.result, CommandResult::SkipLine);
        assert_eq!(
            extra.stderr,
            "psql: warning: \\pset: extra argument \"extra\" ignored\n"
        );
    }

    #[test]
    fn a_connect_is_refused_with_one_message_for_every_caller() {
        // The two dispatch sites used to each spell this string themselves,
        // which in a port judged by byte-identical output is a drift waiting
        // to happen.
        let (status, _, stderr) = dispatch("\\c other");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(
            stderr,
            "psql: error: \\connect is not implemented yet (Linear NAT-405)\n"
        );
    }

    #[test]
    fn a_set_through_the_dispatcher_refreshes_the_settings_it_owns() {
        let (status, pset, stderr) = dispatch("\\set ECHO all");
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(pset.echo, crate::settings::Echo::All);
        assert_eq!(stderr, "");
    }

    #[test]
    fn a_quit_still_reaches_the_caller_as_terminate() {
        assert_eq!(dispatch("\\q").0, CommandResult::Terminate);
    }

    #[test]
    fn echo_text_is_pure() {
        let opt = |value: &str, quote| SlashOption {
            value: value.to_string(),
            quote,
            unquoted_tail: 0,
        };
        assert_eq!(echo_text(&[opt("a", None), opt("b", None)]), b"a b\n");
        assert_eq!(echo_text(&[opt("-n", None), opt("a", None)]), b"a");
        assert_eq!(echo_text(&[]), b"\n");
    }

    /// A `\\d` answer: `text` columns and rows, as the wire delivers them.
    fn relations(headers: &[&str], rows: &[&[&str]]) -> Vec<QueryResult> {
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::RowDescription(
                headers
                    .iter()
                    .map(|h| FieldDescription {
                        name: h.as_bytes().to_vec(),
                        tableid: 0,
                        columnid: 0,
                        typid: 19,
                        typlen: 64,
                        atttypmod: -1,
                        format: 0,
                    })
                    .collect(),
            ))
            .unwrap();
        for row in rows {
            runner
                .push(Backend::DataRow(
                    row.iter().map(|c| Some(c.as_bytes().to_vec())).collect(),
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
        runner.into_results()
    }

    fn pg18() -> PsqlSettings {
        PsqlSettings {
            sversion: 180_006,
            ..PsqlSettings::default()
        }
    }

    const LISTING: [&str; 4] = ["Schema", "Name", "Type", "Owner"];

    #[test]
    fn dt_prints_its_listing_under_its_title() {
        let answer = relations(&LISTING, &[&["public", "t", "table", "me"]]);
        let (run, seen) = run_with("\\dt", pg18(), Some(vec![answer]));
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(seen.len(), 1);
        assert!(
            seen[0].contains("WHERE c.relkind IN ('r','p','')\n"),
            "{}",
            seen[0]
        );
        assert_eq!(
            run.stdout,
            "        List of tables\n \
             Schema | Name | Type  | Owner \n\
             --------+------+-------+-------\n \
             public | t    | table | me\n\
             (1 row)\n\n"
        );
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn the_pattern_loses_its_trailing_semicolon() {
        let answer = relations(&LISTING, &[]);
        let (_, seen) = run_with("\\dv foo;", pg18(), Some(vec![answer]));
        assert!(
            seen[0].contains("c.relname OPERATOR(pg_catalog.~) '^(foo)$'"),
            "{}",
            seen[0]
        );
    }

    #[test]
    fn nothing_found_is_an_error_message_unless_quiet() {
        // `describe.c:4179`: psql says so, and the command still succeeds.
        let (run, _) = run_with("\\dv foo", pg18(), Some(vec![relations(&LISTING, &[])]));
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(run.stdout, "");
        assert_eq!(
            run.stderr,
            "psql: error: Did not find any views named \"foo\".\n"
        );
        // Quiet, the empty table is printed instead.
        let quiet = PsqlSettings {
            quiet: true,
            ..pg18()
        };
        let (run, _) = run_with("\\d", quiet, Some(vec![relations(&LISTING, &[])]));
        assert!(
            run.stdout.starts_with("      List of relations\n"),
            "{}",
            run.stdout
        );
        assert!(run.stdout.ends_with("(0 rows)\n\n"), "{}", run.stdout);
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn x_expands_the_listing_for_this_command_only() {
        let answer = relations(&LISTING, &[&["public", "t", "table", "me"]]);
        let (run, _) = run_with("\\dtx", pg18(), Some(vec![answer]));
        assert_eq!(
            run.stdout,
            "List of tables\n\
             -[ RECORD 1 ]--\n\
             Schema | public\n\
             Name   | t\n\
             Type   | table\n\
             Owner  | me\n\n"
        );
    }

    #[test]
    fn echo_hidden_shows_the_query_and_noexec_stops_there() {
        let noexec = PsqlSettings {
            echo_hidden: EchoHidden::NoExec,
            ..pg18()
        };
        let (run, seen) = run_with("\\dt", noexec, Some(vec![]));
        assert!(seen.is_empty());
        assert!(
            run.stdout
                .starts_with("/******** QUERY *********/\nSELECT n.nspname"),
            "{}",
            run.stdout
        );
        assert!(
            run.stdout
                .ends_with("ORDER BY 1,2;\n/************************/\n\n"),
            "{}",
            run.stdout
        );
        // `PSQLexec` returns NULL, so the command fails (`describe.c:4169`-`:4172`).
        assert_eq!(run.result, CommandResult::Error);
    }

    #[test]
    fn a_bad_pattern_fails_without_a_query() {
        let (run, seen) = run_with("\\dt a.b.c.d", pg18(), Some(vec![]));
        assert!(seen.is_empty());
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(
            run.stderr,
            "psql: error: improper qualified name (too many dotted names): a.b.c.d\n"
        );
    }

    #[test]
    fn without_a_connection_d_says_so() {
        let (run, _) = run_with("\\dt", pg18(), None);
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(
            run.stderr,
            "psql: error: You are currently not connected to a database.\n"
        );
    }

    #[test]
    fn da_reads_one_pattern_and_warns_about_a_second() {
        // `\dA foo bar` (`psql.sql:1336`): `\dA` itself never reads `bar`.
        let answer = relations(&["Name", "Type"], &[]);
        let (run, seen) = run_with("\\dA foo bar", pg18(), Some(vec![answer]));
        assert_eq!(run.result, CommandResult::SkipLine);
        assert!(
            seen[0].contains("amname OPERATOR(pg_catalog.~) '^(foo)$'"),
            "{}",
            seen[0]
        );
        assert!(!seen[0].contains("bar"), "{}", seen[0]);
        assert!(
            run.stdout.starts_with("List of access methods\n"),
            "{}",
            run.stdout
        );
        assert_eq!(
            run.stderr,
            "psql: warning: \\dA: extra argument \"bar\" ignored\n"
        );
    }

    #[test]
    fn dac_reads_a_second_pattern_and_warns_about_a_third() {
        let answer = relations(&["AM", "Input type"], &[]);
        let (run, seen) = run_with("\\dAc brin int4; x", pg18(), Some(vec![answer]));
        assert_eq!(run.result, CommandResult::SkipLine);
        assert!(seen[0].contains("'^(brin)$'"), "{}", seen[0]);
        // The second pattern loses its trailing semicolon too.
        assert!(
            seen[0].contains("t.typname OPERATOR(pg_catalog.~) '^(int4)$'"),
            "{}",
            seen[0]
        );
        assert!(
            run.stdout.starts_with("List of operator classes\n"),
            "{}",
            run.stdout
        );
        assert_eq!(
            run.stderr,
            "psql: warning: \\dAc: extra argument \"x\" ignored\n"
        );
    }

    #[test]
    fn dp_prints_its_listing_under_its_title() {
        let answer = relations(&["Schema", "Name", "Owner"], &[&["s", "p", "me"]]);
        let (run, seen) = run_with("\\dPt", pg18(), Some(vec![answer]));
        assert_eq!(run.result, CommandResult::SkipLine);
        assert!(
            seen[0].contains("WHERE c.relkind IN ('p','')\n"),
            "{}",
            seen[0]
        );
        assert!(
            run.stdout.starts_with("List of partitioned tables\n"),
            "{}",
            run.stdout
        );
        // An empty listing is printed, not reported: `listPartitionedTables`
        // has no not-found message.
        let (run, _) = run_with("\\dP", pg18(), Some(vec![relations(&["Schema"], &[])]));
        assert!(run.stdout.ends_with("(0 rows)\n\n"), "{}", run.stdout);
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn a_server_too_old_is_an_error_message_yet_the_command_succeeds() {
        // `describe.c:155`-`:163`: `pg_log_error`, then `return true`.
        let old = PsqlSettings {
            sversion: 90_524,
            ..PsqlSettings::default()
        };
        let (run, seen) = run_with("\\dA", old, Some(vec![]));
        assert!(seen.is_empty());
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(
            run.stderr,
            "psql: error: The server (version 9.5) does not support access methods.\n"
        );
    }

    #[test]
    fn a_bad_access_method_pattern_fails_the_command() {
        let (run, seen) = run_with("\\dAo regression.brin", pg18(), Some(vec![]));
        assert!(seen.is_empty());
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(
            run.stderr,
            "psql: error: improper qualified name (too many dotted names): regression.brin\n"
        );
    }

    #[test]
    fn df_reads_every_argument_type_and_warns_about_none() {
        // `\\df has_database_privilege oid text -` (`psql.sql:1364`).
        let answer = relations(&["Schema", "Name"], &[]);
        let (run, seen) = run_with(
            "\\df has_database_privilege oid text -",
            pg18(),
            Some(vec![answer]),
        );
        assert_eq!(run.result, CommandResult::SkipLine);
        assert!(
            seen[0].contains("  AND t2.typname IS NULL\n"),
            "{}",
            seen[0]
        );
        assert!(
            run.stdout.starts_with("List of functions\n"),
            "{}",
            run.stdout
        );
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn df_past_func_max_args_warns_about_the_rest() {
        let args = vec!["int"; 102].join(" ");
        let answer = relations(&["Schema", "Name"], &[]);
        let (run, seen) = run_with(&format!("\\df f {args}"), pg18(), Some(vec![answer]));
        assert!(seen[0].contains("t99.typname"), "{}", seen[0]);
        assert!(!seen[0].contains("t100"), "{}", seen[0]);
        assert_eq!(
            run.stderr,
            "psql: warning: \\df: extra argument \"int\" ignored\n\
             psql: warning: \\df: extra argument \"int\" ignored\n"
        );
    }

    #[test]
    fn a_letter_df_does_not_take_is_an_error_message_yet_the_command_succeeds() {
        // `describe.c:314`-`:318`: `pg_log_error`, then `return true`.
        let (run, seen) = run_with("\\dfnq", pg18(), Some(vec![]));
        assert!(seen.is_empty());
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(
            run.stderr,
            "psql: error: \\df only takes [anptwSx+] as options\n"
        );
    }

    #[test]
    fn do_da_dt_and_dconfig_print_under_their_titles() {
        for (cmd, title) in [
            ("\\do - int4", "List of operators"),
            ("\\da", "List of aggregate functions"),
            ("\\dT+ mood", "List of data types"),
            ("\\dconfig", "List of non-default configuration parameters"),
            ("\\dconfig+ work_mem;", "List of configuration parameters"),
        ] {
            let answer = relations(&["Name"], &[]);
            let (run, seen) = run_with(cmd, pg18(), Some(vec![answer]));
            assert_eq!(run.result, CommandResult::SkipLine, "{cmd}");
            assert_eq!(seen.len(), 1, "{cmd}");
            assert!(run.stdout.starts_with(title), "{cmd}: {}", run.stdout);
            assert_eq!(run.stderr, "", "{cmd}");
        }
        // `\\dconfig`'s pattern loses its trailing semicolon too.
        let (_, seen) = run_with(
            "\\dconfig work_mem;",
            pg18(),
            Some(vec![relations(&["Name"], &[])]),
        );
        assert!(seen[0].contains("'^(work_mem)$'"), "{}", seen[0]);
    }

    /// `describeRoles()`'s columns at 18, without `+`.
    const ROLE_COLUMNS: [&str; 10] = [
        "rolname",
        "rolsuper",
        "rolinherit",
        "rolcreaterole",
        "rolcreatedb",
        "rolcanlogin",
        "rolconnlimit",
        "rolvaliduntil",
        "rolreplication",
        "rolbypassrls",
    ];

    #[test]
    fn du_and_dg_fold_each_role_into_attributes_and_print_no_footer() {
        for cmd in ["\\du regress_du_role*", "\\dg regress_du_role*"] {
            let answer = relations(
                &ROLE_COLUMNS,
                &[
                    &[
                        "regress_du_role0",
                        "f",
                        "t",
                        "f",
                        "f",
                        "f",
                        "-1",
                        "",
                        "f",
                        "f",
                    ],
                    &["su", "t", "t", "f", "f", "t", "2", "", "f", "t"],
                ],
            );
            let (run, seen) = run_with(cmd, pg18(), Some(vec![answer]));
            assert_eq!(run.result, CommandResult::SkipLine, "{cmd}");
            assert!(seen[0].starts_with("SELECT r.rolname,"), "{}", seen[0]);
            assert_eq!(
                run.stdout,
                "              List of roles\n    \
                 Role name     |      Attributes       \n\
                 ------------------+-----------------------\n \
                 regress_du_role0 | Cannot login\n \
                 su               | Superuser, Bypass RLS+\n  \
                 \x20               | 2 connections\n\n",
                "{cmd}"
            );
            assert_eq!(run.stderr, "", "{cmd}");
        }
    }

    #[test]
    fn a_dotted_role_pattern_fails_du_without_a_query() {
        let (run, seen) = run_with("\\du a.b", pg18(), Some(vec![]));
        assert!(seen.is_empty());
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(
            run.stderr,
            "psql: error: improper qualified name (too many dotted names): a.b\n"
        );
    }

    #[test]
    fn drds_reads_a_second_pattern_and_says_when_nothing_matched() {
        let empty = || Some(vec![relations(&["Role", "Database", "Settings"], &[])]);
        let (run, seen) = run_with("\\drds r d; extra", pg18(), empty());
        assert!(seen[0].contains("'^(d)$'"), "{}", seen[0]);
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(run.stdout, "");
        assert_eq!(
            run.stderr,
            "psql: error: Did not find any settings for role \"r\" and database \"d\".\n\
             psql: warning: \\drds: extra argument \"extra\" ignored\n"
        );
        let (run, _) = run_with("\\drds r", pg18(), empty());
        assert_eq!(
            run.stderr,
            "psql: error: Did not find any settings for role \"r\".\n"
        );
        let (run, _) = run_with("\\drds", pg18(), empty());
        assert_eq!(run.stderr, "psql: error: Did not find any settings.\n");
        // Quiet, the empty table is printed instead.
        let quiet = PsqlSettings {
            quiet: true,
            ..pg18()
        };
        let (run, _) = run_with("\\drds", quiet, empty());
        assert!(
            run.stdout.starts_with("      List of settings\n"),
            "{}",
            run.stdout
        );
        assert_eq!(run.stderr, "");
    }

    #[test]
    fn dp_ddp_drg_and_dd_print_under_their_titles() {
        for (cmd, title) in [
            ("\\dp", "Access privileges"),
            ("\\ddp", "Default access privileges"),
            ("\\drg", "List of role grants"),
            ("\\dD+", "List of domains"),
        ] {
            let answer = relations(&["Name"], &[]);
            let (run, seen) = run_with(cmd, pg18(), Some(vec![answer]));
            assert_eq!(run.result, CommandResult::SkipLine, "{cmd}");
            assert_eq!(seen.len(), 1, "{cmd}");
            assert!(run.stdout.starts_with(title), "{cmd}: {}", run.stdout);
            assert_eq!(run.stderr, "", "{cmd}");
        }
    }

    #[test]
    fn z_is_dp_in_exactly_five_spellings() {
        let answer = || Some(vec![relations(&["Schema", "Name"], &[&["public", "t"]])]);
        for cmd in ["\\z", "\\zS", "\\zx", "\\zSx", "\\zxS"] {
            let (run, seen) = run_with(&format!("{cmd} t; u"), pg18(), answer());
            assert_eq!(run.result, CommandResult::SkipLine, "{cmd}");
            assert!(seen[0].contains("'^(t)$'"), "{cmd}: {}", seen[0]);
            // `S` only matters without a pattern.
            let (_, seen) = run_with(cmd, pg18(), answer());
            assert_eq!(
                seen[0].contains("<> 'pg_catalog'"),
                !cmd.contains('S'),
                "{cmd}"
            );
            let expanded = run.stdout.contains("-[ RECORD 1 ]");
            assert_eq!(expanded, cmd.contains('x'), "{cmd}: {}", run.stdout);
            assert!(
                run.stdout.starts_with("Access privileges\n")
                    || run.stdout.starts_with(" Access privileges\n"),
                "{cmd}: {}",
                run.stdout
            );
            assert_eq!(
                run.stderr,
                format!(
                    "psql: warning: \\{}: extra argument \"u\" ignored\n",
                    &cmd[1..]
                ),
            );
        }
        for cmd in ["\\z+", "\\zSS", "\\zxx", "\\zp"] {
            let run = run(cmd);
            assert_eq!(run.result, CommandResult::Error, "{cmd}");
            assert_eq!(
                run.stderr,
                format!("psql: error: invalid command {cmd}\n"),
                "{cmd}"
            );
        }
    }

    #[test]
    fn an_unported_d_command_is_refused_by_name_and_an_unknown_one_is_invalid() {
        let refused = run("\\dn");
        assert_eq!(refused.result, CommandResult::Error);
        assert_eq!(
            refused.stderr,
            "psql: error: \\dn: listSchemas is not implemented yet (Linear NAT-401)\n"
        );
        let details = run("\\d t");
        assert_eq!(
            details.stderr,
            "psql: error: \\d: describeTableDetails is not implemented yet (Linear NAT-401)\n"
        );
        let unknown = run("\\dz");
        assert_eq!(unknown.stderr, "psql: error: invalid command \\dz\n");
    }
}
