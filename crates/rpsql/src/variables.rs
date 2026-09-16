//! psql's "shell variable" repository: `src/bin/psql/variables.c`.
//!
//! Upstream stores a name-ordered linked list of `struct _variable`, each with
//! an optional substitute hook and an optional assign hook
//! (`variables.h:57`-`:69`). The assign hooks there are C function pointers
//! that write into the `pset` global; here they are *data* — [`Assign`] says
//! which setting a variable controls — and [`VariableSpace::settings`] is the
//! pure calculation that derives those settings from the current values. Same
//! table, same order of operations, no globals.

use std::fmt::Write as _;

use crate::scan::{QuoteType, VariableSource, is_variable_char};
use crate::settings::{
    CompCase, DEFAULT_PROMPT1, DEFAULT_PROMPT2, DEFAULT_PROMPT3, DEFAULT_WATCH_INTERVAL,
    DEFAULT_WATCH_INTERVAL_MAX, Echo, EchoHidden, ErrorRollback, HistControl, PsqlSettings,
};

/// What a variable's substitute hook does to a proposed value
/// (`variables.h:41`-`:55`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Substitute {
    /// `bool_substitute_hook` (`startup.c:718`): `\unset FOO` becomes
    /// `\set FOO off`, and `\set FOO` becomes `\set FOO on`.
    Bool,
    /// The several `*_substitute_hook`s that only supply a default for an
    /// unset variable: `fetch_count`, `histsize`, `echo`, `verbosity`, ….
    Default(&'static str),
    /// `ignoreeof_substitute_hook` (`startup.c:806`): unset is `0`, and a
    /// non-integer value becomes `10`, as bash does.
    IgnoreEof,
}

/// Which `pset` field an assign hook keeps in sync, and how the value is
/// parsed. One variant per `*_hook` in `startup.c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assign {
    /// `ParseVariableBool` into the named field.
    Bool(BoolField),
    /// `ParseVariableNum` into the named field.
    Num(NumField),
    /// `ParseVariableDouble` into `watch_interval`, range-checked.
    WatchInterval,
    /// One of the enum-valued settings.
    Enum(EnumField),
    /// A prompt string; any value is accepted.
    Prompt(PromptField),
    /// `histfile_hook` (`startup.c:781`): a placeholder that accepts anything,
    /// so HISTFILE stays known to tab completion.
    Accept,
}

/// The boolean `pset` fields (`settings.h:163`-`:169`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolField {
    /// `pset.autocommit`
    Autocommit,
    /// `pset.on_error_stop`
    OnErrorStop,
    /// `pset.quiet`
    Quiet,
    /// `pset.singleline`
    Singleline,
    /// `pset.singlestep`
    Singlestep,
    /// `pset.show_all_results`
    ShowAllResults,
    /// `pset.hide_compression`
    HideCompression,
    /// `pset.hide_tableam`
    HideTableam,
}

/// The integer `pset` fields (`settings.h:170`-`:173`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumField {
    /// `pset.fetch_count`
    FetchCount,
    /// `pset.histsize`
    Histsize,
    /// `pset.ignoreeof`
    Ignoreeof,
}

/// The enum-valued `pset` fields (`settings.h:175`-`:186`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnumField {
    /// `pset.echo`
    Echo,
    /// `pset.echo_hidden`
    EchoHidden,
    /// `pset.on_error_rollback`
    OnErrorRollback,
    /// `pset.comp_case`
    CompCase,
    /// `pset.histcontrol`
    Histcontrol,
    /// `pset.verbosity`
    Verbosity,
    /// `pset.show_context`
    ShowContext,
}

/// The three prompt strings (`settings.h:181`-`:183`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptField {
    /// `pset.prompt1`
    Prompt1,
    /// `pset.prompt2`
    Prompt2,
    /// `pset.prompt3`
    Prompt3,
}

/// One entry of the repository (`variables.h:63`-`:69`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Variable {
    name: String,
    /// `None` means the variable is logically unset, but the entry stays so
    /// its hooks are not forgotten (`variables.h:60`).
    value: Option<String>,
    substitute: Option<Substitute>,
    assign: Option<Assign>,
}

