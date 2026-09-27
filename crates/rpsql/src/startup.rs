//! The psql command line: `src/bin/psql/startup.c`.
//!
//! [`Options`] is `long_options[]` (`startup.c:490`-`:527`) one row at a time,
//! parsed by usage-rs (ADR-0004), which decides whether a command line is
//! well formed. What it means is decided *in argv order*, as upstream's one
//! getopt loop does: `-c` and `-f` build a `SimpleActionList`
//! (`startup.c:550`-`:573`), and a later `-A`, `-H`, `--csv` or `-P` overrides
//! an earlier one — which a declarative option table cannot express. So
//! [`getopt`] is its own pure walk over argv driven by [`SHORT_OPTIONS`] (the
//! getopt string upstream passes) and [`LONG_OPTIONS`], and [`apply`] is the
//! `switch` over what it yields.
//!
//! `--help`, `--help=…` and `--version` are NAT-399's; the `argv[1]` fast path
//! at `startup.c:138` is here because it decides what an invocation *is*.

use std::ffi::{OsStr, OsString};

use usage::Cli;

use crate::pset::do_pset;
use crate::settings::{Expanded, PrintFormat, PrintQueryOpt, PsqlSettings, Trivalue};
use crate::variables::{AssignError, VariableSpace};

/// `getopt_long`'s option string (`startup.c:536`). A `:` means the option
/// takes a value.
pub const SHORT_OPTIONS: &str = "aAbc:d:eEf:F:h:HlL:no:p:P:qR:sStT:U:v:VwWxXz?01";

/// Every option psql accepts, in `long_options[]` order.
///
/// Values are kept as the strings the user typed; turning them into settings
/// is [`apply`], a separate calculation.
// One field per `long_options[]` row, so the switches stay bools here; the
// typed settings built from them live in `PsqlSettings`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Cli, Debug, Clone, Default, PartialEq, Eq)]
#[usage(
    bin = "psql",
    unknown_flags = "error",
    disable_help_flag,
    disable_version_flag,
    args_override_self
)]
pub struct Options {
    /// echo all input from script
    #[usage(short = 'a', long = "echo-all")]
    pub echo_all: bool,
    /// unaligned table output mode
    #[usage(short = 'A', long = "no-align")]
    pub no_align: bool,
    /// execute only a single command (SQL or internal) and exit
    #[usage(short = 'c', long = "command", value_name = "COMMAND")]
    pub command: Vec<String>,
    /// database name to connect to
    #[usage(short = 'd', long = "dbname", value_name = "DBNAME")]
    pub dbname: Option<String>,
    /// echo commands sent to server
    #[usage(short = 'e', long = "echo-queries")]
    pub echo_queries: bool,
    /// echo failed commands
    #[usage(short = 'b', long = "echo-errors")]
    pub echo_errors: bool,
    /// display queries that internal commands generate
    #[usage(short = 'E', long = "echo-hidden")]
    pub echo_hidden: bool,
    /// execute commands from file, then exit
    #[usage(short = 'f', long = "file", value_name = "FILENAME")]
    pub file: Vec<String>,
    /// field separator for unaligned output
    #[usage(short = 'F', long = "field-separator", value_name = "STRING")]
    pub field_separator: Option<String>,
    /// set field separator for unaligned output to zero byte
    #[usage(short = 'z', long = "field-separator-zero")]
    pub field_separator_zero: bool,
    /// database server host or socket directory
    #[usage(short = 'h', long = "host", value_name = "HOSTNAME")]
    pub host: Option<String>,
    /// HTML table output mode
    #[usage(short = 'H', long = "html")]
    pub html: bool,
    /// list available databases, then exit
    #[usage(short = 'l', long = "list")]
    pub list: bool,
    /// send session log to file
    #[usage(short = 'L', long = "log-file", value_name = "FILENAME")]
    pub log_file: Option<String>,
    /// disable enhanced command line editing (readline)
    #[usage(short = 'n', long = "no-readline")]
    pub no_readline: bool,
    /// execute as a single transaction (if non-interactive)
    #[usage(short = '1', long = "single-transaction")]
    pub single_transaction: bool,
    /// send query results to file (or |pipe)
    #[usage(short = 'o', long = "output", value_name = "FILENAME")]
    pub output: Option<String>,
    /// database server port
    #[usage(short = 'p', long = "port", value_name = "PORT")]
    pub port: Option<String>,
    /// set printing option VAR to ARG (see \pset command)
    #[usage(short = 'P', long = "pset", value_name = "VAR[=ARG]")]
    pub pset: Vec<String>,
    /// run quietly (no messages, only query output)
    #[usage(short = 'q', long = "quiet")]
    pub quiet: bool,
    /// record separator for unaligned output
    #[usage(short = 'R', long = "record-separator", value_name = "STRING")]
    pub record_separator: Option<String>,
    /// set record separator for unaligned output to zero byte
    #[usage(short = '0', long = "record-separator-zero")]
    pub record_separator_zero: bool,
    /// single-step mode (confirm each query)
    #[usage(short = 's', long = "single-step")]
    pub single_step: bool,
    /// single-line mode (end of line terminates SQL command)
    #[usage(short = 'S', long = "single-line")]
    pub single_line: bool,
    /// print rows only
    #[usage(short = 't', long = "tuples-only")]
    pub tuples_only: bool,
    /// set HTML table tag attributes (e.g., width, border)
    #[usage(short = 'T', long = "table-attr", value_name = "TEXT")]
    pub table_attr: Option<String>,
    /// database user name
    #[usage(short = 'U', long = "username", value_name = "USERNAME")]
    pub username: Option<String>,
    /// set psql variable NAME to VALUE (e.g., -v ON_ERROR_STOP=1)
    #[usage(
        short = 'v',
        long = "set",
        alias = "variable",
        value_name = "NAME=VALUE"
    )]
    pub set: Vec<String>,
    /// never prompt for password
    #[usage(short = 'w', long = "no-password")]
    pub no_password: bool,
    /// force password prompt (should happen automatically)
    #[usage(short = 'W', long = "password")]
    pub password: bool,
    /// turn on expanded table output
    #[usage(short = 'x', long = "expanded")]
    pub expanded: bool,
    /// do not read startup file (~/.psqlrc)
    #[usage(short = 'X', long = "no-psqlrc")]
    pub no_psqlrc: bool,
    /// CSV (Comma-Separated Values) table output mode
    #[usage(long = "csv")]
    pub csv: bool,
    /// database name, then user name, as bare words
    #[usage(value_name = "DBNAME")]
    pub positional: Vec<String>,
}

