//! What C initdb tells the user along the success path: the preamble,
//! the progress lines of `initialize_data_directory`, the `trust` warning and
//! the closing instructions (`src/bin/initdb/initdb.c`).
//!
//! Pure text over what the run has decided; `crate::run` writes it. A
//! progress line is split where C splits it: the part `printf` writes before
//! `fflush(stdout)` and the step, and the part written after it (`check_ok`'s
//! `ok`, or the value chosen), so a step that fails leaves the unfinished
//! line on stdout exactly as C's does.

use crate::conf::{self, Settings};
use crate::control::DataChecksums;
use crate::help::PROGNAME;
use crate::path;

/// `initdb.c:3481`: whose files these are, printed once the superuser name
/// has passed its `pg_` check.
#[must_use]
pub fn owned_by(effective_user: &str) -> String {
    format!(
        "The files belonging to this database system will be owned by user \"{effective_user}\".\n\
         This user must also own the server process.\n\n"
    )
}

/// The six locale categories `setup_locale_encoding` reports
/// (`initdb.c:2689`), as `setlocales` has resolved them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locales<'a> {
    pub lc_collate: &'a str,
    pub lc_ctype: &'a str,
    pub lc_messages: &'a str,
    pub lc_monetary: &'a str,
    pub lc_numeric: &'a str,
    pub lc_time: &'a str,
}

impl<'a> Locales<'a> {
    /// A cluster made from the template: `lc_collate` and `lc_ctype` are C
    /// (anything else is refused, `crate::cluster::check_template_can_make`)
    /// and the other four are what `postgresql.conf` gets.
    #[must_use]
    pub fn of_template(settings: &'a Settings) -> Self {
        Self {
            lc_collate: "C",
            lc_ctype: "C",
            lc_messages: &settings.lc_messages,
            lc_monetary: &settings.lc_monetary,
            lc_numeric: &settings.lc_numeric,
            lc_time: &settings.lc_time,
        }
    }
}

/// `setup_locale_encoding`'s first report (`initdb.c:2689`-`:2715`) for the
/// `libc` provider with no `datlocale`, the only one the template makes: one
/// line when all six categories agree, the table otherwise.
#[must_use]
pub fn locale_configuration(locales: &Locales<'_>) -> String {
    let lc_ctype = locales.lc_ctype;
    let others = [
        locales.lc_collate,
        locales.lc_time,
        locales.lc_numeric,
        locales.lc_monetary,
        locales.lc_messages,
    ];
    if others.iter().all(|other| *other == lc_ctype) {
        return format!("The database cluster will be initialized with locale \"{lc_ctype}\".\n");
    }
    format!(
        "The database cluster will be initialized with this locale configuration:\n  \
         locale provider:   libc\n  \
         LC_COLLATE:  {}\n  \
         LC_CTYPE:    {}\n  \
         LC_MESSAGES: {}\n  \
         LC_MONETARY: {}\n  \
         LC_NUMERIC:  {}\n  \
         LC_TIME:     {}\n",
        locales.lc_collate,
        lc_ctype,
        locales.lc_messages,
        locales.lc_monetary,
        locales.lc_numeric,
        locales.lc_time
    )
}

/// `initdb.c:2765`: printed only when `-E` was not given, naming the
/// encoding the cluster gets.
#[must_use]
pub fn default_encoding(name: &str) -> String {
    format!("The default database encoding has accordingly been set to \"{name}\".\n")
}

/// `setup_text_search`'s closing line (`initdb.c:2864`).
#[must_use]
pub fn text_search_configuration(name: &str) -> String {
    format!("The default text search configuration will be set to \"{name}\".\n")
}

/// `initdb.c:3494`-`:3504`: the blank line after the text search line, the
/// checksum line, and the blank line after it (`get_su_pwd` would come
/// between those two, but the template refuses `-W` and `--pwfile`).
#[must_use]
pub fn data_checksums(checksums: DataChecksums) -> &'static str {
    match checksums {
        DataChecksums::Enabled => "\nData page checksums are enabled.\n\n",
        DataChecksums::Disabled => "\nData page checksums are disabled.\n\n",
    }
}

/// `create_data_directory` (`initdb.c:2898`) and `create_xlog_or_symlink`
/// (`:2969`) for a directory that is not there yet.
#[must_use]
pub fn creating_directory(path: &str) -> String {
    format!("creating directory {path} ... ")
}

/// The same two sites (`initdb.c:2912`, `:2984`) for one that is there and
/// empty.
#[must_use]
pub fn fixing_permissions(path: &str) -> String {
    format!("fixing permissions on existing directory {path} ... ")
}

/// `initdb.c:3065`.
pub const CREATING_SUBDIRECTORIES: &str = "creating subdirectories ... ";

