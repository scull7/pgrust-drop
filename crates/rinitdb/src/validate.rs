//! Pre-flight validation: everything C initdb decides before it creates a
//! single file, as one pure calculation.
//!
//! [`validate`] walks the checks of `initdb.c`'s `main()` in upstream order —
//! the `getopt_long` switch arms that can fail, the post-loop cross-option
//! rules, `setup_pgdata`, the superuser-name rule, `setlocales` /
//! `setup_locale_encoding`, then `create_data_directory` and
//! `create_xlog_or_symlink` — and turns the first failure into the
//! [`InitdbError`] whose rendering is byte-identical to C's stderr.
//!
//! Data / Calculations / Actions:
//!
//! - [`Environment`] and [`DirState`] are data: what the process was told and
//!   what the filesystem looks like.
//! - [`validate`] is the calculation. It reaches the outside world only
//!   through [`FsProbe`], so every case below is unit-tested against a map of
//!   fake directories, with no temporary files at all.
//! - [`RealFs`] is the single action, a port of `pg_check_dir`
//!   (`src/port/pgcheckdir.c:32`).
//!
//! Order is the whole point: several of these conditions can hold at once and
//! C reports exactly one of them, so a check in the wrong place is a wrong
//! error message even though every individual message is right.

use std::path::{Path, PathBuf};

use crate::cli::Options;
use crate::encoding::{self, Encoding};
use crate::error::{DirRole, InitdbError, LocaleProvider, NotEmpty};
use crate::file_perm::DataDirPerm;

/// What `pg_check_dir` (`src/port/pgcheckdir.c:32`) reports about a directory.
///
/// The upstream return codes are 0, 1, 2, 3, 4 and -1; the two that carry an
/// `errno` carry the text `%m` would print with them, because the caller
/// interpolates it (`initdb.c:2941`, `:3011`, `:3445`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirState {
    /// 0: `opendir` failed with `ENOENT`.
    Nonexistent { reason: String },
    /// 1: exists and holds nothing.
    Empty,
    /// 2: exists and holds only dot-prefixed entries.
    DotFilesOnly,
    /// 3: exists and holds a `lost+found` directory (and nothing else).
    MountPoint,
    /// 4: exists and holds ordinary entries.
    NotEmpty,
    /// -1: the directory could not be read.
    Inaccessible { reason: String },
}

/// The one thing validation asks of the filesystem.
///
/// A trait so the whole of [`validate`] is testable from a table of directory
/// states; `RealFs` is the only implementor that touches a disk.
pub trait FsProbe {
    /// `pg_check_dir(path)`.
    fn check_dir(&self, path: &Path) -> DirState;
}

/// The real filesystem: a port of `pg_check_dir`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealFs;

impl FsProbe for RealFs {
    fn check_dir(&self, path: &Path) -> DirState {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            // pgcheckdir.c:44 — ENOENT is "not there", anything else is trouble.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return DirState::Nonexistent {
                    reason: strerror(&err),
                };
            }
            Err(err) => {
                return DirState::Inaccessible {
                    reason: strerror(&err),
                };
            }
        };

        let mut dot_found = false;
        let mut mount_found = false;
        for entry in entries {
            // read_dir already skips "." and ".." (pgcheckdir.c:48).
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    return DirState::Inaccessible {
                        reason: strerror(&err),
                    };
                }
            };
            let name = entry.file_name();
            let bytes = name.as_encoded_bytes();
            if bytes.first() == Some(&b'.') {
                dot_found = true;
            } else if bytes == b"lost+found" {
                mount_found = true;
            } else {
                // pgcheckdir.c:68 — result = 4 and break.
                return DirState::NotEmpty;
            }
        }

        // pgcheckdir.c:84 — a mount point outranks the dot-file report.
        if mount_found {
            DirState::MountPoint
        } else if dot_found {
            DirState::DotFilesOnly
        } else {
            DirState::Empty
        }
    }
}

/// What `%m` expands to for this error: `strerror(errno)` and nothing else.
///
/// `std::io::Error`'s own `Display` appends ` (os error N)` to the same
/// `strerror` text; C's `%m` does not, so the suffix is removed rather than a
/// second error table being invented.
#[must_use]
pub fn strerror(err: &std::io::Error) -> String {
    let text = err.to_string();
    match err.raw_os_error() {
        Some(code) => text
            .strip_suffix(&format!(" (os error {code})"))
            .unwrap_or(&text)
            .to_owned(),
        None => text,
    }
}

/// What the process was told, outside its arguments.
///
/// `PGDATA` is `setup_pgdata`'s fallback (`initdb.c:2615`). `effective_user`
/// stands in for `get_id()` (`initdb.c:817`); see the note on [`validate`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `getenv("PGDATA")`, `None` when unset or empty (`initdb.c:2616`).
    pub pgdata: Option<String>,
    /// The name `get_id()` would report, when it can be determined.
    pub effective_user: Option<String>,
}

