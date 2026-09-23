//! The errors C initdb reports before it touches anything, and the exact
//! stderr block each one prints.
//!
//! `pg_log_error`, `pg_log_error_detail` and `pg_log_error_hint` all go through
//! `pg_log_generic_v` (`src/common/logging.c:219`), which writes
//! `"<progname>: "`, then `"error: "` / `"detail: "` / `"hint: "`, then the
//! formatted message and one newline. A message may itself contain a newline —
//! `warn_on_mount_point` (`src/bin/initdb/initdb.c:3038`) sends a two-line hint
//! — and the second line carries no prefix, because the whole buffer is printed
//! with a single `fprintf`. [`InitdbError`]'s `Display` reproduces that block
//! verbatim, without a trailing newline, so the caller adds it with `writeln!`.
//!
//! `pg_fatal` is `pg_log_error` + `exit(1)` and prints no hint; only the sites
//! that call `pg_log_error_hint` explicitly get one. Which variants carry a
//! hint therefore follows `initdb.c` site by site, not a house style.

use std::fmt;
use std::fmt::Write as _;

use crate::help::{PROGNAME, try_help};

/// Which directory a "exists but is not empty" complaint is about.
///
/// The two call sites of the same `pg_log_error` print different hints
/// (`initdb.c:2929` for PGDATA, `initdb.c:3001` for the WAL directory), so the
/// role is data on the error rather than a second variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirRole {
    /// The data directory (`create_data_directory`, `initdb.c:2890`).
    Data,
    /// The WAL directory (`create_xlog_or_symlink`, `initdb.c:2948`).
    Wal,
}

impl DirRole {
    /// How `cleanup_directories_atexit` names the directory (`initdb.c:771`
    /// and `:785`) — the one place the two roles are spelled out in prose.
    #[must_use]
    pub fn noun(self) -> &'static str {
        match self {
            DirRole::Data => "data directory",
            DirRole::Wal => "WAL directory",
        }
    }
}

/// Why `pg_check_dir` judged a directory non-empty.
///
/// `initdb.c:2930` branches on the code: 4 gets the "remove or empty it" hint,
/// 2 and 3 get `warn_on_mount_point` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotEmpty {
    /// `pg_check_dir` == 2: only dot-prefixed entries.
    DotFilesOnly,
    /// `pg_check_dir` == 3: a `lost+found` directory.
    LostAndFound,
    /// `pg_check_dir` == 4: ordinary entries.
    Entries,
}

/// The locale providers `--locale-provider` accepts (`initdb.c:3367`).
///
/// `collprovider_name` (`src/backend/utils/adt/pg_locale.c`) spells them in
/// lowercase; the error messages interpolate that spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocaleProvider {
    /// `--locale-provider=builtin`.
    Builtin,
    /// `--locale-provider=icu`.
    Icu,
    /// `--locale-provider=libc`, the default (`initdb.c:147`).
    #[default]
    Libc,
}

impl LocaleProvider {
    /// `collprovider_name()`: the name the error messages use.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            LocaleProvider::Builtin => "builtin",
            LocaleProvider::Icu => "icu",
            LocaleProvider::Libc => "libc",
        }
    }
}

impl fmt::Display for LocaleProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What the embedded template (ADR-0002) fixed at mint time, and so what a
/// command line cannot have yet ([`InitdbError::NotSupportedYet`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Any encoding but UTF8, any locale but C, any provider but libc.
    EncodingOrLocale,
    /// Any WAL segment size but 16 MB.
    WalSegmentSize,
    /// A superuser other than the template's `postgres` (NAT-383).
    Superuser,
    /// `-W` / `--pwfile` (NAT-383).
    Password,
}

impl Unsupported {
    /// The reason, as the tail of the error line.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Unsupported::EncodingOrLocale => {
                "the embedded template cluster has encoding \"UTF8\" and locale \"C\""
            }
            Unsupported::WalSegmentSize => "the embedded template cluster has 16 MB WAL segments",
            Unsupported::Superuser => {
                "the embedded template cluster's superuser is \"postgres\" and renaming it is \
                 not implemented"
            }
            Unsupported::Password => "setting the superuser's password is not implemented",
        }
    }

    /// The hint that follows the error line.
    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Unsupported::EncodingOrLocale | Unsupported::WalSegmentSize => {
                "Clusters with another encoding, locale or WAL segment size need bootstrap \
                 mode, which this initdb does not have yet."
            }
            Unsupported::Superuser => "Run initdb with -U postgres.",
            Unsupported::Password => {
                "Create the cluster without a password and set one with ALTER ROLE."
            }
        }
    }
}

