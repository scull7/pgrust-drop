//! The initdb command line: data ([`Options`]) and the pure decision of what an
//! invocation means ([`plan`]).
//!
//! The option table mirrors `long_options[]` and the getopt string
//! `"A:c:dD:E:gkL:nNsST:U:WX:"` in `initdb.c` one to one, including the two
//! backwards-compatibility spellings `--noclean` and `--nosync`. Parsing is
//! usage-rs (ADR-0004); the `argv[1]`-only fast path for help and version is
//! upstream's and runs before the parser.

use std::ffi::{OsStr, OsString};

use usage::Cli;

/// Every option C initdb accepts, in `long_options[]` order.
///
/// Field names follow the C variables where one exists. All values are kept
/// as the strings the user typed; validation is a later, separate calculation
/// (NAT-378) so its error text can be compared against C's.
// The struct is the C option table, one field per `long_options[]` entry, so
// every switch stays a bool here; the typed `Plan` built from it (NAT-378) is
// where these become enums.
#[allow(clippy::struct_excessive_bools)]
#[derive(Cli, Debug, Clone, Default, PartialEq, Eq)]
#[usage(
    bin = "initdb",
    unknown_flags = "error",
    disable_help_flag,
    disable_version_flag,
    args_override_self
)]
pub struct Options {
    /// location for this database cluster
    #[usage(short = 'D', long = "pgdata", value_name = "DATADIR")]
    pub pgdata: Option<String>,
    /// set default encoding for new databases
    #[usage(short = 'E', long = "encoding", value_name = "ENCODING")]
    pub encoding: Option<String>,
    /// set default locale for new databases
    #[usage(long = "locale", value_name = "LOCALE")]
    pub locale: Option<String>,
    /// set default collation locale
    #[usage(long = "lc-collate", value_name = "LOCALE")]
    pub lc_collate: Option<String>,
    /// set default character classification locale
    #[usage(long = "lc-ctype", value_name = "LOCALE")]
    pub lc_ctype: Option<String>,
    /// set default monetary locale
    #[usage(long = "lc-monetary", value_name = "LOCALE")]
    pub lc_monetary: Option<String>,
    /// set default numeric locale
    #[usage(long = "lc-numeric", value_name = "LOCALE")]
    pub lc_numeric: Option<String>,
    /// set default time locale
    #[usage(long = "lc-time", value_name = "LOCALE")]
    pub lc_time: Option<String>,
    /// set default messages locale
    #[usage(long = "lc-messages", value_name = "LOCALE")]
    pub lc_messages: Option<String>,
    /// equivalent to --locale=C
    #[usage(long = "no-locale")]
    pub no_locale: bool,
    /// default text search configuration
    #[usage(short = 'T', long = "text-search-config", value_name = "CFG")]
    pub text_search_config: Option<String>,
    /// default authentication method for local connections
    #[usage(short = 'A', long = "auth", value_name = "METHOD")]
    pub auth: Option<String>,
    /// default authentication method for local-socket connections
    #[usage(long = "auth-local", value_name = "METHOD")]
    pub auth_local: Option<String>,
    /// default authentication method for local TCP/IP connections
    #[usage(long = "auth-host", value_name = "METHOD")]
    pub auth_host: Option<String>,
    /// prompt for a password for the new superuser
    #[usage(short = 'W', long = "pwprompt")]
    pub pwprompt: bool,
    /// read password for the new superuser from file
    #[usage(long = "pwfile", value_name = "FILE")]
    pub pwfile: Option<String>,
    /// database superuser name
    #[usage(short = 'U', long = "username", value_name = "NAME")]
    pub username: Option<String>,
    /// show this help, then exit (honoured only as the first argument, as upstream)
    #[usage(short = '?', long = "help")]
    pub help: bool,
    /// output version information, then exit (honoured only as the first argument)
    #[usage(short = 'V', long = "version")]
    pub version: bool,
    /// generate lots of debugging output
    #[usage(short = 'd', long = "debug")]
    pub debug: bool,
    /// show internal settings, then exit
    #[usage(short = 's', long = "show")]
    pub show: bool,
    /// do not clean up after errors
    #[usage(short = 'n', long = "no-clean", alias = "noclean")]
    pub no_clean: bool,
    /// do not wait for changes to be written safely to disk
    #[usage(short = 'N', long = "no-sync", alias = "nosync")]
    pub no_sync: bool,
    /// do not print instructions for next steps
    #[usage(long = "no-instructions")]
    pub no_instructions: bool,
    /// override default setting for server parameter
    #[usage(short = 'c', long = "set", value_name = "NAME=VALUE")]
    pub set: Vec<String>,
    /// only sync database files to disk, then exit
    #[usage(short = 'S', long = "sync-only")]
    pub sync_only: bool,
    /// location for the write-ahead log directory
    #[usage(short = 'X', long = "waldir", value_name = "WALDIR")]
    pub waldir: Option<String>,
    /// size of WAL segments, in megabytes
    #[usage(long = "wal-segsize", value_name = "SIZE")]
    pub wal_segsize: Option<String>,
    /// use data page checksums
    #[usage(short = 'k', long = "data-checksums")]
    pub data_checksums: bool,
    /// allow group read/execute on data directory
    #[usage(short = 'g', long = "allow-group-access")]
    pub allow_group_access: bool,
    /// set `debug_discard_caches=1`
    #[usage(long = "discard-caches")]
    pub discard_caches: bool,
    /// set default locale provider for new databases
    #[usage(long = "locale-provider", value_name = "PROVIDER")]
    pub locale_provider: Option<String>,
    /// set builtin locale name for new databases
    #[usage(long = "builtin-locale", value_name = "LOCALE")]
    pub builtin_locale: Option<String>,
    /// set ICU locale ID for new databases
    #[usage(long = "icu-locale", value_name = "LOCALE")]
    pub icu_locale: Option<String>,
    /// set additional ICU collation rules for new databases
    #[usage(long = "icu-rules", value_name = "RULES")]
    pub icu_rules: Option<String>,
    /// set method for syncing files to disk
    #[usage(long = "sync-method", value_name = "METHOD")]
    pub sync_method: Option<String>,
    /// do not use data page checksums
    #[usage(long = "no-data-checksums")]
    pub no_data_checksums: bool,
    /// do not sync files within database directories
    #[usage(long = "no-sync-data-files")]
    pub no_sync_data_files: bool,
    /// where to find the input files (short option only, as upstream)
    #[usage(short = 'L', value_name = "DIRECTORY")]
    pub input_dir: Option<String>,
    /// the data directory, when not given with -D
    pub datadir: Option<String>,
}

