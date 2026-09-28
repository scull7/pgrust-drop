//! `PsqlSettings` and its enums: `src/bin/psql/settings.h`.
//!
//! Upstream declares one global `pset` (`settings.h:189`). Here it is a plain
//! value that the caller owns, so every calculation that reads settings can be
//! unit-tested with a settings value built in the test.
//!
//! Only the fields this port has reached are present; the one-shot
//! `\gset` and `\gexec` fields belong to NAT-402 and are not declared as
//! dead weight here. `\crosstabview`'s, `\g`'s and the pipeline's are
//! (NAT-404).

use rlibpq::{ContextVisibility, Encoding, QueryResult, Verbosity};

/// `DEFAULT_CSV_FIELD_SEP` (`settings.h:14`).
pub const DEFAULT_CSV_FIELD_SEP: char = ',';
/// `DEFAULT_FIELD_SEP` (`settings.h:15`).
pub const DEFAULT_FIELD_SEP: &str = "|";
/// `DEFAULT_RECORD_SEP` (`settings.h:16`).
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

/// `EXIT_SUCCESS` (`settings.h:193`).
pub const EXIT_SUCCESS: u8 = 0;
/// `EXIT_FAILURE` (`settings.h:197`).
pub const EXIT_FAILURE: u8 = 1;
/// `EXIT_BADCONN` (`settings.h:200`).
pub const EXIT_BADCONN: u8 = 2;
/// `EXIT_USER` (`settings.h:202`).
pub const EXIT_USER: u8 = 3;

/// `PSQL_ECHO` (`settings.h:41`).
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

/// `PSQL_ECHO_HIDDEN` (`settings.h:49`).
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

/// `PSQL_ERROR_ROLLBACK` (`settings.h:56`).
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

/// `PSQL_COMP_CASE` (`settings.h:63`).
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

/// `HistControl` (`settings.h:86`).
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

/// `enum trivalue` (`settings.h:94`).
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

/// `printFormat` (`fe_utils/print.h:28`), minus `PRINT_NOTHING`, which
/// exists only to catch an uninitialized struct and which a Rust value cannot
/// be.
///
/// Every format can be *selected* (`\pset format`, `-A`, `-H`, `--csv`);
/// [`crate::print`] refuses the ones it cannot yet render rather than
/// pretending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrintFormat {
    /// `PRINT_ALIGNED`
    #[default]
    Aligned,
    /// `PRINT_ASCIIDOC`
    Asciidoc,
    /// `PRINT_CSV`
    Csv,
    /// `PRINT_HTML`
    Html,
    /// `PRINT_LATEX`
    Latex,
    /// `PRINT_LATEX_LONGTABLE`
    LatexLongtable,
    /// `PRINT_TROFF_MS`
    TroffMs,
    /// `PRINT_UNALIGNED`
    Unaligned,
    /// `PRINT_WRAPPED`
    Wrapped,
}

impl PrintFormat {
    /// `_align2string()` (`command.c:4987`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Aligned => "aligned",
            Self::Asciidoc => "asciidoc",
            Self::Csv => "csv",
            Self::Html => "html",
            Self::Latex => "latex",
            Self::LatexLongtable => "latex-longtable",
            Self::TroffMs => "troff-ms",
            Self::Unaligned => "unaligned",
            Self::Wrapped => "wrapped",
        }
    }
}

/// `printTableOpt.expanded` (`fe_utils/print.h:114`): `0=no, 1=yes, 2=auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Expanded {
    /// `0`
    #[default]
    Off,
    /// `1`
    On,
    /// `2`
    Auto,
}

/// `printXheaderWidthType` (`fe_utils/print.h:69`), with
/// `expanded_header_exact_width` folded into the variant that reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XheaderWidth {
    /// `PRINT_XHEADER_FULL`
    #[default]
    Full,
    /// `PRINT_XHEADER_COLUMN`
    Column,
    /// `PRINT_XHEADER_PAGE`
    Page,
    /// `PRINT_XHEADER_EXACT_WIDTH`, with `expanded_header_exact_width`.
    ExactWidth(i32),
}

