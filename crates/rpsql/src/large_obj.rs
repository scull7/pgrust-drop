//! `\lo_import`, `\lo_export`, `\lo_list` and `\lo_unlink`:
//! `src/bin/psql/large_obj.c`, `exec_command_lo` (`command.c:2368`) and
//! `listLargeObjects` (`describe.c:7284`).
//!
//! The file transfer itself is rlibpq's `lo_import` / `lo_export`
//! (`fe-lobj.c`), which the live connection hands out as
//! [`LargeObjects`]; everything psql adds around it — the argument handling,
//! the transaction it opens when there is none (`start_lo_xact`), the
//! `COMMENT ON LARGE OBJECT` an import may carry, the result line and
//! `LASTOID` — is here. Which command a line names ([`LoCommand::parse`]),
//! `atooid` ([`atooid`]) and `expand_tilde` ([`expand_tilde`]) are pure;
//! [`exec_command_lo`] is the thin action over the executor.

use std::io::Write;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use rlibpq::{ExecStatus, LoError, QueryResult, TransactionStatus};

use crate::command::{CommandContext, CommandResult};
use crate::common::Executor;
use crate::logging;
use crate::output::Output;
use crate::print::print_query;
use crate::settings::{EchoHidden, Expanded, PrintFormat, PsqlSettings, SendMode};
use crate::slash::SlashOption;

/// The large-object calls of `pset.db` that psql makes outside `PSQLexec`:
/// `PQtransactionStatus`, `lo_import`, `lo_export`, `lo_unlink` and
/// `PQescapeStringConn`.
pub trait LargeObjects {
    /// `PQtransactionStatus(pset.db)` (`fe-connect.c:7583`).
    fn transaction_status(&self) -> TransactionStatus;

    /// `lo_import` (`fe-lobj.c:626`): the new large object's OID.
    ///
    /// # Errors
    /// What C returns `InvalidOid` for, with `PQerrorMessage`'s text.
    fn lo_import(&mut self, filename: &Path) -> Result<u32, LoError>;

    /// `lo_export` (`fe-lobj.c:748`).
    ///
    /// # Errors
    /// What C returns -1 for.
    fn lo_export(&mut self, loid: u32, filename: &Path) -> Result<(), LoError>;

    /// `lo_unlink` (`fe-lobj.c:589`).
    ///
    /// # Errors
    /// What C returns -1 for.
    fn lo_unlink(&mut self, loid: u32) -> Result<(), LoError>;

    /// `PQescapeStringConn(pset.db, …, NULL)` (`fe-exec.c:4208`): the
    /// escaped text, whose error `do_lo_import` does not ask for.
    fn escape_string(&self, from: &[u8]) -> Vec<u8>;
}

/// One `\lo_*` command, its arguments read the way `exec_command_lo`
/// (`command.c:2368`) reads them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoCommand {
    /// `\lo_export LOBOID FILE`.
    Export {
        /// The OID argument as typed; [`atooid`] reads it.
        loid: String,
        /// The file, `~` expanded.
        filename: Vec<u8>,
    },
    /// `\lo_import FILE [COMMENT]`.
    Import {
        /// The file, `~` expanded.
        filename: Vec<u8>,
        /// The comment, if given.
        comment: Option<String>,
    },
    /// `\lo_list[+][x]`.
    List {
        /// `+`: the access privileges column.
        verbose: bool,
        /// `x`: expanded output for this listing.
        expanded: bool,
    },
    /// `\lo_unlink LOBOID`.
    Unlink {
        /// The OID argument as typed.
        loid: String,
    },
}

/// What a `\lo_…` command line amounts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoParse {
    /// A command to run.
    Command(LoCommand),
    /// `\%s: missing required argument` (`command.c:2387`, `:2401`, `:2433`).
    MissingArgument,
    /// No such `\lo_` command: `PSQL_CMD_UNKNOWN` (`command.c:2441`).
    Unknown,
}