/// A failed assignment, carrying the message upstream's hook would log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignError {
    /// The `pg_log_error` text, without the `psql: error: ` prefix.
    pub message: String,
}

/// The variable space (`variables.h:72`).
///
/// Entries are kept in name order (`strcmp`), which is what makes
/// [`VariableSpace::print`] read nicely (`variables.c:48`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VariableSpace {
    entries: Vec<Variable>,
}

impl VariableSpace {
    /// `CreateVariableSpace()` (`variables.c:52`) followed by
    /// `EstablishVariableSpace()` (`startup.c:1101`): the hook table, in
    /// upstream's order.
    #[must_use]
    // The body is one table, one row per `SetVariableHooks` call upstream makes;
    // splitting it would only move the rows somewhere else.
    #[allow(clippy::too_many_lines)]
    pub fn new() -> Self {
        let mut space = Self::default();
        let hooks: &[(&str, Option<Substitute>, Option<Assign>)] = &[
            (
                "AUTOCOMMIT",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::Autocommit)),
            ),
            (
                "ON_ERROR_STOP",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::OnErrorStop)),
            ),
            (
                "QUIET",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::Quiet)),
            ),
            (
                "SINGLELINE",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::Singleline)),
            ),
            (
                "SINGLESTEP",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::Singlestep)),
            ),
            (
                "FETCH_COUNT",
                Some(Substitute::Default("0")),
                Some(Assign::Num(NumField::FetchCount)),
            ),
            ("HISTFILE", None, Some(Assign::Accept)),
            (
                "HISTSIZE",
                Some(Substitute::Default("500")),
                Some(Assign::Num(NumField::Histsize)),
            ),
            (
                "IGNOREEOF",
                Some(Substitute::IgnoreEof),
                Some(Assign::Num(NumField::Ignoreeof)),
            ),
            (
                "ECHO",
                Some(Substitute::Default("none")),
                Some(Assign::Enum(EnumField::Echo)),
            ),
            (
                "ECHO_HIDDEN",
                Some(Substitute::Bool),
                Some(Assign::Enum(EnumField::EchoHidden)),
            ),
            (
                "ON_ERROR_ROLLBACK",
                Some(Substitute::Bool),
                Some(Assign::Enum(EnumField::OnErrorRollback)),
            ),
            (
                "COMP_KEYWORD_CASE",
                Some(Substitute::Default("preserve-upper")),
                Some(Assign::Enum(EnumField::CompCase)),
            ),
            (
                "HISTCONTROL",
                Some(Substitute::Default("none")),
                Some(Assign::Enum(EnumField::Histcontrol)),
            ),
            ("PROMPT1", None, Some(Assign::Prompt(PromptField::Prompt1))),
            ("PROMPT2", None, Some(Assign::Prompt(PromptField::Prompt2))),
            ("PROMPT3", None, Some(Assign::Prompt(PromptField::Prompt3))),
            (
                "VERBOSITY",
                Some(Substitute::Default("default")),
                Some(Assign::Enum(EnumField::Verbosity)),
            ),
            (
                "SHOW_ALL_RESULTS",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::ShowAllResults)),
            ),
            (
                "SHOW_CONTEXT",
                Some(Substitute::Default("errors")),
                Some(Assign::Enum(EnumField::ShowContext)),
            ),
            (
                "HIDE_TOAST_COMPRESSION",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::HideCompression)),
            ),
            (
                "HIDE_TABLEAM",
                Some(Substitute::Bool),
                Some(Assign::Bool(BoolField::HideTableam)),
            ),
            (
                "WATCH_INTERVAL",
                Some(Substitute::Default(DEFAULT_WATCH_INTERVAL)),
                Some(Assign::WatchInterval),
            ),
        ];
        for &(name, substitute, assign) in hooks {
            space.set_hooks(name, substitute, assign);
        }
        space
    }

    /// `SetVariableHooks()` (`variables.c:341`): install the hooks and apply
    /// them to the variable's current value.
    fn set_hooks(&mut self, name: &str, substitute: Option<Substitute>, assign: Option<Assign>) {
        let value = self.get(name).map(str::to_string);
        let value = match substitute {
            Some(hook) => apply_substitute(hook, value),
            None => value,
        };
        let entry = Variable {
            name: name.to_string(),
            value,
            substitute,
            assign,
        };
        match self.position(name) {
            Ok(i) => self.entries[i] = entry,
            Err(i) => self.entries.insert(i, entry),
        }
    }

    fn position(&self, name: &str) -> Result<usize, usize> {
        self.entries.binary_search_by(|v| v.name.as_str().cmp(name))
    }

    /// `GetVariable()` (`variables.c:70`): the value, or `None` when unset.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.position(name)
            .ok()
            .and_then(|i| self.entries[i].value.as_deref())
    }

    /// `GetVariableBool()`'s reading of a value (`variables.c` via
    /// `ParseVariableBool`), defaulting to false when unset.
    #[must_use]
    pub fn get_bool(&self, name: &str) -> bool {
        let mut result = false;
        parse_variable_bool(self.get(name), None, &mut result);
        result
    }

    /// `SetVariable()` (`variables.c:252`). `value == None` is `\unset`.
    ///
    /// # Errors
    /// Returns the message upstream's `pg_log_error` would print when the name
    /// is invalid or an assign hook refuses the value.
    pub fn set(&mut self, name: &str, value: Option<&str>) -> Result<(), AssignError> {
        if !valid_variable_name(name) {
            // Deletion of a non-existent variable is not an error.
            if value.is_none() {
                return Ok(());
            }
            return Err(AssignError {
                message: format!("invalid variable name: \"{name}\""),
            });
        }

        match self.position(name) {
            Ok(i) => {
                let mut new_value = value.map(str::to_string);
                if let Some(hook) = self.entries[i].substitute {
                    new_value = apply_substitute(hook, new_value);
                }
                if let Some(hook) = self.entries[i].assign {
                    check_assign(hook, name, new_value.as_deref())?;
                }
                let entry = &mut self.entries[i];
                entry.value = new_value;
                // If the value is gone and there are no hooks to remember, the
                // entry can go too (`variables.c:299`).
                if entry.value.is_none() && entry.substitute.is_none() && entry.assign.is_none() {
                    self.entries.remove(i);
                }
                Ok(())
            }
            Err(i) => {
                // Not present: make a new entry unless we were asked to delete.
                if let Some(value) = value {
                    self.entries.insert(
                        i,
                        Variable {
                            name: name.to_string(),
                            value: Some(value.to_string()),
                            substitute: None,
                            assign: None,
                        },
                    );
                }
                Ok(())
            }
        }
    }

    /// `SetVariableBool()` (`variables.c:335`): `\set NAME on`.
    ///
    /// # Errors
    /// As [`VariableSpace::set`].
    pub fn set_bool(&mut self, name: &str) -> Result<(), AssignError> {
        self.set(name, Some("on"))
    }

    /// `DeleteVariable()` (`variables.c:330`).
    ///
    /// # Errors
    /// As [`VariableSpace::set`].
    pub fn delete(&mut self, name: &str) -> Result<(), AssignError> {
        self.set(name, None)
    }

    /// `PrintVariables()` (`variables.c:229`): `name = value`, one per line,
    /// in name order, skipping the unset ones.
    #[must_use]
    pub fn print(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            if let Some(value) = &entry.value {
                let _ = writeln!(out, "{} = '{}'", entry.name, value);
            }
        }
        out
    }

    /// The pure calculation that upstream spreads across twenty assign hooks:
    /// derive every hook-owned field of [`PsqlSettings`] from the current
    /// values.
    ///
    /// Only the fields listed under "set by assign hooks" in `settings.h:159`
    /// are touched; everything else on `base` is left alone.
    #[must_use]
    // One arm per assign hook `startup.c` installs, which is the same table
    // `VariableSpace::new` lists; splitting it would only move the rows.
    #[allow(clippy::too_many_lines)]
    pub fn settings(&self, base: &PsqlSettings) -> PsqlSettings {
        let mut pset = base.clone();
        for entry in &self.entries {
            let Some(assign) = entry.assign else { continue };
            let value = entry.value.as_deref();
            match assign {
                Assign::Bool(field) => {
                    let slot = match field {
                        BoolField::Autocommit => &mut pset.autocommit,
                        BoolField::OnErrorStop => &mut pset.on_error_stop,
                        BoolField::Quiet => &mut pset.quiet,
                        BoolField::Singleline => &mut pset.singleline,
                        BoolField::Singlestep => &mut pset.singlestep,
                        BoolField::ShowAllResults => &mut pset.show_all_results,
                        BoolField::HideCompression => &mut pset.hide_compression,
                        BoolField::HideTableam => &mut pset.hide_tableam,
                    };
                    parse_variable_bool(value, None, slot);
                }
                Assign::Num(field) => {
                    let slot = match field {
                        NumField::FetchCount => &mut pset.fetch_count,
                        NumField::Histsize => &mut pset.histsize,
                        NumField::Ignoreeof => &mut pset.ignoreeof,
                    };
                    parse_variable_num(value, None, slot);
                }
                Assign::WatchInterval => {
                    parse_variable_double(
                        value,
                        None,
                        &mut pset.watch_interval,
                        0.0,
                        DEFAULT_WATCH_INTERVAL_MAX,
                    );
                }
                Assign::Enum(field) => match field {
                    EnumField::Echo => {
                        if let Some(v) = value.and_then(parse_echo) {
                            pset.echo = v;
                        }
                    }
                    EnumField::EchoHidden => {
                        if let Some(v) = value.and_then(parse_echo_hidden) {
                            pset.echo_hidden = v;
                        }
                    }
                    EnumField::OnErrorRollback => {
                        if let Some(v) = value.and_then(parse_error_rollback) {
                            pset.on_error_rollback = v;
                        }
                    }
                    EnumField::CompCase => {
                        if let Some(v) = value.and_then(parse_comp_case) {
                            pset.comp_case = v;
                        }
                    }
                    EnumField::Histcontrol => {
                        if let Some(v) = value.and_then(parse_histcontrol) {
                            pset.histcontrol = v;
                        }
                    }
                    EnumField::Verbosity => {
                        if let Some(v) = value.and_then(parse_verbosity) {
                            pset.verbosity = v;
                        }
                    }
                    EnumField::ShowContext => {
                        if let Some(v) = value.and_then(parse_show_context) {
                            pset.show_context = v;
                        }
                    }
                },
                Assign::Prompt(field) => {
                    let text = value.unwrap_or("").to_string();
                    match field {
                        PromptField::Prompt1 => pset.prompt1 = text,
                        PromptField::Prompt2 => pset.prompt2 = text,
                        PromptField::Prompt3 => pset.prompt3 = text,
                    }
                }
                Assign::Accept => {}
            }
        }
        pset
    }
}

