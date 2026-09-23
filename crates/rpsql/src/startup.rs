//! The psql command line: `src/bin/psql/startup.c`.
//!
//! [`Options`] is `long_options[]` (`startup.c:490`-`:529`) one row at a time,
//! parsed by usage-rs (ADR-0004). [`actions`] is the other half of
//! `parse_psql_options`: `-c` and `-f` build a `SimpleActionList` *in argv
//! order* (`startup.c:550`-`:573`), which a declarative option table cannot express,
//! so it is its own pure walk over argv driven by [`SHORT_OPTIONS`] — the
//! getopt string upstream passes on the same line.
//!
//! `--help`, `--help=…` and `--version` are NAT-399's; the `argv[1]` fast path
//! at `startup.c:138` is here because it decides what an invocation *is*.

use std::ffi::{OsStr, OsString};

use usage::Cli;

use crate::settings::{PrintFormat, PsqlSettings, Trivalue};
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

/// The ordered `-c`/`-f` action list (`startup.c:550`-`:573`).
///
/// Walks argv the way getopt does, using [`SHORT_OPTIONS`] to know which short
/// options swallow the next word. It answers one question — in what order did
/// `-c` and `-f` appear — and leaves every other judgement to usage-rs.
#[must_use]
pub fn actions(args: &[OsString]) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut iter = args.iter().peekable();
    let mut only_operands = false;

    while let Some(arg) = iter.next() {
        let Some(text) = arg.to_str() else { continue };
        if only_operands || text == "-" || !text.starts_with('-') {
            continue;
        }
        if text == "--" {
            only_operands = true;
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let kind = match name {
                "command" => Some(false),
                "file" => Some(true),
                _ => None,
            };
            if let Some(is_file) = kind {
                let value = inline.or_else(|| next_value(&mut iter));
                push_action(&mut actions, is_file, value);
            }
            continue;
        }

        // A short-option cluster: `-Xc 'select 1'` is `-X` then `-c …`.
        let mut chars = text[1..].chars();
        while let Some(c) = chars.next() {
            let takes_value = SHORT_OPTIONS.contains(&format!("{c}:"));
            if !takes_value {
                continue;
            }
            let rest: String = chars.by_ref().collect();
            let value = if rest.is_empty() {
                next_value(&mut iter)
            } else {
                Some(rest)
            };
            match c {
                'c' => push_action(&mut actions, false, value),
                'f' => push_action(&mut actions, true, value),
                _ => {}
            }
            break;
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

/// `parse_psql_options()` (`startup.c:488`): turn a parsed [`Options`] into the
/// settings and variables the rest of psql reads.
///
/// # Errors
/// The message `-v NAME=VALUE`'s `SetVariable` would have logged before
/// `exit(EXIT_FAILURE)` (`startup.c:659`).
// The body is one table, one `if` per row of upstream's `long_options[]` and
// in its order; splitting it would only move the rows somewhere else.
#[allow(clippy::too_many_lines)]
pub fn apply(options: &Options, args: &[OsString]) -> Result<Session, AssignError> {
    let mut vars = VariableSpace::new();
    let mut pset = PsqlSettings::default();
    let mut warnings = Vec::new();

    // `main()` seeds these before the option loop (`startup.c:202`-`:206`).
    for (name, value) in crate::variables::default_prompts() {
        vars.set(name, Some(value))?;
    }
    vars.set_bool("AUTOCOMMIT")?;
    vars.set_bool("SHOW_ALL_RESULTS")?;

    // The getopt switch, in `long_options[]` order.
    if options.echo_all {
        vars.set("ECHO", Some("all"))?;
    }
    if options.no_align {
        pset.popt.topt.format = PrintFormat::Unaligned;
    }
    if options.echo_queries {
        vars.set("ECHO", Some("queries"))?;
    }
    if options.echo_errors {
        vars.set("ECHO", Some("errors"))?;
    }
    if options.echo_hidden {
        vars.set_bool("ECHO_HIDDEN")?;
    }
    if let Some(sep) = &options.field_separator {
        pset.popt.topt.field_sep.separator = Some(sep.clone());
        pset.popt.topt.field_sep.separator_zero = false;
    }
    if options.html {
        pset.popt.topt.format = PrintFormat::Html;
    }
    if options.quiet {
        vars.set_bool("QUIET")?;
    }
    if let Some(sep) = &options.record_separator {
        pset.popt.topt.record_sep.separator = Some(sep.clone());
        pset.popt.topt.record_sep.separator_zero = false;
    }
    if options.single_step {
        vars.set_bool("SINGLESTEP")?;
    }
    if options.single_line {
        vars.set_bool("SINGLELINE")?;
    }
    if options.tuples_only {
        pset.popt.topt.tuples_only = true;
    }
    if let Some(attr) = &options.table_attr {
        pset.popt.topt.table_attr = Some(attr.clone());
    }
    // `-v NAME=VALUE`, or `-v NAME` to delete (`startup.c:644`).
    for assignment in &options.set {
        match assignment.split_once('=') {
            Some((name, value)) => vars.set(name, Some(value))?,
            None => vars.delete(assignment)?,
        }
    }
    if options.no_password {
        pset.get_password = Trivalue::No;
    }
    if options.password {
        pset.get_password = Trivalue::Yes;
    }
    if options.expanded {
        pset.popt.topt.expanded = true;
    }
    if options.field_separator_zero {
        pset.popt.topt.field_sep.separator_zero = true;
    }
    if options.record_separator_zero {
        pset.popt.topt.record_sep.separator_zero = true;
    }
    if options.csv {
        pset.popt.topt.format = PrintFormat::Csv;
    }

    // The remaining bare words are the database name and the user name
    // (`startup.c:733`).
    let mut dbname = options.dbname.clone();
    let mut username = options.username.clone();
    for word in &options.positional {
        if dbname.is_none() {
            dbname = Some(word.clone());
        } else if username.is_none() {
            username = Some(word.clone());
        } else {
            warnings.push(format!("extra command-line argument \"{word}\" ignored"));
        }
    }

    pset = vars.settings(&pset);
    pset.apply_separator_defaults();

    Ok(Session {
        dbname,
        host: options.host.clone(),
        port: options.port.clone(),
        username,
        logfilename: options.log_file.clone(),
        no_readline: options.no_readline,
        no_psqlrc: options.no_psqlrc,
        single_txn: options.single_transaction,
        list_dbs: options.list,
        actions: actions(args),
        pset,
        vars,
        output: options.output.clone(),
        warnings,
    })
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
            Err(err) => Invocation::Unparsable(format!("psql: error: {}\n", err.message)),
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
        assert!(session(&["-x"]).pset.popt.topt.expanded);
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
            Invocation::Unparsable(text) => {
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
}