impl LoCommand {
    /// Calculation: the `strcmp(cmd + 3, …)` chain of `exec_command_lo`
    /// (`command.c:2383`-`:2441`) over its two options, with `home` for
    /// `expand_tilde`. `cmd` starts with `lo_`.
    #[must_use]
    pub fn parse(cmd: &str, options: &[SlashOption], home: Option<&[u8]>) -> LoParse {
        let opt1 = options.first().map(|o| o.value.as_str());
        let opt2 = options.get(1).map(|o| o.value.as_str());
        let rest = cmd.get(3..).unwrap_or("");
        let tilde = |f: &str| expand_tilde(f.as_bytes(), home);
        let command = match rest {
            "export" => match (opt1, opt2) {
                (Some(loid), Some(filename)) => LoCommand::Export {
                    loid: loid.to_owned(),
                    filename: tilde(filename),
                },
                _ => return LoParse::MissingArgument,
            },
            "import" => match opt1 {
                Some(filename) => LoCommand::Import {
                    filename: tilde(filename),
                    comment: opt2.map(str::to_owned),
                },
                None => return LoParse::MissingArgument,
            },
            // `strncmp(cmd + 3, "list", 4)`: anything that starts `list`,
            // with `+` and `x` looked for anywhere in the command
            // (`command.c:2411`-`:2421`).
            _ if rest.starts_with("list") => LoCommand::List {
                verbose: cmd.contains('+'),
                expanded: cmd.contains('x'),
            },
            "unlink" => match opt1 {
                Some(loid) => LoCommand::Unlink {
                    loid: loid.to_owned(),
                },
                None => return LoParse::MissingArgument,
            },
            _ => return LoParse::Unknown,
        };
        LoParse::Command(command)
    }
}

/// Calculation: `atooid(x)`, `(Oid) strtoul((x), NULL, 10)`
/// (`postgres_ext.h:43`): leading white space, an optional sign, as many
/// decimal digits as there are, and 0 when there are none. `strtoul`
/// saturates at `ULONG_MAX` and negates a `-` value in unsigned arithmetic;
/// the cast to `Oid` keeps the low 32 bits of either.
#[must_use]
// The cast is C's `(Oid)`: truncation to the low 32 bits is the point.
#[allow(clippy::cast_possible_truncation)]
pub fn atooid(s: &str) -> u32 {
    let s = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut value: u64 = 0;
    let mut overflow = false;
    for d in digits.bytes().take_while(u8::is_ascii_digit) {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(d - b'0')))
        {
            Some(v) => value = v,
            None => overflow = true,
        }
    }
    let value = if overflow {
        u64::MAX
    } else if negative {
        value.wrapping_neg()
    } else {
        value
    };
    value as u32
}

/// Calculation: `expand_tilde()` (`common.c:2697`): `~` and `~/…` become
/// `home` and `home/…`. `~user` is left as typed, as upstream leaves it for
/// a user `getpwnam` does not know: that lookup is not reachable from the
/// standard library (see `docs/divergences.md`).
#[must_use]
pub fn expand_tilde(filename: &[u8], home: Option<&[u8]>) -> Vec<u8> {
    if filename.first() != Some(&b'~') {
        return filename.to_vec();
    }
    let user_end = filename
        .iter()
        .position(|&b| b == b'/')
        .unwrap_or(filename.len());
    match home {
        Some(home) if user_end == 1 && !home.is_empty() => {
            let mut out = home.to_vec();
            out.extend_from_slice(&filename[user_end..]);
            out
        }
        _ => filename.to_vec(),
    }
}

/// Calculation: the `COMMENT ON LARGE OBJECT` `do_lo_import` sends for a
/// comment (`large_obj.c:206`-`:209`), `escaped` being the comment through
/// `PQescapeStringConn`.
#[must_use]
pub fn comment_query(loid: u32, escaped: &[u8]) -> Vec<u8> {
    let mut query = format!("COMMENT ON LARGE OBJECT {loid} IS '").into_bytes();
    query.extend_from_slice(escaped);
    query.push(b'\'');
    query
}

/// Calculation: `listLargeObjects`' query (`describe.c:7292`-`:7307`), with
/// `printACLColumn(&buf, "lomacl")` (`describe.c:6880`) for `+`.
#[must_use]
pub fn list_large_objects_query(verbose: bool) -> String {
    let mut buf = String::from(concat!(
        "SELECT oid as \"ID\",\n",
        "  pg_catalog.pg_get_userbyid(lomowner) as \"Owner\",\n  ",
    ));
    if verbose {
        buf.push_str(concat!(
            "CASE WHEN pg_catalog.array_length(lomacl, 1) = 0 THEN '(none)'",
            " ELSE pg_catalog.array_to_string(lomacl, E'\\n') END AS \"Access privileges\"",
        ));
        buf.push_str(",\n  ");
    }
    buf.push_str(concat!(
        "pg_catalog.obj_description(oid, 'pg_largeobject') as \"Description\"\n",
        "FROM pg_catalog.pg_largeobject_metadata\n",
        "ORDER BY oid",
    ));
    buf
}