/// A read-only view of the variable space for the lexer's `:name` callback.
///
/// This is upstream's `psql_get_variable()` (`startup.c:1127`), the one
/// `PsqlScanCallbacks.get_variable` psql installs: it reads `pset.vars` and
/// quotes the value the way the caller asked. It borrows because the lexer
/// only ever reads; a caller that needs the snapshot to outlive a concurrent
/// write clones the space and lends *that*.
pub(crate) struct VarView<'a>(pub &'a VariableSpace);

impl VariableSource for VarView<'_> {
    fn get_variable(&self, name: &str, quote: QuoteType) -> Option<String> {
        let value = self.0.get(name)?.to_string();
        Some(match quote {
            // `PQUOTE_SHELL_ARG` is upstream's fourth case. Nothing asks for
            // it here: the only lexer state that requests it is
            // `<xslashbackquote>`, and this port refuses a backquote rather
            // than running a shell (`slash.rs:8`).
            QuoteType::Plain | QuoteType::ShellArg => value,
            QuoteType::SqlLiteral => escape_literal(&value),
            QuoteType::SqlIdent => escape_identifier(&value),
        })
    }
}

/// `valid_variable_name()` (`variables.c:24`).
#[must_use]
pub fn valid_variable_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(is_variable_char)
}

// The return is `Option` because a substitute hook may legitimately produce
// NULL — `histfile_hook` has none at all — and the caller stores it as the
// variable's value, unset included.
#[allow(clippy::unnecessary_wraps)]
fn apply_substitute(hook: Substitute, newval: Option<String>) -> Option<String> {
    match hook {
        Substitute::Bool => match newval {
            // "\unset FOO" becomes "\set FOO off"
            None => Some("off".to_string()),
            // "\set FOO" becomes "\set FOO on"
            Some(v) if v.is_empty() => Some("on".to_string()),
            Some(v) => Some(v),
        },
        Substitute::Default(default) => Some(newval.unwrap_or_else(|| default.to_string())),
        Substitute::IgnoreEof => match newval {
            None => Some("0".to_string()),
            Some(v) => {
                let mut dummy = 0;
                if parse_variable_num(Some(&v), None, &mut dummy) {
                    Some(v)
                } else {
                    Some("10".to_string())
                }
            }
        },
    }
}