impl Environment {
    /// Action: read the environment this process was started with.
    ///
    /// `get_id()` calls `geteuid()` and `getpwuid()`. Neither is reachable
    /// from the standard library, and this crate is `#![deny(unsafe_code)]`
    /// with no approved libc dependency, so the effective user is taken from
    /// `USER`/`LOGNAME` — see `docs/divergences.md`.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            pgdata: std::env::var("PGDATA").ok().filter(|v| !v.is_empty()),
            effective_user: std::env::var("USER")
                .ok()
                .or_else(|| std::env::var("LOGNAME").ok())
                .filter(|v| !v.is_empty()),
        }
    }
}

/// What `create_data_directory` / `create_xlog_or_symlink` will do with a
/// directory that passed validation (`initdb.c:2896` and `:2909`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirAction {
    /// `pg_check_dir` == 0: make it.
    Create,
    /// `pg_check_dir` == 1: it is there and empty, fix its permissions.
    ReuseEmpty,
}

/// `--sync-only`: fsync an existing cluster and exit 0 (`initdb.c:3438`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPlan {
    pub pgdata: PathBuf,
    /// `--no-sync-data-files` (`initdb.c:3348`).
    pub sync_data_files: bool,
}

/// A validated request to create a cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatePlan {
    pub pgdata: PathBuf,
    pub pgdata_action: DirAction,
    /// `--waldir`, with what will be done to it.
    pub waldir: Option<(PathBuf, DirAction)>,
    /// What `-g` left `pg_dir_create_mode` and friends at (`initdb.c:3360`).
    pub perm: DataDirPerm,
    pub locale_provider: LocaleProvider,
    /// The canonical `datlocale` (`initdb.c:2483`), `None` for `libc`.
    pub datlocale: Option<String>,
    /// `None` when `-E` was not given: C derives it from `LC_CTYPE`, which
    /// needs `setlocale`/`nl_langinfo` and lands with the locale work.
    pub encoding: Option<Encoding>,
    /// The superuser name, `None` when neither `--username` nor the
    /// environment settled it.
    pub username: Option<String>,
    /// `-c NAME=VALUE`, split and kept in command-line order (`initdb.c:3277`).
    pub gucs: Vec<(String, String)>,
}

/// What one validated command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Sync(SyncPlan),
    Create(CreatePlan),
}

/// Validate a parsed command line against the environment and the filesystem.
///
/// # Errors
/// The first `pg_fatal` / `pg_log_error` site `initdb.c` would reach, as
/// [`InitdbError`]; `InitdbError::render` is the stderr C writes.
pub fn validate(
    options: &Options,
    env: &Environment,
    fs: &dyn FsProbe,
) -> Result<Plan, InitdbError> {
    // --- the getopt_long switch arms that can fail, in command-line order ---
    let gucs = split_gucs(&options.set)?;
    let locale_provider = parse_locale_provider(options.locale_provider.as_deref())?;

    // --- immediately after the loop (initdb.c:3416) ---
    let datadir = options.resolve_datadir()?;
    check_provider_specific_options(options, locale_provider)?;

    // --- initdb.c:3438: --sync-only returns before every other check ---
    if options.sync_only {
        let pgdata = require_datadir(datadir, env)?;
        // initdb.c:3444 — "must check that directory is readable"; pg_check_dir
        // <= 0 covers both "not there" and "could not read it".
        match fs.check_dir(&pgdata) {
            DirState::Nonexistent { reason } | DirState::Inaccessible { reason } => {
                return Err(InitdbError::CouldNotAccessDirectory {
                    path: display(&pgdata),
                    reason,
                });
            }
            _ => {}
        }
        return Ok(Plan::Sync(SyncPlan {
            pgdata,
            sync_data_files: !options.no_sync_data_files,
        }));
    }

    // initdb.c:3454, before setup_pgdata.
    if options.pwprompt && options.pwfile.is_some() {
        return Err(InitdbError::PasswordPromptAndFile);
    }

    let pgdata = require_datadir(datadir, env)?;

    // initdb.c:3477 — after setup_bin_paths and get_id().
    let username = options
        .username
        .clone()
        .or_else(|| env.effective_user.clone());
    if let Some(name) = &username
        && name.starts_with("pg_")
    {
        return Err(InitdbError::SuperuserNameDisallowed { name: name.clone() });
    }

    // initdb.c:2420 (setlocales) and :2687 (setup_locale_encoding).
    let datlocale = resolve_datlocale(options, locale_provider)?;
    let encoding = resolve_encoding(options.encoding.as_deref())?;
    check_builtin_locale_encoding(locale_provider, datlocale.as_deref(), encoding)?;

    // initdb.c:2890 — the data directory is judged before the WAL directory,
    // so a non-empty PGDATA outranks a bad --waldir.
    let pgdata_action = classify_dir(fs.check_dir(&pgdata), &pgdata, DirRole::Data)?;

    // initdb.c:2948.
    let waldir = match &options.waldir {
        None => None,
        Some(raw) => {
            let path = PathBuf::from(raw);
            if !path.is_absolute() {
                return Err(InitdbError::WalDirectoryNotAbsolute);
            }
            let action = classify_dir(fs.check_dir(&path), &path, DirRole::Wal)?;
            Some((path, action))
        }
    };

    Ok(Plan::Create(CreatePlan {
        pgdata,
        pgdata_action,
        waldir,
        perm: DataDirPerm::for_allow_group_access(options.allow_group_access),
        locale_provider,
        datlocale,
        encoding,
        username,
        gucs,
    }))
}

