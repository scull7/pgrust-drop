//! The backslash-command dispatcher: `src/bin/psql/command.c`.
//!
//! `HandleSlashCmds` (`command.c:231`) parses the command name, dispatches,
//! then eats whatever arguments are left over with a warning. Each command
//! reads its own arguments, with the option type it needs, because an `\if`
//! branch that is not taken must consume exactly as much text as the taken
//! one would: `\w |cmd \else` keeps the `\else`, `\echo x \else` does not.
//!
//! Commands ported so far: `\q`, `\c` (refused by [`dispatch_slash`] until
//! NAT-405), `\echo`/`\qecho`/`\warn`, `\set`, `\unset`, `\pset` (NAT-400),
//! `\if`/`\elif`/`\else`/`\endif` and a bare `\g`. Every other command upstream knows is in
//! [`unported_shape`]: skipped correctly in an inactive branch, refused with
//! `\X is not implemented yet` in an active one. Anything else renders
//! upstream's `invalid command \X`.

use std::io::Write;

use crate::conditional::{ConditionalStack, IfState};
use crate::logging::{Level, log};
use crate::scan::{NoVariables, Scanner, VariableSource};
use crate::settings::PsqlSettings;
use crate::slash::{OptionType, SlashOption};
use crate::variables::{VarView, VariableSpace, parse_variable_bool};

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

/// `MainLoop`'s `query_buf` and `previous_buf`, which `-c` does not have
/// (`startup.c:411` passes NULL for both).
pub struct QueryBuffers<'a> {
    /// `query_buf`
    pub query: &'a mut Vec<u8>,
    /// `previous_buf`
    pub previous: &'a [u8],
}

/// Everything a backslash command may read or write, so the dispatcher stays
/// one function of its inputs.
pub struct CommandContext<'a> {
    /// `pset`: the print options `\pset` sets, and what the logger reads.
    pub pset: &'a mut PsqlSettings,
    /// `pset.vars`.
    pub vars: &'a mut VariableSpace,
    /// The `\if` stack, which is also the lexer's passthrough: variables are
    /// not substituted while it is inactive (`common.c:197`).
    pub cstack: &'a mut ConditionalStack,
    /// The query buffers, or `None` for a `-c` command.
    pub buffers: Option<QueryBuffers<'a>>,
    /// `stdout`, which is also `pset.queryFout` until `\o` lands.
    pub stdout: &'a mut dyn Write,
    /// `stderr`
    pub stderr: &'a mut dyn Write,
}

/// One whole backslash command, from the lexer to the settings a `\set` may
/// have changed.
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
    cstack: &mut ConditionalStack,
    buffers: Option<QueryBuffers<'_>>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let mut working = pset.clone();
    let status = {
        let mut ctx = CommandContext {
            pset: &mut working,
            vars,
            cstack,
            buffers,
            stdout,
            stderr,
        };
        let status = handle_slash_cmds(scanner, &mut ctx);
        if matches!(status, CommandResult::Connect(_)) {
            log(
                ctx.stderr,
                ctx.pset,
                Level::Error,
                "\\connect is not implemented yet (Linear NAT-405)",
            );
            CommandResult::Error
        } else {
            status
        }
    };
    // `\pset` wrote `working.popt`; a `\set` of a hooked variable wrote the
    // variable space, whose settings are re-derived on top.
    *pset = vars.settings(&working);
    status
}

/// A command in progress: the lexer positioned in its arguments, and the
/// context it runs in.
struct Cmd<'c, 'a> {
    scanner: &'c mut Scanner,
    ctx: &'c mut CommandContext<'a>,
}

impl Cmd<'_, '_> {
    /// `psql_scan_slash_option()` with psql's callbacks: substitution only
    /// while the `\if` stack is active (`common.c:197`), and an unterminated
    /// quote logged and read as the end of the arguments
    /// (`psqlscanslash.l:628`-`:634`).
    fn option(&mut self, option_type: OptionType) -> Option<SlashOption> {
        let live = VarView(&*self.ctx.vars);
        let vars: &dyn VariableSource = if self.ctx.cstack.active() {
            &live
        } else {
            &NoVariables
        };
        if let Ok(option) = self.scanner.slash_option(vars, option_type) {
            option
        } else {
            self.log(Level::Error, "unterminated quoted string");
            None
        }
    }

