//! `PsqlSettings` and its enums: `src/bin/psql/settings.h`.
//!
//! Upstream declares one global `pset` (`settings.h:189`). Here it is a plain
//! value that the caller owns, so every calculation that reads settings can be
//! unit-tested with a settings value built in the test.
//!
//! Only the fields this port has reached are present; the one-shot `\g`,
//! `\gset`, `\crosstabview` and pipeline fields belong to NAT-402/NAT-403 and
//! are not declared as dead weight here.

use rlibpq::{ContextVisibility, Verbosity};

/// `DEFAULT_CSV_FIELD_SEP` (`settings.h:13`).
pub const DEFAULT_CSV_FIELD_SEP: char = ',';
/// `DEFAULT_FIELD_SEP` (`settings.h:14`).
pub const DEFAULT_FIELD_SEP: &str = "|";
/// `DEFAULT_RECORD_SEP` (`settings.h:15`).
pub const DEFAULT_RECORD_SEP: &str = "\n";
/// `DEFAULT_PROMPT1` (`settings.h:26`).
pub const DEFAULT_PROMPT1: &str = "%/%R%x%# ";
/// `DEFAULT_PROMPT2` (`settings.h:27`).
pub const DEFAULT_PROMPT2: &str = "%/%R%x%# ";
/// `DEFAULT_PROMPT3` (`settings.h:28`).
pub const DEFAULT_PROMPT3: &str = ">> ";
/// `DEFAULT_WATCH_INTERVAL` (`settings.h:30`).
pub const DEFAULT_WATCH_INTERVAL: &str = "2";
/// `DEFAULT_WATCH_INTERVAL_MAX` (`settings.h:35`).
pub const DEFAULT_WATCH_INTERVAL_MAX: f64 = 1_000_000.0;

/// `EXIT_SUCCESS` (`settings.h:192`).
pub const EXIT_SUCCESS: u8 = 0;
/// `EXIT_FAILURE` (`settings.h:196`).
pub const EXIT_FAILURE: u8 = 1;
/// `EXIT_BADCONN` (`settings.h:199`).
pub const EXIT_BADCONN: u8 = 2;
/// `EXIT_USER` (`settings.h:201`).
pub const EXIT_USER: u8 = 3;

/// `PSQL_ECHO` (`settings.h:40`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Echo {
    /// `PSQL_ECHO_NONE`
    #[default]
    None,
    /// `PSQL_ECHO_QUERIES`
    Queries,
    /// `PSQL_ECHO_ERRORS`
    Errors,
    /// `PSQL_ECHO_ALL`
    All,
}

/// `PSQL_ECHO_HIDDEN` (`settings.h:48`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EchoHidden {
    /// `PSQL_ECHO_HIDDEN_OFF`
    #[default]
    Off,
    /// `PSQL_ECHO_HIDDEN_ON`
    On,
    /// `PSQL_ECHO_HIDDEN_NOEXEC`
    NoExec,
}

/// `PSQL_ERROR_ROLLBACK` (`settings.h:55`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ErrorRollback {
    /// `PSQL_ERROR_ROLLBACK_OFF`
    #[default]
    Off,
    /// `PSQL_ERROR_ROLLBACK_INTERACTIVE`
    Interactive,
    /// `PSQL_ERROR_ROLLBACK_ON`
    On,
}

/// `PSQL_COMP_CASE` (`settings.h:62`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompCase {
    /// `PSQL_COMP_CASE_PRESERVE_UPPER`
    #[default]
    PreserveUpper,
    /// `PSQL_COMP_CASE_PRESERVE_LOWER`
    PreserveLower,
    /// `PSQL_COMP_CASE_UPPER`
    Upper,
    /// `PSQL_COMP_CASE_LOWER`
    Lower,
}

/// `HistControl` (`settings.h:84`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistControl {
    /// `hctl_none`
    #[default]
    None,
    /// `hctl_ignorespace`
    IgnoreSpace,
    /// `hctl_ignoredups`
    IgnoreDups,
    /// `hctl_ignoreboth`
    IgnoreBoth,
}

/// `enum trivalue` (`settings.h:92`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trivalue {
    /// `TRI_DEFAULT`
    #[default]
    Default,
    /// `TRI_NO`
    No,
    /// `TRI_YES`
    Yes,
}