/// `initdb.c:3268`: every `-c` argument must contain an `=`.
fn split_gucs(set: &[String]) -> Result<Vec<(String, String)>, InitdbError> {
    set.iter()
        .map(|item| match item.split_once('=') {
            Some((name, value)) => Ok((name.to_owned(), value.to_owned())),
            None => Err(InitdbError::SetRequiresValue { name: item.clone() }),
        })
        .collect()
}

/// `initdb.c:3367`: the `--locale-provider` switch arm.
fn parse_locale_provider(name: Option<&str>) -> Result<LocaleProvider, InitdbError> {
    match name {
        // No --locale-provider leaves the initdb.c:139 default in place.
        None | Some("libc") => Ok(LocaleProvider::Libc),
        Some("builtin") => Ok(LocaleProvider::Builtin),
        Some("icu") => Ok(LocaleProvider::Icu),
        Some(other) => Err(InitdbError::UnrecognizedLocaleProvider {
            name: other.to_owned(),
        }),
    }
}

/// `initdb.c:3424`, `:3428`, `:3432`, in that order.
fn check_provider_specific_options(
    options: &Options,
    provider: LocaleProvider,
) -> Result<(), InitdbError> {
    let rules = [
        (
            "--builtin-locale",
            options.builtin_locale.is_some(),
            LocaleProvider::Builtin,
        ),
        (
            "--icu-locale",
            options.icu_locale.is_some(),
            LocaleProvider::Icu,
        ),
        (
            "--icu-rules",
            options.icu_rules.is_some(),
            LocaleProvider::Icu,
        ),
    ];
    for (option, given, required) in rules {
        if given && provider != required {
            return Err(InitdbError::OptionNeedsProvider {
                option,
                provider: required,
            });
        }
    }
    Ok(())
}

/// `setup_pgdata` (`initdb.c:2612`): `-D`, else the positional argument, else
/// `PGDATA`, else a fatal.
fn require_datadir(from_args: Option<&str>, env: &Environment) -> Result<PathBuf, InitdbError> {
    from_args
        .map(ToOwned::to_owned)
        .or_else(|| env.pgdata.clone())
        .map(PathBuf::from)
        .ok_or(InitdbError::NoDataDirectory)
}

/// `setlocales` (`initdb.c:2420`) and the builtin canonicalization at `:2474`.
fn resolve_datlocale(
    options: &Options,
    provider: LocaleProvider,
) -> Result<Option<String>, InitdbError> {
    // --builtin-locale and --icu-locale both write `datlocale` (initdb.c:3378
    // and :3382); check_provider_specific_options has already ruled out the
    // combination where both could be set for the chosen provider.
    let explicit = options
        .builtin_locale
        .clone()
        .or_else(|| options.icu_locale.clone());
    // --no-locale is `locale = "C"` (initdb.c:3336).
    let locale = options
        .locale
        .clone()
        .or_else(|| options.no_locale.then(|| "C".to_owned()));

    let datlocale = match (explicit, provider) {
        (Some(value), _) => Some(value),
        // initdb.c:2444: only a provider other than libc inherits --locale.
        (None, LocaleProvider::Libc) => None,
        (None, _) => locale,
    };

    // libc is the only provider that may leave datlocale unset (initdb.c:2470).
    if provider == LocaleProvider::Libc {
        return Ok(datlocale);
    }
    // initdb.c:2471.
    let Some(name) = datlocale else {
        return Err(InitdbError::LocaleRequiredForProvider { provider });
    };
    if provider == LocaleProvider::Icu {
        // initdb.c:2490 calls icu_language_tag(), which is the #else branch at
        // :2362 in a build without ICU. rinitdb has no ICU dependency.
        return Err(InitdbError::IcuNotSupported);
    }
    // initdb.c:2474: C, C.UTF-8 (either spelling) and PG_UNICODE_FAST only.
    match name.as_str() {
        "C" => Ok(Some("C".to_owned())),
        "C.UTF-8" | "C.UTF8" => Ok(Some("C.UTF-8".to_owned())),
        "PG_UNICODE_FAST" => Ok(Some("PG_UNICODE_FAST".to_owned())),
        _ => Err(InitdbError::InvalidBuiltinLocale { name }),
    }
}