    fn log(&mut self, level: Level, message: impl AsRef<[u8]>) {
        log(self.ctx.stderr, self.ctx.pset, level, message);
    }

    /// `ignore_slash_options()` (`command.c:3741`).
    fn ignore_options(&mut self) {
        while self.option(OptionType::Normal).is_some() {}
    }

    /// `ignore_slash_filepipe()` (`command.c:3758`) and
    /// `ignore_slash_whole_line()` (`command.c:3778`): one argument of the
    /// given type, read and dropped.
    fn ignore_one(&mut self, option_type: OptionType) {
        let _ = self.option(option_type);
    }

    /// `gather_boolean_expression()` (`command.c:3678`): every argument,
    /// joined by single spaces.
    fn gather_boolean_expression(&mut self) -> String {
        let mut expression = String::new();
        let mut first = true;
        while let Some(option) = self.option(OptionType::Normal) {
            if !first {
                expression.push(' ');
            }
            expression.push_str(&option.value);
            first = false;
        }
        expression
    }

    /// `is_true_boolean_expression()` (`command.c:3708`): an unrecognized
    /// value is logged by `ParseVariableBool` (`variables.c:141`) and is false.
    fn is_true_boolean_expression(&mut self, name: &str) -> bool {
        let expression = self.gather_boolean_expression();
        let mut value = false;
        if parse_variable_bool(Some(&expression), Some(name), &mut value) {
            value
        } else {
            self.log(
                Level::Error,
                format!("unrecognized value \"{expression}\" for \"{name}\": Boolean expected"),
            );
            false
        }
    }

    /// `ignore_boolean_expression()` (`command.c:3725`). The stack must
    /// already be inactive, so nothing is substituted.
    fn ignore_boolean_expression(&mut self) {
        let _ = self.gather_boolean_expression();
    }

    /// `save_query_text_state()` (`command.c:3806`).
    fn save_query_text_state(&mut self) {
        if let Some(buffers) = &self.ctx.buffers {
            self.ctx.cstack.set_query_len(buffers.query.len());
        }
        self.ctx.cstack.set_lex_state(self.scanner.lex_state());
    }

    /// `discard_query_text()` (`command.c:3824`): drop what an inactive
    /// branch added to the query buffer, and the lexer state with it.
    fn discard_query_text(&mut self) {
        if let (Some(buffers), Some(len)) = (&mut self.ctx.buffers, self.ctx.cstack.query_len()) {
            buffers.query.truncate(len);
        }
        if let Some(saved) = self.ctx.cstack.lex_state() {
            self.scanner.set_lex_state(saved);
        }
    }
}

/// `HandleSlashCmds()` (`command.c:231`).
///
/// The scanner must be positioned just past the backslash, which is where
/// [`crate::scan::ScanResult::Backslash`] leaves it.
pub fn handle_slash_cmds(scanner: &mut Scanner, ctx: &mut CommandContext<'_>) -> CommandResult {
    let cmd = scanner.slash_command();
    let mut c = Cmd { scanner, ctx };
    let mut status = exec_command(&cmd, &mut c);

    if status == CommandResult::Unknown {
        c.log(Level::Error, format!("invalid command \\{cmd}"));
        if c.ctx.pset.cur_cmd_interactive {
            c.log(Level::Hint, "Try \\? for help.");
        }
        status = CommandResult::Error;
    }

    if status == CommandResult::Error {
        // Silently throw away the rest of the line after an erroneous
        // command (`command.c:290`).
        while c.option(OptionType::WholeLine).is_some() {}
    } else {
        // Eat any remaining arguments after a valid command, with an
        // inactive entry pushed so nothing is substituted (`command.c:271`).
        let active_branch = c.ctx.cstack.active();
        c.ctx.cstack.push(IfState::Ignored);
        while let Some(arg) = c.option(OptionType::Normal) {
            if active_branch {
                c.log(
                    Level::Warning,
                    format!("\\{cmd}: extra argument \"{}\" ignored", arg.value),
                );
            }
        }
        c.ctx.cstack.pop();
    }

    // If there is a trailing `\\`, swallow it.
    c.scanner.slash_command_end();
    status
}