impl Options {
    /// The data directory after `initdb.c`'s rule: `-D` wins, otherwise the
    /// positional argument; both together is "too many command-line arguments".
    /// `None` means "fall back to `PGDATA`" (a later step).
    ///
    /// # Errors
    /// The exact `pg_log_error` message C prints when both are given.
    pub fn resolve_datadir(&self) -> Result<Option<&str>, String> {
        match (&self.pgdata, &self.datadir) {
            (Some(_), Some(positional)) => Err(format!(
                "too many command-line arguments (first is \"{positional}\")"
            )),
            (Some(flag), None) => Ok(Some(flag)),
            (None, Some(positional)) => Ok(Some(positional)),
            (None, None) => Ok(None),
        }
    }
}

/// What one command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// `--help` or `-?` as the first argument.
    PrintHelp,
    /// `--version` or `-V` as the first argument.
    PrintVersion,
    /// `--help`/`--version` anywhere else: C's getopt hands `'?'` to the
    /// `default:` arm, which prints only the hint and exits 1.
    Hint,
    /// A `pg_log_error` + hint + exit 1 that needs no filesystem.
    Fatal(String),
    /// usage-rs could not parse the line; the rendered diagnostic, exit 2.
    Unparsable(String),
    /// Go initialize a cluster (boxed: `Options` is ~600 bytes, the other arms are small).
    Init(Box<Options>),
}

/// Decide what `args` (everything after the program name) asks for.
#[must_use]
pub fn plan(args: &[OsString]) -> Invocation {
    if let Some(fast) = fast_path(args.first().map(OsString::as_os_str)) {
        return fast;
    }
    let words: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    match Options::parse_from(&words) {
        Ok(options) => classify(options),
        Err(err) => Invocation::Unparsable(Options::render_failure(&words, &err).clone()),
    }
}

/// `initdb.c`: only `argv[1]` is checked for help and version.
fn fast_path(first: Option<&OsStr>) -> Option<Invocation> {
    match first?.to_str()? {
        "--help" | "-?" => Some(Invocation::PrintHelp),
        "--version" | "-V" => Some(Invocation::PrintVersion),
        _ => None,
    }
}

