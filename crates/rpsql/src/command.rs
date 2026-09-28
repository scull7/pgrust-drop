//! The backslash-command dispatcher: `src/bin/psql/command.c`.
//!
//! `HandleSlashCmds` (`command.c:231`) parses the command name, dispatches,
//! then eats whatever arguments are left over with a warning. This issue
//! implements the four commands it names — `\q`, `\c`, `\echo` and `\set` —
//! plus the `\unset`, `\qecho` and `\warn` that share their code;
//! NAT-400 adds `\pset`, NAT-403 `\timing`, `\errverbose`, `\copy`, `\o`,
//! `\getenv`, `\g`'s file or pipe and `\watch`,
//! NAT-404 `\crosstabview`, `\g`, `\gx`, the extended-query commands `\parse`,
//! `\bind`, `\bind_named` and `\close_prepared`, and the pipeline commands
//! `\startpipeline`, `\sendpipeline`, `\syncpipeline`, `\flush`,
//! `\flushrequest`, `\getresults` and `\endpipeline`, and NAT-396 the
//! `\lo_*` commands ([`crate::large_obj`]). Everything else is
//! [`CommandResult::Unknown`], which renders upstream's `invalid command \%s`;
//! NAT-401 … NAT-403 fill the table in.

use std::io::Write;

use rlibpq::{ContextVisibility, PipelineStatus, Verbosity};