/// How a command upstream implements, and this port does not yet, consumes
/// its arguments: the `ignore_slash_*` call its `exec_command_*` makes in an
/// inactive branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgShape {
    /// No arguments are read; `HandleSlashCmds` drops any silently.
    None,
    /// `ignore_slash_options()`: `OT_NORMAL` arguments up to the next
    /// backslash.
    Options,
    /// `ignore_slash_filepipe()`: one `OT_FILEPIPE` argument.
    FilePipe,
    /// `ignore_slash_whole_line()`: the rest of the line.
    WholeLine,
}

/// The commands `exec_command()` (`command.c:315`-`:478`) dispatches that
/// this port has not implemented, with the argument shape of each.
///
/// `None` means upstream does not know the command either, and says
/// `invalid command` whether or not the branch is active — which is why
/// `\lo` is refused inside `\if false` while `\lo_list` is skipped.
#[must_use]
pub fn unported_shape(cmd: &str) -> Option<ArgShape> {
    let shape = match cmd {
        "a" | "conninfo" | "copyright" | "errverbose" | "gdesc" | "gexec" | "H" | "html" | "p"
        | "print" | "r" | "reset" => ArgShape::None,
        "o" | "out" | "w" | "write" => ArgShape::FilePipe,
        "ef" | "ev" | "h" | "help" | "sf" | "sf+" | "sv" | "sv+" | "unrestrict" | "!" => {
            ArgShape::WholeLine
        }
        // `pg_strcasecmp(cmd, "copy")` (`command.c:354`).
        _ if cmd.eq_ignore_ascii_case("copy") => ArgShape::WholeLine,
        "bind" | "bind_named" | "C" | "cd" | "close_prepared" | "crosstabview" | "e" | "edit"
        | "encoding" | "f" | "flush" | "flushrequest" | "getenv" | "getresults" | "gset" | "i"
        | "include" | "ir" | "include_relative" | "l" | "list" | "lx" | "listx" | "l+"
        | "list+" | "lx+" | "listx+" | "l+x" | "list+x" | "parse" | "password" | "prompt"
        | "restrict" | "s" | "sendpipeline" | "setenv" | "startpipeline" | "syncpipeline"
        | "endpipeline" | "t" | "T" | "timing" | "watch" | "x" | "z" | "zS" | "zx" | "zSx"
        | "zxS" | "?" => ArgShape::Options,
        // `cmd[0] == 'd'` (`command.c:360`) and `strncmp(cmd, "lo_", 3)`
        // (`:417`) are prefixes, not names.
        _ if cmd.starts_with('d') || cmd.starts_with("lo_") => ArgShape::Options,
        _ => return None,
    };
    Some(shape)
}

/// `is_branching_command()` (`command.c:3790`).
fn is_branching_command(cmd: &str) -> bool {
    matches!(cmd, "if" | "elif" | "else" | "endif")
}

/// `exec_command()` (`command.c:315`).
fn exec_command(cmd: &str, c: &mut Cmd<'_, '_>) -> CommandResult {
    let active_branch = c.ctx.cstack.active();

    // In interactive mode, warn when a command inside a false branch is
    // ignored (`command.c:331`).
    if c.ctx.pset.cur_cmd_interactive && !active_branch && !is_branching_command(cmd) {
        c.log(
            Level::Warning,
            format!("\\{cmd} command ignored; use \\endif or Ctrl-C to exit current \\if block"),
        );
    }

    let status = match cmd {
        "c" | "connect" => exec_command_connect(c, active_branch),
        "echo" | "qecho" | "warn" => exec_command_echo(c, active_branch, cmd),
        "elif" => exec_command_elif(c),
        "else" => exec_command_else(c),
        "endif" => exec_command_endif(c),
        "g" | "gx" => exec_command_g(c, active_branch, cmd),
        "if" => exec_command_if(c),
        "pset" => exec_command_pset(c, active_branch),
        // `exec_command_quit()` (`command.c:2750`).
        "q" | "quit" if active_branch => CommandResult::Terminate,
        "q" | "quit" => CommandResult::SkipLine,
        "set" => exec_command_set(c, active_branch),
        "unset" => exec_command_unset(c, active_branch, cmd),
        _ => match unported_shape(cmd) {
            Some(shape) => exec_command_unported(c, active_branch, cmd, shape),
            None => CommandResult::Unknown,
        },
    };

    // Every command that returns PSQL_CMD_SEND wants to execute previous_buf
    // if query_buf is empty (`command.c:489`).
    if status == CommandResult::Send {
        copy_previous_query(c.ctx.buffers.as_mut());
    }
    status
}