/// `printFormat` (`fe_utils/print.h:29`), as far as this issue reaches.
///
/// NAT-400 owns the rest of the matrix; the variants are declared here so the
/// option table can record what `-A`, `-H` and `--csv` asked for, and
/// [`crate::print`] refuses the ones it cannot yet render rather than
/// pretending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintFormat {
    /// `PRINT_ALIGNED`
    #[default]
    Aligned,
    /// `PRINT_UNALIGNED`
    Unaligned,
    /// `PRINT_WRAPPED`
    Wrapped,
    /// `PRINT_HTML`
    Html,
    /// `PRINT_CSV`
    Csv,
    /// `PRINT_ASCIIDOC`
    Asciidoc,
    /// `PRINT_LATEX`
    Latex,
    /// `PRINT_TROFF_MS`
    TroffMs,
}

/// A field or record separator: a string, or the zero byte
/// (`fe_utils/print.h:74`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Separator {
    /// `separator`
    pub separator: Option<String>,
    /// `separator_zero`
    pub separator_zero: bool,
}

impl Separator {
    /// The bytes that actually go between fields.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        if self.separator_zero {
            vec![0]
        } else {
            self.separator.as_deref().unwrap_or("").as_bytes().to_vec()
        }
    }
}

/// `printTableOpt` (`fe_utils/print.h:92`), the fields `main()` initializes.
// One field per C struct member, and upstream's are `bool`; grouping them into
// an enum here would put this struct out of step with the header it tracks.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOpt {
    /// `format`
    pub format: PrintFormat,
    /// `expanded`
    pub expanded: bool,
    /// `border`
    pub border: u16,
    /// `tuples_only`
    pub tuples_only: bool,
    /// `start_table`
    pub start_table: bool,
    /// `stop_table`
    pub stop_table: bool,
    /// `default_footer`
    pub default_footer: bool,
    /// `fieldSep`
    pub field_sep: Separator,
    /// `recordSep`
    pub record_sep: Separator,
    /// `csvFieldSep`
    pub csv_field_sep: char,
    /// `tableAttr`
    pub table_attr: Option<String>,
    /// `env_columns`: `$COLUMNS`, read before readline can change it.
    pub env_columns: i32,
}

impl Default for TableOpt {
    /// The block at `startup.c:164`-`:180`, which relies on the unmentioned
    /// fields starting out 0/false/NULL.
    fn default() -> Self {
        Self {
            format: PrintFormat::Aligned,
            expanded: false,
            border: 1,
            tuples_only: false,
            start_table: true,
            stop_table: true,
            default_footer: true,
            field_sep: Separator {
                separator: None,
                separator_zero: false,
            },
            record_sep: Separator {
                separator: None,
                separator_zero: false,
            },
            csv_field_sep: DEFAULT_CSV_FIELD_SEP,
            table_attr: None,
            env_columns: 0,
        }
    }
}

/// `printQueryOpt` (`fe_utils/print.h:172`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrintQueryOpt {
    /// `topt`
    pub topt: TableOpt,
    /// `nullPrint`
    pub null_print: Option<String>,
    /// `title`
    pub title: Option<String>,
}

/// `PsqlSettings` (`settings.h:99`), minus the fields this port has not
/// reached and minus the ones that are C file handles.
// As `TableOpt`: these are upstream's `bool` members, one for one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq)]
pub struct PsqlSettings {
    /// `popt`: the active print format settings.
    pub popt: PrintQueryOpt,
    /// `notty`: stdin or stdout is not a tty (as determined on startup).
    pub notty: bool,
    /// `getPassword`
    pub get_password: Trivalue,
    /// `cur_cmd_interactive`
    pub cur_cmd_interactive: bool,
    /// `sversion`: backend server version.
    pub sversion: i32,
    /// `progname`: in case you renamed psql.
    pub progname: String,
    /// `inputfile`: file being currently processed, if any.
    pub inputfile: Option<String>,
    /// `lineno`
    pub lineno: u64,
    /// `stmt_lineno`: line number inside the current statement.
    pub stmt_lineno: u64,
    /// `timing`
    pub timing: bool,