/// One entry of `SimpleActionList` (`startup.c:53`-`:58`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `ACT_SINGLE_QUERY`: `-c` with SQL.
    SingleQuery(String),
    /// `ACT_SINGLE_SLASH`: `-c` whose argument starts with a backslash; the
    /// backslash itself is not part of the value (`startup.c:554`).
    SingleSlash(String),
    /// `ACT_FILE`: `-f`, or the implied `-f -` for a non-tty with no action.
    File(Option<String>),
}

/// What one invocation means.
// No `Eq`: a `Session` carries `watch_interval`, which is a float.
#[derive(Debug, Clone, PartialEq)]
pub enum Invocation {
    /// `usage(NOPAGER)` — `-?`, or `--help` as the only argument
    /// (`startup.c:140`).
    PrintHelp(HelpTopic),
    /// `showVersion()` — `--version`/`-V` as `argv[1]` (`startup.c:145`).
    PrintVersion,
    /// usage-rs rejected the command line; the rendering is its own.
    Unparsable(String),
    /// The option loop refused an option's value and exits 1; the text is
    /// what it printed (`startup.c:617`, `:659`).
    Fatal(String),
    /// A session to run.
    Run(Box<Session>),
}

/// Which help text `--help[=topic]` asks for (`startup.c:704`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTopic {
    /// `--help`, `--help=options`, `-?`
    Options,
    /// `--help=commands`
    Commands,
    /// `--help=variables`
    Variables,
}