/// `copy_previous_query()` (`command.c:3850`).
fn copy_previous_query(buffers: Option<&mut QueryBuffers<'_>>) -> bool {
    match buffers {
        Some(buffers) if buffers.query.is_empty() => {
            buffers.query.extend_from_slice(buffers.previous);
            true
        }
        _ => false,
    }
}

/// A command upstream has and this port does not: consumed exactly as the
/// real one would be in an inactive branch, refused in an active one.
fn exec_command_unported(
    c: &mut Cmd<'_, '_>,
    active_branch: bool,
    cmd: &str,
    shape: ArgShape,
) -> CommandResult {
    if active_branch {
        c.log(Level::Error, format!("\\{cmd} is not implemented yet"));
        return CommandResult::Error;
    }
    match shape {
        ArgShape::None => {}
        ArgShape::Options => c.ignore_options(),
        ArgShape::FilePipe => c.ignore_one(OptionType::FilePipe),
        ArgShape::WholeLine => c.ignore_one(OptionType::WholeLine),
    }
    CommandResult::SkipLine
}

/// `exec_command_connect()` (`command.c:638`), minus `-reuse-previous` and
/// `OT_SQLIDHACK`, which arrive with `do_connect` in NAT-405.
fn exec_command_connect(c: &mut Cmd<'_, '_>, active_branch: bool) -> CommandResult {
    if !active_branch {
        c.ignore_options();
        return CommandResult::SkipLine;
    }
    // `read_connect_arg()` (`command.c:3638`): a literal `-` means "unset".
    let mut arg = || {
        c.option(OptionType::Normal)
            .map(|o| o.value)
            .filter(|v| v != "-")
    };
    let (dbname, user, host, port) = (arg(), arg(), arg(), arg());
    CommandResult::Connect(Box::new(ConnectRequest {
        dbname,
        user,
        host,
        port,
    }))
}

