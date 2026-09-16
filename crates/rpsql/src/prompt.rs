//! The prompt renderer: `src/bin/psql/prompt.c`.
//!
//! `get_prompt()` (`prompt.c:63`) walks the prompt string expanding `%`
//! escapes. This port keeps it pure by taking a [`PromptFacts`] instead of
//! reaching into `pset.db`: the escapes that need a live connection read it,
//! and the ones that run a shell (`` %` ``) are refused rather than executed,
//! because that is an action and NAT-405 owns interactive mode.

use crate::scan::PromptStatus;
use crate::settings::PsqlSettings;

/// Everything `get_prompt` reads that this port does not keep in
/// [`PsqlSettings`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptFacts {
    /// `PQdb(pset.db)`; `None` when there is no connection.
    pub dbname: Option<String>,
    /// `session_username()`
    pub username: Option<String>,
    /// `PQhost(pset.db)`
    pub host: Option<String>,
    /// `PQport(pset.db)`
    pub port: Option<String>,
    /// `PQbackendPID(pset.db)`
    pub backend_pid: Option<i32>,
    /// `is_superuser()`
    pub superuser: bool,
    /// `PQtransactionStatus(pset.db)`, as the `%x` escape renders it.
    pub transaction: TransactionMark,
}

/// What `%x` prints for each transaction status (`prompt.c:250`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransactionMark {
    /// `PQTRANS_IDLE`: nothing.
    #[default]
    Idle,
    /// `PQTRANS_ACTIVE` / `PQTRANS_INTRANS`: `*`.
    InTransaction,
    /// `PQTRANS_INERROR`: `!`.
    Failed,
    /// Anything else: `?`.
    Unknown,
}

