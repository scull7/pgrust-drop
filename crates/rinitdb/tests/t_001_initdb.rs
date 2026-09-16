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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
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

/// Parse and validate a cluster-creation command line the way `run` does, and
/// hand back the plan the layout is calculated from.
///
/// # Panics
/// When the command line is not a create, or does not validate.
#[cfg(unix)]
fn create_plan(argv: &[OsString]) -> rinitdb::validate::CreatePlan {
    let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(argv) else {
        panic!("{argv:?} should be a cluster-creation command line");
    };
    match rinitdb::validate(&options, &rinitdb::Environment::default(), &rinitdb::RealFs) {
        Ok(rinitdb::Plan::Create(create)) => create,
        Ok(rinitdb::Plan::Sync(_)) => panic!("{argv:?} should not be --sync-only"),
        Err(err) => panic!("{argv:?}: {}", err.render()),
    }
}

/// The chunk of `initdb` this port has: everything
/// `initialize_data_directory` (`initdb.c:3049`) does before it starts a
/// backend. The real path — parse, validate, lay out, apply — not a fixture.
///
/// # Panics
/// When any op fails.
#[cfg(unix)]
fn build_layout(argv: &[OsString]) -> PathBuf {
    let plan = create_plan(argv);
    rinitdb::layout::apply(&rinitdb::layout::layout(&plan))
        .unwrap_or_else(|err| panic!("{argv:?}: {}", err.render()));
    plan.pgdata
}

/// `ok(check_mode_recursive($datadir, 0700, 0600), "check PGDATA
/// permissions");` — 001_initdb.pl:67, inside the `SKIP` block upstream takes
/// on Windows only, which is why this is `cfg(unix)` too.
///
/// Upstream runs it over a *finished* cluster; rinitdb has the directory tree
/// (`initdb.c:2890` … `:3086`) and not yet what the backend writes into it, so
/// the walk covers the entries `layout()` makes — which is all of what this
/// port creates, so nothing is excluded from it. The case widens to the
/// finished cluster with Linear NAT-381 … NAT-387, and the comparison against
/// C initdb's own tree is `the_data_directory_tree_matches_reference_initdb`.
#[cfg(unix)]
#[test]
fn check_pgdata_permissions() {
    let tempdir = TempDir::new("perm-default");
    let datadir = build_layout(&args(&[&tempdir.join("data").to_string_lossy()]));
    testkit::check_mode_recursive_ok(
        &datadir,
        testkit::files::PGDATA_DIR_MODE,
        testkit::files::PGDATA_FILE_MODE,
        &[],
    );
}

/// `command_ok([ 'initdb', '--allow-group-access', $datadir_group ],
/// 'successful creation with group access');` and the
/// `ok(check_mode_recursive($datadir_group, 0750, 0640), 'check PGDATA
/// permissions');` that follows it — 001_initdb.pl:105 and :108.
///
/// Same scope note as [`check_pgdata_permissions`]. `-g` is one switch arm
/// (`initdb.c:3359`) and it moves every mode in the tree at once, which is
/// what this pins.
#[cfg(unix)]
#[test]
fn check_pgdata_permissions_with_group_access() {
    let tempdir = TempDir::new("perm-group");
    let datadir = build_layout(&args(&[
        "--allow-group-access",
        &tempdir.join("data_group").to_string_lossy(),
    ]));
    testkit::check_mode_recursive_ok(
        &datadir,
        testkit::files::GROUP_DIR_MODE,
        testkit::files::GROUP_FILE_MODE,
        &[],
    );
}