/// The validation half of an assign hook: does it accept this value?
fn check_assign(hook: Assign, name: &str, value: Option<&str>) -> Result<(), AssignError> {
    let enum_error = |value: &str, suggestions: &str| AssignError {
        // `PsqlVarEnumError` (`variables.c:474`).
        message: format!(
            "unrecognized value \"{value}\" for \"{name}\"\nAvailable values are: {suggestions}."
        ),
    };
    match hook {
        Assign::Bool(_) => {
            let mut slot = false;
            if parse_variable_bool(value, Some(name), &mut slot) {
                Ok(())
            } else {
                Err(AssignError {
                    message: format!(
                        "unrecognized value \"{}\" for \"{name}\": Boolean expected",
                        value.unwrap_or("")
                    ),
                })
            }
        }
        Assign::Num(_) => {
            let mut slot = 0;
            if parse_variable_num(value, Some(name), &mut slot) {
                Ok(())
            } else {
                Err(AssignError {
                    message: format!(
                        "invalid value \"{}\" for \"{name}\": integer expected",
                        value.unwrap_or("")
                    ),
                })
            }
        }
        Assign::WatchInterval => {
            let mut slot = 0.0;
            if parse_variable_double(
                value,
                Some(name),
                &mut slot,
                0.0,
                DEFAULT_WATCH_INTERVAL_MAX,
            ) {
                Ok(())
            } else {
                Err(AssignError {
                    message: format!(
                        "invalid value \"{}\" for \"{name}\": number expected",
                        value.unwrap_or("")
                    ),
                })
            }
        }
        Assign::Enum(field) => {
            let value = value.unwrap_or("");
            let ok = match field {
                EnumField::Echo => parse_echo(value).is_some(),
                EnumField::EchoHidden => parse_echo_hidden(value).is_some(),
                EnumField::OnErrorRollback => parse_error_rollback(value).is_some(),
                EnumField::CompCase => parse_comp_case(value).is_some(),
                EnumField::Histcontrol => parse_histcontrol(value).is_some(),
                EnumField::Verbosity => parse_verbosity(value).is_some(),
                EnumField::ShowContext => parse_show_context(value).is_some(),
            };
            if ok {
                Ok(())
            } else {
                Err(enum_error(value, enum_suggestions(field)))
            }
        }
        Assign::Prompt(_) | Assign::Accept => Ok(()),
    }
}

