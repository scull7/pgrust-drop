//! Port of `src/bin/initdb/t/001_initdb.pl` (PostgreSQL 18.6), in upstream
//! order. Only the server-free assertions exist so far; each later chunk adds
//! the next block of the Perl file (Linear NAT-379 … NAT-386).
//!
//! Every `command_fails` case below is doubly pinned: the stolen assertion
//! itself (the command must fail), and the exact stderr C writes, transcribed
//! from the `pg_log_error` / `pg_fatal` site named in the comment. The second
//! half is what keeps the case honest on a machine where the byte-diff gate
//! has no reference binary and skips.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use testkit::{Gate, reference};

const RINITDB: &str = env!("CARGO_BIN_EXE_rinitdb");

/// Why several gates below are judged on stderr and the exit status only.
///
/// C initdb has already printed its progress ("The files belonging to this
/// database system will be owned by …", "creating directory … ok") by the time
/// it reaches these errors; rinitdb prints that once cluster creation exists.
/// The diagnostics are finished now, so they are gated now — and the stdout
/// difference is still rendered and flagged, never dropped (`testkit::Scope`).
const STDOUT_PENDING: &str = "cluster-creation progress output lands with Linear NAT-379 … NAT-387";

/// `PostgreSQL::Test::Utils::tempdir`: a directory of this test's own, removed
/// when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pgdrop-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the test's temporary directory");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// Make `name` and put one ordinary file in it, so `pg_check_dir` reports 4.
    fn populated(&self, name: &str) -> PathBuf {
        let path = self.join(name);
        std::fs::create_dir_all(&path).expect("create a populated directory");
        std::fs::write(path.join("PG_VERSION"), "18\n").expect("write into it");
        path
    }

    /// Make `name` holding only `lost+found`, so `pg_check_dir` reports 3.
    fn with_lost_and_found(&self, name: &str) -> PathBuf {
        let path = self.join(name);
        std::fs::create_dir_all(path.join("lost+found")).expect("create lost+found");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

/// `command_fails(...)` plus the exact stderr C writes for the same line.
fn fails_with(argv: &[OsString], expected_stderr: &str) {
    testkit::command_fails(Path::new(RINITDB), argv);
    let outcome = testkit::run(Path::new(RINITDB), argv).expect("run rinitdb");
    assert_eq!(outcome.status, Some(1), "{argv:?}");
    assert_eq!(outcome.stdout, Vec::<u8>::new(), "{argv:?}");
    assert_eq!(
        outcome.stderr_text(),
        format!("{expected_stderr}\n"),
        "{argv:?}"
    );
}

/// The byte-diff gate for an invocation where C prints nothing on stdout
/// either, so nothing is out of scope.
fn gate_strictly(argv: &[OsString]) {
    let Some(gate) = Gate::for_tool("initdb", RINITDB) else {
        reference::skip("initdb");
        return;
    };
    gate.with_args(argv).assert_clean();
}

/// The byte-diff gate for an invocation whose C stdout is cluster-creation
/// progress rinitdb does not produce yet; stderr and the exit status are gated
/// in full and the stdout difference is flagged.
fn gate_diagnostics(argv: &[OsString]) {
    let Some(gate) = Gate::for_tool("initdb", RINITDB) else {
        reference::skip("initdb");
        return;
    };
    gate.with_args(argv)
        .stderr_and_status_only(STDOUT_PENDING)
        .assert_clean();
}

/// `program_help_ok('initdb');`
#[test]
fn program_help_ok() {
    testkit::program_help_ok(Path::new(RINITDB));
}

/// `program_version_ok('initdb');`
#[test]
fn program_version_ok() {
    testkit::program_version_ok(Path::new(RINITDB));
}

/// `program_options_handling_ok('initdb');`
#[test]
fn program_options_handling_ok() {
    testkit::program_options_handling_ok(Path::new(RINITDB));
}

/// Byte-diff gate (NAT-374): the same invocation through C `initdb` and
/// through `rinitdb`, with stdout, stderr and exit status diffed byte for
/// byte. No normalizer is justified here — `--help` and `--version` are fixed
/// text — so the comparison is as strict as it gets.
///
/// Missing reference binary → `SKIP (flagged, not silent)`; the gate is real
/// wherever PostgreSQL 18 is installed or `PGDROP_REF_BIN` points at it.
#[test]
fn help_and_version_match_reference_initdb() {
    let Some(gate) = Gate::for_tool("initdb", RINITDB) else {
        reference::skip("initdb");
        return;
    };
    for arg in ["--help", "--version"] {
        gate.clone().arg(arg).assert_clean();
    }
}

/// `command_fails([ 'initdb', '--sync-only', "$tempdir/nonexistent" ],
/// 'sync missing data directory');` — 001_initdb.pl:25.
///
/// `initdb.c:3445`: `pg_check_dir` returns 0 with `errno == ENOENT`
/// (`pgcheckdir.c:44`), which `%m` renders.
#[test]
fn sync_missing_data_directory() {
    let tempdir = TempDir::new("sync-missing");
    let missing = tempdir.join("nonexistent");
    let argv = args(&["--sync-only"])
        .into_iter()
        .chain([OsString::from(&missing)])
        .collect::<Vec<_>>();

    fails_with(
        &argv,
        &format!(
            "initdb: error: could not access directory \"{}\": No such file or directory",
            missing.display()
        ),
    );
    gate_strictly(&argv);
}

/// `command_fails([ 'initdb', '--waldir' => $xlogdir, $datadir ],
/// 'existing nonempty xlog directory');` — 001_initdb.pl:30, with
/// `$xlogdir/lost+found` in place.
///
/// `initdb.c:3001` plus `warn_on_mount_point(3)` at `:3036`.
#[test]
fn existing_nonempty_xlog_directory() {
    let tempdir = TempDir::new("nonempty-xlog");
    let xlogdir = tempdir.with_lost_and_found("pgxlog");
    let datadir = tempdir.join("data");
    let argv = vec![
        OsString::from("--waldir"),
        OsString::from(&xlogdir),
        OsString::from(&datadir),
    ];

    fails_with(
        &argv,
        &format!(
            "initdb: error: directory \"{}\" exists but is not empty\n\
             initdb: detail: It contains a lost+found directory, perhaps due to it being a \
             mount point.\n\
             initdb: hint: Using a mount point directly as the data directory is not recommended.\n\
             Create a subdirectory under the mount point.",
            xlogdir.display()
        ),
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--waldir' => 'pgxlog', $datadir ],
/// 'relative xlog directory not allowed');` — 001_initdb.pl:33.
///
/// `initdb.c:2962`.
#[test]
fn relative_xlog_directory_not_allowed() {
    let tempdir = TempDir::new("relative-xlog");
    let datadir = tempdir.join("data");
    let argv = vec![
        OsString::from("--waldir"),
        OsString::from("pgxlog"),
        OsString::from(&datadir),
    ];

    fails_with(
        &argv,
        "initdb: error: WAL directory location must be an absolute path",
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--username' => 'pg_test', $datadir ],
/// 'role names cannot begin with "pg_"');` — 001_initdb.pl:37.
///
/// `initdb.c:3479`, reached before the first `printf`.
#[test]
fn role_names_cannot_begin_with_pg_() {
    let tempdir = TempDir::new("pg-username");
    let datadir = tempdir.join("data");
    let argv = vec![
        OsString::from("--username"),
        OsString::from("pg_test"),
        OsString::from(&datadir),
    ];

    fails_with(
        &argv,
        "initdb: error: superuser name \"pg_test\" is disallowed; \
         role names cannot begin with \"pg_\"",
    );
    gate_strictly(&argv);
}

/// `command_fails([ 'initdb', $datadir ], 'existing data directory');`
/// — 001_initdb.pl:81, with a cluster already in `$datadir`.
///
/// `initdb.c:2929` with `pg_check_dir` == 4, so the hint at `:2933`.
#[test]
fn existing_data_directory() {
    let tempdir = TempDir::new("existing-data");
    let datadir = tempdir.populated("data");
    let argv = vec![OsString::from(&datadir)];

    fails_with(
        &argv,
        &format!(
            "initdb: error: directory \"{path}\" exists but is not empty\n\
             initdb: hint: If you want to create a new database system, either remove or empty \
             the directory \"{path}\" or run initdb with an argument other than \"{path}\".",
            path = datadir.display()
        ),
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// "$tempdir/data2" ], 'locale provider ICU fails since no ICU support');`
/// — 001_initdb.pl:194, the `$ENV{with_icu} ne 'yes'` branch, which is the one
/// that applies: rinitdb has no ICU dependency.
///
/// `initdb.c:2471` — with no `--locale` and no `--icu-locale`, `datlocale` is
/// still NULL when `setlocales` checks it, so this fails for a missing locale
/// rather than for the missing ICU build.
#[test]
fn locale_provider_icu_fails_since_no_icu_support() {
    let tempdir = TempDir::new("icu-unsupported");
    let datadir = tempdir.join("data2");
    let argv = vec![
        OsString::from("--no-sync"),
        OsString::from("--locale-provider"),
        OsString::from("icu"),
        OsString::from(&datadir),
    ];

    fails_with(
        &argv,
        "initdb: error: locale must be specified if provider is icu",
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'builtin',
/// "$tempdir/data6" ], 'locale provider builtin fails without --locale');`
/// — 001_initdb.pl:203. `initdb.c:2471`.
#[test]
fn locale_provider_builtin_fails_without_locale() {
    let tempdir = TempDir::new("builtin-no-locale");
    let datadir = tempdir.join("data6");
    let argv = vec![
        OsString::from("--no-sync"),
        OsString::from("--locale-provider"),
        OsString::from("builtin"),
        OsString::from(&datadir),
    ];

    fails_with(
        &argv,
        "initdb: error: locale must be specified if provider is builtin",
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'builtin',
/// '--encoding' => 'SQL_ASCII', '--lc-collate' => 'C', '--lc-ctype' => 'C',
/// '--builtin-locale' => 'C.UTF-8', "$tempdir/data9" ], 'locale provider
/// builtin with --builtin-locale=C.UTF-8 fails for SQL_ASCII');`
/// — 001_initdb.pl:232. `initdb.c:2781`.
#[test]
fn locale_provider_builtin_with_builtin_locale_c_utf_8_fails_for_sql_ascii() {
    let tempdir = TempDir::new("builtin-sql-ascii");
    let datadir = tempdir.join("data9");
    let argv = args(&[
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
        "C.UTF-8",
    ])
    .into_iter()
    .chain([OsString::from(&datadir)])
    .collect::<Vec<_>>();

    fails_with(
        &argv,
        "initdb: error: builtin provider locale \"C.UTF-8\" requires encoding \"UTF-8\"",
    );
    gate_diagnostics(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'builtin',
/// '--icu-locale' => 'en', "$tempdir/dataX" ], 'fails for locale provider
/// builtin with ICU locale');` — 001_initdb.pl:255. `initdb.c:3428`.
#[test]
fn fails_for_locale_provider_builtin_with_icu_locale() {
    let tempdir = TempDir::new("builtin-icu-locale");
    let argv = args(&[
        "--no-sync",
        "--locale-provider",
        "builtin",
        "--icu-locale",
        "en",
    ])
    .into_iter()
    .chain([OsString::from(tempdir.join("dataX"))])
    .collect::<Vec<_>>();

    fails_with(
        &argv,
        "initdb: error: --icu-locale cannot be specified unless locale provider \
         \"icu\" is chosen",
    );
    gate_strictly(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'builtin',
/// '--icu-rules' => '""', "$tempdir/dataX" ], 'fails for locale provider
/// builtin with ICU rules');` — 001_initdb.pl:264. `initdb.c:3432`.
#[test]
fn fails_for_locale_provider_builtin_with_icu_rules() {
    let tempdir = TempDir::new("builtin-icu-rules");
    let argv = args(&[
        "--no-sync",
        "--locale-provider",
        "builtin",
        "--icu-rules",
        "\"\"",
    ])
    .into_iter()
    .chain([OsString::from(tempdir.join("dataX"))])
    .collect::<Vec<_>>();

    fails_with(
        &argv,
        "initdb: error: --icu-rules cannot be specified unless locale provider \
         \"icu\" is chosen",
    );
    gate_strictly(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'xyz',
/// "$tempdir/dataX" ], 'fails for invalid locale provider');`
/// — 001_initdb.pl:273. `initdb.c:3375`, inside the getopt loop.
#[test]
fn fails_for_invalid_locale_provider() {
    let tempdir = TempDir::new("invalid-provider");
    let argv = args(&["--no-sync", "--locale-provider", "xyz"])
        .into_iter()
        .chain([OsString::from(tempdir.join("dataX"))])
        .collect::<Vec<_>>();

    fails_with(&argv, "initdb: error: unrecognized locale provider: xyz");
    gate_strictly(&argv);
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'libc',
/// '--icu-locale' => 'en', "$tempdir/dataX" ], 'fails for invalid option
/// combination');` — 001_initdb.pl:281. `initdb.c:3428`.
#[test]
fn fails_for_invalid_option_combination() {
    let tempdir = TempDir::new("libc-icu-locale");
    let argv = args(&[
        "--no-sync",
        "--locale-provider",
        "libc",
        "--icu-locale",
        "en",
    ])
    .into_iter()
    .chain([OsString::from(tempdir.join("dataX"))])
    .collect::<Vec<_>>();

    fails_with(
        &argv,
        "initdb: error: --icu-locale cannot be specified unless locale provider \
         \"icu\" is chosen",
    );
    gate_strictly(&argv);
}