/// `get_encoding_id` (`initdb.c:845`), or `None` when `-E` was not given.
fn resolve_encoding(name: Option<&str>) -> Result<Option<Encoding>, InitdbError> {
    match name {
        None => Ok(None),
        Some(name) => encoding::valid_server_encoding(name)
            .map(Some)
            .ok_or_else(|| InitdbError::InvalidServerEncoding {
                name: name.to_owned(),
            }),
    }
}

/// `initdb.c:2777`: the builtin provider's UTF-8-only locales.
fn check_builtin_locale_encoding(
    provider: LocaleProvider,
    datlocale: Option<&str>,
    encoding: Option<Encoding>,
) -> Result<(), InitdbError> {
    if provider != LocaleProvider::Builtin {
        return Ok(());
    }
    let (Some(locale), Some(encoding)) = (datlocale, encoding) else {
        return Ok(());
    };
    if matches!(locale, "C.UTF-8" | "PG_UNICODE_FAST") && encoding != Encoding::Utf8 {
        return Err(InitdbError::BuiltinLocaleRequiresEncoding {
            locale: locale.to_owned(),
            encoding: "UTF-8",
        });
    }
    Ok(())
}

/// The shared `switch (pg_check_dir(...))` of `create_data_directory`
/// (`initdb.c:2894`) and `create_xlog_or_symlink` (`initdb.c:2966`).
fn classify_dir(state: DirState, path: &Path, role: DirRole) -> Result<DirAction, InitdbError> {
    match state {
        DirState::Nonexistent { .. } => Ok(DirAction::Create),
        DirState::Empty => Ok(DirAction::ReuseEmpty),
        DirState::DotFilesOnly => Err(InitdbError::DirectoryNotEmpty {
            path: display(path),
            role,
            why: NotEmpty::DotFilesOnly,
        }),
        DirState::MountPoint => Err(InitdbError::DirectoryNotEmpty {
            path: display(path),
            role,
            why: NotEmpty::LostAndFound,
        }),
        DirState::NotEmpty => Err(InitdbError::DirectoryNotEmpty {
            path: display(path),
            role,
            why: NotEmpty::Entries,
        }),
        DirState::Inaccessible { reason } => Err(InitdbError::CouldNotAccessDirectory {
            path: display(path),
            reason,
        }),
    }
}