/// `test_config_settings` (`initdb.c:1157`-`:1214`): each probe's line with
/// the value it settled on, in C's order.
#[must_use]
pub fn config_settings(settings: &Settings) -> String {
    format!(
        "selecting dynamic shared memory implementation ... {}\n\
         selecting default \"max_connections\" ... {}\n\
         selecting default \"shared_buffers\" ... {}\n\
         selecting default time zone ... {}\n",
        settings.dynamic_shared_memory_type,
        settings.max_connections,
        conf::shared_buffers_value(settings.shared_buffers_blocks),
        settings.default_timezone.as_deref().unwrap_or("GMT")
    )
}

/// `setup_config` (`initdb.c:1291`).
pub const CREATING_CONFIGURATION_FILES: &str = "creating configuration files ... ";

/// `bootstrap_template1` (`initdb.c:1554`).
pub const RUNNING_BOOTSTRAP_SCRIPT: &str = "running bootstrap script ... ";

/// `initialize_data_directory` (`initdb.c:3108`).
pub const PERFORMING_POST_BOOTSTRAP: &str = "performing post-bootstrap initialization ... ";

/// `initdb.c:3518`-`:3523`: the blank line on stdout, then the warning and
/// its hint on stderr (`pg_log_warning`, `pg_log_warning_hint`), when
/// `check_authmethod_unspecified` (`:2572`) had to fill in either side.
pub const TRUST_WARNING_STDOUT: &str = "\n";

/// The stderr half of [`TRUST_WARNING_STDOUT`], without the final newline
/// (the caller adds it, as for every rendered diagnostic).
#[must_use]
pub fn trust_warning() -> String {
    format!(
        "{PROGNAME}: warning: enabling \"trust\" authentication for local connections\n\
         {PROGNAME}: hint: You can change this by editing pg_hba.conf or using the option -A, or \
         --auth-local and --auth-host, the next time you run initdb."
    )
}

/// `authwarning` (`initdb.c:2576`): true when `-A` did not set both sides
/// and the per-side switch for one of them is missing too.
#[must_use]
pub fn needs_trust_warning(
    auth: Option<&str>,
    auth_local: Option<&str>,
    auth_host: Option<&str>,
) -> bool {
    auth.is_none() && (auth_local.is_none() || auth_host.is_none())
}

/// A string `appendShellString` (`src/fe_utils/string_utils.c:582`) cannot
/// quote: it contains a newline or a carriage return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnquotableArgument(pub String);

impl UnquotableArgument {
    /// The line `appendShellString` prints before `exit(EXIT_FAILURE)`
    /// (`string_utils.c:586`), without its final newline. It has no
    /// `initdb:` prefix: it is a bare `fprintf(stderr, …)`.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "shell command argument contains a newline or carriage return: \"{}\"",
            self.0
        )
    }
}

/// `appendShellStringNoError` (`src/fe_utils/string_utils.c:594`), the
/// non-Windows arm: as it is when it is non-empty and made only of safe
/// characters, otherwise in single quotes with each `'` written `'"'"'`.
///
/// # Errors
/// [`UnquotableArgument`] when `arg` contains `\n` or `\r`, which C drops
/// from the quoted string and then refuses (`appendShellString`, `:584`).
pub fn shell_quote(arg: &str) -> Result<String, UnquotableArgument> {
    const SAFE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_./:";
    if arg.contains(['\n', '\r']) {
        return Err(UnquotableArgument(arg.to_owned()));
    }
    if !arg.is_empty() && arg.chars().all(|c| SAFE.contains(c)) {
        return Ok(arg.to_owned());
    }
    Ok(format!("'{}'", arg.replace('\'', "'\"'\"'")))
}

/// `initdb.c:3531`-`:3551`: the command that starts the new cluster —
/// the `pg_ctl` beside `argv[0]` (canonicalized, its last component
/// replaced), `-D` and the data directory as it was given (`pgdata_native`,
/// `:2633`), each quoted for the shell, then `-l logfile start`.
///
/// # Errors
/// [`UnquotableArgument`] for either path, `pg_ctl`'s first as in C.
pub fn start_command(argv0: &str, pgdata_native: &str) -> Result<String, UnquotableArgument> {
    let canonical = path::canonicalize_path(argv0);
    let pg_ctl = path::join_path_components(path::get_parent_directory(&canonical), "pg_ctl");
    Ok(format!(
        "{} -D {} -l logfile start",
        shell_quote(&pg_ctl)?,
        shell_quote(pgdata_native)?
    ))
}