/// `printTableOpt.pager` (`fe_utils/print.h:122`): `0=off 1=on 2=always`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pager {
    /// `0`
    Off,
    /// `1`
    #[default]
    On,
    /// `2`
    Always,
}

impl Pager {
    /// The number `pset_value_string` prints with `%d` (`command.c:5764`).
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::On => 1,
            Self::Always => 2,
        }
    }
}

/// Which `printTextFormat` `printTableOpt.line_style` points at
/// (`fe_utils/print.h:131`): `pg_asciiformat`, `pg_asciiformat_old` or
/// `pg_utf8format`. `NULL` means ascii (`get_line_style`, `print.c:3678`), so
/// ascii is the default here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineStyle {
    /// `&pg_asciiformat`
    #[default]
    Ascii,
    /// `&pg_asciiformat_old`
    OldAscii,
    /// `&pg_utf8format`
    Unicode,
}

impl LineStyle {
    /// `printTextFormat.name`, which `\pset linestyle` reports.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::OldAscii => "old-ascii",
            Self::Unicode => "unicode",
        }
    }
}

/// `unicode_linestyle` (`fe_utils/print.h:99`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnicodeLinestyle {
    /// `UNICODE_LINESTYLE_SINGLE`
    #[default]
    Single,
    /// `UNICODE_LINESTYLE_DOUBLE`
    Double,
}

impl UnicodeLinestyle {
    /// `_unicode_linestyle2string()` (`command.c:5043`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
        }
    }
}

/// A field or record separator: a string, or the zero byte
/// (`fe_utils/print.h:105`).
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

/// `printTableOpt` (`fe_utils/print.h:111`), minus `prior_records` and
/// `encoding`, which nothing here reads yet.
// One field per C struct member, and upstream's are `bool`; grouping them into
// an enum here would put this struct out of step with the header it tracks.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableOpt {
    /// `format`
    pub format: PrintFormat,
    /// `expanded`
    pub expanded: Expanded,
    /// `expanded_header_width_type` and `expanded_header_exact_width`
    pub expanded_header_width: XheaderWidth,
    /// `border`: an `unsigned short`, so `\pset border -1` stores 65535.
    pub border: u16,
    /// `pager`
    pub pager: Pager,
    /// `pager_min_lines`
    pub pager_min_lines: i32,
    /// `tuples_only`
    pub tuples_only: bool,
    /// `start_table`
    pub start_table: bool,
    /// `stop_table`
    pub stop_table: bool,
    /// `default_footer`
    pub default_footer: bool,
    /// `line_style`
    pub line_style: LineStyle,
    /// `fieldSep`
    pub field_sep: Separator,
    /// `recordSep`
    pub record_sep: Separator,
    /// `csvFieldSep`: a single byte, which `do_pset` enforces.
    pub csv_field_sep: char,
    /// `numericLocale`
    pub numeric_locale: bool,
    /// `tableAttr`
    pub table_attr: Option<String>,
    /// `env_columns`: `$COLUMNS`, read before readline can change it.
    pub env_columns: i32,
    /// `columns`: target width for the wrapped format.
    pub columns: i32,
    /// `unicode_border_linestyle`
    pub unicode_border_linestyle: UnicodeLinestyle,
    /// `unicode_column_linestyle`
    pub unicode_column_linestyle: UnicodeLinestyle,
    /// `unicode_header_linestyle`
    pub unicode_header_linestyle: UnicodeLinestyle,
}