/// Calculation: `print_lo_result()` (`large_obj.c:19`)'s text for
/// `pset.queryFout`, or `None` under `QUIET`. HTML wraps it in `<p>`.
#[must_use]
pub fn lo_result_text(pset: &PsqlSettings, text: &str) -> Option<String> {
    if pset.quiet {
        return None;
    }
    Some(if pset.popt.topt.format == PrintFormat::Html {
        format!("<p>{text}</p>\n")
    } else {
        format!("{text}\n")
    })
}

/// Action: `exec_command_lo()` (`command.c:2368`) for an active branch.
pub fn exec_command_lo(
    cmd: &str,
    options: &[SlashOption],
    ctx: &mut CommandContext<'_>,
    out: &mut Output<'_>,
    stderr: &mut dyn Write,
) -> CommandResult {
    let home = std::env::var_os("HOME");
    let home = home.as_ref().map(|h| h.as_bytes());
    let command = match LoCommand::parse(cmd, options, home) {
        LoParse::Command(command) => command,
        LoParse::MissingArgument => {
            logging::error(
                ctx.pset,
                format!("\\{cmd}: missing required argument"),
                stderr,
            );
            return CommandResult::Error;
        }
        LoParse::Unknown => return CommandResult::Unknown,
    };
    let mut lo = LoContext {
        pset: ctx.pset,
        vars: ctx.vars,
        executor: &mut *ctx.executor,
        out,
        stderr,
    };
    let success = match command {
        LoCommand::Export { loid, filename } => lo.do_lo_export(atooid(&loid), &filename),
        LoCommand::Import { filename, comment } => lo.do_lo_import(&filename, comment.as_deref()),
        LoCommand::List { verbose, expanded } => {
            // `if 'x' option specified, force expanded mode` for this one
            // listing (`command.c:2418`-`:2426`).
            let saved = lo.pset.popt.topt.expanded;
            if expanded {
                lo.pset.popt.topt.expanded = Expanded::On;
            }
            let ok = lo.list_large_objects(verbose);
            lo.pset.popt.topt.expanded = saved;
            ok
        }
        LoCommand::Unlink { loid } => lo.do_lo_unlink(atooid(&loid)),
    };
    if success {
        CommandResult::SkipLine
    } else {
        CommandResult::Error
    }
}

/// What `large_obj.c` reads through the `pset` global.
struct LoContext<'a, 'o> {
    pset: &'a mut PsqlSettings,
    vars: &'a mut crate::variables::VariableSpace,
    executor: &'a mut dyn Executor,
    /// psql's stdout, and `pset.queryFout`, where `large_obj.c` prints
    /// (`large_obj.c:29`, `describe.c:7284`).
    out: &'a mut Output<'o>,
    stderr: &'a mut dyn Write,
}

impl LoContext<'_, '_> {
    /// `do_lo_export()` (`large_obj.c:142`).
    fn do_lo_export(&mut self, loid: u32, filename: &[u8]) -> bool {
        const OP: &str = "\\lo_export";
        let Some(own_transaction) = self.start_lo_xact(OP) else {
            return false;
        };
        // `SetCancelConn(NULL)` … `ResetCancelConn()` (`large_obj.c:150`):
        // a Ctrl-C does not cancel the transfer, and the executor sets no
        // cancel connection for it.
        let outcome = self
            .large_objects()
            .lo_export(loid, Path::new(std::ffi::OsStr::from_bytes(filename)));
        if let Err(err) = outcome {
            self.log_lo_error(&err);
            return self.fail_lo_xact(own_transaction);
        }
        if !self.finish_lo_xact(own_transaction) {
            return false;
        }
        self.print_lo_result("lo_export");
        true
    }

