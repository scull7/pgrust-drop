//! Cluster creation from the embedded template (ADR-0002): what a command
//! line may ask of it, and every file it writes on top of the image.
//!
//! C `initdb` builds the catalogs with `postgres --boot` and then fills them
//! in single-user mode (`initialize_data_directory`, `initdb.c:3044`); pgrust
//! has no `--boot`, so `rinitdb` expands [`crate::image::TEMPLATE`] instead.
//! The image was minted `--no-locale --encoding=UTF8 -U postgres`
//! ([`crate::image::MINT_ARGS`]) and its catalogs say so, so this module is
//! first the list of what that fixes ([`check_template_can_make`]), then the
//! files a new cluster gets that the image does not carry
//! ([`generated_files`]): the configuration files `setup_config` renders
//! (`initdb.c:1283`), a `pg_control` of its own and the first WAL segment its
//! checkpoint lives in.
//!
//! Pure throughout; `crate::run` is where the files are written.

use crate::cli::Options;
use crate::conf::{self, AuthMethods, Settings};
use crate::control::{ChecksumSwitch, ControlFile, DataChecksums, NewCluster, for_new_cluster};
use crate::encoding::Encoding;
use crate::error::{InitdbError, LocaleProvider, Unsupported};
use crate::pg_config;
use crate::validate::CreatePlan;
use crate::wal;

/// The superuser the template was minted with (`-U postgres`).
pub const TEMPLATE_SUPERUSER: &str = "postgres";

/// `lc_ctype`'s text search configuration for locale C: `find_matching_ts_config`
/// over `tsearch_config_languages` (`initdb.c:883`).
pub const C_TEXT_SEARCH_CONFIG: &str = "english";

/// Pure: whether the template can make the cluster `options` ask for.
///
/// Runs after [`crate::validate`], so every upstream error is reported first,
/// and before the first directory is made, so a refusal leaves nothing
/// behind. What it refuses is everything the image fixed at mint time and
/// nothing else:
///
/// - `-E` naming any encoding but UTF8;
/// - `--locale-provider` other than `libc` (the template's databases are
///   `datlocprovider = 'c'`), and `--locale`, `--lc-collate`, `--lc-ctype`
///   naming any locale but `C` or `POSIX`. The other four categories are not
///   refused: C writes them to `postgresql.conf` only (`initdb.c:1315`-`:1325`)
///   and never into the catalogs, so [`settings`] writes them too;
/// - `--wal-segsize` other than 16;
/// - a superuser other than `postgres`, `-W` and `--pwfile`: renaming the
///   superuser and setting its password happen after expansion, in
///   single-user mode (NAT-383).
///
/// # Errors
/// [`InitdbError::NotSupportedYet`] for the first of those found.
pub fn check_template_can_make(options: &Options, plan: &CreatePlan) -> Result<(), InitdbError> {
    let refuse = |what: String, why: Unsupported| Err(InitdbError::NotSupportedYet { what, why });

    if let (Some(encoding), Some(name)) = (plan.encoding, options.encoding.as_deref())
        && encoding != Encoding::Utf8
    {
        return refuse(
            format!("encoding \"{name}\""),
            Unsupported::EncodingOrLocale,
        );
    }
    if plan.locale_provider != LocaleProvider::Libc {
        return refuse(
            format!("locale provider \"{}\"", plan.locale_provider),
            Unsupported::EncodingOrLocale,
        );
    }
    let locales = [
        ("--locale", &options.locale),
        ("--lc-collate", &options.lc_collate),
        ("--lc-ctype", &options.lc_ctype),
    ];
    for (option, value) in locales {
        if let Some(value) = value.as_deref()
            && !is_c_locale(value)
        {
            return refuse(
                format!("locale \"{value}\" ({option})"),
                Unsupported::EncodingOrLocale,
            );
        }
    }
    if let Some(size) = options.wal_segsize.as_deref()
        && size.parse::<u32>().ok() != Some(pg_config::DEFAULT_WAL_SEGMENT_SIZE_MB)
    {
        return refuse(format!("--wal-segsize={size}"), Unsupported::WalSegmentSize);
    }
    if let Some(name) = plan.username.as_deref()
        && name != TEMPLATE_SUPERUSER
    {
        return refuse(format!("superuser name \"{name}\""), Unsupported::Superuser);
    }
    if options.pwprompt {
        return refuse("--pwprompt".to_owned(), Unsupported::Password);
    }
    if options.pwfile.is_some() {
        return refuse("--pwfile".to_owned(), Unsupported::Password);
    }
    Ok(())
}