/// `initdb.c:3554`: the closing instructions, unless `--no-instructions`.
#[must_use]
pub fn success(start_command: &str) -> String {
    format!("\nSuccess. You can now start the database server using:\n\n    {start_command}\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preamble_is_upstreams_bytes() {
        assert_eq!(
            owned_by("alice"),
            "The files belonging to this database system will be owned by user \"alice\".\n\
             This user must also own the server process.\n\n"
        );
        assert_eq!(
            default_encoding("UTF8"),
            "The default database encoding has accordingly been set to \"UTF8\".\n"
        );
        assert_eq!(
            text_search_configuration("english"),
            "The default text search configuration will be set to \"english\".\n"
        );
        assert_eq!(
            data_checksums(DataChecksums::Enabled),
            "\nData page checksums are enabled.\n\n"
        );
        assert_eq!(
            data_checksums(DataChecksums::Disabled),
            "\nData page checksums are disabled.\n\n"
        );
    }

    #[test]
    fn one_locale_line_when_all_six_agree_and_the_table_otherwise() {
        let settings = Settings::default();
        assert_eq!(
            locale_configuration(&Locales::of_template(&settings)),
            "The database cluster will be initialized with locale \"C\".\n"
        );
        let settings = Settings {
            lc_messages: "en_US.UTF-8".to_owned(),
            ..Settings::default()
        };
        assert_eq!(
            locale_configuration(&Locales::of_template(&settings)),
            "The database cluster will be initialized with this locale configuration:\n\
             \x20 locale provider:   libc\n\
             \x20 LC_COLLATE:  C\n\
             \x20 LC_CTYPE:    C\n\
             \x20 LC_MESSAGES: en_US.UTF-8\n\
             \x20 LC_MONETARY: C\n\
             \x20 LC_NUMERIC:  C\n\
             \x20 LC_TIME:     C\n"
        );
    }

    #[test]
    fn the_probe_lines_name_what_was_chosen() {
        let settings = Settings {
            default_timezone: Some("Etc/UTC".to_owned()),
            ..Settings::default()
        };
        assert_eq!(
            config_settings(&settings),
            "selecting dynamic shared memory implementation ... posix\n\
             selecting default \"max_connections\" ... 100\n\
             selecting default \"shared_buffers\" ... 128MB\n\
             selecting default time zone ... Etc/UTC\n"
        );
        // initdb.c:1214 — no zone found is announced as GMT.
        let settings = Settings {
            default_timezone: None,
            ..Settings::default()
        };
        assert!(config_settings(&settings).ends_with("selecting default time zone ... GMT\n"));
    }

    #[test]
    fn the_trust_warning_is_upstreams_and_only_when_a_side_was_left_unset() {
        assert_eq!(
            trust_warning(),
            "initdb: warning: enabling \"trust\" authentication for local connections\n\
             initdb: hint: You can change this by editing pg_hba.conf or using the option -A, \
             or --auth-local and --auth-host, the next time you run initdb."
        );
        assert!(needs_trust_warning(None, None, None));
        assert!(needs_trust_warning(None, Some("peer"), None));
        assert!(needs_trust_warning(None, None, Some("md5")));
        assert!(!needs_trust_warning(Some("trust"), None, None));
        assert!(!needs_trust_warning(None, Some("peer"), Some("md5")));
    }

    #[test]
    fn shell_quoting_is_append_shell_strings() {
        assert_eq!(shell_quote("/usr/bin/pg_ctl").unwrap(), "/usr/bin/pg_ctl");
        assert_eq!(shell_quote("C:/x-y_z.1").unwrap(), "C:/x-y_z.1");
        assert_eq!(shell_quote("").unwrap(), "''");
        assert_eq!(shell_quote("a b").unwrap(), "'a b'");
        assert_eq!(shell_quote("it's").unwrap(), "'it'\"'\"'s'");
        assert_eq!(
            shell_quote("a\nb"),
            Err(UnquotableArgument("a\nb".to_owned()))
        );
        assert_eq!(
            UnquotableArgument("a\rb".to_owned()).render(),
            "shell command argument contains a newline or carriage return: \"a\rb\""
        );
    }

    #[test]
    fn the_start_command_names_the_pg_ctl_beside_argv0() {
        assert_eq!(
            start_command("/usr/lib/postgresql/18/bin/initdb", "/srv/data").unwrap(),
            "/usr/lib/postgresql/18/bin/pg_ctl -D /srv/data -l logfile start"
        );
        // Found on PATH: no directory, so a bare pg_ctl.
        assert_eq!(
            start_command("initdb", "data").unwrap(),
            "pg_ctl -D data -l logfile start"
        );
        // argv[0] is canonicalized; the data directory is not (pgdata_native).
        assert_eq!(
            start_command("./bin//initdb", "./my data/").unwrap(),
            "bin/pg_ctl -D './my data/' -l logfile start"
        );
        assert_eq!(
            start_command("initdb", "a\nb"),
            Err(UnquotableArgument("a\nb".to_owned()))
        );
    }

    #[test]
    fn the_closing_instructions_are_upstreams_bytes() {
        assert_eq!(
            success("pg_ctl -D data -l logfile start"),
            "\nSuccess. You can now start the database server using:\n\n    \
             pg_ctl -D data -l logfile start\n\n"
        );
    }
}