use crate::common::{CommandSource, Executor};
use crate::crosstab::CtvArgs;
use crate::logging;
use crate::output::Output;
use crate::scan::{Scanner, VariableSource};
use crate::settings::{Expanded, PsqlSettings, SendMode};
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
    /// `\copy`'s whole line, for `do_copy`, which needs the connection and
    /// so is performed by [`dispatch_slash`] (`command.c:963`).
    Copy(Option<Vec<u8>>),
    /// `\watch`'s parsed arguments, for `do_watch`, or `None` when they were
    /// refused (and the refusal logged). Either way the query buffer is
    /// reset afterwards, so the caller that holds it performs the rest
    /// (`command.c:3504`-`:3514`).
    Watch(Option<crate::watch::WatchArgs>),
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
    /// `PQpipelineStatus(pset.db)`, which `\g`, `\gx` and `\sendpipeline`
    /// read. It only changes while a query is sent, never during a command.
    pub pipeline: PipelineStatus,
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
/// thing. [`CommandResult::Copy`] never comes back either: `\copy` runs here,
/// over `executor`, with `source` as `pset.cur_cmd_source`.
pub fn dispatch_slash(
    scanner: &mut Scanner,
    pset: &mut PsqlSettings,
    vars: &mut VariableSpace,
    executor: &mut dyn Executor,
    source: &mut CommandSource<'_>,
    out: &mut Output<'_>,
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
            pipeline: executor.pipeline_status(),
            executor: &mut *executor,
        };
        handle_slash_cmds(scanner, &mut ctx, &VarView(&snapshot), out, stderr)
    };
    // `\pset` wrote `working.popt`; a `\set` of a hooked variable wrote the
    // variable space, whose settings are re-derived on top.
    *pset = vars.settings(&working);

    if matches!(status, CommandResult::Connect(_)) {
        logging::error(
            pset,
            "\\connect is not implemented yet (Linear NAT-405)",
            stderr,
        );
        return CommandResult::Error;
    }
    if let CommandResult::Copy(args) = status {
        // `exec_command_copy()` (`command.c:963`).
        return if crate::copy::do_copy(args.as_deref(), executor, pset, vars, source, out, stderr) {
            CommandResult::SkipLine
        } else {
            CommandResult::Error
        };
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
    out: &mut Output<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    let cmd = scanner.slash_command();
    // `\copy`, in any case (`command.c:354`), takes its line whole
    // (`OT_WHOLE_LINE`, `:970`); every other command here takes `OT_NORMAL`
    // arguments.
    if cmd.eq_ignore_ascii_case("copy") {
        let line = scanner.slash_option_whole_line();
        scanner.slash_command_end();
        return CommandResult::Copy(line);
    }
    // `\g` and `\o` take a file or a `|command` first (`OT_FILEPIPE`,
    // `command.c:1749`, `:2466`), and `\o` strips its trailing semicolons.
    let mut options: Vec<SlashOption> = match cmd.as_str() {
        "g" | "gx" => g_options(scanner, vars_view),
        "o" | "out" => scanner
            .slash_option_filepipe(vars_view, true)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    options.extend(scanner.slash_options(vars_view));
    let status = exec_command(&cmd, &options, ctx, out, stderr);

    let status = if status == CommandResult::Unknown {
        logging::error(ctx.pset, format!("invalid command \\{cmd}"), stderr);
        CommandResult::Error
    } else {
        status
    };

    // Eat any remaining arguments after a valid command (`command.c:271`).
    // `slash_options` above already consumed them, so what is left is the
    // warning upstream prints for the ones a command did not use.
    if status != CommandResult::Error {
        for extra in extra_arguments(&cmd, &options) {
            logging::warning(
                ctx.pset,
                format!("\\{cmd}: extra argument \"{extra}\" ignored"),
                stderr,
            );
        }
    }

    scanner.slash_command_end();
    status
}

/// `\g`'s arguments as `exec_command_g` (`command.c:1749`-`:1760`) scans
/// them: the first is `OT_FILEPIPE`; one that opens a `(` is followed by
/// `OT_NORMAL` print options up to the one that closes it
/// (`process_command_g_options`, `command.c:1800`), and then by the file
/// name, `OT_FILEPIPE` again.
fn g_options(scanner: &mut Scanner, vars: &dyn VariableSource) -> Vec<SlashOption> {
    let mut options = Vec::new();
    let Some(first) = scanner.slash_option_filepipe(vars, false) else {
        return options;
    };
    let open = first.value.starts_with('(');
    let mut closed = first.value.ends_with(')');
    options.push(first);
    if open {
        while !closed {
            let Some(option) = scanner.slash_option(vars) else {
                break;
            };
            closed = option.value.ends_with(')');
            options.push(option);
        }
        options.extend(scanner.slash_option_filepipe(vars, false));
    }
    options
}

/// How many arguments each implemented command consumes; the rest draw
/// upstream's "extra argument … ignored" warning.
fn extra_arguments<'a>(cmd: &str, options: &'a [SlashOption]) -> Vec<&'a str> {
    let takes = match cmd {
        // `\echo` and friends take everything, and so do `\bind` and
        // `\bind_named`, whose parameters run to the end of the command, and
        // `\watch`'s loop.
        "echo" | "qecho" | "warn" | "set" | "bind" | "bind_named" | "watch" => return Vec::new(),
        "c" | "connect" | "crosstabview" => 4,
        "pset" | "getenv" => 2,
        "unset" | "timing" | "parse" | "close_prepared" | "getresults" | "o" | "out" => 1,
        "g" | "gx" => GArgs::split(options).consumed,
        // `exec_command_lo` always reads two (`command.c:2378`-`:2381`).
        _ if cmd.starts_with("lo_") => 2,
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
    out: &mut Output<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    match cmd {
        // `exec_command_quit()` (`command.c:2750`).
        "q" | "quit" => CommandResult::Terminate,
        // `exec_command_bind()` (`command.c:520`): every argument is a text
        // parameter for the next query, which goes out through
        // `PQsendQueryParams`; nothing is sent yet.
        "bind" => {
            ctx.pset.send_mode = SendMode::ExtendedQueryParams {
                params: options.iter().map(|o| o.value.clone()).collect(),
            };
            CommandResult::SkipLine
        }
        // `exec_command_bind_named()` (`command.c:556`): the same, for a
        // statement `\parse` prepared.
        "bind_named" => {
            // `clean_extended_state()` first (`command.c:567`), so a failed
            // call also forgets what an earlier one set.
            ctx.pset.send_mode = SendMode::Query;
            let Some((name, params)) = options.split_first() else {
                return missing_required_argument(cmd, ctx.pset, stderr);
            };
            ctx.pset.send_mode = SendMode::ExtendedQueryPrepared {
                statement: name.value.clone(),
                params: params.iter().map(|o| o.value.clone()).collect(),
            };
            CommandResult::SkipLine
        }
        // `exec_command_close_prepared()` (`command.c:755`) and
        // `exec_command_parse()` (`command.c:2508`): name the statement,
        // and send.
        "close_prepared" | "parse" => {
            ctx.pset.send_mode = SendMode::Query;
            let Some(name) = options.first() else {
                return missing_required_argument(cmd, ctx.pset, stderr);
            };
            let statement = name.value.clone();
            ctx.pset.send_mode = if cmd == "parse" {
                SendMode::ExtendedParse { statement }
            } else {
                SendMode::ExtendedClose { statement }
            };
            CommandResult::Send
        }
        // `exec_command_g()` (`command.c:1739`).
        "g" | "gx" => exec_command_g(cmd, options, ctx, stderr),
        // `exec_command_startpipeline()` (`command.c:3065`),
        // `exec_command_syncpipeline()` (`:3084`),
        // `exec_command_endpipeline()` (`:3103`), `exec_command_flush()`
        // (`:1695`) and `exec_command_flushrequest()` (`:1714`): each is a
        // send mode, and sends.
        "startpipeline" | "syncpipeline" | "endpipeline" | "flush" | "flushrequest" => {
            ctx.pset.send_mode = match cmd {
                "startpipeline" => SendMode::StartPipelineMode,
                "syncpipeline" => SendMode::PipelineSync,
                "endpipeline" => SendMode::EndPipelineMode,
                "flush" => SendMode::Flush,
                _ => SendMode::FlushRequest,
            };
            CommandResult::Send
        }
        // `exec_command_getresults()` (`command.c:1929`).
        "getresults" => exec_command_getresults(options, ctx, stderr),
        // `exec_command_sendpipeline()` (`command.c:2844`).
        "sendpipeline" => exec_command_sendpipeline(ctx, stderr),
        // `exec_command_connect()` (`command.c:638`).
        "c" | "connect" => CommandResult::Connect(Box::new(ConnectRequest::from_options(options))),
        // `exec_command_crosstabview()` (`command.c:997`): keep up to four
        // arguments for the next `SendQuery`, and send.
        "crosstabview" => {
            let mut args = CtvArgs::default();
            for (slot, option) in args.0.iter_mut().zip(options) {
                *slot = Some(option.value.clone());
            }
            ctx.pset.crosstab = Some(args);
            CommandResult::Send
        }
        // `exec_command_echo()` (`command.c:1559`): `\qecho` writes to
        // `pset.queryFout`, `\warn` to stderr, `\echo` to stdout
        // (`command.c:1569`-`:1574`).
        "echo" | "qecho" | "warn" => {
            let sink: &mut dyn Write = match cmd {
                "warn" => stderr,
                "qecho" => out.query_fout(),
                _ => &mut *out.stdout,
            };
            let _ = sink.write_all(&echo_text(options));
            CommandResult::SkipLine
        }
        // `exec_command_getenv()` (`command.c:1892`).
        "getenv" => exec_command_getenv(cmd, options, ctx, stderr),
        // `exec_command_out()` (`command.c:2459`).
        "o" | "out" => {
            let fname = options.first().map(|o| filename(&o.value));
            if out.set_query_fout(fname.as_deref(), ctx.pset, ctx.vars, stderr) {
                CommandResult::SkipLine
            } else {
                CommandResult::Error
            }
        }
        // `exec_command_pset()` (`command.c:2695`).
        "pset" => exec_command_pset(options, ctx, &mut *out.stdout, stderr),
        // `exec_command_set()` (`command.c:2881`).
        "set" => exec_command_set(options, ctx, &mut *out.stdout, stderr),
        // `exec_command_unset()` (`command.c:3238`).
        "unset" => {
            let Some(name) = options.first() else {
                logging::error(ctx.pset, "\\unset: missing required argument", stderr);
                return CommandResult::Error;
            };
            match ctx.vars.delete(&name.value) {
                Ok(()) => CommandResult::SkipLine,
                Err(err) => {
                    logging::error(ctx.pset, &err.message, stderr);
                    CommandResult::Error
                }
            }
        }
        // `exec_command_timing()` (`command.c:3166`).
        "timing" => exec_command_timing(options, ctx, &mut *out.stdout, stderr),
        // `exec_command_errverbose()` (`command.c:1643`).
        "errverbose" => {
            exec_command_errverbose(ctx.pset, &mut *out.stdout, stderr);
            CommandResult::SkipLine
        }
        // `exec_command_watch()` (`command.c:3370`): the arguments here, the
        // runs where the query buffer is.
        "watch" => exec_command_watch(options, ctx, stderr),
        // `exec_command_lo()` (`command.c:2368`), for every `lo_` command
        // (`command.c:417`).
        _ if cmd.starts_with("lo_") => {
            crate::large_obj::exec_command_lo(cmd, options, ctx, out, stderr)
        }
        _ => CommandResult::Unknown,
    }
}