/// `'--waldir' => $xlogdir` in the 'successful creation' case
/// (001_initdb.pl:56), from the success side this time; the two
/// `command_fails` cases above cover a relative and a non-empty `--waldir`.
///
/// `initdb.c:3015` — `$PGDATA/pg_wal` becomes a symbolic link to the directory
/// given, and the `subdirs[]` loop then makes `archive_status` and `summaries`
/// through it (`:3068`).
#[cfg(unix)]
#[test]
fn waldir_becomes_a_pg_wal_symlink() {
    let tempdir = TempDir::new("waldir-symlink");
    let xlogdir = tempdir.join("pgxlog");
    let datadir = build_layout(&args(&[
        "--waldir",
        &xlogdir.to_string_lossy(),
        &tempdir.join("data").to_string_lossy(),
    ]));

    let pg_wal = datadir.join("pg_wal");
    let link = std::fs::symlink_metadata(&pg_wal).expect("stat $PGDATA/pg_wal");
    assert!(link.file_type().is_symlink(), "$PGDATA/pg_wal is a symlink");
    assert_eq!(
        std::fs::read_link(&pg_wal).expect("read the link"),
        xlogdir,
        "the link names the --waldir argument"
    );
    for through in ["archive_status", "summaries"] {
        assert!(
            xlogdir.join(through).is_dir(),
            "{through} was created through the link"
        );
    }
    // check_mode_recursive stats through the link (`Utils.pm:601`), so the
    // same modes cover the WAL directory wherever it lives.
    testkit::check_mode_recursive_ok(
        &datadir,
        testkit::files::PGDATA_DIR_MODE,
        testkit::files::PGDATA_FILE_MODE,
        &[],
    );
}

/// The four `pg_fatal` sites `apply` can reach, each driven by a real failing
/// syscall rather than a hand-made error value, so the `%m` text is the one
/// the kernel produced.
///
/// `initdb.c:3079` (mkdir), `:2917` (chmod), `:3015` (symlink) and `:1035`
/// (the `fopen` in `write_version_file`). The fifth, `:1038`, is the `fprintf`
/// failing mid-write — ENOSPC and friends, which a test cannot provoke without
/// a full filesystem; its rendering is pinned in
/// `error::tests::the_filesystem_failures_render_their_pg_fatal_line_and_no_hint`.
#[cfg(unix)]
#[test]
fn a_failed_filesystem_op_reports_its_upstream_pg_fatal() {
    use rinitdb::layout::FsOp;

    let tempdir = TempDir::new("apply-failures");
    let file = tempdir.join("a-file");
    std::fs::write(&file, b"not a directory").expect("make a regular file");
    let absent = tempdir.join("absent");

    // mkdir under something that is not a directory.
    let under_file = file.join("global");
    expect_apply_error(
        &[FsOp::CreateDir {
            path: under_file.clone(),
            mode: 0o700,
            parents: false,
        }],
        &format!(
            "initdb: error: could not create directory \"{}\": Not a directory",
            under_file.display()
        ),
    );

    // chmod on a directory that is not there.
    expect_apply_error(
        &[FsOp::SetMode {
            path: absent.clone(),
            mode: 0o700,
        }],
        &format!(
            "initdb: error: could not change permissions of directory \"{}\": \
             No such file or directory",
            absent.display()
        ),
    );

    // symlink onto a path that is already taken.
    expect_apply_error(
        &[FsOp::Symlink {
            target: tempdir.join("anywhere"),
            link: file.clone(),
        }],
        &format!(
            "initdb: error: could not create symbolic link \"{}\": File exists",
            file.display()
        ),
    );

    // open for writing inside a directory that is not there.
    let orphan = absent.join("PG_VERSION");
    expect_apply_error(
        &[FsOp::WriteFile {
            path: orphan.clone(),
            mode: 0o600,
            contents: "18\n".to_owned(),
        }],
        &format!(
            "initdb: error: could not open file \"{}\" for writing: \
             No such file or directory",
            orphan.display()
        ),
    );
}

/// `apply(ops)` must fail, with exactly the stderr C's `pg_fatal` writes.
#[cfg(unix)]
fn expect_apply_error(ops: &[rinitdb::layout::FsOp], expected: &str) {
    match rinitdb::layout::apply(ops) {
        Ok(()) => panic!("{ops:?} should not have succeeded"),
        Err(err) => assert_eq!(err.render(), expected),
    }
}