/// The spelling a path gets inside an error message: C interpolates the bytes
/// it was handed, so a path that is not UTF-8 is shown lossily rather than
/// hidden.
fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::cli::{Invocation, plan};

    /// What `%m` prints for the `ENOENT` `pg_check_dir` leaves behind.
    const ENOENT: &str = "No such file or directory";

    /// A directory table: anything not listed is simply not there, which is
    /// what `pg_check_dir` reports for a path that does not exist.
    struct FakeFs(Vec<(PathBuf, DirState)>);

    impl FakeFs {
        fn empty() -> Self {
            Self(Vec::new())
        }

        fn with(path: &str, state: DirState) -> Self {
            Self(vec![(PathBuf::from(path), state)])
        }

        fn and(mut self, path: &str, state: DirState) -> Self {
            self.0.push((PathBuf::from(path), state));
            self
        }
    }

    impl FsProbe for FakeFs {
        fn check_dir(&self, path: &Path) -> DirState {
            self.0.iter().find(|(known, _)| known == path).map_or_else(
                || DirState::Nonexistent {
                    reason: ENOENT.to_owned(),
                },
                |(_, state)| state.clone(),
            )
        }
    }

    fn env() -> Environment {
        Environment {
            pgdata: None,
            effective_user: Some("alice".to_owned()),
        }
    }

    fn options(list: &[&str]) -> Options {
        let args: Vec<OsString> = list.iter().map(OsString::from).collect();
        match plan(&args) {
            Invocation::Init(options) => *options,
            other => panic!("{list:?} did not parse as Init: {other:?}"),
        }
    }

    fn check(list: &[&str], fs: &dyn FsProbe) -> Result<Plan, InitdbError> {
        validate(&options(list), &env(), fs)
    }

    fn failure(list: &[&str], fs: &dyn FsProbe) -> InitdbError {
        match check(list, fs) {
            Err(err) => err,
            Ok(plan) => panic!("{list:?} validated: {plan:?}"),
        }
    }

    fn created(list: &[&str], fs: &dyn FsProbe) -> CreatePlan {
        match check(list, fs) {
            Ok(Plan::Create(plan)) => plan,
            other => panic!("{list:?}: {other:?}"),
        }
    }

    // --- the switch arms of the getopt loop -------------------------------

    #[test]
    fn a_set_argument_without_an_equals_sign_is_rejected() {
        // initdb.c:3268.
        assert_eq!(
            failure(&["-c", "work_mem", "dd"], &FakeFs::empty()),
            InitdbError::SetRequiresValue {
                name: "work_mem".to_owned()
            }
        );
    }

    #[test]
    fn a_set_value_may_itself_contain_an_equals_sign() {
        // *equals++ = '\0' splits at the first '=' only (initdb.c:3277).
        let plan = created(&["-c", "search_path=a=b", "dd"], &FakeFs::empty());
        assert_eq!(
            plan.gucs,
            vec![("search_path".to_owned(), "a=b".to_owned())]
        );
    }

    #[test]
    fn an_unrecognized_locale_provider_is_rejected() {
        // initdb.c:3375. Upstream: 'fails for invalid locale provider'.
        assert_eq!(
            failure(&["--locale-provider", "xyz", "dd"], &FakeFs::empty()),
            InitdbError::UnrecognizedLocaleProvider {
                name: "xyz".to_owned()
            }
        );
    }

    #[test]
    fn the_getopt_arms_are_checked_before_too_many_arguments() {
        // C reaches initdb.c:3375 inside the loop, :3418 only after it.
        assert_eq!(
            failure(
                &["--locale-provider", "xyz", "-D", "dd", "extra"],
                &FakeFs::empty()
            ),
            InitdbError::UnrecognizedLocaleProvider {
                name: "xyz".to_owned()
            }
        );
    }

    // --- the cross-option rules right after the loop ----------------------

    #[test]
    fn too_many_arguments_is_checked_before_the_provider_rules() {
        // initdb.c:3418 precedes :3424.
        assert_eq!(
            failure(
                &["--icu-locale", "en", "-D", "dd", "extra"],
                &FakeFs::empty()
            ),
            InitdbError::TooManyArguments {
                first: "extra".to_owned()
            }
        );
    }

    #[test]
    fn each_provider_specific_option_names_the_provider_it_needs() {
        // initdb.c:3424, :3428, :3432. Upstream: 'fails for locale provider
        // builtin with ICU locale', '… with ICU rules', 'fails for invalid
        // option combination'.
        let cases = [
            (
                vec!["--locale-provider", "builtin", "--icu-locale", "en", "dd"],
                "--icu-locale",
                LocaleProvider::Icu,
            ),
            (
                vec!["--locale-provider", "builtin", "--icu-rules", "\"\"", "dd"],
                "--icu-rules",
                LocaleProvider::Icu,
            ),
            (
                vec!["--locale-provider", "libc", "--icu-locale", "en", "dd"],
                "--icu-locale",
                LocaleProvider::Icu,
            ),
            (
                vec!["--locale-provider", "icu", "--builtin-locale", "C", "dd"],
                "--builtin-locale",
                LocaleProvider::Builtin,
            ),
        ];
        for (args, option, provider) in cases {
            assert_eq!(
                failure(&args, &FakeFs::empty()),
                InitdbError::OptionNeedsProvider { option, provider },
                "{args:?}"
            );
        }
    }

    #[test]
    fn builtin_locale_is_accepted_under_the_builtin_provider() {
        let plan = created(
            &[
                "--locale-provider",
                "builtin",
                "--builtin-locale",
                "C",
                "dd",
            ],
            &FakeFs::empty(),
        );
        assert_eq!(plan.locale_provider, LocaleProvider::Builtin);
        assert_eq!(plan.datlocale.as_deref(), Some("C"));
    }

    // --- --sync-only ------------------------------------------------------

    #[test]
    fn sync_only_on_a_missing_directory_reports_the_errno() {
        // initdb.c:3444. Upstream: 'sync missing data directory'.
        assert_eq!(
            failure(&["--sync-only", "/tmp/nonexistent"], &FakeFs::empty()),
            InitdbError::CouldNotAccessDirectory {
                path: "/tmp/nonexistent".to_owned(),
                reason: ENOENT.to_owned(),
            }
        );
    }

    #[test]
    fn sync_only_on_an_unreadable_directory_reports_its_errno() {
        assert_eq!(
            failure(
                &["--sync-only", "/tmp/locked"],
                &FakeFs::with(
                    "/tmp/locked",
                    DirState::Inaccessible {
                        reason: "Permission denied".to_owned()
                    }
                )
            ),
            InitdbError::CouldNotAccessDirectory {
                path: "/tmp/locked".to_owned(),
                reason: "Permission denied".to_owned(),
            }
        );
    }

    #[test]
    fn sync_only_accepts_a_populated_directory_and_skips_every_later_check() {
        // initdb.c:3438 returns before the superuser-name and locale checks,
        // so --sync-only --username pg_x is not an error.
        let fs = FakeFs::with("/tmp/data", DirState::NotEmpty);
        let plan = check(&["--sync-only", "--username", "pg_x", "/tmp/data"], &fs);
        assert_eq!(
            plan,
            Ok(Plan::Sync(SyncPlan {
                pgdata: PathBuf::from("/tmp/data"),
                sync_data_files: true,
            }))
        );
    }

    #[test]
    fn no_sync_data_files_turns_off_data_file_syncing() {
        let fs = FakeFs::with("/tmp/data", DirState::NotEmpty);
        let plan = check(&["--sync-only", "--no-sync-data-files", "/tmp/data"], &fs);
        assert_eq!(
            plan,
            Ok(Plan::Sync(SyncPlan {
                pgdata: PathBuf::from("/tmp/data"),
                sync_data_files: false,
            }))
        );
    }

    // --- the data directory -----------------------------------------------

    #[test]
    fn a_missing_data_directory_is_only_fatal_without_pgdata() {
        assert_eq!(
            validate(&options(&[]), &env(), &FakeFs::empty()),
            Err(InitdbError::NoDataDirectory)
        );
        let from_env = Environment {
            pgdata: Some("/tmp/from-env".to_owned()),
            ..env()
        };
        let plan = validate(&options(&[]), &from_env, &FakeFs::empty());
        assert!(matches!(plan, Ok(Plan::Create(_))), "{plan:?}");
    }

    #[test]
    fn a_password_prompt_with_a_password_file_beats_the_missing_data_directory() {
        // initdb.c:3454 runs before setup_pgdata() at :3468.
        assert_eq!(
            failure(&["--pwprompt", "--pwfile", "/tmp/pw"], &FakeFs::empty()),
            InitdbError::PasswordPromptAndFile
        );
    }

    #[test]
    fn existing_data_directory_reports_every_flavour_of_not_empty() {
        // initdb.c:2929. Upstream: 'existing data directory'.
        let cases = [
            (DirState::NotEmpty, NotEmpty::Entries),
            (DirState::MountPoint, NotEmpty::LostAndFound),
            (DirState::DotFilesOnly, NotEmpty::DotFilesOnly),
        ];
        for (state, why) in cases {
            assert_eq!(
                failure(&["/tmp/data"], &FakeFs::with("/tmp/data", state.clone())),
                InitdbError::DirectoryNotEmpty {
                    path: "/tmp/data".to_owned(),
                    role: DirRole::Data,
                    why,
                },
                "{state:?}"
            );
        }
    }

    #[test]
    fn an_empty_data_directory_is_reused_and_a_missing_one_is_created() {
        assert_eq!(
            created(&["/tmp/data"], &FakeFs::with("/tmp/data", DirState::Empty)).pgdata_action,
            DirAction::ReuseEmpty
        );
        assert_eq!(
            created(&["/tmp/data"], &FakeFs::empty()).pgdata_action,
            DirAction::Create
        );
    }

    // --- the superuser name -----------------------------------------------

    #[test]
    fn a_superuser_name_beginning_with_pg_is_disallowed() {
        // initdb.c:3479. Upstream: 'role names cannot begin with "pg_"'.
        assert_eq!(
            failure(&["--username", "pg_test", "/tmp/data"], &FakeFs::empty()),
            InitdbError::SuperuserNameDisallowed {
                name: "pg_test".to_owned()
            }
        );
    }

    #[test]
    fn the_effective_user_is_the_default_superuser_name_and_is_checked_too() {
        // initdb.c:3477: `if (!username) username = effective_user;`.
        let banned = Environment {
            effective_user: Some("pg_robot".to_owned()),
            ..env()
        };
        assert_eq!(
            validate(&options(&["/tmp/data"]), &banned, &FakeFs::empty()),
            Err(InitdbError::SuperuserNameDisallowed {
                name: "pg_robot".to_owned()
            })
        );
        assert_eq!(
            created(&["/tmp/data"], &FakeFs::empty())
                .username
                .as_deref(),
            Some("alice")
        );
    }

    #[test]
    fn the_superuser_name_is_checked_before_the_locale_provider() {
        // initdb.c:3479 precedes setup_locale_encoding() at :3491.
        assert_eq!(
            failure(
                &[
                    "--username",
                    "pg_test",
                    "--locale-provider",
                    "builtin",
                    "/tmp/data"
                ],
                &FakeFs::empty()
            ),
            InitdbError::SuperuserNameDisallowed {
                name: "pg_test".to_owned()
            }
        );
    }

    // --- locales and encodings --------------------------------------------

    #[test]
    fn a_non_libc_provider_without_a_locale_is_fatal() {
        // initdb.c:2471. Upstream: 'locale provider builtin fails without
        // --locale' and 'locale provider ICU fails since no ICU support'.
        for provider in [LocaleProvider::Builtin, LocaleProvider::Icu] {
            assert_eq!(
                failure(
                    &[
                        "--no-sync",
                        "--locale-provider",
                        provider.name(),
                        "/tmp/data"
                    ],
                    &FakeFs::empty()
                ),
                InitdbError::LocaleRequiredForProvider { provider },
                "{provider}"
            );
        }
    }

    #[test]
    fn a_non_libc_provider_inherits_locale_but_libc_does_not() {
        // initdb.c:2444.
        let plan = created(
            &["--locale-provider", "builtin", "--locale", "C", "/tmp/data"],
            &FakeFs::empty(),
        );
        assert_eq!(plan.datlocale.as_deref(), Some("C"));
        let libc = created(&["--locale", "C", "/tmp/data"], &FakeFs::empty());
        assert_eq!(libc.datlocale, None);
    }

    #[test]
    fn no_locale_stands_in_for_locale_c() {
        // initdb.c:3336 sets locale = "C" for --no-locale.
        let plan = created(
            &["--locale-provider", "builtin", "--no-locale", "/tmp/data"],
            &FakeFs::empty(),
        );
        assert_eq!(plan.datlocale.as_deref(), Some("C"));
    }

    #[test]
    fn the_builtin_provider_canonicalizes_its_three_locales_and_rejects_the_rest() {
        // initdb.c:2474.
        let cases = [
            ("C", "C"),
            ("C.UTF-8", "C.UTF-8"),
            ("C.UTF8", "C.UTF-8"),
            ("PG_UNICODE_FAST", "PG_UNICODE_FAST"),
        ];
        for (given, canonical) in cases {
            let plan = created(
                &[
                    "--locale-provider",
                    "builtin",
                    "--builtin-locale",
                    given,
                    "--encoding",
                    "UTF8",
                    "/tmp/data",
                ],
                &FakeFs::empty(),
            );
            assert_eq!(plan.datlocale.as_deref(), Some(canonical), "{given}");
        }
        assert_eq!(
            failure(
                &[
                    "--locale-provider",
                    "builtin",
                    "--builtin-locale",
                    "de_DE.UTF-8",
                    "/tmp/data"
                ],
                &FakeFs::empty()
            ),
            InitdbError::InvalidBuiltinLocale {
                name: "de_DE.UTF-8".to_owned()
            }
        );
    }

    #[test]
    fn the_icu_provider_is_not_supported_in_this_build() {
        // initdb.c:2362, the #else branch of icu_language_tag().
        assert_eq!(
            failure(
                &[
                    "--locale-provider",
                    "icu",
                    "--icu-locale",
                    "en",
                    "/tmp/data"
                ],
                &FakeFs::empty()
            ),
            InitdbError::IcuNotSupported
        );
    }

    #[test]
    fn an_unknown_encoding_name_is_rejected() {
        // initdb.c:855.
        assert_eq!(
            failure(&["--encoding", "BOGUS", "/tmp/data"], &FakeFs::empty()),
            InitdbError::InvalidServerEncoding {
                name: "BOGUS".to_owned()
            }
        );
        // A client-only encoding is a name, but not a server encoding.
        assert_eq!(
            failure(&["--encoding", "SJIS", "/tmp/data"], &FakeFs::empty()),
            InitdbError::InvalidServerEncoding {
                name: "SJIS".to_owned()
            }
        );
    }

    #[test]
    fn the_builtin_utf8_locales_refuse_a_non_utf8_encoding() {
        // initdb.c:2781. Upstream: 'locale provider builtin with
        // --builtin-locale=C.UTF-8 fails for SQL_ASCII'.
        for locale in ["C.UTF-8", "PG_UNICODE_FAST"] {
            assert_eq!(
                failure(
                    &[
                        "--no-sync",
                        "--locale-provider",
                        "builtin",
                        "--encoding",
                        "SQL_ASCII",
                        "--lc-collate",
                        "C",
                        "--lc-ctype",
                        "C",
                        "--builtin-locale",
                        locale,
                        "/tmp/data9"
                    ],
                    &FakeFs::empty()
                ),
                InitdbError::BuiltinLocaleRequiresEncoding {
                    locale: locale.to_owned(),
                    encoding: "UTF-8",
                },
                "{locale}"
            );
        }
    }

    #[test]
    fn the_builtin_utf8_locales_accept_every_utf8_spelling() {
        for spelling in ["UTF8", "UTF-8", "Unicode"] {
            let plan = created(
                &[
                    "--locale-provider",
                    "builtin",
                    "--encoding",
                    spelling,
                    "--builtin-locale",
                    "C.UTF-8",
                    "/tmp/data8",
                ],
                &FakeFs::empty(),
            );
            assert_eq!(plan.encoding, Some(Encoding::Utf8), "{spelling}");
        }
        // Locale "C" carries no encoding requirement at all.
        let plan = created(
            &[
                "--locale-provider",
                "builtin",
                "--encoding",
                "SQL_ASCII",
                "--builtin-locale",
                "C",
                "/tmp/data",
            ],
            &FakeFs::empty(),
        );
        assert_eq!(plan.encoding, Some(Encoding::SqlAscii));
    }

    // --- the WAL directory ------------------------------------------------

    #[test]
    fn a_relative_wal_directory_is_rejected() {
        // initdb.c:2962. Upstream: 'relative xlog directory not allowed'.
        assert_eq!(
            failure(&["--waldir", "pgxlog", "/tmp/data"], &FakeFs::empty()),
            InitdbError::WalDirectoryNotAbsolute
        );
    }

    #[test]
    fn an_existing_nonempty_wal_directory_is_rejected_with_the_wal_hint() {
        // initdb.c:3001. Upstream: 'existing nonempty xlog directory'.
        let fs = FakeFs::with("/tmp/pgxlog", DirState::MountPoint);
        let err = failure(&["--waldir", "/tmp/pgxlog", "/tmp/data"], &fs);
        assert_eq!(
            err,
            InitdbError::DirectoryNotEmpty {
                path: "/tmp/pgxlog".to_owned(),
                role: DirRole::Wal,
                why: NotEmpty::LostAndFound,
            }
        );
    }

    #[test]
    fn the_data_directory_is_judged_before_the_wal_directory() {
        // create_data_directory() runs first (initdb.c:3060), so a non-empty
        // PGDATA is reported even when --waldir is also wrong.
        let fs = FakeFs::with("/tmp/data", DirState::NotEmpty);
        assert_eq!(
            failure(&["--waldir", "pgxlog", "/tmp/data"], &fs),
            InitdbError::DirectoryNotEmpty {
                path: "/tmp/data".to_owned(),
                role: DirRole::Data,
                why: NotEmpty::Entries,
            }
        );
    }

    #[test]
    fn a_usable_wal_directory_is_part_of_the_plan() {
        let fs = FakeFs::with("/tmp/pgxlog", DirState::Empty).and("/tmp/data", DirState::Empty);
        let plan = created(&["--waldir", "/tmp/pgxlog", "/tmp/data"], &fs);
        assert_eq!(
            plan.waldir,
            Some((PathBuf::from("/tmp/pgxlog"), DirAction::ReuseEmpty))
        );
    }

    // --- documented divergences (docs/divergences.md) ---------------------

    #[test]
    fn an_explicit_locale_wins_over_no_locale_whatever_the_order() {
        // C's getopt applies case 8 (--no-locale, initdb.c:3336) and case 1
        // (--locale) in command-line order, so the last one wins. usage-rs
        // reports flags, not positions, so --locale always wins here.
        for order in [
            vec![
                "--locale-provider",
                "builtin",
                "--no-locale",
                "--locale",
                "C.UTF-8",
                "/tmp/d",
            ],
            vec![
                "--locale-provider",
                "builtin",
                "--locale",
                "C.UTF-8",
                "--no-locale",
                "/tmp/d",
            ],
        ] {
            let plan = created(&order, &FakeFs::empty());
            assert_eq!(plan.datlocale.as_deref(), Some("C.UTF-8"), "{order:?}");
        }
    }

    #[test]
    fn two_getopt_stage_errors_are_reported_in_a_fixed_order() {
        // C stops at whichever switch arm getopt_long reaches first; rinitdb
        // always reports the -c one, because the parser has already collapsed
        // the command line into fields.
        for order in [
            vec!["-c", "work_mem", "--locale-provider", "xyz", "/tmp/d"],
            vec!["--locale-provider", "xyz", "-c", "work_mem", "/tmp/d"],
        ] {
            assert_eq!(
                failure(&order, &FakeFs::empty()),
                InitdbError::SetRequiresValue {
                    name: "work_mem".to_owned()
                },
                "{order:?}"
            );
        }
    }

    #[test]
    fn a_data_directory_path_is_reported_as_it_was_typed() {
        // C runs canonicalize_path() (initdb.c:2646) before pg_check_dir and
        // before every message, so it would print "/tmp/x/nonexistent" here.
        let err = failure(&["--sync-only", "/tmp/x//nonexistent/"], &FakeFs::empty());
        assert_eq!(
            err,
            InitdbError::CouldNotAccessDirectory {
                path: "/tmp/x//nonexistent/".to_owned(),
                reason: ENOENT.to_owned(),
            }
        );
    }

    // --- %m ---------------------------------------------------------------

    #[test]
    fn strerror_drops_the_os_error_suffix_rust_adds() {
        let err = std::io::Error::from_raw_os_error(2);
        assert!(err.to_string().contains("(os error 2)"));
        assert_eq!(strerror(&err), ENOENT);
    }

    #[test]
    fn strerror_leaves_an_error_without_an_errno_alone() {
        let err = std::io::Error::other("something else");
        assert_eq!(strerror(&err), "something else");
    }
}