/// `exec_command_echo()` (`command.c:1559`).
fn exec_command_echo(c: &mut Cmd<'_, '_>, active_branch: bool, cmd: &str) -> CommandResult {
    if !active_branch {
        c.ignore_options();
        return CommandResult::SkipLine;
    }
    let mut options = Vec::new();
    while let Some(option) = c.option(OptionType::Normal) {
        options.push(option);
    }
    let text = echo_text(&options);
    let sink: &mut dyn Write = if cmd == "warn" {
        c.ctx.stderr
    } else {
        c.ctx.stdout
    };
    let _ = sink.write_all(&text);
    CommandResult::SkipLine
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

/// `exec_command_g()` (`command.c:1739`), for a bare `\g`: a file, a pipe,
/// `\gx` and the parenthesized pset options arrive with `\o` and `do_pset`.
fn exec_command_g(c: &mut Cmd<'_, '_>, active_branch: bool, cmd: &str) -> CommandResult {
    let mut fname = c.option(OptionType::FilePipe);
    if let Some(first) = fname.as_ref().filter(|f| f.value.starts_with('(')) {
        if active_branch {
            c.log(
                Level::Error,
                format!("\\{cmd} with options is not implemented yet"),
            );
            return CommandResult::Error;
        }
        // `process_command_g_options()` (`command.c:1800`): consume options
        // through the one ending in `)`, then try the file name again.
        let mut last = first.value.clone();
        while !last.ends_with(')') {
            match c.option(OptionType::Normal) {
                Some(option) => last = option.value,
                None => break,
            }
        }
        fname = c.option(OptionType::FilePipe);
    }
    if !active_branch {
        return CommandResult::SkipLine;
    }
    if fname.is_some() || cmd == "gx" {
        c.log(
            Level::Error,
            format!("\\{cmd} with a file, a pipe or expanded output is not implemented yet"),
        );
        return CommandResult::Error;
    }
    CommandResult::Send
}

/// `exec_command_if()` (`command.c:2104`).
fn exec_command_if(c: &mut Cmd<'_, '_>) -> CommandResult {
    if c.ctx.cstack.active() {
        // Push an active entry first, so the expression is scanned with
        // variable substitution.
        c.ctx.cstack.push(IfState::True);
        c.save_query_text_state();
        if !c.is_true_boolean_expression("\\if expression") {
            c.ctx.cstack.poke(IfState::False);
        }
    } else {
        // Inside an inactive outer branch the whole block is ignored, and
        // the expression is not evaluated.
        c.ctx.cstack.push(IfState::Ignored);
        c.save_query_text_state();
        c.ignore_boolean_expression();
    }
    CommandResult::SkipLine
}

/// `exec_command_elif()` (`command.c:2150`).
fn exec_command_elif(c: &mut Cmd<'_, '_>) -> CommandResult {
    match c.ctx.cstack.peek() {
        IfState::True => {
            // Keep what the active branch put in the query buffer, then
            // ignore the rest until `\endif`.
            c.save_query_text_state();
            c.ctx.cstack.poke(IfState::Ignored);
            c.ignore_boolean_expression();
        }
        IfState::False => {
            c.discard_query_text();
            c.ctx.cstack.poke(IfState::True);
            if !c.is_true_boolean_expression("\\elif expression") {
                c.ctx.cstack.poke(IfState::False);
            }
        }
        IfState::Ignored => {
            c.discard_query_text();
            c.ignore_boolean_expression();
        }
        IfState::ElseTrue | IfState::ElseFalse => {
            c.log(Level::Error, "\\elif: cannot occur after \\else");
            return CommandResult::Error;
        }
        IfState::None => {
            c.log(Level::Error, "\\elif: no matching \\if");
            return CommandResult::Error;
        }
    }
    CommandResult::SkipLine
}

/// `exec_command_else()` (`command.c:2226`).
fn exec_command_else(c: &mut Cmd<'_, '_>) -> CommandResult {
    match c.ctx.cstack.peek() {
        IfState::True => {
            c.save_query_text_state();
            c.ctx.cstack.poke(IfState::ElseFalse);
        }
        IfState::False => {
            c.discard_query_text();
            c.ctx.cstack.poke(IfState::ElseTrue);
        }
        IfState::Ignored => {
            c.discard_query_text();
            c.ctx.cstack.poke(IfState::ElseFalse);
        }
        IfState::ElseTrue | IfState::ElseFalse => {
            c.log(Level::Error, "\\else: cannot occur after \\else");
            return CommandResult::Error;
        }
        IfState::None => {
            c.log(Level::Error, "\\else: no matching \\if");
            return CommandResult::Error;
        }
    }
    CommandResult::SkipLine
}

/// `exec_command_endif()` (`command.c:2291`).
fn exec_command_endif(c: &mut Cmd<'_, '_>) -> CommandResult {
    match c.ctx.cstack.peek() {
        IfState::True | IfState::ElseTrue => {
            c.ctx.cstack.pop();
        }
        IfState::False | IfState::Ignored | IfState::ElseFalse => {
            c.discard_query_text();
            c.ctx.cstack.pop();
        }
        IfState::None => {
            c.log(Level::Error, "\\endif: no matching \\if");
            return CommandResult::Error;
        }
    }
    CommandResult::SkipLine
}

/// `exec_command_pset()` (`command.c:2695`): list every print option, or
/// `do_pset` the first argument to the second.
fn exec_command_pset(c: &mut Cmd<'_, '_>, active_branch: bool) -> CommandResult {
    if !active_branch {
        c.ignore_options();
        return CommandResult::SkipLine;
    }
    let param = c.option(OptionType::Normal);
    let value = c.option(OptionType::Normal);
    let Some(param) = param else {
        let listing = crate::pset::list_all(&c.ctx.pset.popt);
        let _ = c.ctx.stdout.write_all(listing.as_bytes());
        return CommandResult::SkipLine;
    };
    let quiet = c.ctx.pset.quiet;
    let value = value.as_ref().map(|o| o.value.as_str());
    match crate::pset::do_pset(&param.value, value, &mut c.ctx.pset.popt, quiet) {
        Ok(info) => {
            if let Some(info) = info {
                let _ = c.ctx.stdout.write_all(info.as_bytes());
            }
            CommandResult::SkipLine
        }
        Err(err) => {
            c.log(Level::Error, err.to_string());
            CommandResult::Error
        }
    }
}

/// `exec_command_set()` (`command.c:2881`).
fn exec_command_set(c: &mut Cmd<'_, '_>, active_branch: bool) -> CommandResult {
    if !active_branch {
        c.ignore_options();
        return CommandResult::SkipLine;
    }
    let Some(name) = c.option(OptionType::Normal) else {
        // No arguments: list all variables (`command.c:2892`).
        let listing = c.ctx.vars.print();
        let _ = c.ctx.stdout.write_all(listing.as_bytes());
        return CommandResult::SkipLine;
    };
    // The value is the concatenation of the remaining arguments
    // (`command.c:2899`).
    let mut value = String::new();
    while let Some(option) = c.option(OptionType::Normal) {
        value.push_str(&option.value);
    }
    match c.ctx.vars.set(&name.value, Some(&value)) {
        Ok(()) => CommandResult::SkipLine,
        Err(err) => {
            c.log(Level::Error, err.message);
            CommandResult::Error
        }
    }
}

/// `exec_command_unset()` (`command.c:3238`).
fn exec_command_unset(c: &mut Cmd<'_, '_>, active_branch: bool, cmd: &str) -> CommandResult {
    if !active_branch {
        c.ignore_options();
        return CommandResult::SkipLine;
    }
    let Some(name) = c.option(OptionType::Normal) else {
        c.log(Level::Error, format!("\\{cmd}: missing required argument"));
        return CommandResult::Error;
    };
    match c.ctx.vars.delete(&name.value) {
        Ok(()) => CommandResult::SkipLine,
        Err(err) => {
            c.log(Level::Error, err.message);
            CommandResult::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::ScanResult;

    struct Run {
        result: CommandResult,
        stdout: String,
        stderr: String,
        vars: VariableSpace,
    }

    /// Run the first backslash command of `line` through `handle_slash_cmds`
    /// with piped-input logging (terse, no file), in a fresh session.
    fn run(line: &str) -> Run {
        run_in(line, &mut ConditionalStack::new())
    }

    fn run_in(line: &str, cstack: &mut ConditionalStack) -> Run {
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
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
                cstack,
                buffers: None,
                stdout: &mut stdout,
                stderr: &mut stderr,
            };
            handle_slash_cmds(&mut scanner, &mut ctx)
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
            run.stderr.starts_with("unrecognized value \"sideways\""),
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
    fn unset_without_a_name_is_an_error() {
        let run = run("\\unset");
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(run.stderr, "\\unset: missing required argument\n");
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
        assert_eq!(run.stderr, "invalid command \\nosuch\n");
    }

    #[test]
    fn an_unported_command_is_refused_by_name_not_called_invalid() {
        let run = run("\\timing on");
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(run.stderr, "\\timing is not implemented yet\n");
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
        assert_eq!(refused.stderr, "\\pset: unknown option: nosuch\n");
        let extra = run("\\pset border 0 extra");
        assert_eq!(extra.result, CommandResult::SkipLine);
        assert_eq!(extra.stderr, "\\pset: extra argument \"extra\" ignored\n");
    }

    #[test]
    fn extra_arguments_draw_a_warning_with_nothing_substituted() {
        // `command.c:271`-`:284`: an inactive entry is pushed while the
        // extras are read, so `:foo` stays as typed.
        let run = run("\\q one :foo :'foo'");
        assert_eq!(
            run.stderr,
            "\\q: extra argument \"one\" ignored\n\
             \\q: extra argument \":foo\" ignored\n\
             \\q: extra argument \":'foo'\" ignored\n"
        );
    }

    #[test]
    fn after_an_error_the_rest_of_the_line_is_thrown_away() {
        // `command.c:290`: OT_WHOLE_LINE, so no warning and no second command.
        let run = run("\\unset \\\\ \\echo not reached");
        assert_eq!(run.result, CommandResult::Error);
        assert_eq!(run.stdout, "");
        assert_eq!(run.stderr, "\\unset: missing required argument\n");
    }

    #[test]
    fn an_unterminated_quote_is_logged_and_ends_the_arguments() {
        let run = run("\\echo a 'b");
        assert_eq!(run.stdout, "a\n");
        assert_eq!(run.stderr, "unterminated quoted string\n");
    }

    #[test]
    fn if_pushes_a_branch_whose_state_is_the_expression() {
        let mut cstack = ConditionalStack::new();
        assert_eq!(
            run_in("\\if true", &mut cstack).result,
            CommandResult::SkipLine
        );
        assert_eq!(cstack.peek(), IfState::True);
        run_in("\\if off", &mut cstack);
        assert_eq!(cstack.peek(), IfState::False);
        // Nested inside a false branch, even a true expression is ignored.
        run_in("\\if true", &mut cstack);
        assert_eq!(cstack.peek(), IfState::Ignored);
        assert_eq!(cstack.depth(), 3);
    }

    #[test]
    fn an_invalid_expression_is_logged_and_false() {
        // `psql.out:4634`.
        let mut cstack = ConditionalStack::new();
        let run = run_in("\\if invalid boolean expression", &mut cstack);
        assert_eq!(run.result, CommandResult::SkipLine);
        assert_eq!(
            run.stderr,
            "unrecognized value \"invalid boolean expression\" for \"\\if expression\": \
             Boolean expected\n"
        );
        assert_eq!(cstack.peek(), IfState::False);
    }

    #[test]
    fn elif_and_else_move_through_the_branch_states() {
        // `command.c:2150`-`:2330`, one arm per row.
        for (start, cmd, end) in [
            (IfState::True, "\\elif true", IfState::Ignored),
            (IfState::False, "\\elif true", IfState::True),
            (IfState::False, "\\elif false", IfState::False),
            (IfState::Ignored, "\\elif true", IfState::Ignored),
            (IfState::True, "\\else", IfState::ElseFalse),
            (IfState::False, "\\else", IfState::ElseTrue),
            (IfState::Ignored, "\\else", IfState::ElseFalse),
        ] {
            let mut cstack = ConditionalStack::new();
            cstack.push(start);
            let run = run_in(cmd, &mut cstack);
            assert_eq!(run.result, CommandResult::SkipLine, "{start:?} {cmd}");
            assert_eq!(cstack.peek(), end, "{start:?} {cmd}");
        }
    }

    #[test]
    fn branching_out_of_order_is_an_error() {
        // `psql.out:4642`-`:4659`.
        for (start, cmd, message) in [
            (None, "\\endif", "\\endif: no matching \\if\n"),
            (None, "\\else", "\\else: no matching \\if\n"),
            (None, "\\elif", "\\elif: no matching \\if\n"),
            (
                Some(IfState::ElseTrue),
                "\\else",
                "\\else: cannot occur after \\else\n",
            ),
            (
                Some(IfState::ElseFalse),
                "\\elif",
                "\\elif: cannot occur after \\else\n",
            ),
        ] {
            let mut cstack = ConditionalStack::new();
            if let Some(state) = start {
                cstack.push(state);
            }
            let run = run_in(cmd, &mut cstack);
            assert_eq!(run.result, CommandResult::Error, "{cmd}");
            assert_eq!(run.stderr, message);
        }
    }

    #[test]
    fn endif_pops_every_kind_of_branch() {
        for state in [
            IfState::True,
            IfState::False,
            IfState::Ignored,
            IfState::ElseTrue,
            IfState::ElseFalse,
        ] {
            let mut cstack = ConditionalStack::new();
            cstack.push(state);
            assert_eq!(
                run_in("\\endif", &mut cstack).result,
                CommandResult::SkipLine
            );
            assert!(cstack.is_empty(), "{state:?}");
        }
    }

    #[test]
    fn an_inactive_branch_runs_nothing_and_says_nothing() {
        let mut cstack = ConditionalStack::new();
        cstack.push(IfState::False);
        for line in [
            "\\q",
            "\\echo hi",
            "\\set x 1",
            "\\unset",
            "\\c arg1 arg2 arg3 arg4",
            "\\pset arg1 arg2",
            "\\g arg1",
            "\\g (format=csv) x",
            "\\dt arg1",
            "\\lo_list",
        ] {
            let run = run_in(line, &mut cstack);
            assert_eq!(run.result, CommandResult::SkipLine, "{line}");
            assert_eq!(
                (run.stdout.as_str(), run.stderr.as_str()),
                ("", ""),
                "{line}"
            );
            assert_eq!(run.vars.get("x"), None);
        }
        // A command upstream does not know is invalid in any branch
        // (`psql.out:4724`).
        let run = run_in("\\lo arg1 arg2", &mut cstack);
        assert_eq!(run.stderr, "invalid command \\lo\n");
    }

    /// The text an inactive command leaves for the SQL lexer.
    fn rest_after(line: &str) -> String {
        let mut cstack = ConditionalStack::new();
        cstack.push(IfState::False);
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings::default();
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        scanner.scan(&mut buf, &NoVariables);
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut ctx = CommandContext {
            pset: &mut pset,
            vars: &mut vars,
            cstack: &mut cstack,
            buffers: None,
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        handle_slash_cmds(&mut scanner, &mut ctx);
        String::from_utf8_lossy(scanner.rest()).into_owned()
    }

    #[test]
    fn an_inactive_command_consumes_what_the_active_one_would() {
        // `psql.sql`'s `\if false` block: "\else here is eaten as part of
        // OT_FILEPIPE argument", "\endif here is eaten as part of whole-line
        // argument".
        assert_eq!(rest_after("\\w |/no/such/file \\else"), "");
        assert_eq!(rest_after("\\! whole_line \\endif"), "");
        assert_eq!(rest_after("\\sf whole_line \\endif"), "");
        // Normal options stop at the next backslash.
        assert_eq!(rest_after("\\echo a b \\endif"), "\\endif");
        assert_eq!(rest_after("\\w file \\endif"), "\\endif");
        // A `\\` between commands is swallowed.
        assert_eq!(rest_after("\\a \\\\ \\endif"), " \\endif");
    }

    #[test]
    fn unported_shapes_follow_the_ignore_call_of_each_command() {
        assert_eq!(unported_shape("a"), Some(ArgShape::None));
        assert_eq!(unported_shape("w"), Some(ArgShape::FilePipe));
        assert_eq!(unported_shape("COPY"), Some(ArgShape::WholeLine));
        assert_eq!(unported_shape("dt+"), Some(ArgShape::Options));
        assert_eq!(unported_shape("lo_import"), Some(ArgShape::Options));
        assert_eq!(unported_shape("lo"), None);
        assert_eq!(unported_shape("echo"), None, "ported, so not in the table");
    }

    /// Run one backslash command through the whole dispatch sequence, the way
    /// both `MainLoop` and the `-c \…` action do.
    fn dispatch(line: &str) -> (CommandResult, PsqlSettings, String) {
        let mut vars = VariableSpace::new();
        let mut pset = PsqlSettings::default();
        let mut cstack = ConditionalStack::new();
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
            &mut cstack,
            None,
            &mut stdout,
            &mut stderr,
        );

        (status, pset, String::from_utf8(stderr).unwrap())
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
    fn a_bare_g_sends_and_copies_the_previous_query_into_an_empty_buffer() {
        // `copy_previous_query()` (`command.c:3850`).
        let mut query = Vec::new();
        let previous = b"select 1;".to_vec();
        assert!(copy_previous_query(Some(&mut QueryBuffers {
            query: &mut query,
            previous: &previous,
        })));
        assert_eq!(query, b"select 1;");
        let mut query = b"select 2".to_vec();
        assert!(!copy_previous_query(Some(&mut QueryBuffers {
            query: &mut query,
            previous: &previous,
        })));
        assert_eq!(query, b"select 2");
        assert_eq!(run("\\g").result, CommandResult::Send);
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
}