/// `struct adhoc_opts` (`startup.c:66`) plus everything the option loop wrote
/// straight into `pset`.
// One field per `struct adhoc_opts` member, and upstream's are `bool`.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    /// `options->dbname`
    pub dbname: Option<String>,
    /// `options->host`
    pub host: Option<String>,
    /// `options->port`
    pub port: Option<String>,
    /// `options->username`
    pub username: Option<String>,
    /// `options->logfilename`
    pub logfilename: Option<String>,
    /// `options->no_readline`
    pub no_readline: bool,
    /// `options->no_psqlrc`
    pub no_psqlrc: bool,
    /// `options->single_txn`
    pub single_txn: bool,
    /// `options->list_dbs`
    pub list_dbs: bool,
    /// `options->actions`, in argv order.
    pub actions: Vec<Action>,
    /// `pset`, as the option loop left it.
    pub pset: PsqlSettings,
    /// `pset.vars`, as the option loop left it.
    pub vars: VariableSpace,
    /// `-o FILENAME`: where query output goes (`setQFout`).
    pub output: Option<String>,
    /// Extra bare words the loop warned about (`startup.c:740`).
    pub warnings: Vec<String>,
}

/// `main()`'s `argv[1]`-only fast path (`startup.c:138`-`:150`).
///
/// `-?` is a help request wherever it appears, but `--help` only when it is
/// the *sole* argument; `--version`/`-V` only as `argv[1]`.
#[must_use]
pub fn fast_path(args: &[OsString]) -> Option<Invocation> {
    let first = args.first()?.to_str()?;
    if first == "-?" || (args.len() == 1 && first == "--help") {
        return Some(Invocation::PrintHelp(HelpTopic::Options));
    }
    if first == "--version" || first == "-V" {
        return Some(Invocation::PrintVersion);
    }
    None
}

/// What `getopt_long` hands the option loop for one option
/// (`startup.c:536`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// A short option's letter; a long option with a letter maps to it.
    Short(char),
    /// `--help[=topic]`, code 1.
    Help,
    /// `--csv`, code 2.
    Csv,
}

/// `long_options[]` (`startup.c:490`-`:527`), in its order: the long name,
/// whether it takes a value, and the code the loop sees. `--help` is
/// `optional_argument` upstream; here it takes a value only as `--help=…`.
pub const LONG_OPTIONS: [(&str, bool, Code); 36] = [
    ("echo-all", false, Code::Short('a')),
    ("no-align", false, Code::Short('A')),
    ("command", true, Code::Short('c')),
    ("dbname", true, Code::Short('d')),
    ("echo-queries", false, Code::Short('e')),
    ("echo-errors", false, Code::Short('b')),
    ("echo-hidden", false, Code::Short('E')),
    ("file", true, Code::Short('f')),
    ("field-separator", true, Code::Short('F')),
    ("field-separator-zero", false, Code::Short('z')),
    ("host", true, Code::Short('h')),
    ("html", false, Code::Short('H')),
    ("list", false, Code::Short('l')),
    ("log-file", true, Code::Short('L')),
    ("no-readline", false, Code::Short('n')),
    ("single-transaction", false, Code::Short('1')),
    ("output", true, Code::Short('o')),
    ("port", true, Code::Short('p')),
    ("pset", true, Code::Short('P')),
    ("quiet", false, Code::Short('q')),
    ("record-separator", true, Code::Short('R')),
    ("record-separator-zero", false, Code::Short('0')),
    ("single-step", false, Code::Short('s')),
    ("single-line", false, Code::Short('S')),
    ("tuples-only", false, Code::Short('t')),
    ("table-attr", true, Code::Short('T')),
    ("username", true, Code::Short('U')),
    ("set", true, Code::Short('v')),
    ("variable", true, Code::Short('v')),
    ("version", false, Code::Short('V')),
    ("no-password", false, Code::Short('w')),
    ("password", false, Code::Short('W')),
    ("expanded", false, Code::Short('x')),
    ("no-psqlrc", false, Code::Short('X')),
    ("help", false, Code::Help),
    ("csv", false, Code::Csv),
];