    // The remaining fields are the ones `settings.h:159` says are set by the
    // assign hooks in `vars`; `crate::variables::VariableSpace::settings`
    // derives every one of them.
    /// `autocommit`
    pub autocommit: bool,
    /// `on_error_stop`
    pub on_error_stop: bool,
    /// `quiet`
    pub quiet: bool,
    /// `singleline`
    pub singleline: bool,
    /// `singlestep`
    pub singlestep: bool,
    /// `hide_compression`
    pub hide_compression: bool,
    /// `hide_tableam`
    pub hide_tableam: bool,
    /// `fetch_count`
    pub fetch_count: i32,
    /// `histsize`
    pub histsize: i32,
    /// `ignoreeof`
    pub ignoreeof: i32,
    /// `watch_interval`
    pub watch_interval: f64,
    /// `echo`
    pub echo: Echo,
    /// `echo_hidden`
    pub echo_hidden: EchoHidden,
    /// `on_error_rollback`
    pub on_error_rollback: ErrorRollback,
    /// `comp_case`
    pub comp_case: CompCase,
    /// `histcontrol`
    pub histcontrol: HistControl,
    /// `prompt1`
    pub prompt1: String,
    /// `prompt2`
    pub prompt2: String,
    /// `prompt3`
    pub prompt3: String,
    /// `verbosity`: current error verbosity level.
    pub verbosity: Verbosity,
    /// `show_all_results`
    pub show_all_results: bool,
    /// `show_context`: current context display level.
    pub show_context: ContextVisibility,
}

impl Default for PsqlSettings {
    /// What `main()` sets before `parse_psql_options` runs
    /// (`startup.c:152`-`:212`).
    fn default() -> Self {
        Self {
            popt: PrintQueryOpt::default(),
            notty: false,
            get_password: Trivalue::Default,
            cur_cmd_interactive: false,
            sversion: 0,
            progname: "psql".to_string(),
            inputfile: None,
            lineno: 0,
            stmt_lineno: 1,
            timing: false,
            // `SetVariableBool(pset.vars, "AUTOCOMMIT")` at `startup.c:201`.
            autocommit: true,
            on_error_stop: false,
            quiet: false,
            singleline: false,
            singlestep: false,
            hide_compression: false,
            hide_tableam: false,
            fetch_count: 0,
            histsize: 500,
            ignoreeof: 0,
            watch_interval: 2.0,
            echo: Echo::None,
            echo_hidden: EchoHidden::Off,
            on_error_rollback: ErrorRollback::Off,
            comp_case: CompCase::PreserveUpper,
            histcontrol: HistControl::None,
            prompt1: DEFAULT_PROMPT1.to_string(),
            prompt2: DEFAULT_PROMPT2.to_string(),
            prompt3: DEFAULT_PROMPT3.to_string(),
            verbosity: Verbosity::Default,
            // `SetVariableBool(pset.vars, "SHOW_ALL_RESULTS")` at
            // `startup.c:205`.
            show_all_results: true,
            show_context: ContextVisibility::Errors,
        }
    }
}

impl PsqlSettings {
    /// The separator defaults `main()` supplies after option parsing, once it
    /// knows `-F`/`-R`/`-z`/`-0` did not (`startup.c:228`-`:238`).
    pub fn apply_separator_defaults(&mut self) {
        let topt = &mut self.popt.topt;
        if topt.field_sep.separator.is_none() && !topt.field_sep.separator_zero {
            topt.field_sep.separator = Some(DEFAULT_FIELD_SEP.to_string());
        }
        if topt.record_sep.separator.is_none() && !topt.record_sep.separator_zero {
            topt.record_sep.separator = Some(DEFAULT_RECORD_SEP.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_settings_h() {
        assert_eq!(
            (EXIT_SUCCESS, EXIT_FAILURE, EXIT_BADCONN, EXIT_USER),
            (0, 1, 2, 3)
        );
    }

    #[test]
    fn the_print_defaults_are_the_ones_main_sets() {
        let pset = PsqlSettings::default();
        assert_eq!(pset.popt.topt.format, PrintFormat::Aligned);
        assert_eq!(pset.popt.topt.border, 1);
        assert!(pset.popt.topt.start_table);
        assert!(pset.popt.topt.stop_table);
        assert!(pset.popt.topt.default_footer);
        assert!(pset.autocommit);
        assert!(pset.show_all_results);
    }

    #[test]
    fn separator_defaults_are_only_applied_when_nothing_asked_otherwise() {
        let mut pset = PsqlSettings::default();
        pset.apply_separator_defaults();
        assert_eq!(pset.popt.topt.field_sep.bytes(), b"|");
        assert_eq!(pset.popt.topt.record_sep.bytes(), b"\n");

        let mut pset = PsqlSettings::default();
        pset.popt.topt.field_sep.separator_zero = true;
        pset.apply_separator_defaults();
        assert_eq!(pset.popt.topt.field_sep.bytes(), vec![0]);
    }
}