/// The `Available values are:` list each enum hook prints (`startup.c:866`…).
const fn enum_suggestions(field: EnumField) -> &'static str {
    match field {
        EnumField::Echo => "none, errors, queries, all",
        EnumField::EchoHidden | EnumField::OnErrorRollback => "on, off, noexec",
        EnumField::CompCase => "lower, upper, preserve-lower, preserve-upper",
        EnumField::Histcontrol => "none, ignorespace, ignoredups, ignoreboth",
        EnumField::Verbosity => "default, verbose, terse, sqlstate",
        EnumField::ShowContext => "never, errors, always",
    }
}

/// `ParseVariableBool()` (`variables.c:104`).
///
/// Valid values are true, false, yes, no, on, off, 1, 0 and unique prefixes
/// thereof; `on`/`off` need two characters because `o` is not unique.
/// `*result` is left alone when the value is not recognized.
pub fn parse_variable_bool(value: Option<&str>, _name: Option<&str>, result: &mut bool) -> bool {
    let value = value.unwrap_or("").to_ascii_lowercase();
    let value = value.as_bytes();
    let len = value.len();
    // `pg_strncasecmp(value, whole, len)` with len > 0: a non-empty prefix.
    let prefix_of = |whole: &str| len > 0 && whole.as_bytes().get(..len) == Some(value);
    // `pg_strncasecmp(value, whole, max(len, 2))`: comparing at least two
    // characters means a one-character value runs into `whole`'s bytes past
    // its own NUL and fails, so "o" is not unique enough while "of" still
    // means "off".
    let prefix_of_at_least_two = |whole: &str| len >= 2 && prefix_of(whole);

    if prefix_of("true") || prefix_of("yes") {
        *result = true;
    } else if prefix_of("false") || prefix_of("no") {
        *result = false;
    } else if prefix_of_at_least_two("on") {
        *result = true;
    } else if prefix_of_at_least_two("off") {
        *result = false;
    } else if value == b"1" {
        *result = true;
    } else if value == b"0" {
        *result = false;
    } else {
        return false;
    }
    true
}