fn classify(options: Options) -> Invocation {
    if options.help || options.version {
        return Invocation::Hint;
    }
    match options.resolve_datadir() {
        Err(message) => Invocation::Fatal(message),
        Ok(_) => Invocation::Init(Box::new(options)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field accessors for the table-driven option tests.
    type ReadStr = for<'a> fn(&'a Options) -> Option<&'a str>;
    type ReadBool = fn(&Options) -> bool;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn init(list: &[&str]) -> Options {
        match plan(&args(list)) {
            Invocation::Init(options) => *options,
            other => panic!("{list:?} did not parse as Init: {other:?}"),
        }
    }

    #[test]
    fn help_and_version_only_as_the_first_argument() {
        assert_eq!(plan(&args(&["--help"])), Invocation::PrintHelp);
        assert_eq!(plan(&args(&["-?"])), Invocation::PrintHelp);
        assert_eq!(plan(&args(&["--version"])), Invocation::PrintVersion);
        assert_eq!(plan(&args(&["-V"])), Invocation::PrintVersion);
        assert_eq!(plan(&args(&["-D", "x", "--help"])), Invocation::Hint);
        assert_eq!(plan(&args(&["dd", "-V"])), Invocation::Hint);
    }

    #[test]
    fn every_long_option_is_recognized() {
        // long_options[] in initdb.c, minus help/version (fast path) and the
        // two compatibility aliases (own test).
        let valued: [(&str, ReadStr); 22] = [
            ("--pgdata", |o: &Options| o.pgdata.as_deref()),
            ("--encoding", |o: &Options| o.encoding.as_deref()),
            ("--locale", |o: &Options| o.locale.as_deref()),
            ("--lc-collate", |o: &Options| o.lc_collate.as_deref()),
            ("--lc-ctype", |o: &Options| o.lc_ctype.as_deref()),
            ("--lc-monetary", |o: &Options| o.lc_monetary.as_deref()),
            ("--lc-numeric", |o: &Options| o.lc_numeric.as_deref()),
            ("--lc-time", |o: &Options| o.lc_time.as_deref()),
            ("--lc-messages", |o: &Options| o.lc_messages.as_deref()),
            ("--text-search-config", |o: &Options| {
                o.text_search_config.as_deref()
            }),
            ("--auth", |o: &Options| o.auth.as_deref()),
            ("--auth-local", |o: &Options| o.auth_local.as_deref()),
            ("--auth-host", |o: &Options| o.auth_host.as_deref()),
            ("--pwfile", |o: &Options| o.pwfile.as_deref()),
            ("--username", |o: &Options| o.username.as_deref()),
            ("--waldir", |o: &Options| o.waldir.as_deref()),
            ("--wal-segsize", |o: &Options| o.wal_segsize.as_deref()),
            ("--locale-provider", |o: &Options| {
                o.locale_provider.as_deref()
            }),
            ("--builtin-locale", |o: &Options| {
                o.builtin_locale.as_deref()
            }),
            ("--icu-locale", |o: &Options| o.icu_locale.as_deref()),
            ("--icu-rules", |o: &Options| o.icu_rules.as_deref()),
            ("--sync-method", |o: &Options| o.sync_method.as_deref()),
        ];
        for (flag, read) in valued {
            assert_eq!(
                read(&init(&[flag, "v"])),
                Some("v"),
                "{flag} detached value"
            );
            assert_eq!(
                read(&init(&[&format!("{flag}=v")])),
                Some("v"),
                "{flag}=value"
            );
        }

        let switches: [(&str, ReadBool); 13] = [
            ("--no-locale", |o: &Options| o.no_locale),
            ("--pwprompt", |o: &Options| o.pwprompt),
            ("--debug", |o: &Options| o.debug),
            ("--show", |o: &Options| o.show),
            ("--no-clean", |o: &Options| o.no_clean),
            ("--no-sync", |o: &Options| o.no_sync),
            ("--no-instructions", |o: &Options| o.no_instructions),
            ("--sync-only", |o: &Options| o.sync_only),
            ("--data-checksums", |o: &Options| o.data_checksums),
            ("--allow-group-access", |o: &Options| o.allow_group_access),
            ("--discard-caches", |o: &Options| o.discard_caches),
            ("--no-data-checksums", |o: &Options| o.no_data_checksums),
            ("--no-sync-data-files", |o: &Options| o.no_sync_data_files),
        ];
        for (flag, read) in switches {
            assert!(read(&init(&[flag])), "{flag}");
        }
    }

    #[test]
    fn every_short_option_is_recognized() {
        // "A:c:dD:E:gkL:nNsST:U:WX:"
        let options = init(&[
            "-A", "trust", "-c", "a=1", "-d", "-D", "dd", "-E", "UTF8", "-g", "-k", "-L", "/in",
            "-n", "-N", "-s", "-S", "-T", "german", "-U", "alice", "-W", "-X", "/wal",
        ]);
        assert_eq!(options.auth.as_deref(), Some("trust"));
        assert_eq!(options.set, vec!["a=1"]);
        assert!(options.debug);
        assert_eq!(options.pgdata.as_deref(), Some("dd"));
        assert_eq!(options.encoding.as_deref(), Some("UTF8"));
        assert!(options.allow_group_access && options.data_checksums);
        assert_eq!(options.input_dir.as_deref(), Some("/in"));
        assert!(options.no_clean && options.no_sync && options.show && options.sync_only);
        assert_eq!(options.text_search_config.as_deref(), Some("german"));
        assert_eq!(options.username.as_deref(), Some("alice"));
        assert!(options.pwprompt);
        assert_eq!(options.waldir.as_deref(), Some("/wal"));
    }

    #[test]
    fn short_options_cluster_and_attach_values_like_getopt() {
        let options = init(&["-Nk", "-Ddd", "-nL/in"]);
        assert!(options.no_sync && options.data_checksums && options.no_clean);
        assert_eq!(options.pgdata.as_deref(), Some("dd"));
        assert_eq!(options.input_dir.as_deref(), Some("/in"));
    }

    #[test]
    fn compatibility_aliases_noclean_and_nosync() {
        let options = init(&["--noclean", "--nosync"]);
        assert!(options.no_clean && options.no_sync);
    }

    #[test]
    fn input_dir_has_no_long_spelling() {
        assert!(matches!(
            plan(&args(&["--input-dir", "/in"])),
            Invocation::Unparsable(_)
        ));
    }

    #[test]
    fn set_collects_every_occurrence_in_order() {
        let options = init(&[
            "-c",
            "work_mem=128",
            "--set",
            "Work_Mem=256",
            "--set=WORK_MEM=512",
        ]);
        assert_eq!(
            options.set,
            vec!["work_mem=128", "Work_Mem=256", "WORK_MEM=512"]
        );
    }

    #[test]
    fn repeated_single_value_options_last_wins() {
        assert_eq!(init(&["-D", "a", "-D", "b"]).pgdata.as_deref(), Some("b"));
    }

    #[test]
    fn positional_datadir_and_too_many_arguments() {
        assert_eq!(init(&["dd"]).resolve_datadir(), Ok(Some("dd")));
        assert_eq!(init(&["-D", "dd"]).resolve_datadir(), Ok(Some("dd")));
        assert_eq!(init(&[]).resolve_datadir(), Ok(None));
        assert_eq!(
            plan(&args(&["-D", "dd", "extra"])),
            Invocation::Fatal("too many command-line arguments (first is \"extra\")".to_owned())
        );
        // A second positional is caught by the parser instead (divergence: exit 2, usage-rs text).
        assert!(matches!(
            plan(&args(&["dd", "extra"])),
            Invocation::Unparsable(_)
        ));
    }

    #[test]
    fn unknown_option_is_an_error_with_a_message() {
        match plan(&args(&["--not-a-valid-option"])) {
            Invocation::Unparsable(text) => {
                assert!(text.contains("--not-a-valid-option"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(plan(&args(&["-x"])), Invocation::Unparsable(_)));
        assert!(matches!(plan(&args(&["-h"])), Invocation::Unparsable(_)));
    }

    #[test]
    fn abbreviation_is_rejected() {
        // docs/divergences.md: glibc getopt would accept the unique prefix --no-syn.
        assert!(matches!(
            plan(&args(&["--no-syn"])),
            Invocation::Unparsable(_)
        ));
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(matches!(plan(&args(&["--set"])), Invocation::Unparsable(_)));
        assert!(matches!(plan(&args(&["-D"])), Invocation::Unparsable(_)));
    }
}