/// `exec_command_watch()` (`command.c:3370`), up to `do_watch`: refused in
/// a pipeline, else the arguments parsed. `MainLoop` runs the query and
/// resets the buffer, whether or not they were accepted.
fn exec_command_watch(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    if ctx.pipeline != PipelineStatus::Off {
        // `command.c:3384`: refused before any argument is read.
        logging::error(ctx.pset, "\\watch not allowed in pipeline mode", stderr);
        // `clean_extended_state()`.
        ctx.pset.send_mode = SendMode::Query;
        return CommandResult::Watch(None);
    }
    let values: Vec<&str> = options.iter().map(|o| o.value.as_str()).collect();
    match crate::watch::parse_watch_args(&values, ctx.pset.watch_interval) {
        Ok(args) => CommandResult::Watch(Some(args)),
        Err(message) => {
            logging::error(ctx.pset, message, stderr);
            CommandResult::Watch(None)
        }
    }
}

/// `exec_command_getresults()` (`command.c:1929`): read the requested
/// number of pipeline results, or all of them. The send mode is set before
/// the count is read, and stays set if it is refused.
fn exec_command_getresults(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    ctx.pset.send_mode = SendMode::GetResults;
    ctx.pset.pipeline.requested_results = 0;
    if let Some(option) = options.first() {
        let Ok(requested) = usize::try_from(atoi(&option.value)) else {
            logging::error(
                ctx.pset,
                "\\getresults: invalid number of requested results",
                stderr,
            );
            return CommandResult::Error;
        };
        ctx.pset.pipeline.requested_results = requested;
    }
    CommandResult::Send
}

/// `exec_command_sendpipeline()` (`command.c:2844`): send what `\bind` or
/// `\bind_named` prepared, into the pipeline.
fn exec_command_sendpipeline(
    ctx: &mut CommandContext<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    let refusal: &[u8] = if ctx.pipeline == PipelineStatus::Off {
        b"\\sendpipeline not allowed outside of pipeline mode"
    } else if matches!(
        ctx.pset.send_mode,
        SendMode::ExtendedQueryParams { .. } | SendMode::ExtendedQueryPrepared { .. }
    ) {
        return CommandResult::Send;
    } else {
        b"\\sendpipeline must be used after \\bind or \\bind_named"
    };
    logging::error(ctx.pset, refusal, stderr);
    // `clean_extended_state()`.
    ctx.pset.send_mode = SendMode::Query;
    CommandResult::Error
}

/// C's `atoi`, which `\getresults` reads its count with: leading
/// whitespace, an optional sign and the digits after it; anything else ends
/// the number, and no digits at all is 0.
#[must_use]
pub fn atoi(text: &str) -> i64 {
    let text = text.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = digits
        .bytes()
        .take_while(u8::is_ascii_digit)
        .fold(0_i64, |n, d| {
            n.saturating_mul(10).saturating_add(i64::from(d - b'0'))
        });
    if negative { -magnitude } else { magnitude }
}

/// `pg_log_error("\\%s: missing required argument", cmd)`, the refusal every
/// command with a mandatory argument shares.
fn missing_required_argument(
    cmd: &str,
    pset: &PsqlSettings,
    stderr: &mut dyn Write,
) -> CommandResult {
    logging::error(pset, format!("\\{cmd}: missing required argument"), stderr);
    CommandResult::Error
}

/// `\g`'s arguments, `[(pset-option[=pset-value] ...)] [filename]`, as
/// `exec_command_g` (`command.c:1739`) and `process_command_g_options`
/// (`command.c:1800`) read them.
#[derive(Debug, PartialEq, Eq)]
struct GArgs<'a> {
    /// The parenthesized print options, each `name` or `name=value`, with
    /// the parentheses stripped. An empty one (`(` or `)` standing alone)
    /// is not here, as upstream skips it (`command.c:1837`).
    psets: Vec<&'a str>,
    /// An opening `(` was never closed (`command.c:1822`).
    unclosed: bool,
    /// The file name or `|command` after them.
    fname: Option<&'a str>,
    /// How many arguments `\g` consumed; the rest draw the "extra argument"
    /// warning.
    consumed: usize,
}

impl<'a> GArgs<'a> {
    /// The pure half of `exec_command_g`: split the arguments into print
    /// options and a file name.
    fn split(options: &'a [SlashOption]) -> Self {
        let mut args = GArgs {
            psets: Vec::new(),
            unclosed: false,
            fname: None,
            consumed: 0,
        };
        let mut next = options.iter().map(|o| o.value.as_str());
        let mut first = next.next();
        if let Some(open) = first.and_then(|f| f.strip_prefix('(')) {
            args.consumed += 1;
            let mut option = Some(open);
            loop {
                let Some(o) = option else {
                    args.unclosed = true;
                    break;
                };
                let (o, closed) = match o.strip_suffix(')') {
                    Some(o) => (o, true),
                    None => (o, false),
                };
                if !o.is_empty() {
                    args.psets.push(o);
                }
                if closed {
                    break;
                }
                option = next.next();
                if option.is_some() {
                    args.consumed += 1;
                }
            }
            first = next.next();
        }
        if let Some(fname) = first {
            args.fname = Some(fname);
            args.consumed += 1;
        }
        args
    }
}

/// `exec_command_g()` (`command.c:1739`): send the query buffer, with the
/// parenthesized print options in force for this one query, for `\gx`
/// expanded output on, and its tuples going to the file or pipe named, if
/// one is (`pset.gfname`).
fn exec_command_g(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    let args = GArgs::split(options);

    // `process_command_g_options()` (`command.c:1800`): save the settings
    // once, apply every option quietly, and put them back if any failed.
    let mut success = true;
    for option in &args.psets {
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (*option, None),
        };
        if ctx.pset.gsavepopt.is_none() {
            ctx.pset.gsavepopt = Some(ctx.pset.popt.clone());
        }
        if let Err(err) = crate::pset::do_pset(name, value, &mut ctx.pset.popt, true) {
            let message = err.to_string();
            logging::error(ctx.pset, message, stderr);
            success = false;
        }
    }
    if args.unclosed {
        let message = format!("\\{cmd}: missing right parenthesis");
        logging::error(ctx.pset, message, stderr);
        success = false;
    }
    if success && ctx.pipeline != PipelineStatus::Off {
        // `command.c:1764`: refused once the options are in force, which
        // they stay until the next query puts them back; the send mode
        // `\bind` left is cleaned up.
        let message = format!("\\{cmd} not allowed in pipeline mode");
        logging::error(ctx.pset, message, stderr);
        ctx.pset.send_mode = SendMode::Query;
        return CommandResult::Error;
    }
    if !success {
        if let Some(saved) = ctx.pset.gsavepopt.take() {
            ctx.pset.popt = saved;
        }
        return CommandResult::Error;
    }
    // `command.c:1772`-`:1778`.
    ctx.pset.gfname = args.fname.map(filename);
    if cmd == "gx" {
        // Save the settings if not done already, then force expanded=on
        // (`command.c:1779`).
        if ctx.pset.gsavepopt.is_none() {
            ctx.pset.gsavepopt = Some(ctx.pset.popt.clone());
        }
        ctx.pset.popt.topt.expanded = Expanded::On;
    }
    CommandResult::Send
}