impl Default for TableOpt {
    /// The block at `startup.c:165`-`:184`, which relies on the unmentioned
    /// fields starting out 0/false/NULL.
    fn default() -> Self {
        Self {
            format: PrintFormat::Aligned,
            expanded: Expanded::Off,
            expanded_header_width: XheaderWidth::Full,
            border: 1,
            pager: Pager::On,
            pager_min_lines: 0,
            tuples_only: false,
            start_table: true,
            stop_table: true,
            default_footer: true,
            line_style: LineStyle::Ascii,
            field_sep: Separator {
                separator: None,
                separator_zero: false,
            },
            record_sep: Separator {
                separator: None,
                separator_zero: false,
            },
            csv_field_sep: DEFAULT_CSV_FIELD_SEP,
            numeric_locale: false,
            table_attr: None,
            env_columns: 0,
            columns: 0,
            unicode_border_linestyle: UnicodeLinestyle::Single,
            unicode_column_linestyle: UnicodeLinestyle::Single,
            unicode_header_linestyle: UnicodeLinestyle::Single,
        }
    }
}

/// `printQueryOpt` (`fe_utils/print.h:183`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrintQueryOpt {
    /// `topt`
    pub topt: TableOpt,
    /// `nullPrint`
    pub null_print: Option<String>,
    /// `title`
    pub title: Option<String>,
}

/// `PSQL_SEND_MODE` (`settings.h:71`), each variant carrying the `stmtName`
/// and `bind_params` it reads, so a mode cannot be set without its arguments
/// or keep a stale one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SendMode {
    /// `PSQL_SEND_QUERY`: the simple query protocol.
    #[default]
    Query,
    /// `PSQL_SEND_EXTENDED_CLOSE`: `\close_prepared`, `PQsendClosePrepared`.
    ExtendedClose {
        /// `stmtName`
        statement: String,
    },
    /// `PSQL_SEND_EXTENDED_PARSE`: `\parse`, `PQsendPrepare`.
    ExtendedParse {
        /// `stmtName`
        statement: String,
    },
    /// `PSQL_SEND_EXTENDED_QUERY_PARAMS`: `\bind`, `PQsendQueryParams`.
    ExtendedQueryParams {
        /// `bind_params`, each a text parameter.
        params: Vec<String>,
    },
    /// `PSQL_SEND_EXTENDED_QUERY_PREPARED`: `\bind_named`,
    /// `PQsendQueryPrepared`.
    ExtendedQueryPrepared {
        /// `stmtName`
        statement: String,
        /// `bind_params`
        params: Vec<String>,
    },
    /// `PSQL_SEND_PIPELINE_SYNC`: `\syncpipeline`, `PQsendPipelineSync`.
    PipelineSync,
    /// `PSQL_SEND_START_PIPELINE_MODE`: `\startpipeline`,
    /// `PQenterPipelineMode`.
    StartPipelineMode,
    /// `PSQL_SEND_END_PIPELINE_MODE`: `\endpipeline`, `PQpipelineSync`, then
    /// every result, then `PQexitPipelineMode`.
    EndPipelineMode,
    /// `PSQL_SEND_FLUSH`: `\flush`, `PQflush`.
    Flush,
    /// `PSQL_SEND_FLUSH_REQUEST`: `\flushrequest`, `PQsendFlushRequest`.
    FlushRequest,
    /// `PSQL_SEND_GET_RESULTS`: `\getresults`, which sends nothing and reads
    /// [`PipelineCounters::requested_results`] results.
    GetResults,
}

impl SendMode {
    /// The modes that drive a pipeline rather than send a statement: they
    /// are sent even when the query buffer is empty, and only the pipeline
    /// path of `ExecQueryAndProcessResults` knows them.
    #[must_use]
    pub fn is_pipeline_control(&self) -> bool {
        matches!(
            self,
            Self::PipelineSync
                | Self::StartPipelineMode
                | Self::EndPipelineMode
                | Self::Flush
                | Self::FlushRequest
                | Self::GetResults
        )
    }
}

/// `piped_commands`, `piped_syncs`, `available_results` and
/// `requested_results` (`settings.h:126`-`:131`): psql's own account of a
/// pipeline, which `PIPELINE_COMMAND_COUNT`, `PIPELINE_SYNC_COUNT` and
/// `PIPELINE_RESULT_COUNT` publish (`SetPipelineVariables`, `common.c:536`).
///
/// Upstream's are `int`s that only its underflow guards keep from going
/// negative; unsigned counters with saturating decrements say the same.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipelineCounters {
    /// `piped_commands`: commands sent since the last sync or flush request.
    pub piped_commands: usize,
    /// `piped_syncs`: syncs sent whose `PGRES_PIPELINE_SYNC` is unread.
    pub piped_syncs: usize,
    /// `available_results`: results the server has been asked to send.
    pub available_results: usize,
    /// `requested_results`: how many results, syncs included, the current
    /// `\getresults` or `\endpipeline` still wants.
    pub requested_results: usize,
}