/// The acceptance gate for this port (Linear NAT-380): the tree listing —
/// names and modes — that `layout()` produces is exactly what C initdb leaves
/// behind, for a default run and for an `--allow-group-access` run.
///
/// C's finished cluster is a superset of this stage (the backend adds
/// `base/4`, `base/5` and every relation file), so the gate is checked one
/// entry at a time rather than as a whole-tree diff: every entry this port
/// claims must exist in C's tree, as the same kind, with C's mode. An entry
/// rinitdb invents, or gives the wrong mode, fails it. Nothing is normalized.
///
/// Missing reference binary → `SKIP (flagged, not silent)`.
#[cfg(unix)]
#[test]
fn the_data_directory_tree_matches_reference_initdb() {
    let Some(reference) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    for (tag, extra) in [
        ("tree-default", Vec::new()),
        ("tree-group", vec!["--allow-group-access"]),
    ] {
        let tempdir = TempDir::new(tag);
        let datadir = tempdir.join("data");

        let mut argv = args(&extra);
        argv.push(OsString::from("--no-sync"));
        argv.push(OsString::from("-D"));
        argv.push(OsString::from(&datadir));
        let outcome = testkit::run(&reference, &argv).expect("run the reference initdb");
        assert!(
            outcome.succeeded(),
            "reference initdb {argv:?} failed: {}",
            outcome.stderr_text()
        );

        let reference_tree: BTreeMap<PathBuf, (testkit::EntryKind, u32)> =
            testkit::files::walk(&datadir, &[])
                .expect("walk the reference cluster")
                .into_iter()
                .filter_map(|entry| {
                    let relative = entry.path.strip_prefix(&datadir).ok()?.to_path_buf();
                    Some((relative, (entry.kind, entry.mode)))
                })
                .collect();

        // The same command line, validated against a directory that does not
        // exist yet, then pointed at the cluster C just built.
        let mut plan_argv = args(&extra);
        plan_argv.push(OsString::from(tempdir.join("mine")));
        let mut plan = create_plan(&plan_argv);
        plan.pgdata.clone_from(&datadir);

        // `tree_listing` is relative to PGDATA, so PGDATA's own mode — the one
        // `create_data_directory` sets (`initdb.c:2902`) — is checked here.
        assert_eq!(
            std::fs::metadata(&datadir)
                .expect("stat the reference PGDATA")
                .mode()
                & 0o7777,
            plan.perm.masked_dir_mode(),
            "PGDATA mode differs from C initdb's ({tag})"
        );

        let listing = rinitdb::layout::tree_listing(&plan);
        for (relative, mode) in &listing {
            let Some((kind, found)) = reference_tree.get(relative) else {
                panic!("C initdb did not create {} ({tag})", relative.display());
            };
            let expected_kind = if relative == Path::new("PG_VERSION") {
                testkit::EntryKind::File
            } else {
                testkit::EntryKind::Dir
            };
            assert_eq!(*kind, expected_kind, "{} ({tag})", relative.display());
            assert_eq!(
                *found,
                *mode,
                "{} mode differs from C initdb's ({tag})",
                relative.display()
            );
        }

        // ...and the other direction. The loop above only proves the listing
        // is a *subset* of C's cluster, so a subdirectory `layout` forgot to
        // create would pass it unseen. C's finished cluster is a strict
        // superset of this stage — the backend adds `base/4`, `base/5`, every
        // relation file and the config files — so the comparison is narrowed
        // to the paths this stage owns, and over those it is an equality.
        // The narrowing set comes from `SUBDIRS`, so it cannot by itself catch
        // a row missing from that table; `layout::tests::
        // the_subdirs_table_is_upstreams_in_upstream_order` transcribes
        // initdb.c:231-255 a second time and is what pins the table.
        let owned: BTreeSet<PathBuf> = rinitdb::layout::SUBDIRS
            .iter()
            .map(PathBuf::from)
            .chain([PathBuf::from("pg_wal"), PathBuf::from("PG_VERSION")])
            .collect();
        let c_owned: BTreeSet<PathBuf> = reference_tree
            .keys()
            .filter(|path| owned.contains(*path))
            .cloned()
            .collect();
        let ours: BTreeSet<PathBuf> = listing.iter().map(|(path, _)| path.clone()).collect();
        assert_eq!(
            ours, c_owned,
            "the tree this port creates is not C initdb's, restricted to the \
             entries this stage owns ({tag})"
        );

        // PG_VERSION is the one file this stage writes; the bytes are C's too.
        assert_eq!(
            testkit::slurp_file(&datadir.join("PG_VERSION"), None).expect("slurp PG_VERSION"),
            rinitdb::layout::version_file_contents().into_bytes(),
            "PG_VERSION content ({tag})"
        );
    }
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

/// `command_ok([ 'initdb', '--no-sync', '--set' => 'work_mem=128',
/// '--set' => 'Work_Mem=256', '--set' => 'WORK_MEM=512', "$tempdir/dataY" ],
/// 'multiple --set options with different case');` plus the three assertions
/// over the slurped file — 001_initdb.pl:298-313.
///
/// The `command_ok` half needs a finished cluster and lands with it
/// (NAT-381 … NAT-387). What the case is *about* — that three `-c` switches
/// spelled three different ways collapse onto the file's one `work_mem`, last
/// one winning — is here in full: the command line goes through the real
/// parser and the real `validate`, and the three stolen `qr//` patterns are run
/// verbatim against the bytes `setup_config` would have written.
#[test]
fn multiple_set_options_with_different_case() {
    let tempdir = TempDir::new("dataY");
    let argv = args(&[
        "--no-sync",
        "--set",
        "work_mem=128",
        "--set",
        "Work_Mem=256",
        "--set",
        "WORK_MEM=512",
    ])
    .into_iter()
    .chain([OsString::from(tempdir.join("dataY"))])
    .collect::<Vec<_>>();

    let plan = create_plan(&argv);
    let settings = rinitdb::conf::Settings {
        gucs: plan.gucs.clone(),
        ..rinitdb::conf::Settings::default()
    };
    let conf =
        rinitdb::conf::render_postgresql_conf(rinitdb::conf::POSTGRESQL_CONF_SAMPLE, &settings);

    for (source, expected, what) in [
        (
            "(?m)^WORK_MEM = ",
            false,
            "WORK_MEM should not be configured",
        ),
        (
            "(?m)^Work_Mem = ",
            false,
            "Work_Mem should not be configured",
        ),
        ("(?m)^work_mem = 512", true, "work_mem should be in config"),
    ] {
        let pattern = testkit::Pattern::new(source).expect("compile the stolen pattern");
        assert_eq!(pattern.is_match(&conf), expected, "{what}");
    }
}

/// Gate: the four files `setup_config` writes, diffed byte for byte against
/// the ones C initdb writes, for `-A trust`, `-A md5` and
/// `--auth-host scram-sha-256` (the issue's three cases).
///
/// `setup_config` writes values it probed the machine for — the DSM
/// implementation, `max_connections`, `shared_buffers` and the time zone
/// (`test_config_settings`, `initdb.c:1140`). Probing is a separate stage and
/// is not ported yet, so those four arrive here from C's own stdout, which is
/// where it announces each one (`selecting default "max_connections" ... 100`).
/// They are read from the *progress output*, never from the files under
/// comparison, so nothing about the rendering is taken from the answer: the
/// template, the order of the replacements, the quoting, the comment columns,
/// the commented-out compile-time defaults and the `-c` overrides are all still
/// judged against C's bytes. `autovacuum_worker_slots` is not announced and is
/// recomputed from `AV_SLOTS_FOR_CONNS` (`initdb.c:1135`).
///
/// Everything else is pinned on the command line instead: `--no-locale` fixes
/// the four `lc_*` values and the date order, `-T simple` the text search
/// configuration, `TZ` gives `select_default_timezone` an answer to find, and
/// two `--set`s of one parameter gate the second, re-aligning application of
/// `replace_guc_value` that an override causes.
///
/// The superuser password the md5 case needs reaches only the bootstrap SQL
/// (`initdb.c:1650`); none of the four files under comparison mentions it.
///
/// Missing reference binary → `SKIP (flagged, not silent)`.
#[cfg(unix)]
#[test]
fn the_configuration_files_match_reference_initdb() {
    let Some(reference) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    for (tag, auth, needs_password) in [
        ("conf-trust", ["-A", "trust"], false),
        // `-A md5` puts md5 on both sides, and `check_need_password`
        // (`initdb.c:2597`) refuses that without a superuser password.
        ("conf-md5", ["-A", "md5"], true),
        ("conf-scram", ["--auth-host", "scram-sha-256"], false),
    ] {
        let tempdir = TempDir::new(tag);
        let datadir = tempdir.join("data");

        let mut common = args(&auth);
        if needs_password {
            let pwfile = tempdir.join("pwfile");
            std::fs::write(&pwfile, "gate\n").expect("write the password file");
            common.push(OsString::from("--pwfile"));
            common.push(OsString::from(&pwfile));
        }
        common.extend(args(&[
            "--no-sync",
            "--no-locale",
            "-T",
            "simple",
            "--set",
            "work_mem=128",
            "--set",
            "WORK_MEM=512",
        ]));

        let mut argv = common.clone();
        argv.push(OsString::from(&datadir));
        let output = std::process::Command::new(&reference)
            .args(&argv)
            .env("TZ", "UTC")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run the reference initdb");
        assert!(
            output.status.success(),
            "reference initdb {argv:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let progress = String::from_utf8(output.stdout).expect("initdb progress is UTF-8");

        // Each probe result, from the line where C announces it.
        let announced = |prefix: &str| -> String {
            progress
                .lines()
                .find_map(|line| line.strip_prefix(prefix))
                .unwrap_or_else(|| panic!("C initdb did not announce {prefix:?} ({tag})"))
                .trim()
                .to_owned()
        };
        let max_connections: u32 = announced("selecting default \"max_connections\" ... ")
            .parse()
            .expect("max_connections is a number");
        let shared_buffers = announced("selecting default \"shared_buffers\" ... ");
        let timezone = announced("selecting default time zone ... ");
        let dsm = announced("selecting dynamic shared memory implementation ... ");

        // `shared_buffers` is announced in the units it is written in, so it
        // converts straight back to the block count `Settings` carries.
        let kb_per_block = rinitdb::pg_config::BLCKSZ / 1024;
        let shared_buffers_blocks = if let Some(mb) = shared_buffers.strip_suffix("MB") {
            mb.parse::<u32>().expect("shared_buffers MB") * 1024 / kb_per_block
        } else {
            shared_buffers
                .strip_suffix("kB")
                .expect("shared_buffers is MB or kB")
                .parse::<u32>()
                .expect("shared_buffers kB")
                / kb_per_block
        };

        // The same command line, pointed at a directory that does not exist,
        // so `validate` sees what it saw before C initdb built the cluster.
        let mut plan_argv = common;
        plan_argv.push(OsString::from(tempdir.join("mine")));
        let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(&plan_argv) else {
            panic!("{plan_argv:?} should be a cluster-creation command line");
        };
        let plan = create_plan(&plan_argv);

        let settings = rinitdb::conf::Settings {
            max_connections,
            // AV_SLOTS_FOR_CONNS(nconns), initdb.c:1135 — not announced.
            autovacuum_worker_slots: max_connections / 6,
            shared_buffers_blocks,
            default_timezone: Some(timezone),
            dynamic_shared_memory_type: dsm,
            auth: rinitdb::conf::AuthMethods::resolve(&options),
            gucs: plan.gucs.clone(),
            perm: plan.perm,
            ..rinitdb::conf::Settings::default()
        };

        for (name, ours) in rinitdb::conf::render_all(&settings) {
            let theirs = testkit::slurp_file(&datadir.join(name), None)
                .unwrap_or_else(|err| panic!("slurp C initdb's {name} ({tag}): {err}"));
            let theirs = String::from_utf8(theirs).expect("a config file is UTF-8");
            if let Some(diff) = testkit::diff::unified(&theirs, &ours, "C initdb", "rinitdb") {
                panic!("{name} differs from C initdb's ({tag})\n{diff}");
            }
        }
    }
}