/// A `\g` or `\o` argument as a file name: `expand_tilde()`
/// (`common.c:2697`) over `HOME`.
fn filename(value: &str) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    let home = std::env::var_os("HOME");
    crate::copy::expand_tilde(value.as_bytes(), home.as_deref().map(OsStrExt::as_bytes))
}

/// `exec_command_getenv()` (`command.c:1892`): copy an environment variable
/// into a psql variable, if it is set.
fn exec_command_getenv(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    let (Some(myvar), Some(envvar)) = (options.first(), options.get(1)) else {
        return missing_required_argument(cmd, ctx.pset, stderr);
    };
    let Some(value) = std::env::var_os(&envvar.value) else {
        return CommandResult::SkipLine;
    };
    match ctx.vars.set(&myvar.value, Some(&value.to_string_lossy())) {
        Ok(()) => CommandResult::SkipLine,
        Err(err) => {
            logging::error(ctx.pset, &err.message, stderr);
            CommandResult::Error
        }
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
            logging::error(ctx.pset, err.to_string(), stderr);
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
            logging::error(ctx.pset, &err.message, stderr);
            CommandResult::Error
        }
    }
}

/// `exec_command_timing()` (`command.c:3166`): set `\timing` from its
/// argument, or toggle it without one, and say which it is unless quiet.
fn exec_command_timing(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let mut success = true;
    match options.first() {
        Some(opt) => match crate::pset::parse_bool_named(&opt.value, "\\timing") {
            Ok(on) => ctx.pset.timing = on,
            Err(err) => {
                // `ParseVariableBool` leaves the switch alone and logs; the
                // state line below is still printed (`command.c:3179`).
                logging::error(ctx.pset, err.to_string(), stderr);
                success = false;
            }
        },
        None => ctx.pset.timing = !ctx.pset.timing,
    }
    if !ctx.pset.quiet {
        let _ = stdout.write_all(if ctx.pset.timing {
            b"Timing is on.\n"
        } else {
            b"Timing is off.\n"
        });
    }
    if success {
        CommandResult::SkipLine
    } else {
        CommandResult::Error
    }
}

/// `exec_command_errverbose()` (`command.c:1643`): the last failed result,
/// again, at `PQERRORS_VERBOSE` with `PQSHOW_CONTEXT_ALWAYS`.
fn exec_command_errverbose(pset: &PsqlSettings, stdout: &mut dyn Write, stderr: &mut dyn Write) {
    match &pset.last_error_result {
        Some(result) => {
            // `PQresultVerboseErrorMessage()` (`fe-exec.c:3466`).
            let message = match result.error() {
                Some(error) => error.message(
                    result.status(),
                    Verbosity::Verbose,
                    ContextVisibility::Always,
                ),
                None => result.error_message(),
            };
            logging::error(pset, message, stderr);
        }
        None => {
            let _ = stdout.write_all(b"There is no previous error.\n");
        }
    }
}

/// `connection_warnings(in_startup)` (`command.c:4445`): the banner, and a
/// warning when the server is newer than psql or older than 9.2.
/// `server_version` is `PQparameterStatus(pset.db, "server_version")`. There
/// is no SSL or GSS to report in this build, and no Windows code page.
#[must_use]
pub fn connection_warnings(
    pset: &PsqlSettings,
    server_version: Option<&str>,
    in_startup: bool,
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if pset.quiet || pset.notty {
        return out;
    }
    let client_ver = crate::PG_VERSION_NUM;
    if pset.sversion != client_ver {
        let server_version = server_version.map_or_else(
            || format_pg_version_number(pset.sversion, true),
            str::to_string,
        );
        let _ = writeln!(
            out,
            "{} ({}, server {server_version})",
            pset.progname,
            crate::PG_VERSION
        );
    } else if in_startup {
        // For version match, only print psql banner on startup.
        let _ = writeln!(out, "{} ({})", pset.progname, crate::PG_VERSION);
    }
    if pset.sversion / 100 > client_ver / 100 || pset.sversion < 90200 {
        let _ = writeln!(
            out,
            "WARNING: {} major version {}, server major version {}.\n         Some psql features might not work.",
            pset.progname,
            format_pg_version_number(client_ver, false),
            format_pg_version_number(pset.sversion, false)
        );
    }
    out
}