/// `get_prompt()` (`prompt.c:63`).
///
/// `conditional_active` is `\if` state, which NAT-402 owns; `true` here is the
/// "not inside an inactive branch" case.
#[must_use]
pub fn get_prompt(
    status: PromptStatus,
    pset: &PsqlSettings,
    facts: &PromptFacts,
    conditional_active: bool,
) -> String {
    let template = match status {
        PromptStatus::Ready => &pset.prompt1,
        PromptStatus::Copy => &pset.prompt3,
        _ => &pset.prompt2,
    };

    let mut out = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(escape) = chars.next() else { break };
        match escape {
            '/' => out.push_str(facts.dbname.as_deref().unwrap_or("")),
            '~' => {
                // `%~` is `~` when the database has the user's name.
                match (&facts.dbname, &facts.username) {
                    (Some(db), Some(user)) if db == user => out.push('~'),
                    (Some(db), _) => out.push_str(db),
                    (None, _) => {}
                }
            }
            'n' => out.push_str(facts.username.as_deref().unwrap_or("")),
            'M' | 'm' => {
                if facts.dbname.is_some() {
                    out.push_str(&host_mark(facts.host.as_deref(), escape == 'm'));
                }
            }
            '>' => out.push_str(facts.port.as_deref().unwrap_or("")),
            'p' => {
                if let Some(pid) = facts.backend_pid {
                    out.push_str(&pid.to_string());
                }
            }
            'l' => out.push_str(&pset.stmt_lineno.to_string()),
            'R' => out.push_str(&render_r(status, facts, pset, conditional_active)),
            'x' => out.push_str(match facts.transaction {
                TransactionMark::Idle => {
                    if facts.dbname.is_none() {
                        "?"
                    } else {
                        ""
                    }
                }
                TransactionMark::InTransaction => "*",
                TransactionMark::Failed => "!",
                TransactionMark::Unknown => "?",
            }),
            '#' => out.push(if facts.superuser { '#' } else { '>' }),
            // `%%` is a literal percent; `%?` is "not here yet" upstream too.
            '%' => out.push('%'),
            '?' => {}
            // `` %` `` runs a shell command (`prompt.c:277`), which is an
            // action; it renders as nothing here and NAT-405 owns it.
            '`' => {
                for c in chars.by_ref() {
                    if c == '`' {
                        break;
                    }
                }
            }
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

/// The `%M`/`%m` host rendering (`prompt.c:130`).
fn host_mark(host: Option<&str>, short: bool) -> String {
    match host {
        Some(host) if !host.is_empty() && !host.starts_with('/') => {
            if short {
                host.split('.').next().unwrap_or(host).to_string()
            } else {
                host.to_string()
            }
        }
        Some(host) if short || host.is_empty() => "[local]".to_string(),
        Some(host) => format!("[local:{host}]"),
        None => "[local]".to_string(),
    }
}

/// The `%R` escape (`prompt.c:216`).
fn render_r(
    status: PromptStatus,
    facts: &PromptFacts,
    pset: &PsqlSettings,
    conditional_active: bool,
) -> String {
    match status {
        PromptStatus::Ready => {
            if !conditional_active {
                "@".to_string()
            } else if facts.dbname.is_none() {
                "!".to_string()
            } else if pset.singleline {
                "^".to_string()
            } else {
                "=".to_string()
            }
        }
        PromptStatus::Continue => "-".to_string(),
        PromptStatus::SingleQuote => "'".to_string(),
        PromptStatus::DoubleQuote => "\"".to_string(),
        PromptStatus::DollarQuote => "$".to_string(),
        PromptStatus::Comment => "*".to_string(),
        PromptStatus::Paren => "(".to_string(),
        PromptStatus::Copy => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> PromptFacts {
        PromptFacts {
            dbname: Some("postgres".to_string()),
            username: Some("alice".to_string()),
            ..PromptFacts::default()
        }
    }

    fn prompt(status: PromptStatus, facts: &PromptFacts) -> String {
        get_prompt(status, &PsqlSettings::default(), facts, true)
    }

    #[test]
    fn the_default_prompt1_is_database_equals_hash() {
        // DEFAULT_PROMPT1 is "%/%R%x%# " (`settings.h:26`).
        assert_eq!(prompt(PromptStatus::Ready, &connected()), "postgres=> ");
    }

    #[test]
    fn a_superuser_gets_a_hash() {
        let facts = PromptFacts {
            superuser: true,
            ..connected()
        };
        assert_eq!(prompt(PromptStatus::Ready, &facts), "postgres=# ");
    }

    #[test]
    fn with_no_connection_r_is_a_bang_and_x_is_a_question_mark() {
        let facts = PromptFacts::default();
        // `%/` is empty, `%R` is `!`, `%x` is `?`, `%#` is `>`.
        assert_eq!(prompt(PromptStatus::Ready, &facts), "!?> ");
    }

    #[test]
    fn continuation_prompts_carry_the_state_mark() {
        let facts = connected();
        for (status, mark) in [
            (PromptStatus::Continue, "-"),
            (PromptStatus::SingleQuote, "'"),
            (PromptStatus::DoubleQuote, "\""),
            (PromptStatus::DollarQuote, "$"),
            (PromptStatus::Comment, "*"),
            (PromptStatus::Paren, "("),
        ] {
            assert_eq!(prompt(status, &facts), format!("postgres{mark}> "));
        }
    }

    #[test]
    fn an_open_transaction_shows_a_star_and_a_failed_one_a_bang() {
        let facts = PromptFacts {
            transaction: TransactionMark::InTransaction,
            ..connected()
        };
        assert_eq!(prompt(PromptStatus::Ready, &facts), "postgres=*> ");
        let facts = PromptFacts {
            transaction: TransactionMark::Failed,
            ..connected()
        };
        assert_eq!(prompt(PromptStatus::Ready, &facts), "postgres=!> ");
    }

    #[test]
    fn an_inactive_if_branch_shows_an_at_sign() {
        let pset = PsqlSettings::default();
        assert_eq!(
            get_prompt(PromptStatus::Ready, &pset, &connected(), false),
            "postgres@> "
        );
    }

    #[test]
    fn tilde_abbreviates_a_database_named_after_the_user() {
        let pset = PsqlSettings {
            prompt1: "%~ ".to_string(),
            ..PsqlSettings::default()
        };
        let facts = PromptFacts {
            dbname: Some("alice".to_string()),
            username: Some("alice".to_string()),
            ..PromptFacts::default()
        };
        assert_eq!(get_prompt(PromptStatus::Ready, &pset, &facts, true), "~ ");
    }

    #[test]
    fn host_escapes_distinguish_a_socket_from_a_hostname() {
        assert_eq!(host_mark(Some("db.example.com"), false), "db.example.com");
        assert_eq!(host_mark(Some("db.example.com"), true), "db");
        assert_eq!(host_mark(Some("/tmp"), false), "[local:/tmp]");
        assert_eq!(host_mark(None, false), "[local]");
    }

    #[test]
    fn a_backquoted_shell_command_is_not_run() {
        let pset = PsqlSettings {
            prompt1: "a%`rm -rf /`b".to_string(),
            ..PsqlSettings::default()
        };
        assert_eq!(
            get_prompt(PromptStatus::Ready, &pset, &connected(), true),
            "ab"
        );
    }

    #[test]
    fn the_line_number_escape_reads_stmt_lineno() {
        let pset = PsqlSettings {
            prompt1: "%l ".to_string(),
            stmt_lineno: 7,
            ..PsqlSettings::default()
        };
        assert_eq!(
            get_prompt(PromptStatus::Ready, &pset, &connected(), true),
            "7 "
        );
    }
}