/// A pre-flight failure, one variant per `pg_fatal` / `pg_log_error` site.
///
/// Every variant names the `initdb.c` line it reproduces. The `Display`
/// implementation is the whole stderr block, so a test can compare it to the
/// bytes C writes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InitdbError {
    /// `initdb.c:3418`.
    #[error("too many command-line arguments (first is \"{first}\")")]
    TooManyArguments { first: String },

    /// `initdb.c:3273`: `-c` with no `=` in its argument.
    #[error("-c {name} requires a value")]
    SetRequiresValue { name: String },

    /// `initdb.c:3375`.
    #[error("unrecognized locale provider: {name}")]
    UnrecognizedLocaleProvider { name: String },

    /// `initdb.c:3424`, `:3428`, `:3432` — one message, three options.
    #[error("{option} cannot be specified unless locale provider \"{provider}\" is chosen")]
    OptionNeedsProvider {
        option: &'static str,
        provider: LocaleProvider,
    },

    /// `initdb.c:2625` (`setup_pgdata`).
    #[error("no data directory specified")]
    NoDataDirectory,

    /// `initdb.c:3445` (`--sync-only`) and `:2941` / `:3011` (`pg_check_dir`
    /// could not read the directory). `reason` is what `%m` expands to.
    #[error("could not access directory \"{path}\": {reason}")]
    CouldNotAccessDirectory { path: String, reason: String },

    /// `initdb.c:3455`.
    #[error("password prompt and password file cannot be specified together")]
    PasswordPromptAndFile,

    /// `initdb.c:3479`.
    #[error("superuser name \"{name}\" is disallowed; role names cannot begin with \"pg_\"")]
    SuperuserNameDisallowed { name: String },

    /// `initdb.c:2472` (`setlocales`).
    #[error("locale must be specified if provider is {provider}")]
    LocaleRequiredForProvider { provider: LocaleProvider },

    /// `initdb.c:2485`.
    #[error("invalid locale name \"{name}\" for builtin provider")]
    InvalidBuiltinLocale { name: String },

    /// `initdb.c:2362` (`icu_language_tag` compiled without `USE_ICU`).
    #[error("ICU is not supported in this build")]
    IcuNotSupported,

    /// `initdb.c:855` (`get_encoding_id`).
    #[error("\"{name}\" is not a valid server encoding name")]
    InvalidServerEncoding { name: String },

    /// `initdb.c:2781`.
    #[error("builtin provider locale \"{locale}\" requires encoding \"{encoding}\"")]
    BuiltinLocaleRequiresEncoding {
        locale: String,
        encoding: &'static str,
    },

    /// `initdb.c:2929` / `:3001`.
    #[error("directory \"{path}\" exists but is not empty")]
    DirectoryNotEmpty {
        path: String,
        role: DirRole,
        why: NotEmpty,
    },

    /// `initdb.c:2962`.
    #[error("WAL directory location must be an absolute path")]
    WalDirectoryNotAbsolute,

    /// `initdb.c:2903` (`pg_mkdir_p` on PGDATA), `:2974` (on the WAL
    /// directory), `:3022` (`pg_wal` itself) and `:3079` (the `subdirs[]`
    /// loop) — one message, four `mkdir` sites.
    #[error("could not create directory \"{path}\": {reason}")]
    CouldNotCreateDirectory { path: String, reason: String },

    /// `initdb.c:2917` and `:2989`: the `chmod` on a directory that was
    /// already there and empty.
    #[error("could not change permissions of directory \"{path}\": {reason}")]
    CouldNotChangePermissionsOfDirectory { path: String, reason: String },

    /// `initdb.c:3015`. `path` is `subdirloc`, the link, not its target.
    #[error("could not create symbolic link \"{path}\": {reason}")]
    CouldNotCreateSymbolicLink { path: String, reason: String },

    /// `initdb.c:1035` (`write_version_file`).
    #[error("could not open file \"{path}\" for writing: {reason}")]
    CouldNotOpenFileForWriting { path: String, reason: String },

    /// `initdb.c:1038` (`write_version_file`).
    #[error("could not write file \"{path}\": {reason}")]
    CouldNotWriteFile { path: String, reason: String },

    /// `src/fe_utils/option_utils.c:99`, reached from the `--sync-method`
    /// switch arm at `initdb.c:3389`. The `%s` is always `syncfs`: `fsync` is
    /// unconditional and any third spelling takes the variant below.
    #[error("this build does not support sync method \"{name}\"")]
    UnsupportedSyncMethod { name: &'static str },

    /// `src/fe_utils/option_utils.c:106`. Unlike the line above, the value is
    /// not quoted.
    #[error("unrecognized sync method: {name}")]
    UnrecognizedSyncMethod { name: String },

    /// `src/common/file_utils.c:123` (the `pg_wal` `lstat` in `sync_pgdata`)
    /// and `:588` (`get_dirent_type`). Both are reported and walked past.
    #[error("could not stat file \"{path}\": {reason}")]
    CouldNotStatFile { path: String, reason: String },

    /// `src/common/file_utils.c:304` (`walkdir`). Reported, and the directory
    /// is skipped.
    #[error("could not open directory \"{path}\": {reason}")]
    CouldNotOpenDirectory { path: String, reason: String },

    /// `src/common/file_utils.c:338` (`walkdir`) — `readdir` itself failed
    /// part-way through. Reported after the entries it did read have been
    /// acted on, and the directory is still fsync'd.
    #[error("could not read directory \"{path}\": {reason}")]
    CouldNotReadDirectory { path: String, reason: String },

    /// `src/common/file_utils.c:428` (`fsync_fname`). Reported; the file is
    /// not synced and the walk carries on.
    #[error("could not open file \"{path}\": {reason}")]
    CouldNotOpenFile { path: String, reason: String },

    /// `src/common/file_utils.c:440` (`fsync_fname`), the one fatal site in
    /// the walk: `pg_log_error` then `exit(EXIT_FAILURE)`.
    #[error("could not fsync file \"{path}\": {reason}")]
    CouldNotFsyncFile { path: String, reason: String },

    /// Not an upstream site: the command line asks for something the embedded
    /// template cannot make (`crate::cluster::check_template_can_make`,
    /// ADR-0002, `docs/divergences.md`). Reported before anything is created.
    #[error("{what} is not supported yet: {}", why.reason())]
    NotSupportedYet { what: String, why: Unsupported },

    /// Not an upstream site: the image or control file compiled into this
    /// binary does not parse — a build defect, not a user error.
    #[error("the embedded template is damaged: {reason}")]
    TemplateDamaged { reason: String },

    /// `xlog.c:4213` (`InitControlFile`): `pg_strong_random` failed.
    #[error("could not generate secret authorization token")]
    CouldNotGenerateSecretToken,
}