    /// `do_lo_import()` (`large_obj.c:176`).
    fn do_lo_import(&mut self, filename: &[u8], comment: Option<&str>) -> bool {
        const OP: &str = "\\lo_import";
        let Some(own_transaction) = self.start_lo_xact(OP) else {
            return false;
        };
        let outcome = self
            .large_objects()
            .lo_import(Path::new(std::ffi::OsStr::from_bytes(filename)));
        let loid = match outcome {
            Ok(loid) => loid,
            Err(err) => {
                self.log_lo_error(&err);
                return self.fail_lo_xact(own_transaction);
            }
        };

        // Insert the description if given (`large_obj.c:196`).
        if let Some(comment) = comment {
            let escaped = self.large_objects().escape_string(comment.as_bytes());
            if self.psql_exec(&comment_query(loid, &escaped)).is_none() {
                return self.fail_lo_xact(own_transaction);
            }
        }

        if !self.finish_lo_xact(own_transaction) {
            return false;
        }
        self.print_lo_result(&format!("lo_import {loid}"));
        // `SetVariable(pset.vars, "LASTOID", oidbuf)` (`large_obj.c:227`):
        // LASTOID has no hook, so the assignment cannot be refused.
        let _ = self.vars.set("LASTOID", Some(&loid.to_string()));
        true
    }

    /// `do_lo_unlink()` (`large_obj.c:239`).
    fn do_lo_unlink(&mut self, loid: u32) -> bool {
        const OP: &str = "\\lo_unlink";
        let Some(own_transaction) = self.start_lo_xact(OP) else {
            return false;
        };
        if let Err(err) = self.large_objects().lo_unlink(loid) {
            self.log_lo_error(&err);
            return self.fail_lo_xact(own_transaction);
        }
        if !self.finish_lo_xact(own_transaction) {
            return false;
        }
        self.print_lo_result(&format!("lo_unlink {loid}"));
        true
    }

    /// `listLargeObjects()` (`describe.c:7284`), under the title "Large
    /// objects" and the current print options.
    fn list_large_objects(&mut self, verbose: bool) -> bool {
        let Some(result) = self.psql_exec(list_large_objects_query(verbose).as_bytes()) else {
            return false;
        };
        let mut opt = self.pset.popt.clone();
        opt.title = Some("Large objects".to_owned());
        match print_query(&result, &opt) {
            Ok(text) => {
                let _ = self.out.query_fout().write_all(&text);
                true
            }
            Err(err) => {
                logging::error(self.pset, err.to_string(), self.stderr);
                false
            }
        }
    }

    /// The executor's large-object calls. [`Self::start_lo_xact`] has
    /// already refused an executor without them.
    fn large_objects(&mut self) -> &mut dyn LargeObjects {
        self.executor
            .large_objects()
            .expect("start_lo_xact refused an executor without large objects")
    }

    /// `start_lo_xact()` (`large_obj.c:56`): `Some(own_transaction)` once
    /// inside a transaction block, opening one if there was none.
    fn start_lo_xact(&mut self, operation: &str) -> Option<bool> {
        let status = if self.executor.connected() {
            self.executor
                .large_objects()
                .map(|lo| lo.transaction_status())
        } else {
            None
        };
        match status {
            None => {
                logging::error(
                    self.pset,
                    format!("{operation}: not connected to a database"),
                    self.stderr,
                );
                None
            }
            Some(TransactionStatus::Idle) => self.psql_exec(b"BEGIN").map(|_| true),
            Some(TransactionStatus::InTransaction) => Some(false),
            Some(TransactionStatus::InError) => {
                logging::error(
                    self.pset,
                    format!("{operation}: current transaction is aborted"),
                    self.stderr,
                );
                None
            }
            Some(TransactionStatus::Unknown) => {
                logging::error(
                    self.pset,
                    format!("{operation}: unknown transaction status"),
                    self.stderr,
                );
                None
            }
        }
    }

    /// `finish_lo_xact()` (`large_obj.c:98`): commit a transaction of our
    /// own under `AUTOCOMMIT`, rolling back if the commit fails.
    fn finish_lo_xact(&mut self, own_transaction: bool) -> bool {
        if own_transaction && self.pset.autocommit && self.psql_exec(b"COMMIT").is_none() {
            let _ = self.psql_exec(b"ROLLBACK");
            return false;
        }
        true
    }

    /// `fail_lo_xact()` (`large_obj.c:121`): roll back a transaction of our
    /// own under `AUTOCOMMIT`. Always `false`.
    fn fail_lo_xact(&mut self, own_transaction: bool) -> bool {
        if own_transaction && self.pset.autocommit {
            let _ = self.psql_exec(b"ROLLBACK");
        }
        false
    }