/// `ParseVariableNum()` (`variables.c:155`). `strtol` with base 0, so `0x10`
/// and `010` are read the way C reads them.
pub fn parse_variable_num(value: Option<&str>, _name: Option<&str>, result: &mut i32) -> bool {
    let value = value.unwrap_or("").trim_start();
    let (negative, digits) = match value.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let (radix, digits) = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (16, hex)
    } else if digits.len() > 1 && digits.starts_with('0') {
        (8, &digits[1..])
    } else {
        (10, digits)
    };
    let Ok(magnitude) = i64::from_str_radix(digits, radix) else {
        return false;
    };
    let value = if negative { -magnitude } else { magnitude };
    if let Ok(value) = i32::try_from(value) {
        *result = value;
        true
    } else {
        false
    }
}

/// `ParseVariableDouble()` (`variables.c:186`), range-checked into
/// `[min, max]`.
pub fn parse_variable_double(
    value: Option<&str>,
    _name: Option<&str>,
    result: &mut f64,
    min: f64,
    max: f64,
) -> bool {
    let Ok(parsed) = value.unwrap_or("").trim().parse::<f64>() else {
        return false;
    };
    if !parsed.is_finite() || parsed < min || parsed > max {
        return false;
    }
    *result = parsed;
    true
}

fn parse_echo(value: &str) -> Option<Echo> {
    match value {
        "none" => Some(Echo::None),
        "errors" => Some(Echo::Errors),
        "queries" => Some(Echo::Queries),
        "all" => Some(Echo::All),
        _ => None,
    }
}

fn parse_echo_hidden(value: &str) -> Option<EchoHidden> {
    if value == "noexec" {
        return Some(EchoHidden::NoExec);
    }
    let mut on = false;
    parse_variable_bool(Some(value), None, &mut on).then_some(if on {
        EchoHidden::On
    } else {
        EchoHidden::Off
    })
}

fn parse_error_rollback(value: &str) -> Option<ErrorRollback> {
    if value == "interactive" {
        return Some(ErrorRollback::Interactive);
    }
    let mut on = false;
    parse_variable_bool(Some(value), None, &mut on).then_some(if on {
        ErrorRollback::On
    } else {
        ErrorRollback::Off
    })
}

fn parse_comp_case(value: &str) -> Option<CompCase> {
    match value {
        "lower" => Some(CompCase::Lower),
        "upper" => Some(CompCase::Upper),
        "preserve-lower" => Some(CompCase::PreserveLower),
        "preserve-upper" => Some(CompCase::PreserveUpper),
        _ => None,
    }
}