impl InitdbError {
    /// The `pg_log_error_detail` lines this site emits, in order.
    #[must_use]
    pub fn details(&self) -> Vec<String> {
        match self {
            // warn_on_mount_point(), initdb.c:3034 and :3036.
            InitdbError::DirectoryNotEmpty { why, .. } => match why {
                NotEmpty::DotFilesOnly => vec![
                    "It contains a dot-prefixed/invisible file, perhaps due to it being a mount point."
                        .to_owned(),
                ],
                NotEmpty::LostAndFound => vec![
                    "It contains a lost+found directory, perhaps due to it being a mount point."
                        .to_owned(),
                ],
                NotEmpty::Entries => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// The `pg_log_error_hint` lines this site emits, in order.
    ///
    /// A hint may be multi-line: `warn_on_mount_point` passes a string with an
    /// embedded newline, and only the first line gets the `hint: ` prefix.
    #[must_use]
    pub fn hints(&self) -> Vec<String> {
        match self {
            // initdb.c:3274, :3400 and :3420 all add the same one.
            InitdbError::TooManyArguments { .. } | InitdbError::SetRequiresValue { .. } => {
                vec![try_help(PROGNAME)]
            }
            // initdb.c:2626.
            InitdbError::NoDataDirectory => vec![
                "You must identify the directory where the data for this database system \
                 will reside.  Do this with either the invocation option -D or the \
                 environment variable PGDATA."
                    .to_owned(),
            ],
            InitdbError::DirectoryNotEmpty { path, role, why } => match why {
                // initdb.c:3038, reached through warn_on_mount_point().
                NotEmpty::DotFilesOnly | NotEmpty::LostAndFound => vec![
                    "Using a mount point directly as the data directory is not recommended.\n\
                     Create a subdirectory under the mount point."
                        .to_owned(),
                ],
                // initdb.c:2933 and :3005.
                NotEmpty::Entries => match role {
                    DirRole::Data => vec![format!(
                        "If you want to create a new database system, either remove or empty \
                         the directory \"{path}\" or run {PROGNAME} with an argument other than \
                         \"{path}\"."
                    )],
                    DirRole::Wal => vec![format!(
                        "If you want to store the WAL there, either remove or empty the directory \
                         \"{path}\"."
                    )],
                },
            },
            InitdbError::NotSupportedYet { why, .. } => vec![why.hint().to_owned()],
            _ => Vec::new(),
        }
    }

    /// The whole stderr block, as `pg_log_generic_v` writes it, without the
    /// final newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{PROGNAME}: error: {self}");
        // Writing into a String cannot fail; the Results are the fmt API's.
        for detail in self.details() {
            let _ = write!(out, "\n{PROGNAME}: detail: {detail}");
        }
        for hint in self.hints() {
            let _ = write!(out, "\n{PROGNAME}: hint: {hint}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pg_fatal_site_prints_one_line_and_no_hint() {
        // initdb.c:3479 is pg_fatal, which never calls pg_log_error_hint.
        let err = InitdbError::SuperuserNameDisallowed {
            name: "pg_test".to_owned(),
        };
        assert_eq!(
            err.render(),
            "initdb: error: superuser name \"pg_test\" is disallowed; \
             role names cannot begin with \"pg_\""
        );
    }

    #[test]
    fn too_many_arguments_carries_the_try_help_hint() {
        let err = InitdbError::TooManyArguments {
            first: "extra".to_owned(),
        };
        assert_eq!(
            err.render(),
            "initdb: error: too many command-line arguments (first is \"extra\")\n\
             initdb: hint: Try \"initdb --help\" for more information."
        );
    }

    #[test]
    fn the_try_help_hint_is_the_same_text_the_bare_hint_path_prints() {
        // lib.rs prints help::try_help_hint for Invocation::Hint (the getopt
        // `default:` arm); InitdbError renders the same upstream string
        // through its own prefix. They must not drift apart.
        let rendered = InitdbError::TooManyArguments {
            first: "extra".to_owned(),
        }
        .render();
        assert!(
            rendered.ends_with(&crate::help::try_help_hint(PROGNAME)),
            "{rendered}"
        );
    }

    #[test]
    fn no_data_directory_hint_is_one_line_of_upstream_text() {
        assert_eq!(
            InitdbError::NoDataDirectory.render(),
            "initdb: error: no data directory specified\n\
             initdb: hint: You must identify the directory where the data for this database \
             system will reside.  Do this with either the invocation option -D or the \
             environment variable PGDATA."
        );
    }

    #[test]
    fn the_filesystem_failures_render_their_pg_fatal_line_and_no_hint() {
        // The five sites apply() can reach. Each is a pg_fatal, so each is one
        // line with no detail and no hint, and `%m` is already expanded.
        let cases = [
            (
                InitdbError::CouldNotCreateDirectory {
                    path: "/tmp/data/global".to_owned(),
                    reason: "Permission denied".to_owned(),
                },
                "initdb: error: could not create directory \"/tmp/data/global\": \
                 Permission denied",
            ),
            (
                InitdbError::CouldNotChangePermissionsOfDirectory {
                    path: "/tmp/data".to_owned(),
                    reason: "Operation not permitted".to_owned(),
                },
                "initdb: error: could not change permissions of directory \"/tmp/data\": \
                 Operation not permitted",
            ),
            (
                InitdbError::CouldNotCreateSymbolicLink {
                    path: "/tmp/data/pg_wal".to_owned(),
                    reason: "File exists".to_owned(),
                },
                "initdb: error: could not create symbolic link \"/tmp/data/pg_wal\": \
                 File exists",
            ),
            (
                InitdbError::CouldNotOpenFileForWriting {
                    path: "/tmp/data/PG_VERSION".to_owned(),
                    reason: "No such file or directory".to_owned(),
                },
                "initdb: error: could not open file \"/tmp/data/PG_VERSION\" for writing: \
                 No such file or directory",
            ),
            (
                InitdbError::CouldNotWriteFile {
                    path: "/tmp/data/PG_VERSION".to_owned(),
                    reason: "No space left on device".to_owned(),
                },
                "initdb: error: could not write file \"/tmp/data/PG_VERSION\": \
                 No space left on device",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.render(), expected);
            assert!(err.details().is_empty(), "{err:?}");
            assert!(err.hints().is_empty(), "{err:?}");
        }
    }

    #[test]
    fn a_nonempty_data_directory_names_itself_twice_in_the_hint() {
        let err = InitdbError::DirectoryNotEmpty {
            path: "/tmp/data".to_owned(),
            role: DirRole::Data,
            why: NotEmpty::Entries,
        };
        assert_eq!(
            err.render(),
            "initdb: error: directory \"/tmp/data\" exists but is not empty\n\
             initdb: hint: If you want to create a new database system, either remove or empty \
             the directory \"/tmp/data\" or run initdb with an argument other than \"/tmp/data\"."
        );
    }

    #[test]
    fn a_lost_and_found_directory_gets_the_mount_point_detail_and_a_two_line_hint() {
        // The second hint line carries no "initdb: hint: " prefix: logging.c
        // prints the whole message with one fprintf (src/common/logging.c:334).
        let err = InitdbError::DirectoryNotEmpty {
            path: "/tmp/pgxlog".to_owned(),
            role: DirRole::Wal,
            why: NotEmpty::LostAndFound,
        };
        assert_eq!(
            err.render(),
            "initdb: error: directory \"/tmp/pgxlog\" exists but is not empty\n\
             initdb: detail: It contains a lost+found directory, perhaps due to it being a \
             mount point.\n\
             initdb: hint: Using a mount point directly as the data directory is not recommended.\n\
             Create a subdirectory under the mount point."
        );
    }

    #[test]
    fn a_dot_file_only_directory_gets_the_other_mount_point_detail() {
        let err = InitdbError::DirectoryNotEmpty {
            path: "/tmp/data".to_owned(),
            role: DirRole::Data,
            why: NotEmpty::DotFilesOnly,
        };
        assert!(err.details()[0].contains("dot-prefixed/invisible file"));
        assert!(err.hints()[0].starts_with("Using a mount point directly"));
    }

    #[test]
    fn a_nonempty_wal_directory_gets_the_wal_hint() {
        let err = InitdbError::DirectoryNotEmpty {
            path: "/tmp/pgxlog".to_owned(),
            role: DirRole::Wal,
            why: NotEmpty::Entries,
        };
        assert_eq!(
            err.hints(),
            vec![
                "If you want to store the WAL there, either remove or empty the directory \
                 \"/tmp/pgxlog\"."
            ]
        );
    }

    #[test]
    fn provider_names_follow_collprovider_name() {
        assert_eq!(LocaleProvider::Builtin.name(), "builtin");
        assert_eq!(LocaleProvider::Icu.name(), "icu");
        assert_eq!(LocaleProvider::Libc.name(), "libc");
        assert_eq!(LocaleProvider::default(), LocaleProvider::Libc);
    }

    #[test]
    fn the_provider_specific_option_message_names_both_option_and_provider() {
        let err = InitdbError::OptionNeedsProvider {
            option: "--icu-locale",
            provider: LocaleProvider::Icu,
        };
        assert_eq!(
            err.render(),
            "initdb: error: --icu-locale cannot be specified unless locale provider \
             \"icu\" is chosen"
        );
    }

    #[test]
    fn every_remaining_variant_renders_its_upstream_sentence() {
        let cases = [
            (
                InitdbError::SetRequiresValue {
                    name: "foo".to_owned(),
                },
                "initdb: error: -c foo requires a value\n\
                 initdb: hint: Try \"initdb --help\" for more information.",
            ),
            (
                InitdbError::UnrecognizedLocaleProvider {
                    name: "xyz".to_owned(),
                },
                "initdb: error: unrecognized locale provider: xyz",
            ),
            (
                InitdbError::CouldNotAccessDirectory {
                    path: "/tmp/nonexistent".to_owned(),
                    reason: "No such file or directory".to_owned(),
                },
                "initdb: error: could not access directory \"/tmp/nonexistent\": \
                 No such file or directory",
            ),
            (
                InitdbError::PasswordPromptAndFile,
                "initdb: error: password prompt and password file cannot be specified together",
            ),
            (
                InitdbError::LocaleRequiredForProvider {
                    provider: LocaleProvider::Builtin,
                },
                "initdb: error: locale must be specified if provider is builtin",
            ),
            (
                InitdbError::InvalidBuiltinLocale {
                    name: "de_DE".to_owned(),
                },
                "initdb: error: invalid locale name \"de_DE\" for builtin provider",
            ),
            (
                InitdbError::IcuNotSupported,
                "initdb: error: ICU is not supported in this build",
            ),
            (
                InitdbError::InvalidServerEncoding {
                    name: "BOGUS".to_owned(),
                },
                "initdb: error: \"BOGUS\" is not a valid server encoding name",
            ),
            (
                InitdbError::BuiltinLocaleRequiresEncoding {
                    locale: "C.UTF-8".to_owned(),
                    encoding: "UTF-8",
                },
                "initdb: error: builtin provider locale \"C.UTF-8\" requires encoding \"UTF-8\"",
            ),
            (
                InitdbError::WalDirectoryNotAbsolute,
                "initdb: error: WAL directory location must be an absolute path",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.render(), expected, "{err:?}");
        }
    }
}