    /// `pg_log_info("%s", PQerrorMessage(pset.db))` after a failed call
    /// (`large_obj.c:157`, `:192`, `:254`). A server's refusal is rendered
    /// at `VERBOSITY` and `SHOW_CONTEXT`, which psql sets on the connection.
    fn log_lo_error(&mut self, err: &LoError) {
        let message = match err {
            LoError::Server(result) => result_error_message(result, self.pset),
            other => other.message(),
        };
        logging::info(self.pset, message, self.stderr);
    }

    /// `print_lo_result()` (`large_obj.c:19`). No `-L` log file exists yet.
    fn print_lo_result(&mut self, text: &str) {
        if let Some(text) = lo_result_text(self.pset, text) {
            let _ = self.out.query_fout().write_all(text.as_bytes());
        }
    }

    /// `PSQLexec()` (`common.c:657`): run a query psql builds for itself and
    /// hand back its last result, or `None` after logging why there is none.
    fn psql_exec(&mut self, query: &[u8]) -> Option<QueryResult> {
        if !self.executor.connected() {
            logging::error(
                self.pset,
                "You are currently not connected to a database.",
                self.stderr,
            );
            return None;
        }
        if self.pset.echo_hidden != EchoHidden::Off {
            let _ = self.out.stdout.write_all(b"/******** QUERY *********/\n");
            let _ = self.out.stdout.write_all(query);
            let _ = self
                .out
                .stdout
                .write_all(b"\n/************************/\n\n");
            if self.pset.echo_hidden == EchoHidden::NoExec {
                return None;
            }
        }
        // `PQexec` keeps only the last result.
        let result = match self.executor.exec(query, &SendMode::Query) {
            Ok(mut results) => results.pop()?,
            Err(err) => {
                logging::info(self.pset, err.as_bytes(), self.stderr);
                return None;
            }
        };
        // `AcceptResult(res, true)` (`common.c:418`).
        if matches!(
            result.status(),
            ExecStatus::CommandOk
                | ExecStatus::TuplesOk
                | ExecStatus::EmptyQuery
                | ExecStatus::CopyIn
                | ExecStatus::CopyOut
        ) {
            return Some(result);
        }
        let message = result_error_message(&result, self.pset);
        if !message.is_empty() {
            logging::info(self.pset, message, self.stderr);
        }
        crate::common::clear_or_save_result(&result, self.pset);
        None
    }
}