/// Every option on the command line, in argv order, with its value: the
/// sequence `getopt_long` feeds the `switch` at `startup.c:539`.
///
/// Walks argv the way getopt does, using [`SHORT_OPTIONS`] and
/// [`LONG_OPTIONS`] to know which options swallow the next word. It answers
/// one question — which options, in what order — and leaves every judgement
/// about whether the command line is well formed to usage-rs, which has
/// already accepted it by the time [`apply`] asks. Bare words are operands;
/// `--` ends the options.
#[must_use]
pub fn getopt(args: &[OsString]) -> Vec<(Code, Option<String>)> {
    let mut options = Vec::new();
    let mut iter = args.iter().peekable();

    while let Some(arg) = iter.next() {
        let Some(text) = arg.to_str() else { continue };
        if text == "-" || !text.starts_with('-') {
            continue;
        }
        if text == "--" {
            break;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            if let Some(&(_, takes_value, code)) =
                LONG_OPTIONS.iter().find(|(long, _, _)| *long == name)
            {
                let value = match inline {
                    Some(value) => Some(value),
                    None if takes_value => next_value(&mut iter),
                    None => None,
                };
                options.push((code, value));
            }
            continue;
        }

        // A short-option cluster: `-Xc 'select 1'` is `-X` then `-c …`.
        let mut chars = text[1..].chars();
        while let Some(c) = chars.next() {
            if !SHORT_OPTIONS.contains(&format!("{c}:")) {
                options.push((Code::Short(c), None));
                continue;
            }
            let rest: String = chars.by_ref().collect();
            let value = if rest.is_empty() {
                next_value(&mut iter)
            } else {
                Some(rest)
            };
            options.push((Code::Short(c), value));
            break;
        }
    }
    options
}

/// The ordered `-c`/`-f` action list (`startup.c:550`-`:573`): [`getopt`]'s
/// `-c` and `-f`, in argv order.
#[must_use]
pub fn actions(args: &[OsString]) -> Vec<Action> {
    let mut actions = Vec::new();
    for (code, value) in getopt(args) {
        match code {
            Code::Short('c') => push_action(&mut actions, false, value),
            Code::Short('f') => push_action(&mut actions, true, value),
            _ => {}
        }
    }
    actions
}

fn next_value<'a>(
    iter: &mut std::iter::Peekable<impl Iterator<Item = &'a OsString>>,
) -> Option<String> {
    iter.next().and_then(|v| v.to_str().map(str::to_string))
}

fn push_action(actions: &mut Vec<Action>, is_file: bool, value: Option<String>) {
    let Some(value) = value else { return };
    if is_file {
        actions.push(Action::File(Some(value)));
    } else if let Some(slash) = value.strip_prefix('\\') {
        actions.push(Action::SingleSlash(slash.to_string()));
    } else {
        actions.push(Action::SingleQuery(value));
    }
}

/// An option the loop refused: everything upstream writes to stderr before
/// `exit(EXIT_FAILURE)`, each line with its `psql: error: ` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fatal {
    /// The stderr text, newline-terminated.
    pub stderr: String,
}

impl From<AssignError> for Fatal {
    /// `-v`'s `SetVariable`/`DeleteVariable` logged its error, then
    /// `exit(EXIT_FAILURE)` (`startup.c:653`, `:659`).
    fn from(err: AssignError) -> Self {
        Self {
            stderr: format!("psql: error: {}\n", err.message),
        }
    }
}

/// `-P VAR[=ARG]` (`startup.c:600`): `do_pset` with `quiet` true, then
/// `pg_fatal` naming the parameter alone if it refused.
fn pset_option(value: &str, popt: &mut PrintQueryOpt) -> Result<(), Fatal> {
    let (param, arg) = match value.split_once('=') {
        Some((param, arg)) => (param, Some(arg)),
        None => (value, None),
    };
    match do_pset(param, arg, popt, true) {
        Ok(_) => Ok(()),
        Err(err) => Err(Fatal {
            stderr: format!(
                "psql: error: {}\npsql: error: could not set printing parameter \"{param}\"\n",
                err.message
            ),
        }),
    }
}