/// The spellings of the C locale `setlocale` accepts everywhere.
fn is_c_locale(name: &str) -> bool {
    matches!(name, "C" | "POSIX")
}

/// Pure: what `setup_config` (`initdb.c:1283`) writes the configuration files
/// from, for a cluster made from the template.
///
/// `lc_messages`, `lc_monetary`, `lc_numeric` and `lc_time` are the
/// command line's, as [`conf_locale`] resolves them; `date_order` stays
/// `DATEORDER_MDY`, because `locale_date_order` (`initdb.c:2143`) runs
/// `strftime` under `setlocale` and this crate reaches neither. The
/// environment's locale is not consulted (`docs/divergences.md`).
/// `max_connections`, `shared_buffers` and `dynamic_shared_memory_type` are
/// the first values `test_config_settings` (`initdb.c:1118`) and
/// `choose_dsm_implementation` (`:1076`) try, because probing them means
/// running the server — also a divergence.
#[must_use]
pub fn settings(
    options: &Options,
    plan: &CreatePlan,
    default_timezone: Option<String>,
) -> Settings {
    Settings {
        default_text_search_config: options
            .text_search_config
            .clone()
            .unwrap_or_else(|| C_TEXT_SEARCH_CONFIG.to_owned()),
        default_timezone,
        lc_messages: conf_locale(options.lc_messages.as_deref(), options),
        lc_monetary: conf_locale(options.lc_monetary.as_deref(), options),
        lc_numeric: conf_locale(options.lc_numeric.as_deref(), options),
        lc_time: conf_locale(options.lc_time.as_deref(), options),
        auth: AuthMethods::resolve(options),
        perm: plan.perm,
        gucs: plan.gucs.clone(),
        ..Settings::default()
    }
}

/// Pure: the value `setup_config` writes for one of the four locale
/// categories that only reach `postgresql.conf`.
///
/// `setlocales` (`initdb.c:2424`): the category's own `--lc-*`, else
/// `--locale` (`:2432`-`:2443`; `--no-locale` is `locale = "C"`, `:3338`),
/// then `check_locale_name` (`:2202`) canonicalizes it through `setlocale`.
/// That call is out of reach (`#![deny(unsafe_code)]`, no libc dependency),
/// so the name is written as given, except that `POSIX` is written `C`, as
/// musl's and glibc's `setlocale` return it. Nothing given — where C asks
/// the environment — is `C` (`docs/divergences.md`).
fn conf_locale(category: Option<&str>, options: &Options) -> String {
    let given = category
        .filter(|name| !name.is_empty())
        .or_else(|| options.locale.as_deref().filter(|name| !name.is_empty()));
    match given {
        None | Some("POSIX") => "C".to_owned(),
        Some(name) => name.to_owned(),
    }
}

/// Pure: `setup_text_search`'s warning (`initdb.c:2850`-`:2861`) for a `-T`
/// that is not the configuration `lc_ctype` suggests.
///
/// `lc_ctype` is C here ([`check_template_can_make`]), for which
/// `find_matching_ts_config` finds [`C_TEXT_SEARCH_CONFIG`], so the
/// `is unknown` branch (`:2854`) cannot be taken. Compared as C compares
/// it, with `strcmp` on the name as given.
#[must_use]
pub fn text_search_warning(options: &Options) -> Option<String> {
    let given = options.text_search_config.as_deref()?;
    (given != C_TEXT_SEARCH_CONFIG).then(|| {
        format!(
            "{}: warning: specified text search configuration \"{given}\" might not match \
             locale \"C\"",
            crate::help::PROGNAME
        )
    })
}