/// `formatPGVersionNumber` (`fe_utils/string_utils.c:313`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ErrorMessage;
    use crate::scan::{NoVariables, ScanResult};
    use rlibpq::QueryResult;

    /// No server: nothing here sends a query.
    struct NoServer;

    impl Executor for NoServer {
        fn exec(
            &mut self,
            _query: &[u8],
            _mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            Err(ErrorMessage::new(b"no connection to the server".to_vec()))
        }
        fn connected(&self) -> bool {
            false
        }
        fn abandon(&mut self) {}
    }

    /// [`NoServer`], with its pipeline in a state a test chooses.
    struct Piped(PipelineStatus);

    impl Executor for Piped {
        fn exec(
            &mut self,
            query: &[u8],
            mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            NoServer.exec(query, mode)
        }
        fn connected(&self) -> bool {
            false
        }
        fn abandon(&mut self) {}
        fn pipeline_status(&self) -> PipelineStatus {
            self.0
        }
    }

    struct Run {
        result: CommandResult,
        stdout: String,
        stderr: String,
        vars: VariableSpace,
    }

    fn run(line: &str) -> Run {
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings::default();
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
                pipeline: PipelineStatus::Off,
                executor: &mut NoServer,
            };
            handle_slash_cmds(
                &mut scanner,
                &mut ctx,
                &NoVariables,
                &mut Output::new(&mut stdout),
                &mut stderr,
            )
        };
        Run {
            result,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            vars,
        }
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

    #[test]
    fn crosstabview_keeps_four_arguments_for_the_next_query_and_sends() {
        // `command.c:997`: the arguments are read as OT_NORMAL, so a double-
        // quoted name keeps its quotes for `dequote_downcase_identifier`.
        let (status, pset, stderr) = dispatch("\\crosstabview v \"month name\" 4 num extra");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(
            pset.crosstab,
            Some(CtvArgs([
                Some("v".into()),
                Some("\"month name\"".into()),
                Some("4".into()),
                Some("num".into()),
            ]))
        );
        assert_eq!(
            stderr,
            "psql: warning: \\crosstabview: extra argument \"extra\" ignored\n"
        );
        let (_, pset, _) = dispatch("\\crosstabview");
        assert_eq!(pset.crosstab, Some(CtvArgs::default()));
    }

    #[test]
    fn under_terse_logging_a_message_has_no_prefix() {
        // What `pg_regress` sees: psql reading a script from stdin
        // (`command.c:4970`), e.g. `psql.out:4724`.
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        let mut scanner = Scanner::new();
        scanner.setup(b"\\lo", true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = dispatch_slash(
            &mut scanner,
            &mut pset,
            &mut vars,
            &mut NoServer,
            &mut CommandSource::file(&mut &b""[..]),
            &mut Output::new(&mut stdout),
            &mut stderr,
        );
        assert_eq!(status, CommandResult::Error);
        assert_eq!(String::from_utf8(stderr).unwrap(), "invalid command \\lo\n");
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

        let status = dispatch_slash(
            &mut scanner,
            &mut pset,
            &mut vars,
            &mut NoServer,
            &mut CommandSource::file(&mut &b""[..]),
            &mut Output::new(&mut stdout),
            &mut stderr,
        );

        (status, pset, String::from_utf8(stderr).unwrap())
    }

    #[test]
    fn copy_takes_the_whole_line_in_any_case_and_reports_its_parse_errors() {
        // `command.c:354` compares with `pg_strcasecmp`; `copy.c:98` and
        // `:253` are the messages. Neither reaches the server.
        let (status, _, stderr) = dispatch("\\COPY");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(stderr, "psql: error: \\copy: arguments required\n");
        let (status, _, stderr) = dispatch("\\copy t sideways 'f' \\\\ \\echo x");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(stderr, "psql: error: \\copy: parse error at \"sideways\"\n");
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

    /// [`dispatch`] on settings a test sets up, and keeps: the extended-query
    /// commands leave their state for the next query to take.
    fn dispatch_on(pset: &mut PsqlSettings, line: &str) -> (CommandResult, String) {
        dispatch_in(pset, line, PipelineStatus::Off)
    }

    /// [`dispatch_on`] with a pipeline in the state `pipeline`.
    fn dispatch_in(
        pset: &mut PsqlSettings,
        line: &str,
        pipeline: PipelineStatus,
    ) -> (CommandResult, String) {
        let mut vars = VariableSpace::new();
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let status = dispatch_slash(
            &mut scanner,
            pset,
            &mut vars,
            &mut Piped(pipeline),
            &mut CommandSource::file(&mut &b""[..]),
            &mut Output::new(&mut stdout),
            &mut stderr,
        );
        (status, String::from_utf8(stderr).unwrap())
    }

    fn terse() -> PsqlSettings {
        PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        }
    }

    #[test]
    fn bind_keeps_every_argument_as_a_text_parameter_and_sends_nothing() {
        // `command.c:520`: `\bind` only sets the mode; `\g` sends.
        let mut pset = terse();
        let (status, stderr) = dispatch_on(&mut pset, "\\bind 'foo' 2 ''");
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(stderr, "");
        assert_eq!(
            pset.send_mode,
            SendMode::ExtendedQueryParams {
                params: vec!["foo".into(), "2".into(), String::new()]
            }
        );
        // `psql.sql:84`: the last `\bind` wins.
        dispatch_on(&mut pset, "\\bind 2");
        assert_eq!(
            pset.send_mode,
            SendMode::ExtendedQueryParams {
                params: vec!["2".into()]
            }
        );
    }

    #[test]
    fn bind_named_takes_a_statement_and_its_parameters() {
        let mut pset = terse();
        let (status, _) = dispatch_on(&mut pset, "\\bind_named stmt3 'foo' 'bar'");
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(
            pset.send_mode,
            SendMode::ExtendedQueryPrepared {
                statement: "stmt3".into(),
                params: vec!["foo".into(), "bar".into()]
            }
        );
    }

    #[test]
    fn a_failed_bind_named_forgets_what_an_earlier_one_set() {
        // `psql.sql:61`-`:65`: "The second call generates an error, cleaning
        // up the statement name set by the first call."
        let mut pset = terse();
        dispatch_on(&mut pset, "\\bind_named stmt4");
        let (status, stderr) = dispatch_on(&mut pset, "\\bind_named");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(stderr, "\\bind_named: missing required argument\n");
        assert_eq!(pset.send_mode, SendMode::Query);
    }

    #[test]
    fn parse_and_close_prepared_name_a_statement_and_send() {
        let mut pset = terse();
        assert_eq!(dispatch_on(&mut pset, "\\parse ''").0, CommandResult::Send);
        assert_eq!(
            pset.send_mode,
            SendMode::ExtendedParse {
                statement: String::new()
            }
        );
        let (status, stderr) = dispatch_on(&mut pset, "\\close_prepared stmt2 extra");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(
            pset.send_mode,
            SendMode::ExtendedClose {
                statement: "stmt2".into()
            }
        );
        assert_eq!(
            stderr,
            "\\close_prepared: extra argument \"extra\" ignored\n"
        );
        for cmd in ["parse", "close_prepared"] {
            let (status, stderr) = dispatch_on(&mut pset, &format!("\\{cmd}"));
            assert_eq!(status, CommandResult::Error);
            assert_eq!(stderr, format!("\\{cmd}: missing required argument\n"));
            assert_eq!(pset.send_mode, SendMode::Query);
        }
    }

    fn opts(values: &[&str]) -> Vec<SlashOption> {
        values
            .iter()
            .map(|v| SlashOption {
                value: (*v).to_string(),
                quote: None,
            })
            .collect()
    }

    #[test]
    fn g_arguments_are_parenthesized_options_then_a_file_name() {
        // `process_command_g_options` (`command.c:1800`): the parentheses
        // are stripped, and one standing alone is no option at all.
        let options = opts(&["(format=csv", "csv_fieldsep=\t)"]);
        assert_eq!(
            GArgs::split(&options),
            GArgs {
                psets: vec!["format=csv", "csv_fieldsep=\t"],
                unclosed: false,
                fname: None,
                consumed: 2,
            }
        );
        let options = opts(&["(", "title=x", ")", "out.txt", "extra"]);
        assert_eq!(
            GArgs::split(&options),
            GArgs {
                psets: vec!["title=x"],
                unclosed: false,
                fname: Some("out.txt"),
                consumed: 4,
            }
        );
        let options = opts(&["(expanded", "border=2"]);
        let args = GArgs::split(&options);
        assert!(args.unclosed);
        assert_eq!(args.consumed, 2);
        assert_eq!(GArgs::split(&opts(&[])).consumed, 0);
        assert_eq!(GArgs::split(&opts(&["file", "x"])).consumed, 1);
    }

    #[test]
    fn g_options_hold_for_one_query_and_are_saved_for_restoring() {
        let mut pset = terse();
        let before = pset.popt.clone();
        let (status, stderr) = dispatch_on(&mut pset, "\\g (format=unaligned border=2)");
        assert_eq!((status, stderr.as_str()), (CommandResult::Send, ""));
        assert_eq!(
            pset.popt.topt.format,
            crate::settings::PrintFormat::Unaligned
        );
        assert_eq!(pset.popt.topt.border, 2);
        assert_eq!(pset.gsavepopt, Some(before));
    }

    #[test]
    fn a_bad_g_option_is_refused_and_every_option_undone() {
        // `command.c:1861`: "If we failed after already changing some
        // options, undo side-effects".
        let mut pset = terse();
        let before = pset.popt.clone();
        let (status, stderr) = dispatch_on(&mut pset, "\\g (border=2 nosuch)");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(stderr, "\\pset: unknown option: nosuch\n");
        assert_eq!(pset.popt, before);
        assert_eq!(pset.gsavepopt, None);

        let (status, stderr) = dispatch_on(&mut pset, "\\gx (border=2");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(stderr, "\\gx: missing right parenthesis\n");
        assert_eq!(pset.popt, before);
        assert_eq!(pset.gsavepopt, None);
    }

    #[test]
    fn gx_turns_expanded_on_for_one_query() {
        // `command.c:1779`.
        let mut pset = terse();
        let before = pset.popt.clone();
        let (status, _) = dispatch_on(&mut pset, "\\gx (title='foo bar')");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(pset.popt.topt.expanded, Expanded::On);
        assert_eq!(pset.popt.title.as_deref(), Some("foo bar"));
        assert_eq!(pset.gsavepopt, Some(before));
    }

    #[test]
    fn g_options_then_a_file_or_a_pipe() {
        // `command.c:1749`-`:1777`: the options, then the file name scanned
        // `OT_FILEPIPE` again, so a pipe takes the rest of the line.
        let mut pset = terse();
        let (status, stderr) = dispatch_on(&mut pset, "\\g (border=2) out.txt");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(stderr, "");
        assert_eq!(pset.popt.topt.border, 2);
        assert_eq!(pset.gfname.as_deref(), Some(&b"out.txt"[..]));
        let mut pset = terse();
        let (status, stderr) = dispatch_on(&mut pset, "\\gx (border=2 tuples_only) | cat 'a b'");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(stderr, "");
        assert_eq!(pset.gfname.as_deref(), Some(&b"| cat 'a b'"[..]));
    }

    #[test]
    fn each_pipeline_command_is_a_send_mode_and_sends() {
        // `command.c:3065`, `:3084`, `:3103`, `:1695`, `:1714`.
        for (line, mode) in [
            ("\\startpipeline", SendMode::StartPipelineMode),
            ("\\syncpipeline", SendMode::PipelineSync),
            ("\\endpipeline", SendMode::EndPipelineMode),
            ("\\flush", SendMode::Flush),
            ("\\flushrequest", SendMode::FlushRequest),
        ] {
            let mut pset = terse();
            let (status, stderr) = dispatch_on(&mut pset, line);
            assert_eq!(status, CommandResult::Send, "{line}");
            assert_eq!(stderr, "", "{line}");
            assert_eq!(pset.send_mode, mode, "{line}");
        }
        // They read no argument, so one draws the usual warning.
        let mut pset = terse();
        let (status, stderr) = dispatch_on(&mut pset, "\\startpipeline now");
        assert_eq!(status, CommandResult::Send);
        assert_eq!(stderr, "\\startpipeline: extra argument \"now\" ignored\n");
    }

    #[test]
    fn getresults_reads_its_count_as_atoi_does() {
        // `command.c:1929`.
        for (line, requested) in [
            ("\\getresults", 0),
            ("\\getresults 3", 3),
            ("\\getresults 0", 0),
            ("\\getresults 2abc", 2),
            ("\\getresults abc", 0),
        ] {
            let mut pset = terse();
            pset.pipeline.requested_results = 9;
            let (status, stderr) = dispatch_on(&mut pset, line);
            assert_eq!(status, CommandResult::Send, "{line}");
            assert_eq!(stderr, "", "{line}");
            assert_eq!(pset.send_mode, SendMode::GetResults, "{line}");
            assert_eq!(pset.pipeline.requested_results, requested, "{line}");
        }
        assert_eq!(atoi("  -12x"), -12);
        assert_eq!(atoi("+7"), 7);
        assert_eq!(atoi(""), 0);
    }

    #[test]
    fn a_negative_count_is_refused_but_the_send_mode_stays_set() {
        // `psql_pipeline.sql:354`; upstream sets the mode before it reads
        // the count, and returns without cleaning it up.
        let mut pset = terse();
        let (status, stderr) = dispatch_on(&mut pset, "\\getresults -1");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(
            stderr,
            "\\getresults: invalid number of requested results\n"
        );
        assert_eq!(pset.send_mode, SendMode::GetResults);
        assert_eq!(pset.pipeline.requested_results, 0);
    }

    #[test]
    fn sendpipeline_needs_a_pipeline_and_a_bind() {
        // `command.c:2844`, `psql_pipeline.sql:283`-`:292`.
        let mut pset = terse();
        pset.send_mode = SendMode::ExtendedQueryParams {
            params: vec!["1".into()],
        };
        let (status, stderr) = dispatch_on(&mut pset, "\\sendpipeline");
        assert_eq!(status, CommandResult::Error);
        assert_eq!(
            stderr,
            "\\sendpipeline not allowed outside of pipeline mode\n"
        );
        assert_eq!(pset.send_mode, SendMode::Query);

        let mut pset = terse();
        let (status, stderr) = dispatch_in(&mut pset, "\\sendpipeline", PipelineStatus::On);
        assert_eq!(status, CommandResult::Error);
        assert_eq!(
            stderr,
            "\\sendpipeline must be used after \\bind or \\bind_named\n"
        );

        for mode in [
            SendMode::ExtendedQueryParams { params: vec![] },
            SendMode::ExtendedQueryPrepared {
                statement: "s".into(),
                params: vec![],
            },
        ] {
            let mut pset = terse();
            pset.send_mode = mode.clone();
            let (status, stderr) =
                dispatch_in(&mut pset, "\\sendpipeline", PipelineStatus::Aborted);
            assert_eq!(status, CommandResult::Send);
            assert_eq!(stderr, "");
            assert_eq!(pset.send_mode, mode);
        }
    }

    #[test]
    fn watch_is_refused_in_a_pipeline_before_its_arguments_are_read() {
        // `command.c:3384`: the refusal comes first, so a bad argument draws
        // no second message, and the bind is forgotten.
        let mut pset = terse();
        pset.send_mode = SendMode::ExtendedQueryParams {
            params: vec!["1".into()],
        };
        let (status, stderr) = dispatch_in(&mut pset, "\\watch m=x", PipelineStatus::On);
        assert_eq!(status, CommandResult::Watch(None));
        assert_eq!(stderr, "\\watch not allowed in pipeline mode\n");
        assert_eq!(pset.send_mode, SendMode::Query);
    }

    #[test]
    fn g_and_gx_are_refused_in_a_pipeline_after_their_options_apply() {
        // `command.c:1764`: the options are already in force, and stay so
        // until the next query restores them; the bind is forgotten.
        for cmd in ["g", "gx"] {
            let mut pset = terse();
            pset.send_mode = SendMode::ExtendedQueryParams {
                params: vec!["1".into()],
            };
            let before = pset.popt.clone();
            let line = format!("\\{cmd} (format=unaligned tuples_only=on)");
            let (status, stderr) = dispatch_in(&mut pset, &line, PipelineStatus::On);
            assert_eq!(status, CommandResult::Error);
            assert_eq!(stderr, format!("\\{cmd} not allowed in pipeline mode\n"));
            assert_eq!(pset.send_mode, SendMode::Query);
            assert_eq!(pset.gsavepopt, Some(before));
            assert!(pset.popt.topt.tuples_only);
        }
    }

    #[test]
    fn echo_text_is_pure() {
        let opt = |value: &str, quote| SlashOption {
            value: value.to_string(),
            quote,
        };
        assert_eq!(echo_text(&[opt("a", None), opt("b", None)]), b"a b\n");
        assert_eq!(echo_text(&[opt("-n", None), opt("a", None)]), b"a");
        assert_eq!(echo_text(&[]), b"\n");
    }

    /// Run `line` from `pset`, returning the result, the settings after it,
    /// stdout and stderr.
    fn run_from(line: &str, pset: PsqlSettings) -> (CommandResult, PsqlSettings, String, String) {
        let mut vars = VariableSpace::new();
        let mut pset = pset;
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        assert_eq!(
            scanner.scan(&mut buf, &NoVariables).0,
            ScanResult::Backslash
        );
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let status = dispatch_slash(
            &mut scanner,
            &mut pset,
            &mut vars,
            &mut NoServer,
            &mut CommandSource::file(&mut &b""[..]),
            &mut Output::new(&mut stdout),
            &mut stderr,
        );
        (
            status,
            pset,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[test]
    fn timing_toggles_without_an_argument_and_says_so() {
        // `command.c:3175`-`:3184`.
        let (status, pset, out, _) = run_from("\\timing", PsqlSettings::default());
        assert_eq!(status, CommandResult::SkipLine);
        assert!(pset.timing);
        assert_eq!(out, "Timing is on.\n");
        let (_, pset, out, _) = run_from("\\timing", pset);
        assert!(!pset.timing);
        assert_eq!(out, "Timing is off.\n");
    }

    #[test]
    fn timing_takes_a_boolean_and_is_silent_when_quiet() {
        let quiet = PsqlSettings {
            quiet: true,
            ..PsqlSettings::default()
        };
        let (status, pset, out, err) = run_from("\\timing on", quiet.clone());
        assert_eq!(status, CommandResult::SkipLine);
        assert!(pset.timing);
        assert_eq!((out.as_str(), err.as_str()), ("", ""));
        let (_, pset, _, _) = run_from("\\timing off", pset);
        assert!(!pset.timing);
    }

    #[test]
    fn a_bad_timing_value_is_an_error_that_leaves_the_switch_and_still_reports_it() {
        // `ParseVariableBool` leaves `pset.timing` alone (`variables.c:141`),
        // and `\timing` prints the state regardless (`command.c:3179`).
        let on = PsqlSettings {
            timing: true,
            ..PsqlSettings::default()
        };
        let (status, pset, out, err) = run_from("\\timing sideways", on);
        assert_eq!(status, CommandResult::Error);
        assert!(pset.timing);
        assert_eq!(out, "Timing is on.\n");
        assert_eq!(
            err,
            "psql: error: unrecognized value \"sideways\" for \"\\timing\": Boolean expected\n"
        );
    }

    #[test]
    fn timing_ignores_a_second_argument_with_a_warning() {
        let (_, _, _, err) = run_from("\\timing on off", PsqlSettings::default());
        assert_eq!(
            err,
            "psql: warning: \\timing: extra argument \"off\" ignored\n"
        );
    }

    #[test]
    fn errverbose_with_nothing_saved_says_so_on_stdout() {
        // `command.c:1663`; `001_basic.pl:159`.
        let (status, _, out, err) = run_from("\\errverbose", PsqlSettings::default());
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(out, "There is no previous error.\n");
        assert_eq!(err, "");
    }

    #[test]
    fn errverbose_logs_the_saved_error_verbosely_with_context() {
        use rlibpq::{Backend, QueryRunner, ResultError, TransactionStatus};
        let mut runner = QueryRunner::new();
        runner
            .push(Backend::ErrorResponse(ResultError::new(vec![
                (b'S', b"ERROR".to_vec()),
                (b'C', b"42703".to_vec()),
                (b'M', b"column \"error\" does not exist".to_vec()),
                (b'W', b"SQL function \"f\"".to_vec()),
                (b'F', b"parse_relation.c".to_vec()),
                (b'L', b"3859".to_vec()),
                (b'R', b"errorMissingColumn".to_vec()),
            ])))
            .unwrap();
        runner
            .push(Backend::ReadyForQuery(TransactionStatus::Idle))
            .unwrap();
        let pset = PsqlSettings {
            last_error_result: runner.into_results().pop(),
            inputfile: Some("<stdin>".into()),
            lineno: 2,
            ..PsqlSettings::default()
        };
        let (status, pset, out, err) = run_from("\\errverbose", pset);
        assert_eq!(status, CommandResult::SkipLine);
        assert_eq!(out, "");
        // `001_basic.pl:178`-`:181`'s shape, less the `LINE 1:` cursor: a
        // result replayed without sending a query keeps no `errQuery` to draw
        // it over. `t_001_basic.rs` runs the case with the cursor, live.
        assert_eq!(
            err,
            "psql:<stdin>:2: error: ERROR:  42703: column \"error\" does not exist\n\
             CONTEXT:  SQL function \"f\"\n\
             LOCATION:  errorMissingColumn, parse_relation.c:3859\n"
        );
        assert!(pset.last_error_result.is_some(), "\\errverbose keeps it");
    }

    #[test]
    fn a_message_is_terse_under_c_and_located_under_f() {
        // `psql.out:4724`: `invalid command \lo`, no prefix at all.
        let terse = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        assert_eq!(run_from("\\lo", terse).3, "invalid command \\lo\n");
        let located = PsqlSettings {
            inputfile: Some("a.sql".into()),
            lineno: 3,
            ..PsqlSettings::default()
        };
        assert_eq!(
            run_from("\\lo", located).3,
            "psql:a.sql:3: error: invalid command \\lo\n"
        );
    }
    #[test]
    fn g_names_its_file_and_sends() {
        // `command.c:1771`-`:1786`: `pset.gfname`, then `PSQL_CMD_SEND`.
        let (status, pset, _, err) = run_from("\\g /tmp/out", PsqlSettings::default());
        assert_eq!(status, CommandResult::Send);
        assert_eq!(pset.gfname.as_deref(), Some(&b"/tmp/out"[..]));
        assert_eq!(err, "");
        let (status, pset, _, _) = run_from("\\g", PsqlSettings::default());
        assert_eq!(status, CommandResult::Send);
        assert_eq!(pset.gfname, None);
        let (_, pset, _, _) = run_from("\\g | cat > x", PsqlSettings::default());
        assert_eq!(pset.gfname.as_deref(), Some(&b"| cat > x"[..]));
    }

    #[test]
    fn g_warns_about_a_second_argument() {
        let (status, _, _, err) = run_from("\\g a b", PsqlSettings::default());
        assert_eq!(status, CommandResult::Send);
        assert_eq!(err, "psql: warning: \\g: extra argument \"b\" ignored\n");
    }

    #[test]
    fn getenv_copies_a_set_environment_variable_only() {
        // `command.c:1892`; `psql.sql:1509` reads `PG_ABS_BUILDDIR` this way.
        let path = std::env::var("PATH").expect("PATH is set");
        let run1 = run("\\getenv p PATH");
        assert_eq!(run1.result, CommandResult::SkipLine);
        assert_eq!(run1.vars.get("p"), Some(path.as_str()));
        let unset = run("\\getenv p RPSQL_TEST_NO_SUCH_VARIABLE");
        assert_eq!(unset.result, CommandResult::SkipLine);
        assert_eq!(unset.vars.get("p"), None);
        let missing = run("\\getenv p");
        assert_eq!(missing.result, CommandResult::Error);
        assert_eq!(
            missing.stderr,
            "psql: error: \\getenv: missing required argument\n"
        );
    }

    #[test]
    fn o_moves_query_output_and_qecho_with_it_but_not_echo() {
        // `command.c:1569`-`:1574`: `\qecho` writes to `pset.queryFout`,
        // `\echo` to stdout; `\o` alone goes back to stdout.
        let dir = std::env::temp_dir().join(format!("rpsql-o-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("o.out");
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings::default();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        {
            let mut out = Output::new(&mut stdout);
            for line in [
                format!("\\o {};", file.display()),
                "\\qecho to the file".to_string(),
                "\\echo to stdout".to_string(),
                "\\o".to_string(),
                "\\qecho to stdout again".to_string(),
            ] {
                let mut scanner = Scanner::new();
                scanner.setup(line.as_bytes(), true);
                let mut buf = Vec::new();
                scanner.scan(&mut buf, &NoVariables);
                let status = dispatch_slash(
                    &mut scanner,
                    &mut pset,
                    &mut vars,
                    &mut NoServer,
                    &mut CommandSource::file(&mut &b""[..]),
                    &mut out,
                    &mut stderr,
                );
                assert_eq!(status, CommandResult::SkipLine, "{line}");
            }
        }
        assert_eq!(std::fs::read(&file).unwrap(), b"to the file\n");
        assert_eq!(stdout, b"to stdout\nto stdout again\n");
        assert!(stderr.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn o_to_a_file_that_cannot_be_opened_is_an_error() {
        let (status, _, _, err) = run_from("\\o /nonexistent/dir/f", PsqlSettings::default());
        assert_eq!(status, CommandResult::Error);
        assert_eq!(
            err,
            "psql: error: /nonexistent/dir/f: No such file or directory\n"
        );
    }

    #[test]
    fn the_banner_names_the_server_only_when_it_differs() {
        // `command.c:4447`-`:4486`.
        let pset = PsqlSettings {
            sversion: 180_006,
            ..PsqlSettings::default()
        };
        assert_eq!(
            connection_warnings(&pset, Some("18.6"), true),
            "psql (18.6)\n"
        );
        assert_eq!(connection_warnings(&pset, Some("18.6"), false), "");
        let older = PsqlSettings {
            sversion: 170_002,
            ..PsqlSettings::default()
        };
        assert_eq!(
            connection_warnings(&older, Some("17.2 (Debian)"), true),
            "psql (18.6, server 17.2 (Debian))\n"
        );
        let newer = PsqlSettings {
            sversion: 190_000,
            ..PsqlSettings::default()
        };
        assert_eq!(
            connection_warnings(&newer, None, false),
            "psql (18.6, server 19.0)\n\
             WARNING: psql major version 18, server major version 19.\n         \
             Some psql features might not work.\n"
        );
        let ancient = PsqlSettings {
            sversion: 90_105,
            ..PsqlSettings::default()
        };
        assert!(connection_warnings(&ancient, None, true).contains("server major version 9.1."));
        let quiet = PsqlSettings {
            quiet: true,
            ..older
        };
        assert_eq!(connection_warnings(&quiet, None, true), "");
    }
}