/// `parse_psql_options()` (`startup.c:488`): turn the command line into the
/// settings and variables the rest of psql reads.
///
/// The options are applied in argv order, one arm per case of upstream's
/// `switch`, so a later option overrides an earlier one exactly as it does
/// there: `-A -P format=html` is HTML, `-P format=html -A` unaligned.
/// `options` is usage-rs's parse of the same argv, which supplies the bare
/// words.
///
/// # Errors
/// What `-v NAME=VALUE`'s `SetVariable` (`startup.c:659`) or `-P VAR=ARG`'s
/// `do_pset` and `pg_fatal` (`startup.c:617`) print before exiting 1.
pub fn apply(options: &Options, args: &[OsString]) -> Result<Session, Fatal> {
    let mut vars = VariableSpace::new();
    let mut pset = PsqlSettings::default();
    let mut session = Session {
        dbname: None,
        host: None,
        port: None,
        username: None,
        logfilename: None,
        no_readline: false,
        no_psqlrc: false,
        single_txn: false,
        list_dbs: false,
        actions: actions(args),
        pset: PsqlSettings::default(),
        vars: VariableSpace::new(),
        output: None,
        warnings: Vec::new(),
    };

    // `main()` seeds these before the option loop (`startup.c:202`-`:206`).
    for (name, value) in crate::variables::default_prompts() {
        vars.set(name, Some(value))?;
    }
    vars.set_bool("AUTOCOMMIT")?;
    vars.set_bool("SHOW_ALL_RESULTS")?;

    // The getopt switch (`startup.c:539`-`:727`), in argv order.
    for (code, value) in getopt(args) {
        let Code::Short(c) = code else {
            if code == Code::Csv {
                pset.popt.topt.format = PrintFormat::Csv;
            }
            continue;
        };
        // usage-rs has already refused an option missing its value.
        let value = value.unwrap_or_default();
        match c {
            'a' => vars.set("ECHO", Some("all"))?,
            'A' => pset.popt.topt.format = PrintFormat::Unaligned,
            'b' => vars.set("ECHO", Some("errors"))?,
            'd' => session.dbname = Some(value),
            'e' => vars.set("ECHO", Some("queries"))?,
            'E' => vars.set_bool("ECHO_HIDDEN")?,
            'F' => {
                pset.popt.topt.field_sep.separator = Some(value);
                pset.popt.topt.field_sep.separator_zero = false;
            }
            'h' => session.host = Some(value),
            'H' => pset.popt.topt.format = PrintFormat::Html,
            'l' => session.list_dbs = true,
            'L' => session.logfilename = Some(value),
            'n' => session.no_readline = true,
            'o' => session.output = Some(value),
            'p' => session.port = Some(value),
            'P' => pset_option(&value, &mut pset.popt)?,
            'q' => vars.set_bool("QUIET")?,
            'R' => {
                pset.popt.topt.record_sep.separator = Some(value);
                pset.popt.topt.record_sep.separator_zero = false;
            }
            's' => vars.set_bool("SINGLESTEP")?,
            'S' => vars.set_bool("SINGLELINE")?,
            't' => pset.popt.topt.tuples_only = true,
            'T' => pset.popt.topt.table_attr = Some(value),
            'U' => session.username = Some(value),
            // `-v NAME=VALUE`, or `-v NAME` to delete (`startup.c:644`).
            'v' => match value.split_once('=') {
                Some((name, value)) => vars.set(name, Some(value))?,
                None => vars.delete(&value)?,
            },
            'w' => pset.get_password = Trivalue::No,
            'W' => pset.get_password = Trivalue::Yes,
            'x' => pset.popt.topt.expanded = Expanded::On,
            'X' => session.no_psqlrc = true,
            'z' => pset.popt.topt.field_sep.separator_zero = true,
            '0' => pset.popt.topt.record_sep.separator_zero = true,
            '1' => session.single_txn = true,
            // `-c` and `-f` are `actions`; `-V` and `-?` never reach here
            // (`fast_path`).
            _ => {}
        }
    }

    // The remaining bare words are the database name and the user name
    // (`startup.c:733`).
    for word in &options.positional {
        if session.dbname.is_none() {
            session.dbname = Some(word.clone());
        } else if session.username.is_none() {
            session.username = Some(word.clone());
        } else {
            session
                .warnings
                .push(format!("extra command-line argument \"{word}\" ignored"));
        }
    }

    pset = vars.settings(&pset);
    pset.apply_separator_defaults();
    session.pset = pset;
    session.vars = vars;
    Ok(session)
}