/// `PQerrorMessage` after a failed result, at the connection's verbosity.
fn result_error_message(result: &QueryResult, pset: &PsqlSettings) -> Vec<u8> {
    match result.error() {
        Some(error) => error.message(result.status(), pset.verbosity, pset.show_context),
        None => result.error_message(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ErrorMessage;
    use crate::variables::VariableSpace;

    fn opts(values: &[&str]) -> Vec<SlashOption> {
        values
            .iter()
            .map(|v| SlashOption {
                value: (*v).to_owned(),
                quote: None,
            })
            .collect()
    }

    #[test]
    fn each_lo_command_reads_its_arguments_as_exec_command_lo_does() {
        let home = Some(b"/home/u".as_slice());
        assert_eq!(
            LoCommand::parse("lo_export", &opts(&["42", "~/f"]), home),
            LoParse::Command(LoCommand::Export {
                loid: "42".to_owned(),
                filename: b"/home/u/f".to_vec(),
            })
        );
        assert_eq!(
            LoCommand::parse("lo_import", &opts(&["f", "a comment"]), home),
            LoParse::Command(LoCommand::Import {
                filename: b"f".to_vec(),
                comment: Some("a comment".to_owned()),
            })
        );
        assert_eq!(
            LoCommand::parse("lo_unlink", &opts(&["7"]), home),
            LoParse::Command(LoCommand::Unlink {
                loid: "7".to_owned()
            })
        );
    }

    #[test]
    fn a_missing_argument_is_refused() {
        // `command.c:2385`, `:2399`, `:2431`: export needs two, the others one.
        for (cmd, args) in [
            ("lo_export", &["42"][..]),
            ("lo_export", &[][..]),
            ("lo_import", &[][..]),
            ("lo_unlink", &[][..]),
        ] {
            assert_eq!(
                LoCommand::parse(cmd, &opts(args), None),
                LoParse::MissingArgument,
                "{cmd} {args:?}"
            );
        }
    }

    #[test]
    fn lo_list_takes_any_suffix_and_reads_plus_and_x_anywhere() {
        // `strncmp(cmd + 3, "list", 4)` and `strchr(cmd, '+')`, `strchr(cmd, 'x')`.
        let list = |cmd| LoCommand::parse(cmd, &[], None);
        assert_eq!(
            list("lo_list"),
            LoParse::Command(LoCommand::List {
                verbose: false,
                expanded: false
            })
        );
        assert_eq!(
            list("lo_list+"),
            LoParse::Command(LoCommand::List {
                verbose: true,
                expanded: false
            })
        );
        assert_eq!(
            list("lo_listx+"),
            LoParse::Command(LoCommand::List {
                verbose: true,
                expanded: true
            })
        );
        assert_eq!(
            list("lo_listing"),
            LoParse::Command(LoCommand::List {
                verbose: false,
                expanded: false
            })
        );
        assert_eq!(list("lo_lis"), LoParse::Unknown);
        assert_eq!(list("lo_"), LoParse::Unknown);
        assert_eq!(list("lo_exports"), LoParse::Unknown);
    }

    #[test]
    fn atooid_is_strtoul_cast_to_oid() {
        assert_eq!(atooid("42"), 42);
        assert_eq!(atooid("  42abc"), 42);
        assert_eq!(atooid("+7"), 7);
        assert_eq!(atooid("abc"), 0);
        assert_eq!(atooid(""), 0);
        assert_eq!(atooid("4294967295"), u32::MAX);
        // Past 32 bits the cast keeps the low bits.
        assert_eq!(atooid("4294967297"), 1);
        // `-1` is `ULONG_MAX` in unsigned arithmetic.
        assert_eq!(atooid("-1"), u32::MAX);
        // Past 64 bits `strtoul` saturates at `ULONG_MAX`.
        assert_eq!(atooid("99999999999999999999999"), u32::MAX);
    }

    #[test]
    fn a_tilde_is_expanded_to_home_but_not_for_another_user() {
        assert_eq!(expand_tilde(b"~", Some(b"/home/u")), b"/home/u");
        assert_eq!(expand_tilde(b"~/f", Some(b"/home/u")), b"/home/u/f");
        assert_eq!(expand_tilde(b"~bob/f", Some(b"/home/u")), b"~bob/f");
        assert_eq!(expand_tilde(b"~/f", None), b"~/f");
        assert_eq!(expand_tilde(b"a~/f", Some(b"/home/u")), b"a~/f");
    }

    #[test]
    fn the_listing_query_is_describe_c_s() {
        assert_eq!(
            list_large_objects_query(false),
            "SELECT oid as \"ID\",\n  pg_catalog.pg_get_userbyid(lomowner) as \"Owner\",\n  \
             pg_catalog.obj_description(oid, 'pg_largeobject') as \"Description\"\n\
             FROM pg_catalog.pg_largeobject_metadata\nORDER BY oid"
        );
        assert!(list_large_objects_query(true).contains(
            ",\n  CASE WHEN pg_catalog.array_length(lomacl, 1) = 0 THEN '(none)' \
             ELSE pg_catalog.array_to_string(lomacl, E'\\n') END AS \"Access privileges\",\n  \
             pg_catalog.obj_description"
        ));
    }

    #[test]
    fn the_result_line_is_quiet_under_quiet_and_a_paragraph_in_html() {
        let mut pset = PsqlSettings::default();
        assert_eq!(
            lo_result_text(&pset, "lo_import 5").as_deref(),
            Some("lo_import 5\n")
        );
        pset.popt.topt.format = PrintFormat::Html;
        assert_eq!(
            lo_result_text(&pset, "lo_export").as_deref(),
            Some("<p>lo_export</p>\n")
        );
        pset.quiet = true;
        assert_eq!(lo_result_text(&pset, "lo_export"), None);
    }

    #[test]
    fn the_comment_is_quoted_around_the_escaped_text() {
        assert_eq!(
            comment_query(16_384, b"it''s"),
            b"COMMENT ON LARGE OBJECT 16384 IS 'it''s'".to_vec()
        );
    }

    /// A server that answers every query with `COMMAND_OK` and records it,
    /// and whose large-object calls succeed or fail as told.
    struct Fake {
        status: TransactionStatus,
        queries: Vec<String>,
        fail: bool,
    }

    impl Executor for Fake {
        fn exec(
            &mut self,
            query: &[u8],
            _mode: &SendMode,
        ) -> Result<Vec<QueryResult>, ErrorMessage> {
            self.queries
                .push(String::from_utf8_lossy(query).into_owned());
            Ok(vec![QueryResult::new(ExecStatus::CommandOk)])
        }
        fn connected(&self) -> bool {
            true
        }
        fn abandon(&mut self) {}
        fn large_objects(&mut self) -> Option<&mut dyn LargeObjects> {
            Some(self)
        }
    }

    impl LargeObjects for Fake {
        fn transaction_status(&self) -> TransactionStatus {
            self.status
        }
        fn lo_import(&mut self, _filename: &Path) -> Result<u32, LoError> {
            if self.fail {
                Err(LoError::NoFunction("lo_create"))
            } else {
                Ok(16_385)
            }
        }
        fn lo_export(&mut self, _loid: u32, _filename: &Path) -> Result<(), LoError> {
            if self.fail {
                Err(LoError::NoFunction("lo_open"))
            } else {
                Ok(())
            }
        }
        fn lo_unlink(&mut self, _loid: u32) -> Result<(), LoError> {
            Ok(())
        }
        fn escape_string(&self, from: &[u8]) -> Vec<u8> {
            rlibpq::escape_string(from, rlibpq::Encoding::default(), true).bytes
        }
    }

    struct Ran {
        result: CommandResult,
        queries: Vec<String>,
        stdout: String,
        stderr: String,
        vars: VariableSpace,
    }

    fn run(cmd: &str, args: &[&str], status: TransactionStatus, fail: bool) -> Ran {
        let mut fake = Fake {
            status,
            queries: Vec::new(),
            fail,
        };
        let mut pset = PsqlSettings {
            log_terse: true,
            ..PsqlSettings::default()
        };
        let mut vars = VariableSpace::new();
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let result = {
            let mut ctx = CommandContext {
                pset: &mut pset,
                vars: &mut vars,
                pipeline: rlibpq::PipelineStatus::Off,
                executor: &mut fake,
            };
            exec_command_lo(
                cmd,
                &opts(args),
                &mut ctx,
                &mut Output::new(&mut stdout),
                &mut stderr,
            )
        };
        Ran {
            result,
            queries: fake.queries,
            stdout: String::from_utf8(stdout).unwrap(),
            stderr: String::from_utf8(stderr).unwrap(),
            vars,
        }
    }

    #[test]
    fn outside_a_transaction_an_import_opens_and_commits_its_own() {
        let ran = run("lo_import", &["f", "it's"], TransactionStatus::Idle, false);
        assert_eq!(ran.result, CommandResult::SkipLine);
        assert_eq!(
            ran.queries,
            [
                "BEGIN",
                "COMMENT ON LARGE OBJECT 16385 IS 'it''s'",
                "COMMIT"
            ]
        );
        assert_eq!(ran.stdout, "lo_import 16385\n");
        assert_eq!(ran.vars.get("LASTOID"), Some("16385"));
    }

    #[test]
    fn inside_a_transaction_the_operation_uses_it() {
        let ran = run(
            "lo_unlink",
            &["42"],
            TransactionStatus::InTransaction,
            false,
        );
        assert_eq!(ran.result, CommandResult::SkipLine);
        assert!(ran.queries.is_empty());
        assert_eq!(ran.stdout, "lo_unlink 42\n");
    }

    #[test]
    fn a_failed_transfer_rolls_back_the_transaction_it_opened() {
        let ran = run("lo_export", &["42", "f"], TransactionStatus::Idle, true);
        assert_eq!(ran.result, CommandResult::Error);
        assert_eq!(ran.queries, ["BEGIN", "ROLLBACK"]);
        assert_eq!(ran.stderr, "cannot determine OID of function lo_open\n");
        assert_eq!(ran.stdout, "");
    }

    #[test]
    fn an_aborted_transaction_is_refused_before_anything_is_sent() {
        let ran = run("lo_import", &["f"], TransactionStatus::InError, false);
        assert_eq!(ran.result, CommandResult::Error);
        assert!(ran.queries.is_empty());
        assert_eq!(ran.stderr, "\\lo_import: current transaction is aborted\n");
        assert_eq!(ran.vars.get("LASTOID"), None);
    }

    #[test]
    fn an_unknown_lo_command_is_left_to_the_dispatcher() {
        let ran = run("lo_frob", &[], TransactionStatus::Idle, false);
        assert_eq!(ran.result, CommandResult::Unknown);
        assert!(ran.queries.is_empty());
    }
}