/// `PsqlSettings` (`settings.h:101`), minus the fields this port has not
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
    /// `encoding`: the client encoding, `PQclientEncoding` as `SyncVariables`,
    /// `\encoding` and `SendQuery` last read it (`command.c:4580`, `:1624`,
    /// `common.c:1296`).
    pub encoding: Encoding,
    /// `progname`: in case you renamed psql.
    pub progname: String,
    /// `inputfile`: file being currently processed, if any.
    pub inputfile: Option<String>,
    /// `lineno`
    pub lineno: u64,
    /// `stmt_lineno`: line number inside the current statement.
    pub stmt_lineno: u64,
    /// `timing`: `\timing`'s switch.
    pub timing: bool,
    /// `last_error_result`: the most recent failed result, for `\errverbose`
    /// (`ClearOrSaveResult`, `common.c:560`).
    pub last_error_result: Option<QueryResult>,
    /// `log_flags & PG_LOG_FLAG_TERSE`. Not a `pset` member upstream but
    /// `logging.c`'s own global, which psql sets through `pg_logging_config`
    /// at `startup.c:384`, `:397`, `:462` and `command.c:4970`, `:4979`; it is
    /// kept here so [`crate::logging`] reads all of its state from one place.
    pub log_terse: bool,
    /// `crosstab_flag` and `ctv_args` (`settings.h:132`-`:133`): the one-shot
    /// request `\crosstabview` leaves for the next `SendQuery`, which takes it.
    pub crosstab: Option<crate::crosstab::CtvArgs>,
    /// `gsavepopt` (`settings.h:115`): the print options as they were before
    /// `\g (…)` or `\gx` changed them for one query, which `SendQuery` puts
    /// back (`common.c:1319`).
    pub gsavepopt: Option<PrintQueryOpt>,
    /// `send_mode`, `bind_nparams`, `bind_params` and `stmtName`
    /// (`settings.h:120`-`:124`): how the next `SendQuery` sends its query,
    /// which it resets (`clean_extended_state`, `common.c:2781`).
    pub send_mode: SendMode,
    /// The pipeline counters (`settings.h:126`-`:131`).
    pub pipeline: PipelineCounters,

    // The remaining fields are the ones `settings.h:161` says are set by the
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
    /// (`startup.c:152`-`:211`).
    fn default() -> Self {
        Self {
            popt: PrintQueryOpt::default(),
            notty: false,
            get_password: Trivalue::Default,
            cur_cmd_interactive: false,
            sversion: 0,
            // `pset.encoding` is zero, PG_SQL_ASCII, until `SyncVariables`.
            encoding: Encoding::SqlAscii,
            progname: "psql".to_string(),
            inputfile: None,
            lineno: 0,
            stmt_lineno: 1,
            timing: false,
            last_error_result: None,
            // `log_flags` starts at 0 (`logging.c:24`).
            log_terse: false,
            crosstab: None,
            gsavepopt: None,
            send_mode: SendMode::Query,
            pipeline: PipelineCounters::default(),
            // `SetVariableBool(pset.vars, "AUTOCOMMIT")` at `startup.c:202`.
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
            // `startup.c:206`.
            show_all_results: true,
            show_context: ContextVisibility::Errors,
        }
    }
}

impl PsqlSettings {
    /// The separator defaults `main()` supplies after option parsing, once it
    /// knows `-F`/`-R`/`-z`/`-0` did not (`startup.c:227`-`:238`).
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
        assert_eq!(pset.popt.topt.pager, Pager::On);
        assert_eq!(pset.popt.topt.line_style, LineStyle::Ascii);
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