/// The whole of "what does this command line mean": [`fast_path`], then
/// usage-rs, then [`apply`].
#[must_use]
pub fn plan(args: &[OsString]) -> Invocation {
    if let Some(fast) = fast_path(args) {
        return fast;
    }
    // `--help=topic` (`startup.c:704`). usage-rs owns `--help` itself, so the
    // topic form is recognized here before the parser sees it.
    for arg in args {
        if let Some(topic) = arg.to_str().and_then(|a| a.strip_prefix("--help=")) {
            return match topic {
                "options" => Invocation::PrintHelp(HelpTopic::Options),
                "commands" => Invocation::PrintHelp(HelpTopic::Commands),
                "variables" => Invocation::PrintHelp(HelpTopic::Variables),
                _ => Invocation::Unparsable(format!(
                    "psql: error: unrecognized value \"{topic}\" for \"--help\"\n"
                )),
            };
        }
    }

    let words: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    match Options::parse_from(&words) {
        Ok(options) => match apply(&options, args) {
            Ok(session) => Invocation::Run(Box::new(session)),
            Err(fatal) => Invocation::Fatal(fatal.stderr),
        },
        Err(err) => Invocation::Unparsable(Options::render_failure(&words, &err).clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Echo;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn session(list: &[&str]) -> Session {
        match plan(&args(list)) {
            Invocation::Run(session) => *session,
            other => panic!("expected a session, got {other:?}"),
        }
    }

    #[test]
    fn version_and_help_use_the_argv_one_fast_path() {
        // `startup.c:138`: -? anywhere, --help only alone, --version as argv[1].
        assert_eq!(plan(&args(&["--version"])), Invocation::PrintVersion);
        assert_eq!(plan(&args(&["-V"])), Invocation::PrintVersion);
        assert_eq!(
            plan(&args(&["--help"])),
            Invocation::PrintHelp(HelpTopic::Options)
        );
        assert_eq!(
            plan(&args(&["-?", "-X"])),
            Invocation::PrintHelp(HelpTopic::Options)
        );
        // `--help` with company is not the fast path.
        assert!(!matches!(
            plan(&args(&["--help", "postgres"])),
            Invocation::PrintVersion
        ));
    }

    #[test]
    fn help_topics_are_recognized() {
        assert_eq!(
            plan(&args(&["--help=commands"])),
            Invocation::PrintHelp(HelpTopic::Commands)
        );
        assert_eq!(
            plan(&args(&["--help=variables"])),
            Invocation::PrintHelp(HelpTopic::Variables)
        );
        assert_eq!(
            plan(&args(&["--help=options"])),
            Invocation::PrintHelp(HelpTopic::Options)
        );
    }

    #[test]
    fn the_acceptance_line_parses() {
        let session = session(&["-X", "-c", "select 1"]);
        assert!(session.no_psqlrc);
        assert_eq!(
            session.actions,
            vec![Action::SingleQuery("select 1".into())]
        );
    }

    #[test]
    fn actions_keep_argv_order() {
        // `simple_action_list_append` (`startup.c:753`) appends to the tail.
        let session = session(&["-c", "one", "-f", "a.sql", "-c", "two"]);
        assert_eq!(
            session.actions,
            vec![
                Action::SingleQuery("one".into()),
                Action::File(Some("a.sql".into())),
                Action::SingleQuery("two".into()),
            ]
        );
    }

    #[test]
    fn a_backslash_command_becomes_a_slash_action_without_its_backslash() {
        // `startup.c:554`: optarg + 1.
        let session = session(&["-c", "\\echo hi"]);
        assert_eq!(session.actions, vec![Action::SingleSlash("echo hi".into())]);
    }

    #[test]
    fn actions_are_found_through_clusters_and_attached_values() {
        assert_eq!(
            actions(&args(&["-Xc", "select 1"])),
            vec![Action::SingleQuery("select 1".into())]
        );
        assert_eq!(
            actions(&args(&["-cselect 1"])),
            vec![Action::SingleQuery("select 1".into())]
        );
        assert_eq!(
            actions(&args(&["--command=select 1"])),
            vec![Action::SingleQuery("select 1".into())]
        );
    }

    #[test]
    fn a_value_that_looks_like_an_option_is_not_read_as_one() {
        // `-c -f` gives -c the literal string "-f"; no file action follows.
        assert_eq!(
            actions(&args(&["-c", "-f"])),
            vec![Action::SingleQuery("-f".into())]
        );
    }

    #[test]
    fn the_short_option_string_and_the_table_agree() {
        // Every short option in the getopt string (`startup.c:536`) must be a
        // row of the table, and vice versa; the two are upstream's own
        // duplication and this is what keeps them from drifting apart here.
        let mut from_string: Vec<char> = SHORT_OPTIONS
            .chars()
            .filter(|c| *c != ':' && *c != '?')
            .collect();
        from_string.sort_unstable();
        let mut from_table: Vec<char> = "aAcdebEfFzhHlLn1opPqR0sStTUvVwWxX".chars().collect();
        from_table.sort_unstable();
        assert_eq!(from_string, from_table);
    }

    #[test]
    fn no_psqlrc_is_recorded_but_the_startup_file_is_never_read() {
        // `-X` sets `options->no_psqlrc` (`startup.c:679`), which upstream
        // then consults at `startup.c:352` to decide whether to call
        // `process_psqlrc`. This port records the flag and has no
        // `process_psqlrc` to call, so *every* invocation behaves as if `-X`
        // had been given — the divergence `docs/divergences.md` records.
        // This test pins the flag; when `process_psqlrc` lands it gains the
        // behavioural half and the divergence row goes away.
        assert!(session(&["-X", "-c", "select 1"]).no_psqlrc);
        assert!(session(&["--no-psqlrc", "-c", "select 1"]).no_psqlrc);
        assert!(!session(&["-c", "select 1"]).no_psqlrc);
        // Nothing in the crate reads a startup file, so there is no path the
        // flag could change: `Session` carries it and `run_session` never
        // branches on it.
        assert!(
            !std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"))
                .expect("read the crate root")
                .contains("psqlrc_file"),
            "a psqlrc reader landed; update docs/divergences.md and this test"
        );
    }

    #[test]
    fn output_mode_switches_reach_the_print_options() {
        assert_eq!(
            session(&["-A"]).pset.popt.topt.format,
            PrintFormat::Unaligned
        );
        assert_eq!(session(&["-H"]).pset.popt.topt.format, PrintFormat::Html);
        assert_eq!(session(&["--csv"]).pset.popt.topt.format, PrintFormat::Csv);
        assert!(session(&["-t"]).pset.popt.topt.tuples_only);
        assert_eq!(
            session(&["-x"]).pset.popt.topt.expanded,
            crate::settings::Expanded::On
        );
    }

    #[test]
    fn echo_switches_reach_the_echo_setting() {
        assert_eq!(session(&["-a"]).pset.echo, Echo::All);
        assert_eq!(session(&["-e"]).pset.echo, Echo::Queries);
        assert_eq!(session(&["-b"]).pset.echo, Echo::Errors);
        assert_eq!(session(&[]).pset.echo, Echo::None);
    }

    #[test]
    fn dash_v_sets_and_unsets_variables() {
        assert!(session(&["-v", "ON_ERROR_STOP=1"]).pset.on_error_stop);
        // `-v NAME` with no `=` deletes (`startup.c:651`).
        assert_eq!(
            session(&["-v", "AUTOCOMMIT"]).vars.get("AUTOCOMMIT"),
            Some("off")
        );
    }

    #[test]
    fn a_bad_variable_assignment_is_reported_not_swallowed() {
        match plan(&args(&["-v", "ECHO=sideways"])) {
            Invocation::Fatal(text) => {
                assert!(
                    text.starts_with("psql: error: unrecognized value \"sideways\""),
                    "{text}"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn bare_words_are_the_database_then_the_user() {
        // `startup.c:733`.
        let session = session(&["mydb", "myuser", "extra"]);
        assert_eq!(session.dbname.as_deref(), Some("mydb"));
        assert_eq!(session.username.as_deref(), Some("myuser"));
        assert_eq!(
            session.warnings,
            vec!["extra command-line argument \"extra\" ignored"]
        );
    }

    #[test]
    fn separator_switches_win_over_the_defaults() {
        assert_eq!(session(&["-F", ";"]).pset.popt.topt.field_sep.bytes(), b";");
        assert_eq!(session(&["-z"]).pset.popt.topt.field_sep.bytes(), vec![0]);
        assert_eq!(session(&[]).pset.popt.topt.field_sep.bytes(), b"|");
    }

    #[test]
    fn an_unknown_option_is_refused_with_a_nonempty_message() {
        // What the stolen `program_options_handling_ok` requires: a nonzero
        // exit and a non-empty stderr (AGENTS.md, ADR-0004).
        match plan(&args(&["--nosuchoption"])) {
            Invocation::Unparsable(text) => assert!(!text.is_empty()),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn connection_switches_are_carried_through() {
        let connected = session(&["-h", "srv", "-p", "5433", "-U", "bob", "-d", "db"]);
        assert_eq!(connected.host.as_deref(), Some("srv"));
        assert_eq!(connected.port.as_deref(), Some("5433"));
        assert_eq!(connected.username.as_deref(), Some("bob"));
        assert_eq!(connected.dbname.as_deref(), Some("db"));
        assert_eq!(connected.pset.get_password, Trivalue::Default);
        assert_eq!(session(&["-w"]).pset.get_password, Trivalue::No);
        assert_eq!(session(&["-W"]).pset.get_password, Trivalue::Yes);
    }

    #[test]
    fn the_long_option_table_and_the_short_option_string_agree() {
        // A long option takes a value exactly when its letter has a `:` in
        // the getopt string (`startup.c:490`, `:536`).
        for (name, takes_value, code) in LONG_OPTIONS {
            if let Code::Short(c) = code {
                assert!(SHORT_OPTIONS.contains(c), "--{name}");
                assert_eq!(
                    SHORT_OPTIONS.contains(&format!("{c}:")),
                    takes_value,
                    "--{name}"
                );
            }
        }
    }

    #[test]
    fn getopt_yields_every_option_in_argv_order() {
        assert_eq!(
            getopt(&args(&[
                "-XA",
                "db",
                "--pset=border=2",
                "-Pnull=x",
                "--csv",
                "--",
                "-t"
            ])),
            vec![
                (Code::Short('X'), None),
                (Code::Short('A'), None),
                (Code::Short('P'), Some("border=2".into())),
                (Code::Short('P'), Some("null=x".into())),
                (Code::Csv, None),
            ]
        );
    }

    #[test]
    fn dash_p_sets_a_printing_parameter() {
        // `startup.c:600`: `-P VAR=ARG` is `\pset VAR ARG`, `-P VAR` is
        // `\pset VAR`.
        let topt = session(&["-P", "border=2", "-P", "null=(nil)", "-P", "tuples_only"])
            .pset
            .popt;
        assert_eq!(topt.topt.border, 2);
        assert_eq!(topt.null_print.as_deref(), Some("(nil)"));
        assert!(topt.topt.tuples_only);
        assert_eq!(
            session(&["--pset=format=csv"]).pset.popt.topt.format,
            PrintFormat::Csv
        );
    }

    #[test]
    fn options_apply_in_argv_order() {
        // The option loop is one pass over argv (`startup.c:536`), so the
        // later of two options writing the same setting wins.
        let format = |list: &[&str]| session(list).pset.popt.topt.format;
        assert_eq!(format(&["-A", "-P", "format=html"]), PrintFormat::Html);
        assert_eq!(format(&["-P", "format=html", "-A"]), PrintFormat::Unaligned);
        assert_eq!(format(&["--csv", "-H"]), PrintFormat::Html);
        assert_eq!(format(&["-H", "--csv"]), PrintFormat::Csv);
        assert_eq!(session(&["-e", "-a"]).pset.echo, Echo::All);
        assert_eq!(session(&["-W", "-w"]).pset.get_password, Trivalue::No);
        assert_eq!(
            session(&["-d", "one", "--dbname=two"]).dbname.as_deref(),
            Some("two")
        );
    }

    #[test]
    fn a_refused_printing_parameter_is_fatal() {
        // `do_pset` logs its error, then `pg_fatal` names the parameter
        // alone: `value` was cut at the `=` (`startup.c:612`-`:617`).
        assert_eq!(
            plan(&args(&["-P", "nosuch=1"])),
            Invocation::Fatal(
                "psql: error: \\pset: unknown option: nosuch\n\
                 psql: error: could not set printing parameter \"nosuch\"\n"
                    .into()
            )
        );
        assert_eq!(
            plan(&args(&["-P", "format=bogus"])),
            Invocation::Fatal(
                "psql: error: \\pset: allowed formats are aligned, asciidoc, csv, html, latex, latex-longtable, troff-ms, unaligned, wrapped\n\
                 psql: error: could not set printing parameter \"format\"\n"
                    .into()
            )
        );
    }
}
