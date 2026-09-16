//! The backslash-command dispatcher: `src/bin/psql/command.c`.
//!
//! `HandleSlashCmds` (`command.c:214`) parses the command name, dispatches,
//! then eats whatever arguments are left over with a warning. This issue
//! implements the four commands it names — `\q`, `\c`, `\echo` and `\set` —
//! plus the `\unset`, `\qecho` and `\warn` that share their code. Everything
//! else is [`CommandResult::Unknown`], which renders upstream's
//! `invalid command \%s`; NAT-401 … NAT-403 fill the table in.

use std::io::Write;

use crate::scan::{Scanner, VariableSource};
use crate::settings::PsqlSettings;
use crate::slash::SlashOption;
use crate::variables::VariableSpace;

/// `backslashResult` (`command.h:18`).
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
    /// `\c`'s reconnection, which the caller performs (`command.c:3462`).
    Connect(Box<ConnectRequest>),
}

/// The four arguments `\connect` takes (`command.c:731`).
///
/// `None` means "keep what the current connection uses"; `Some("-")` is
/// upstream's explicit "use the default" (`command.c:756`).
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
    /// `read_connect_arg()` (`command.c:960`): a literal `-` means "unset".
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
    /// `pset`, for `quiet` and the print options.
    pub pset: &'a PsqlSettings,
    /// `pset.vars`.
    pub vars: &'a mut VariableSpace,
}

/// `HandleSlashCmds()` (`command.c:214`).
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
        let _ = writeln!(stderr, "psql: error: invalid command \\{cmd}");
        CommandResult::Error
    } else {
        status
    };

    // Eat any remaining arguments after a valid command (`command.c:245`).
    // `slash_options` above already consumed them, so what is left is the
    // warning upstream prints for the ones a command did not use.
    if status != CommandResult::Error {
        for extra in extra_arguments(&cmd, &options) {
            let _ = writeln!(
                stderr,
                "psql: warning: \\{cmd}: extra argument \"{extra}\" ignored"
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
        "unset" => 1,
        _ => 0,
    };
    options[options.len().min(takes)..]
        .iter()
        .map(|o| o.value.as_str())
        .collect()
}

/// `exec_command()` (`command.c:301`), for the commands this issue ports.
fn exec_command(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    match cmd {
        // `exec_command_quit()` (`command.c:2620`).
        "q" | "quit" => CommandResult::Terminate,
        // `exec_command_connect()` (`command.c:731`).
        "c" | "connect" => CommandResult::Connect(Box::new(ConnectRequest::from_options(options))),
        // `exec_command_echo()` (`command.c:1187`).
        "echo" | "qecho" | "warn" => {
            let sink: &mut dyn Write = if cmd == "warn" { stderr } else { stdout };
            let _ = sink.write_all(&echo_text(options));
            CommandResult::SkipLine
        }
        // `exec_command_set()` (`command.c:2744`).
        "set" => exec_command_set(options, ctx, stdout, stderr),
        // `exec_command_unset()` (`command.c:2955`).
        "unset" => {
            let Some(name) = options.first() else {
                let _ = writeln!(stderr, "psql: error: \\unset: missing required argument");
                return CommandResult::Error;
            };
            match ctx.vars.delete(&name.value) {
                Ok(()) => CommandResult::SkipLine,
                Err(err) => {
                    let _ = writeln!(stderr, "psql: error: {}", err.message);
                    CommandResult::Error
                }
            }
        }
        _ => CommandResult::Unknown,
    }
}

/// The pure half of `exec_command_echo` (`command.c:1187`): the bytes `\echo`
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

fn exec_command_set(
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> CommandResult {
    let Some(name) = options.first() else {
        // No arguments: list all variables (`command.c:2752`).
        let _ = stdout.write_all(ctx.vars.print().as_bytes());
        return CommandResult::SkipLine;
    };
    // The value is the concatenation of the remaining arguments
    // (`command.c:2765`).
    let value: String = options[1..].iter().map(|o| o.value.as_str()).collect();
    match ctx.vars.set(&name.value, Some(&value)) {
        Ok(()) => CommandResult::SkipLine,
        Err(err) => {
            let _ = writeln!(stderr, "psql: error: {}", err.message);
            CommandResult::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{NoVariables, ScanResult};

    struct Run {
        result: CommandResult,
        stdout: String,
        stderr: String,
        vars: VariableSpace,
    }

    fn run(line: &str) -> Run {
        let mut vars = VariableSpace::new();
        let pset = PsqlSettings::default();
        let mut scanner = Scanner::new();
        scanner.setup(line.as_bytes(), true);
        let mut buf = Vec::new();
        let (res, _) = scanner.scan(&mut buf, &NoVariables);
        assert_eq!(res, ScanResult::Backslash);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let result = {
            let mut ctx = CommandContext {
                pset: &pset,
                vars: &mut vars,
            };
            handle_slash_cmds(
                &mut scanner,
                &mut ctx,
                &NoVariables,
                &mut stdout,
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
        // Quoted, it is just text (`command.c:1206`).
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
        // `command.c:2765`.
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
        // `command.c:252`.
        let run = run("\\q one");
        assert_eq!(
            run.stderr,
            "psql: warning: \\q: extra argument \"one\" ignored\n"
        );
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