fn parse_histcontrol(value: &str) -> Option<HistControl> {
    match value {
        "none" => Some(HistControl::None),
        "ignorespace" => Some(HistControl::IgnoreSpace),
        "ignoredups" => Some(HistControl::IgnoreDups),
        "ignoreboth" => Some(HistControl::IgnoreBoth),
        _ => None,
    }
}

fn parse_verbosity(value: &str) -> Option<rlibpq::Verbosity> {
    match value {
        "default" => Some(rlibpq::Verbosity::Default),
        "verbose" => Some(rlibpq::Verbosity::Verbose),
        "terse" => Some(rlibpq::Verbosity::Terse),
        "sqlstate" => Some(rlibpq::Verbosity::Sqlstate),
        _ => None,
    }
}

fn parse_show_context(value: &str) -> Option<rlibpq::ContextVisibility> {
    match value {
        "never" => Some(rlibpq::ContextVisibility::Never),
        "errors" => Some(rlibpq::ContextVisibility::Errors),
        "always" => Some(rlibpq::ContextVisibility::Always),
        _ => None,
    }
}

/// `PQescapeLiteral`'s output shape (`fe-exec.c:3960`) for a value that needs
/// no `E''` prefix: single quotes doubled.
#[must_use]
pub fn escape_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// `PQescapeIdentifier` (`fe-exec.c:4046`): always double-quoted, embedded
/// double quotes doubled.
#[must_use]
pub fn escape_identifier(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The three prompt defaults, so `startup.c:main`'s `SetVariable` calls have
/// one home (`settings.h:26`-`:28`).
#[must_use]
pub fn default_prompts() -> [(&'static str, &'static str); 3] {
    [
        ("PROMPT1", DEFAULT_PROMPT1),
        ("PROMPT2", DEFAULT_PROMPT2),
        ("PROMPT3", DEFAULT_PROMPT3),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_names_follow_variable_char() {
        assert!(valid_variable_name("FOO_1"));
        assert!(!valid_variable_name(""));
        assert!(!valid_variable_name("a-b"));
        assert!(!valid_variable_name("a b"));
    }

    #[test]
    fn unique_prefixes_of_booleans_are_accepted() {
        // `variables.c:96`: true, false, yes, no, on, off, 1, 0 and unique
        // prefixes; 'o' is not unique enough.
        let mut result = false;
        for (text, want) in [
            ("t", true),
            ("tr", true),
            ("true", true),
            ("f", false),
            ("yes", true),
            ("n", false),
            ("on", true),
            ("off", false),
            ("1", true),
            ("0", false),
        ] {
            assert!(parse_variable_bool(Some(text), None, &mut result), "{text}");
            assert_eq!(result, want, "{text}");
        }
        assert!(!parse_variable_bool(Some("o"), None, &mut result));
        assert!(!parse_variable_bool(Some("maybe"), None, &mut result));
        assert!(!parse_variable_bool(None, None, &mut result));
    }

    #[test]
    fn integers_are_read_the_way_strtol_base_zero_does() {
        let mut result = 0;
        assert!(parse_variable_num(Some("42"), None, &mut result));
        assert_eq!(result, 42);
        assert!(parse_variable_num(Some("0x10"), None, &mut result));
        assert_eq!(result, 16);
        assert!(parse_variable_num(Some("-7"), None, &mut result));
        assert_eq!(result, -7);
        assert!(!parse_variable_num(Some("12x"), None, &mut result));
        assert!(!parse_variable_num(Some(""), None, &mut result));
        assert_eq!(result, -7, "a rejected value must not clobber the result");
    }

    #[test]
    fn unset_of_a_bool_variable_becomes_off() {
        // `bool_substitute_hook` (`startup.c:718`).
        let mut vars = VariableSpace::new();
        vars.set("AUTOCOMMIT", None).unwrap();
        assert_eq!(vars.get("AUTOCOMMIT"), Some("off"));
        vars.set("AUTOCOMMIT", Some("")).unwrap();
        assert_eq!(vars.get("AUTOCOMMIT"), Some("on"));
    }

    #[test]
    fn an_assign_hook_refusing_a_value_leaves_the_old_one() {
        let mut vars = VariableSpace::new();
        vars.set("ECHO", Some("all")).unwrap();
        let err = vars.set("ECHO", Some("sideways")).unwrap_err();
        assert_eq!(
            err.message,
            "unrecognized value \"sideways\" for \"ECHO\"\n\
             Available values are: none, errors, queries, all."
        );
        assert_eq!(vars.get("ECHO"), Some("all"));
    }

    #[test]
    fn an_invalid_name_is_an_error_but_unsetting_one_is_not() {
        let mut vars = VariableSpace::new();
        assert_eq!(
            vars.set("a-b", Some("1")).unwrap_err().message,
            "invalid variable name: \"a-b\""
        );
        vars.set("a-b", None).unwrap();
    }

    #[test]
    fn plain_variables_are_kept_in_name_order() {
        let mut vars = VariableSpace::default();
        for name in ["zeta", "alpha", "mu"] {
            vars.set(name, Some("x")).unwrap();
        }
        assert_eq!(vars.print(), "alpha = 'x'\nmu = 'x'\nzeta = 'x'\n");
    }

    #[test]
    fn a_hookless_variable_disappears_when_unset() {
        let mut vars = VariableSpace::default();
        vars.set("FOO", Some("1")).unwrap();
        vars.set("FOO", None).unwrap();
        assert_eq!(vars.get("FOO"), None);
        assert_eq!(vars.print(), "");
    }

    #[test]
    fn a_hooked_variable_survives_unset_so_its_hooks_are_remembered() {
        // `variables.c:299`: the struct stays when there are hooks.
        let mut vars = VariableSpace::new();
        vars.set("QUIET", None).unwrap();
        assert_eq!(vars.get("QUIET"), Some("off"));
    }

    #[test]
    fn ignoreeof_mimics_bash() {
        // `ignoreeof_substitute_hook` (`startup.c:806`).
        let mut vars = VariableSpace::new();
        assert_eq!(vars.get("IGNOREEOF"), Some("0"));
        vars.set("IGNOREEOF", Some("banana")).unwrap();
        assert_eq!(vars.get("IGNOREEOF"), Some("10"));
        vars.set("IGNOREEOF", Some("3")).unwrap();
        assert_eq!(vars.get("IGNOREEOF"), Some("3"));
    }

    #[test]
    fn settings_are_derived_from_the_values() {
        let mut vars = VariableSpace::new();
        vars.set("ECHO", Some("queries")).unwrap();
        vars.set("QUIET", Some("on")).unwrap();
        vars.set("FETCH_COUNT", Some("100")).unwrap();
        let pset = vars.settings(&PsqlSettings::default());
        assert_eq!(pset.echo, Echo::Queries);
        assert!(pset.quiet);
        assert_eq!(pset.fetch_count, 100);
    }

    #[test]
    fn the_hook_table_defaults_match_startup_c() {
        let vars = VariableSpace::new();
        let pset = vars.settings(&PsqlSettings::default());
        assert_eq!(pset.echo, Echo::None);
        assert_eq!(pset.histsize, 500);
        assert_eq!(pset.verbosity, rlibpq::Verbosity::Default);
        assert_eq!(pset.show_context, rlibpq::ContextVisibility::Errors);
        assert_eq!(pset.comp_case, CompCase::PreserveUpper);
        assert!((pset.watch_interval - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn escaping_matches_libpq() {
        assert_eq!(escape_literal("a'b"), "'a''b'");
        assert_eq!(escape_identifier("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn the_lexer_view_quotes_the_way_each_request_asks() {
        let mut vars = VariableSpace::new();
        vars.set("x", Some("a'b")).unwrap();
        let view = VarView(&vars);

        assert_eq!(
            view.get_variable("x", QuoteType::Plain).as_deref(),
            Some("a'b")
        );
        assert_eq!(
            view.get_variable("x", QuoteType::SqlLiteral).as_deref(),
            Some("'a''b'")
        );
        assert_eq!(
            view.get_variable("x", QuoteType::SqlIdent).as_deref(),
            Some("\"a'b\"")
        );
        assert_eq!(view.get_variable("nosuch", QuoteType::Plain), None);
    }
}