/// Pure: `-k` / `--no-data-checksums` (`initdb.c:3311`, `:3393`).
///
/// C's getopt applies whichever came last; usage-rs reports which were
/// given, not where, so when both are given `-k` wins here
/// (`docs/divergences.md`, like `--locale` over `--no-locale`).
#[must_use]
pub fn checksums(options: &Options) -> DataChecksums {
    let mut switches = Vec::with_capacity(2);
    if options.no_data_checksums {
        switches.push(ChecksumSwitch::NoDataChecksums);
    }
    if options.data_checksums {
        switches.push(ChecksumSwitch::DataChecksums);
    }
    DataChecksums::resolve(switches)
}

/// One file a new cluster gets on top of the image: a path relative to
/// PGDATA, and its bytes.
pub type GeneratedFile = (String, Vec<u8>);

/// Pure: every file [`crate::run`] writes into the expanded template, in the
/// order it writes them — the four configuration files, `global/pg_control`
/// and the first WAL segment.
///
/// `template` is [`crate::image::TEMPLATE_CONTROL`], parsed; `new` is what
/// this cluster gets that no other does.
#[must_use]
pub fn generated_files(
    settings: &Settings,
    template: &ControlFile,
    new: &NewCluster,
) -> Vec<GeneratedFile> {
    let control = for_new_cluster(template, new);
    let mut files: Vec<GeneratedFile> = conf::render_all(settings)
        .into_iter()
        .map(|(name, contents)| (name.to_owned(), contents.into_bytes()))
        .collect();
    files.push(("global/pg_control".to_owned(), control.to_bytes().to_vec()));
    files.push((
        format!("pg_wal/{}", wal::checkpoint_segment_file_name(&control)),
        wal::segment(&control),
    ));
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conf::DateOrder;
    use crate::control::{DbState, MOCK_AUTH_NONCE_LEN, PG_CONTROL_FILE_SIZE, SystemIdentifier};
    use crate::validate::{Environment, FsProbe, Plan, RealFs, validate};
    use std::ffi::OsString;

    /// Parse and validate `args` (after the program name) with `-D` added,
    /// as the effective user `postgres`.
    fn parsed(words: &[&str]) -> (Options, CreatePlan) {
        parsed_as("postgres", words)
    }

    /// [`parsed`], as the effective user `user`.
    fn parsed_as(user: &str, words: &[&str]) -> (Options, CreatePlan) {
        let mut argv: Vec<OsString> = words.iter().map(OsString::from).collect();
        let dir = std::env::temp_dir().join(format!(
            "rinitdb-cluster-unit-{}-does-not-exist",
            std::process::id()
        ));
        argv.push(dir.into());
        let crate::cli::Invocation::Init(options) = crate::cli::plan(&argv) else {
            panic!("{words:?} should be a cluster-creation command line");
        };
        let env = Environment {
            pgdata: None,
            effective_user: Some(user.to_owned()),
        };
        let fs: &dyn FsProbe = &RealFs;
        let Ok(Plan::Create(plan)) = validate(&options, &env, fs) else {
            panic!("{words:?} should validate");
        };
        (*options, plan)
    }

    fn verdict(args: &[&str]) -> Result<(), String> {
        let (options, plan) = parsed(args);
        check_template_can_make(&options, &plan).map_err(|err| err.render())
    }

    #[test]
    fn the_template_makes_utf8_clusters_with_locale_c() {
        for ok in [
            &[][..],
            &["--no-locale"],
            &["-E", "UTF8"],
            &["--encoding=utf-8", "--locale=C"],
            &["--lc-ctype=POSIX", "--lc-collate=C", "--lc-messages=C"],
            &[
                "--lc-messages=fr_FR.UTF-8",
                "--lc-monetary=de_DE",
                "--lc-numeric=xx_ZZ",
            ],
            &["--lc-time=de_DE", "--locale=C"],
            &["--locale-provider=libc"],
            &["--wal-segsize=16"],
            &["-U", "postgres"],
            &["-T", "simple", "-k", "-g"],
        ] {
            assert_eq!(verdict(ok), Ok(()), "{ok:?}");
        }
    }

    #[test]
    fn other_encodings_and_locales_are_refused_clearly() {
        assert_eq!(
            verdict(&["-E", "LATIN1"]),
            Err(
                "initdb: error: encoding \"LATIN1\" is not supported yet: the embedded template \
                 cluster has encoding \"UTF8\" and locale \"C\"\n\
                 initdb: hint: Clusters with another encoding, locale or WAL segment size need \
                 bootstrap mode, which this initdb does not have yet."
                    .to_owned()
            )
        );
        let first_line = |args: &[&str]| {
            verdict(args)
                .unwrap_err()
                .lines()
                .next()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            first_line(&["--locale=en_US.UTF-8"]),
            "initdb: error: locale \"en_US.UTF-8\" (--locale) is not supported yet: the embedded \
             template cluster has encoding \"UTF8\" and locale \"C\""
        );
        assert_eq!(
            first_line(&["--lc-ctype=de_DE", "--lc-time=de_DE"]),
            "initdb: error: locale \"de_DE\" (--lc-ctype) is not supported yet: the embedded \
             template cluster has encoding \"UTF8\" and locale \"C\""
        );
        assert_eq!(
            first_line(&["--locale-provider=builtin", "--builtin-locale=C"]),
            "initdb: error: locale provider \"builtin\" is not supported yet: the embedded \
             template cluster has encoding \"UTF8\" and locale \"C\""
        );
        assert_eq!(
            first_line(&["--wal-segsize=64"]),
            "initdb: error: --wal-segsize=64 is not supported yet: the embedded template \
             cluster has 16 MB WAL segments"
        );
    }

    #[test]
    fn the_superuser_is_the_templates_until_it_can_be_changed() {
        let (options, mut plan) = parsed(&[]);
        plan.username = Some("alice".to_owned());
        assert_eq!(
            check_template_can_make(&options, &plan)
                .unwrap_err()
                .render()
                .lines()
                .next(),
            Some(
                "initdb: error: superuser name \"alice\" is not supported yet: the embedded \
                 template cluster's superuser is \"postgres\" and renaming it is not implemented"
            )
        );
        // No -U: validate's `username = effective_user` (initdb.c:3475)
        // reaches the same refusal.
        let (options, plan) = parsed_as("alice", &[]);
        assert_eq!(options.username, None);
        assert_eq!(
            check_template_can_make(&options, &plan)
                .unwrap_err()
                .render()
                .lines()
                .next(),
            Some(
                "initdb: error: superuser name \"alice\" is not supported yet: the embedded \
                 template cluster's superuser is \"postgres\" and renaming it is not implemented"
            )
        );
        assert!(
            verdict(&["--pwfile=/nonexistent"])
                .unwrap_err()
                .starts_with("initdb: error: --pwfile is not supported yet")
        );
        assert!(
            verdict(&["-W"])
                .unwrap_err()
                .starts_with("initdb: error: --pwprompt is not supported yet")
        );
    }

    #[test]
    fn checksums_default_on_and_dash_k_wins_over_no_data_checksums() {
        assert_eq!(checksums(&parsed(&[]).0), DataChecksums::Enabled);
        assert_eq!(
            checksums(&parsed(&["--no-data-checksums"]).0),
            DataChecksums::Disabled
        );
        assert_eq!(checksums(&parsed(&["-k"]).0), DataChecksums::Enabled);
        assert_eq!(
            checksums(&parsed(&["-k", "--no-data-checksums"]).0),
            DataChecksums::Enabled
        );
    }

    #[test]
    fn settings_carry_the_command_line_and_locale_c() {
        let (options, plan) = parsed(&["-g", "-A", "md5", "-c", "work_mem=8MB"]);
        let settings = settings(&options, &plan, Some("UTC".to_owned()));
        assert_eq!(settings.default_text_search_config, "english");
        assert_eq!(settings.default_timezone.as_deref(), Some("UTC"));
        assert_eq!(settings.auth.local, "md5");
        assert_eq!(settings.perm, plan.perm);
        assert_eq!(settings.gucs, [("work_mem".to_owned(), "8MB".to_owned())]);
        assert_eq!(settings.lc_messages, "C");
        // Not probed (docs/divergences.md): the first values
        // test_config_settings and choose_dsm_implementation try.
        assert_eq!(settings.max_connections, 100);
        assert_eq!(settings.shared_buffers_blocks, 16384);
        assert_eq!(settings.dynamic_shared_memory_type, "posix");

        let (options, plan) = parsed(&["-T", "simple"]);
        assert_eq!(
            super::settings(&options, &plan, None).default_text_search_config,
            "simple"
        );
    }

    #[test]
    fn the_four_conf_only_locales_are_written_as_given() {
        // initdb.c:1315-:1325: lc_messages, lc_monetary, lc_numeric and
        // lc_time reach postgresql.conf and nothing else, so the template
        // takes any of them. The reference initdb on musl writes the same
        // four lines for this command line.
        let (options, plan) = parsed(&[
            "--no-locale",
            "--lc-messages=fr_FR.UTF-8",
            "--lc-numeric=xx_ZZ",
            "--lc-time=de_DE",
        ]);
        let settings = settings(&options, &plan, None);
        assert_eq!(settings.lc_messages, "fr_FR.UTF-8");
        assert_eq!(settings.lc_monetary, "C");
        assert_eq!(settings.lc_numeric, "xx_ZZ");
        assert_eq!(settings.lc_time, "de_DE");
        // locale_date_order is not run (docs/divergences.md).
        assert_eq!(settings.date_order, DateOrder::Mdy);
        let conf = &conf::render_all(&settings)[0].1;
        for line in [
            "lc_messages = 'fr_FR.UTF-8'\t\t# locale for system error message",
            "lc_monetary = C\t\t\t\t# locale for monetary formatting",
            "lc_numeric = 'xx_ZZ'\t\t\t# locale for number formatting",
            "lc_time = 'de_DE'\t\t\t# locale for time formatting",
            "datestyle = 'iso, mdy'",
        ] {
            assert!(conf.lines().any(|l| l == line), "{line:?}");
        }

        // --locale fills the ones not given (initdb.c:2432-:2443), and
        // setlocale returns POSIX as C.
        let (options, plan) = parsed(&["--locale=POSIX", "--lc-time=de_DE"]);
        let settings = super::settings(&options, &plan, None);
        assert_eq!(
            [
                settings.lc_messages.as_str(),
                &settings.lc_monetary,
                &settings.lc_numeric,
                &settings.lc_time,
            ],
            ["C", "C", "C", "de_DE"]
        );
    }

    #[test]
    fn a_text_search_config_other_than_english_warns_as_c_does() {
        // initdb.c:2859; the reference initdb writes exactly this line for
        // `--no-locale -E UTF8 -T simple`.
        assert_eq!(
            text_search_warning(&parsed(&["-T", "simple"]).0).as_deref(),
            Some(
                "initdb: warning: specified text search configuration \"simple\" might not \
                 match locale \"C\""
            )
        );
        assert_eq!(text_search_warning(&parsed(&["-T", "english"]).0), None);
        assert_eq!(text_search_warning(&parsed(&[]).0), None);
    }

    #[test]
    fn the_generated_files_are_the_config_files_the_control_file_and_one_segment() {
        let mut template = ControlFile::parse(&[0u8; PG_CONTROL_FILE_SIZE]).unwrap();
        template.xlog_seg_size = 16 * 1024 * 1024;
        template.check_point_copy.redo = 0x0175_B1F0;
        template.check_point_copy.this_time_line_id = 1;
        template.state = DbState::Shutdowned;
        let new = NewCluster {
            system_identifier: SystemIdentifier::from_raw(7),
            checksums: DataChecksums::Enabled,
            mock_authentication_nonce: [1; MOCK_AUTH_NONCE_LEN],
            now: 1,
        };
        let files = generated_files(&Settings::default(), &template, &new);
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "postgresql.conf",
                "postgresql.auto.conf",
                "pg_hba.conf",
                "pg_ident.conf",
                "global/pg_control",
                "pg_wal/000000010000000000000002",
            ]
        );
        let control = ControlFile::parse(&files[4].1).unwrap();
        assert!(control.crc_is_valid());
        assert_eq!(
            files[4].1,
            for_new_cluster(&template, &new).to_bytes().to_vec()
        );
        assert_eq!(files[5].1, wal::segment(&control));
    }
}
