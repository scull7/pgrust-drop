//! Port of `src/bin/initdb/t/001_initdb.pl` (PostgreSQL 18.6), in upstream
//! order. Only the server-free assertions exist so far; each later chunk adds
//! the next block of the Perl file (Linear NAT-379 … NAT-386).
//!
//! Beside the Perl file's own cases sits the whole-datadir tree diff against
//! C initdb that NAT-386 adds (`the_finished_data_directory_matches_reference_initdb`),
//! with its allow-list recorded in `docs/test-stealing.md`.
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
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use rinitdb::control::{ControlFile, DataChecksums, SystemIdentifier};
use testkit::env::Environment;
use testkit::normalize::EXTRA_VERSION;
use testkit::{Gate, Pattern, reference};

const RINITDB: &str = env!("CARGO_BIN_EXE_rinitdb");

/// Why several gates below are judged on stderr and the exit status only.
///
/// C initdb has already printed its progress ("The files belonging to this
/// database system will be owned by …", "creating directory … ok") by the time
/// it reaches these errors; rinitdb prints that once cluster creation exists.
/// The diagnostics are finished now, so they are gated now — and the stdout
/// difference is still rendered and flagged, never dropped (`testkit::Scope`).
const STDOUT_PENDING: &str = "cluster-creation progress output lands with Linear NAT-379 … NAT-387";

/// Names an existing PostgreSQL 18 data directory, for the round-trip gate on a
/// machine that has a cluster but not the binaries that made it.
const REF_PGDATA_ENV: &str = "PGDROP_REF_PGDATA";

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
    fails_with_in(argv, &Environment::inherited(), expected_stderr);
}

/// [`fails_with`], in `env`.
fn fails_with_in(argv: &[OsString], env: &Environment, expected_stderr: &str) {
    testkit::command_fails(Path::new(RINITDB), argv);
    let outcome = testkit::run_in(Path::new(RINITDB), argv, &[], env).expect("run rinitdb");
    assert_eq!(outcome.status, Some(1), "{argv:?}");
    assert_eq!(outcome.stdout, Vec::<u8>::new(), "{argv:?}");
    assert_eq!(
        outcome.stderr_text(),
        format!("{expected_stderr}\n"),
        "{argv:?}"
    );
}

/// `command_ok(...)` plus the exact stdout C writes for the same line, and an
/// empty stderr.
fn succeeds_with(argv: &[OsString], expected_stdout: &str) {
    // One spawn, not two: these cases sync a directory tree, so running the
    // binary twice would do the work twice. The stolen `command_ok` assertion
    // is the pure check over the outcome that `testkit::command_ok` wraps.
    let outcome = testkit::run(Path::new(RINITDB), argv).expect("run rinitdb");
    let violations = testkit::checks::command_ok(&outcome);
    assert!(violations.is_empty(), "{argv:?}: {violations:?}");
    assert_eq!(outcome.status, Some(0), "{argv:?}");
    assert_eq!(outcome.stdout_text(), expected_stdout, "{argv:?}");
    assert_eq!(outcome.stderr_text(), String::new(), "{argv:?}");
}

/// `[ 'initdb', @before, $datadir, @after ]`, keeping upstream's word order:
/// `001_initdb.pl:86` puts `--sync-method` *after* the data directory.
fn sync_argv(before: &[&str], datadir: &Path, after: &[&str]) -> Vec<OsString> {
    let mut argv = args(before);
    argv.push(OsString::from(datadir));
    argv.extend(args(after));
    argv
}

/// The byte-diff gate for an invocation where C prints nothing on stdout
/// either, so nothing is out of scope.
fn gate_strictly(argv: &[OsString]) {
    let Some(gate) = Gate::for_tool_or_skip("initdb", RINITDB) else {
        return;
    };
    gate.with_args(argv).assert_clean();
}

/// The byte-diff gate for an invocation whose C stdout is cluster-creation
/// progress rinitdb does not produce yet; stderr and the exit status are gated
/// in full and the stdout difference is flagged.
fn gate_diagnostics(argv: &[OsString]) {
    gate_diagnostics_in(argv, Environment::inherited());
}

/// [`gate_diagnostics`], with both binaries run in `env`.
fn gate_diagnostics_in(argv: &[OsString], env: Environment) {
    let Some(gate) = Gate::for_tool_or_skip("initdb", RINITDB) else {
        return;
    };
    gate.with_args(argv)
        .with_env(env)
        .stderr_and_status_only(STDOUT_PENDING)
        .assert_clean();
}

/// The environment of an installed `initdb`, which has a `postgres` beside
/// it: `setup_bin_paths` (`initdb.c:3472`) looks for one before `--waldir`
/// is judged, and without `--username` the superuser is the effective user,
/// which the template's `postgres` is renamed to (`rinitdb::single_user`)
/// unless they agree. Nothing is beside `rinitdb` in `target/`, so a
/// stand-in that only answers `-V` is named by `PGDROP_POSTGRES` — which C
/// `initdb` ignores. The cases that use it fail before any session starts.
#[cfg(unix)]
fn installed(tempdir: &TempDir) -> Environment {
    use std::os::unix::fs::PermissionsExt as _;
    let path = tempdir.join("postgres");
    std::fs::write(
        &path,
        "#!/bin/sh\n[ \"$1\" = -V ] && echo 'postgres (PostgreSQL) 18.6' && exit 0\nexit 1\n",
    )
    .expect("write the stand-in postgres");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the stand-in postgres executable");
    Environment::inherited().with(rinitdb::single_user::SERVER_ENV, path)
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
/// byte.
///
/// `--help` is fixed text in both builds, so it is gated as raw bytes — the
/// strictest comparison there is. `--version` prints the `PG_VERSION` its own
/// build was compiled with, and a distribution's `--with-extra-version`
/// appends a vendor suffix to that constant (PGDG's Ubuntu 18.6 package says
/// `18.6 (Ubuntu 18.6-1.pgdg24.04+2)`), so that half carries
/// `normalize::EXTRA_VERSION` and only that half. The version number itself is
/// still compared.
///
/// Missing reference binary → `SKIP (flagged, not silent)`; the gate is real
/// wherever PostgreSQL 18 is installed or `PGDROP_REF_BIN` points at it.
#[test]
fn help_and_version_match_reference_initdb() {
    let Some(gate) = Gate::for_tool_or_skip("initdb", RINITDB) else {
        return;
    };
    gate.clone().arg("--help").assert_clean();
    gate.arg("--version")
        .normalizer(EXTRA_VERSION)
        .assert_clean();
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
/// `initdb.c:3001` plus `warn_on_mount_point(3)` at `:3036`, and then the
/// `removing data directory` of `cleanup_directories_atexit` (`:771`):
/// `create_data_directory` has already made PGDATA by the time
/// `create_xlog_or_symlink` looks at `--waldir`, so the handler takes it back
/// again. That last line is what PGDATA no longer being there proves.
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

    let env = installed(&tempdir);
    fails_with_in(
        &argv,
        &env,
        &format!(
            "initdb: error: directory \"{}\" exists but is not empty\n\
             initdb: detail: It contains a lost+found directory, perhaps due to it being a \
             mount point.\n\
             initdb: hint: Using a mount point directly as the data directory is not recommended.\n\
             Create a subdirectory under the mount point.\n\
             initdb: removing data directory \"{}\"",
            xlogdir.display(),
            datadir.display()
        ),
    );
    assert!(!datadir.exists(), "the data directory was not taken back");
    gate_diagnostics_in(&argv, env);
}

/// `command_fails([ 'initdb', '--waldir' => 'pgxlog', $datadir ],
/// 'relative xlog directory not allowed');` — 001_initdb.pl:33.
///
/// `initdb.c:2962`, then `cleanup_directories_atexit` (`:762`) for the same
/// reason as [`existing_nonempty_xlog_directory`]: the absolute-path rule
/// lives in `create_xlog_or_symlink`, which C runs after PGDATA is made.
#[test]
fn relative_xlog_directory_not_allowed() {
    let tempdir = TempDir::new("relative-xlog");
    let datadir = tempdir.join("data");
    let argv = vec![
        OsString::from("--waldir"),
        OsString::from("pgxlog"),
        OsString::from(&datadir),
    ];

    let env = installed(&tempdir);
    fails_with_in(
        &argv,
        &env,
        &format!(
            "initdb: error: WAL directory location must be an absolute path\n\
             initdb: removing data directory \"{}\"",
            datadir.display()
        ),
    );
    assert!(!datadir.exists(), "the data directory was not taken back");
    gate_diagnostics_in(&argv, env);
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
/// `initialize_data_directory` (`initdb.c:3044`) does before it starts a
/// backend. The real path — parse, validate, lay out, apply — not a fixture.
///
/// # Panics
/// When any op fails.
#[cfg(unix)]
fn build_layout(argv: &[OsString]) -> PathBuf {
    let plan = create_plan(argv);
    // `create_xlog_or_symlink`'s verdict on --waldir (`initdb.c:2955`), which
    // C reaches only after PGDATA exists. Here the whole tree is applied in
    // one go, so the two are asked for together.
    let waldir = rinitdb::classify_waldir(plan.waldir.as_deref(), &rinitdb::RealFs)
        .unwrap_or_else(|err| panic!("{argv:?}: {}", err.render()));
    rinitdb::layout::apply(&rinitdb::layout::layout(&plan, waldir.as_ref()))
        .unwrap_or_else(|err| panic!("{argv:?}: {}", err.render()));
    plan.pgdata
}

/// `ok(check_mode_recursive($datadir, 0700, 0600), "check PGDATA
/// permissions");` — 001_initdb.pl:67, inside the `SKIP` block upstream takes
/// on Windows only, which is why this is `cfg(unix)` too.
///
/// Upstream runs it over a *finished* cluster; rinitdb has the directory tree
/// (`initdb.c:2890` … `:3087`) and not yet what the backend writes into it, so
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
/// `initdb.c:3014` — `$PGDATA/pg_wal` becomes a symbolic link to the directory
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

        let listing = rinitdb::layout::tree_listing(&plan, None);
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

// --- data directory tree diff (Linear NAT-386) ------------------------------

/// The differences the tree diff expects between the data directory C initdb
/// makes and the one rinitdb makes for the same command line, each with the
/// reason and the narrower check that runs in its place. Recorded in
/// `docs/test-stealing.md` ("The data directory tree diff").
///
/// Nothing else may differ: not a directory, not a mode, not `PG_VERSION`,
/// and — when the reference is the template's twin — not one catalog file.
/// Timestamps are not compared at all (`testkit::tree`).
#[cfg(unix)]
const TREE_ALLOWANCES: [testkit::tree::Allowance; 4] = {
    use testkit::tree::{Allowance, Aspect, Covers};
    [
        Allowance {
            covers: Covers::Path("global/pg_control"),
            aspects: &[Aspect::Content],
            why: "every cluster gets its own system identifier, mock authentication nonce \
                  (InitControlFile, xlog.c:4217-4218) and timestamps (controldata_utils.c:197), \
                  and rinitdb's checkpoint sits one segment past the template's redo pointer, \
                  where pg_resetwal -f puts it (ADR-0002, 2026-09-23 amendment); every other \
                  field is compared by pg_control_matches_the_reference",
        },
        Allowance {
            covers: Covers::FilesUnder("pg_wal"),
            aspects: &[Aspect::Presence, Aspect::Content],
            why: "C's segments hold the WAL its bootstrap and single-user sessions wrote; \
                  rinitdb writes one segment holding one shutdown checkpoint (ADR-0002, \
                  2026-09-23 amendment; tests/first_segment.rs holds it to pg_resetwal byte for \
                  byte); the_wal_is_the_segment_pg_control_names checks which one it is",
        },
        Allowance {
            covers: Covers::Path("pg_stat/pgstat.stat"),
            aspects: &[Aspect::Presence],
            why: "the statistics C initdb's own server wrote at shutdown (pgstat_write_statsfile, \
                  pgstat.c:1570); the template strips them (image::STRIPPED_FILES) and a server \
                  that finds no file starts from empty counters (pgstat.c:1776-1784); checked to \
                  be absent from ours, not merely allowed to be",
        },
        Allowance {
            covers: Covers::Path("postgresql.conf"),
            aspects: &[Aspect::Content],
            why: "setup_config writes what the reference build and host decide — its \
                  DEFAULT_PGSOCKET_DIR, a distribution's patched sample, the probed \
                  max_connections and shared_buffers, the host's time zone and on macOS its \
                  locale — and the_configuration_files_match_reference_initdb and \
                  the_time_zone_lines_match_reference_initdb diff it byte for byte with each \
                  of those accounted for",
        },
    ]
};

/// Pure: is `path` a file the template image decides — one `image::strip`
/// keeps, so rinitdb copies it out of the image rather than writing it?
#[cfg(unix)]
fn template_owned(path: &Path, node: &testkit::tree::Node) -> bool {
    node.kind == testkit::EntryKind::File
        && path
            .to_str()
            .and_then(|path| rinitdb::image::ImagePath::new(path).ok())
            .is_some_and(|path| rinitdb::image::keeps(&rinitdb::image::Entry::file(path, ())))
}

/// Pure: the template-owned files of C's tree `theirs` that are not the
/// embedded template's, byte for byte — missing from one side, or different.
///
/// Empty means the reference is the template's twin: the `initdb` and the
/// host that minted it (ADR-0002). Then the catalogs rinitdb copies out of the
/// image are the very ones C wrote, and the tree diff holds them to that.
/// Otherwise the host put other rows into `pg_collation`
/// (`pg_import_system_collations`, `initdb.c:1781`; ICU's version and `locale
/// -a`'s answer), every OID assigned after them moves, and no catalog file can
/// be compared — which is ADR-0002's design, not a defect.
#[cfg(unix)]
fn template_mismatches(theirs: &testkit::tree::Tree) -> Vec<PathBuf> {
    let template: BTreeMap<PathBuf, (u64, u64)> = rinitdb::image::parse(rinitdb::image::TEMPLATE)
        .expect("parse the embedded template")
        .into_iter()
        .filter_map(|entry| match entry.node {
            rinitdb::image::Node::File(contents) => Some((
                PathBuf::from(entry.path.as_str()),
                (contents.len() as u64, testkit::tree::digest(contents)),
            )),
            rinitdb::image::Node::Dir => None,
        })
        .collect();
    let c_owned: BTreeMap<PathBuf, (u64, u64)> = theirs
        .iter()
        .filter(|(path, node)| template_owned(path, node))
        .map(|(path, node)| (path.clone(), (node.size, node.digest)))
        .collect();
    let paths: BTreeSet<&PathBuf> = template.keys().chain(c_owned.keys()).collect();
    paths
        .into_iter()
        .filter(|path| template.get(*path) != c_owned.get(*path))
        .cloned()
        .collect()
}

/// The narrower check behind the `global/pg_control` allowance: C's
/// `pg_control` and ours agree on every field but the ones named here.
///
/// Taken from ours, whatever the reference: the four facts every cluster gets
/// of its own (`InitControlFile`, `xlog.c:4217`-`:4218`; the file's time,
/// `controldata_utils.c:197`, and the checkpoint's), and the checkpoint's
/// position. With a twin, that position is checked instead to be
/// `pg_resetwal -f`'s: one segment past C's redo pointer, just after the long
/// page header (`control::for_new_cluster`). Without one, the counters the
/// catalogs are consistent with — the checkpoint's next XID, next OID and
/// oldest XID — are the template's and not C's to agree with (another host's
/// collation import runs another number of transactions and assigns another
/// number of OIDs), so they are taken from ours too. The multixact fields,
/// the oldest XID's database and the rest of the checkpoint are not host
/// facts and must still be C's.
///
/// Everything else — versions, state, the recorded server settings, the
/// build's sizes and alignment, the checksum version, char signedness — must
/// be C's, byte for byte once the CRC is recomputed.
#[cfg(unix)]
fn pg_control_matches_the_reference(theirs_dir: &Path, ours_dir: &Path, twin: bool, tag: &str) {
    let read = |dir: &Path| {
        let path = testkit::control_file_path(dir);
        let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        ControlFile::parse(&bytes).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    };
    let (theirs, ours) = (read(theirs_dir), read(ours_dir));
    assert!(ours.crc_is_valid(), "our pg_control's CRC ({tag})");

    let mut expected = theirs;
    expected.system_identifier = ours.system_identifier;
    expected.mock_authentication_nonce = ours.mock_authentication_nonce;
    expected.time = ours.time;
    expected.check_point_copy.time = ours.check_point_copy.time;
    expected.check_point = ours.check_point;
    expected.check_point_copy.redo = ours.check_point_copy.redo;
    if twin {
        let seg_size = theirs.xlog_seg_size;
        let resetwal = rinitdb::control::segment_offset_to_lsn(
            rinitdb::control::segment_of(theirs.check_point_copy.redo, seg_size) + 1,
            rinitdb::control::SIZE_OF_XLOG_LONG_PHD,
            seg_size,
        );
        assert_eq!(
            (ours.check_point, ours.check_point_copy.redo),
            (resetwal, resetwal),
            "our checkpoint is not pg_resetwal -f's placement past C's redo pointer ({tag})"
        );
    } else {
        let (cp, from) = (&mut expected.check_point_copy, &ours.check_point_copy);
        cp.next_xid = from.next_xid;
        cp.next_oid = from.next_oid;
        cp.oldest_xid = from.oldest_xid;
    }
    // Not a field to agree on: `to_bytes` recomputes it. Set so the rendered
    // diff below shows only fields that matter.
    expected.crc = ours.crc;

    if expected.to_bytes() != ours.to_bytes() {
        let diff = testkit::diff::unified(
            &format!("{expected:#?}\n"),
            &format!("{ours:#?}\n"),
            "C initdb (allowed fields taken from rinitdb)",
            "rinitdb",
        )
        .unwrap_or_else(|| "(the fields agree; the bytes do not)".to_owned());
        panic!("pg_control differs from C initdb's beyond the allowance ({tag})\n{diff}");
    }
}

/// The narrower check behind the `pg_wal` allowance: our `pg_wal` holds one
/// regular file, the segment our `pg_control`'s checkpoint is in, at the
/// build's segment size.
#[cfg(unix)]
fn the_wal_is_the_segment_pg_control_names(ours_dir: &Path, ours: &testkit::tree::Tree, tag: &str) {
    let control = ControlFile::parse(
        &std::fs::read(testkit::control_file_path(ours_dir)).expect("read our pg_control"),
    )
    .expect("parse our pg_control");
    let segments: Vec<(&PathBuf, u64)> = ours
        .iter()
        .filter(|(path, node)| path.starts_with("pg_wal") && node.kind == testkit::EntryKind::File)
        .map(|(path, node)| (path, node.size))
        .collect();
    let expected = Path::new("pg_wal").join(rinitdb::wal::checkpoint_segment_file_name(&control));
    assert_eq!(
        segments,
        [(&expected, u64::from(control.xlog_seg_size))],
        "our pg_wal ({tag})"
    );
}

/// The issue's gate (Linear NAT-386): the whole data directory C initdb makes
/// against the one rinitdb makes for the same command line — every entry's
/// presence, kind, mode, size and contents — with nothing allowed to differ
/// but [`TREE_ALLOWANCES`], each of which runs a narrower check in its place.
///
/// The command line is the template's own recipe (`image::MINT_ARGS`), once
/// as it is and once with `--allow-group-access`. It is the one C initdb
/// command line whose catalogs rinitdb can claim: the template fixes the
/// encoding, locale and superuser (`docs/divergences.md`), and any other
/// command line would be refused by one side.
///
/// The catalog files are compared only when the reference is the template's
/// twin ([`template_mismatches`]). With any other reference they are left out
/// of both trees and the narrowing is printed `SKIP (flagged, not silent)`,
/// naming the first file that differs from the template; the directories,
/// the modes, the files rinitdb writes itself and the checks behind every
/// allowance still run. `PGDROP_REQUIRE_REF` does not turn that into a
/// failure: a reference that is not the minting host is ADR-0002's normal
/// case (CI's musl container is Alpine 3.23, the template's mint host 3.24),
/// not a missing tool.
///
/// Missing reference binary → `SKIP (flagged, not silent)`.
#[cfg(unix)]
#[test]
fn the_finished_data_directory_matches_reference_initdb() {
    let Some(reference) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    for (tag, extra) in [
        ("finished-default", &[][..]),
        ("finished-group", &["--allow-group-access"][..]),
    ] {
        let tempdir = TempDir::new(tag);
        let theirs_dir = tempdir.join("c");
        let ours_dir = tempdir.join("rinitdb");
        for (binary, datadir) in [
            (reference.as_path(), &theirs_dir),
            (Path::new(RINITDB), &ours_dir),
        ] {
            let mut argv = args(&rinitdb::image::MINT_ARGS);
            argv.extend(args(extra));
            argv.push(OsString::from("-D"));
            argv.push(OsString::from(datadir));
            let outcome = testkit::run(binary, &argv).expect("run initdb");
            assert!(
                outcome.succeeded(),
                "{} {argv:?} failed: {}",
                binary.display(),
                outcome.stderr_text()
            );
        }

        let read = |dir: &Path| {
            testkit::tree::read_tree(dir)
                .unwrap_or_else(|err| panic!("read the tree under {}: {err}", dir.display()))
        };
        let (mut theirs, mut ours) = (read(&theirs_dir), read(&ours_dir));

        let mismatched = template_mismatches(&theirs);
        let twin = mismatched.is_empty();
        if !twin {
            reference::announce_skip(&format!(
                "{}: the catalog files of the tree diff ({tag}): the reference initdb at {} is \
                 not the template's twin — {} of the files the template decides differ from \
                 it, the first {} — so they are left out of both trees; everything else is \
                 still compared (ADR-0002)",
                reference::SKIP_FLAG,
                reference.display(),
                mismatched.len(),
                mismatched[0].display()
            ));
            theirs.retain(|path, node| !template_owned(path, node));
            ours.retain(|path, node| !template_owned(path, node));
        }

        let found = testkit::tree::differences(&theirs, &ours);
        let left = testkit::tree::unexplained(&found, &TREE_ALLOWANCES);
        assert!(
            left.is_empty(),
            "the data directory differs from C initdb's ({tag}):\n{}",
            left.iter()
                .map(|difference| format!("  - {difference}"))
                .collect::<Vec<_>>()
                .join("\n")
        );

        pg_control_matches_the_reference(&theirs_dir, &ours_dir, twin, tag);
        the_wal_is_the_segment_pg_control_names(&ours_dir, &ours, tag);
        assert!(
            !ours.contains_key(Path::new("pg_stat/pgstat.stat")),
            "a new cluster carries no statistics ({tag})"
        );
    }
}

/// The twin branch of [`pg_control_matches_the_reference`], which no CI lane
/// reaches with a real reference (none is the template's mint host), driven
/// with a stand-in for C's file: ours, with the four per-cluster facts
/// changed and the checkpoint put back where C's would be, one segment
/// earlier. That must pass; the same stand-in with one recorded setting
/// changed, or with the checkpoint left where ours is, must not.
#[cfg(unix)]
#[test]
fn the_pg_control_check_takes_from_ours_only_what_it_names() {
    let tempdir = TempDir::new("pg-control-check");
    let ours_dir = tempdir.join("rinitdb");
    let mut argv = args(&rinitdb::image::MINT_ARGS);
    argv.push(OsString::from(&ours_dir));
    let outcome = testkit::run(Path::new(RINITDB), &argv).expect("run rinitdb");
    assert!(outcome.succeeded(), "{}", outcome.stderr_text());
    let ours = ControlFile::parse(
        &std::fs::read(testkit::control_file_path(&ours_dir)).expect("read our pg_control"),
    )
    .expect("parse our pg_control");

    let mut c_like = ours;
    c_like.system_identifier = SystemIdentifier::from_raw(ours.system_identifier.get() ^ 1);
    c_like.mock_authentication_nonce[0] ^= 0xff;
    c_like.time -= 7;
    c_like.check_point_copy.time -= 7;
    let seg_size = ours.xlog_seg_size;
    let c_redo = rinitdb::control::segment_offset_to_lsn(
        rinitdb::control::segment_of(ours.check_point_copy.redo, seg_size) - 1,
        4096,
        seg_size,
    );
    c_like.check_point = c_redo + 104;
    c_like.check_point_copy.redo = c_redo;

    let write = |name: &str, control: &ControlFile| {
        let dir = tempdir.join(name);
        std::fs::create_dir_all(dir.join("global")).expect("create global/");
        std::fs::write(testkit::control_file_path(&dir), control.to_bytes())
            .expect("write pg_control");
        dir
    };
    pg_control_matches_the_reference(&write("c", &c_like), &ours_dir, true, "stand-in");

    let mut other_setting = c_like;
    other_setting.max_connections += 1;
    let mut same_segment = c_like;
    same_segment.check_point_copy.redo = ours.check_point_copy.redo;
    for (name, control) in [("setting", other_setting), ("segment", same_segment)] {
        let dir = write(name, &control);
        let caught = std::panic::catch_unwind(|| {
            pg_control_matches_the_reference(&dir, &ours_dir, true, name);
        });
        assert!(caught.is_err(), "{name}: the check let it through");
    }
}

/// The pure half of the gate, over trees made by hand: an allowance hides
/// only its own aspects at its own entries, so a catalog file or a mode that
/// differed would still fail the gate — the gate is not vacuous even where no
/// reference binary is installed to drive it.
#[cfg(unix)]
#[test]
fn the_tree_allowances_explain_only_what_they_name() {
    use testkit::tree::{Node, Tree, differences, unexplained};

    let tree = |entries: &[(&str, Node)]| -> Tree {
        entries
            .iter()
            .map(|(path, node)| (PathBuf::from(path), *node))
            .collect()
    };
    let theirs = tree(&[
        ("", Node::dir(0o700)),
        ("base/1/1259", Node::file(0o600, b"catalog")),
        ("global/pg_control", Node::file(0o600, b"C's")),
        ("pg_stat/pgstat.stat", Node::file(0o600, b"stats")),
        (
            "pg_wal/000000010000000000000001",
            Node::file(0o600, b"C's WAL"),
        ),
        ("pg_hba.conf", Node::file(0o600, b"host all all")),
        ("postgresql.conf", Node::file(0o600, b"#port = 5432")),
    ]);
    let ours = tree(&[
        ("", Node::dir(0o700)),
        ("base/1/1259", Node::file(0o600, b"CATALOG")),
        ("global/pg_control", Node::file(0o640, b"ours")),
        (
            "pg_wal/000000010000000000000002",
            Node::file(0o600, b"ours"),
        ),
        ("pg_hba.conf", Node::file(0o600, b"host all none")),
        ("postgresql.conf", Node::file(0o600, b"#port = 5433")),
    ]);
    let found = differences(&theirs, &ours);
    let left: Vec<String> = unexplained(&found, &TREE_ALLOWANCES)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        left,
        [
            "base/1/1259: contents differ (both 7 bytes)",
            "global/pg_control: mode 0600 in the reference, 0640 here",
            "pg_hba.conf: 12 bytes in the reference, 13 here",
        ]
    );

    // And which files a twin check and the narrowing speak for.
    for (path, owned) in [
        ("base/1/1259", true),
        ("pg_xact/0000", true),
        ("base/1/PG_VERSION", true),
        ("PG_VERSION", false),
        ("global/pg_control", false),
        ("postgresql.conf", false),
        ("pg_wal/000000010000000000000001", false),
        ("pg_stat/pgstat.stat", false),
    ] {
        assert_eq!(
            template_owned(Path::new(path), &Node::file(0o600, b"")),
            owned,
            "{path}"
        );
    }
    assert!(!template_owned(Path::new("base/1"), &Node::dir(0o700)));
}

// --- pg_control (Linear NAT-382) --------------------------------------------

/// The `pg_control` a template cluster brings with it (ADR-0002): the
/// committed `template.control` (NAT-381), which is what `rinitdb` makes a
/// new cluster's from. `rewrite` treats everything but the system identifier
/// and the checksum version as the template's opaque bytes.
fn a_template_control_file() -> &'static [u8] {
    rinitdb::image::TEMPLATE_CONTROL
}

/// Expand one cluster's `$PGDATA/global/pg_control` from the template, which is
/// `rewrite` plus the one write to disk, and answer with the identifier it got.
fn expand_control_file(datadir: &Path, checksums: DataChecksums) -> SystemIdentifier {
    let system_identifier = SystemIdentifier::generate();
    let image = rinitdb::control::rewrite(a_template_control_file(), system_identifier, checksums)
        .expect("rewrite the template's pg_control");
    let path = testkit::control_file_path(datadir);
    std::fs::create_dir_all(path.parent().expect("pg_control is inside global/"))
        .expect("create the cluster's global/ directory");
    std::fs::write(&path, image).expect("write the cluster's pg_control");
    system_identifier
}

/// The stolen `command_like([ 'pg_controldata', $datadir ], qr/…/)`, made twice:
/// against the C tool when this machine has one, and against the control file
/// itself either way.
///
/// `pg_controldata` is a PostgreSQL binary, so without PostgreSQL 18 on the box
/// the first half prints `SKIP (flagged, not silent)`. The second half is not a
/// weakened version of it — the pattern is the stolen one, run unchanged over
/// the line `pg_controldata.c:337` prints — so the assertion is still made.
fn assert_data_page_checksum_version(datadir: &Path, expected: u32) {
    let pattern = testkit::Pattern::new(&format!("Data page checksum version:.*{expected}"))
        .expect("compile the stolen pattern");

    let data = testkit::read_control_file(datadir);
    assert_eq!(data.data_checksum_version, expected);
    assert!(
        pattern.is_match(&data.data_page_checksum_version_line()),
        "{:?} does not match the stolen pattern",
        data.data_page_checksum_version_line()
    );

    match reference::find("pg_controldata") {
        Some(pg_controldata) => testkit::command_like(&pg_controldata, [datadir], &pattern),
        None => reference::skip("pg_controldata"),
    }
}

/// `command_like([ 'pg_controldata', $datadir ],
/// qr/Data page checksum version:.*1/,
/// 'checksums are enabled in control file');` — 001_initdb.pl:72-76.
///
/// What upstream's comment at `001_initdb.pl:72` claims is that checksums are
/// enabled *by default*: `$datadir` is the cluster `successful creation` made
/// at `:51`-`:59`, whose command line names neither `-k` nor
/// `--no-data-checksums`. So this names neither either — `--no-sync` is
/// carried over from that command line and the rest of it is about text search
/// configuration and `--waldir` — and the setting comes out of
/// `DataChecksums::resolve` over an empty switch list, which is
/// `initdb.c:167`'s `static bool data_checksums = true;` and nothing else.
/// Passing `DataChecksums::Enabled` here instead would assert less than the
/// Perl does: it would still pass with that default flipped.
///
/// The version written when it is on is `PG_DATA_CHECKSUM_VERSION`
/// (`src/include/storage/bufpage.h:208`).
#[test]
fn checksums_are_enabled_in_control_file() {
    let tempdir = TempDir::new("checksums-on");
    let datadir = tempdir.join("data");

    let argv = sync_argv(&["--no-sync"], &datadir, &[]);
    let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(&argv) else {
        panic!("{argv:?} should be a cluster-creation command line");
    };
    assert!(!options.data_checksums && !options.no_data_checksums);

    expand_control_file(&datadir, DataChecksums::resolve([]));
    assert_data_page_checksum_version(&datadir, 1);
}

/// `command_ok([ 'initdb', '--sync-only', $datadir ], 'sync only');`
/// — 001_initdb.pl:78.
///
/// `initdb.c:3439`: `setup_pgdata`, `pg_check_dir > 0`, the progress line at
/// `:3447`, `sync_pgdata` (`src/common/file_utils.c:99`) and `check_ok`.
///
/// Upstream syncs the cluster the `successful creation` case left behind; this
/// runs over the directory tree `build_layout` makes, which is what this port
/// creates so far. `sync_pgdata` does not care what is in the tree — it walks
/// whatever is there — and the gate walks the very same tree through C initdb,
/// so the comparison is exact either way. It widens to a finished cluster with
/// Linear NAT-387.
#[cfg(unix)]
#[test]
fn sync_only() {
    let tempdir = TempDir::new("sync-only");
    let datadir = build_layout(&args(&[&tempdir.join("data").to_string_lossy()]));
    let argv = sync_argv(&["--sync-only"], &datadir, &[]);

    succeeds_with(&argv, "syncing data to disk ... ok\n");
    gate_strictly(&argv);
}

/// `command_ok([ 'initdb', '--sync-only', '--no-sync-data-files', $datadir ],
/// '--no-sync-data-files');` — 001_initdb.pl:79.
///
/// `initdb.c:3396` clears `sync_data_files`, which `file_utils.c:193` turns
/// into an `exclude_dir` of `$PGDATA/base` and `:220` into a skipped
/// `pg_tblspc`. Neither shows up in the output, which is the point: the case
/// pins that the option is accepted and changes nothing a user can see. What
/// it excludes is pinned by the unit test
/// `sync::tests::no_sync_data_files_excludes_base_and_skips_pg_tblspc`.
#[cfg(unix)]
#[test]
fn no_sync_data_files() {
    let tempdir = TempDir::new("sync-no-data-files");
    let datadir = build_layout(&args(&[&tempdir.join("data").to_string_lossy()]));
    let argv = sync_argv(&["--sync-only", "--no-sync-data-files"], &datadir, &[]);

    succeeds_with(&argv, "syncing data to disk ... ok\n");
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

/// ```perl
/// if ($supports_syncfs)
/// {
///     command_ok(
///         [ 'initdb', '--sync-only', $datadir, '--sync-method' => 'syncfs' ],
///         'sync method syncfs');
/// }
/// else
/// {
///     command_fails(
///         [ 'initdb', '--sync-only', $datadir, '--sync-method' => 'syncfs' ],
///         'sync method syncfs');
/// }
/// ```
/// — 001_initdb.pl:83. `$supports_syncfs` is `check_pg_config("#define
/// HAVE_SYNCFS 1")` (`:19`); [`rinitdb::sync::HAVE_SYNCFS`] is the same answer
/// for this build, so the same branch runs here and in the C reference.
///
/// The failing half is `parse_sync_method`'s `#else`
/// (`src/fe_utils/option_utils.c:99`) followed by `exit(1)` at
/// `initdb.c:3390`.
#[cfg(unix)]
#[test]
fn sync_method_syncfs() {
    let tempdir = TempDir::new("sync-method-syncfs");
    let datadir = build_layout(&args(&[&tempdir.join("data").to_string_lossy()]));
    let argv = sync_argv(&["--sync-only"], &datadir, &["--sync-method", "syncfs"]);

    if rinitdb::sync::HAVE_SYNCFS {
        succeeds_with(&argv, "syncing data to disk ... ok\n");
    } else {
        fails_with(
            &argv,
            "initdb: error: this build does not support sync method \"syncfs\"",
        );
    }
    gate_strictly(&argv);
}

/// Which side of `if ($ENV{with_icu} eq 'yes')` (001_initdb.pl:114) a build
/// is on. Upstream reads it from the build's configuration
/// (`src/bin/initdb/Makefile:64`, `src/bin/initdb/meson.build:34`); a
/// reference binary carries no configuration to read, so it is asked of the
/// binary itself ([`icu_support`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IcuSupport {
    /// `with_icu` is `yes`: the cases at 001_initdb.pl:116-190 apply.
    With,
    /// `with_icu` is `no`: the `else` case at 001_initdb.pl:194 applies.
    Without,
}

/// `[ 'initdb', '--no-sync', '--locale-provider' => 'icu', @rest, $datadir ]`,
/// the shape every case of the ICU block shares.
fn icu_argv(rest: &[&str], datadir: &Path) -> Vec<OsString> {
    let mut argv = args(&["--no-sync", "--locale-provider", "icu"]);
    argv.extend(args(rest));
    argv.push(OsString::from(datadir));
    argv
}

/// The probe that tells the two builds apart: 'fails for encoding not
/// supported by ICU' (001_initdb.pl:161). Either build stops inside
/// `setup_locale_encoding` (`initdb.c:3490`), before
/// `initialize_data_directory` (`:3506`) creates anything. A build with ICU
/// stops at the encoding check (`:2786`, `check_icu_locale_encoding`'s
/// `encoding mismatch`, `:2304`). A build without ICU never gets there:
/// `setlocales` (`:2687`) runs first and stops in `icu_language_tag`'s `#else`
/// (`:2362`).
const ICU_PROBE: [&str; 4] = ["--encoding", "SQL_ASCII", "--icu-locale", "en"];

/// `icu_language_tag`'s `#else` (`initdb.c:2362`): what a build without ICU
/// writes once a command line reaches ICU.
const ICU_NOT_SUPPORTED: &str = "initdb: error: ICU is not supported in this build";

/// Pure: read [`ICU_PROBE`]'s outcome. Anything but one of the two known
/// failures is an error, never a guess.
fn icu_support_from_probe(outcome: &testkit::CommandOutcome) -> Result<IcuSupport, String> {
    let stderr = outcome.stderr_text();
    if outcome.succeeded() {
        Err(format!("the ICU probe succeeded; stderr: {stderr:?}"))
    } else if stderr.lines().any(|line| line == ICU_NOT_SUPPORTED) {
        Ok(IcuSupport::Without)
    } else if stderr
        .lines()
        .any(|line| line == "initdb: error: encoding mismatch")
    {
        Ok(IcuSupport::With)
    } else {
        Err(format!(
            "the ICU probe failed with neither `encoding mismatch` nor `{ICU_NOT_SUPPORTED}`; \
             stderr: {stderr:?}"
        ))
    }
}

/// Action: run [`ICU_PROBE`] through `initdb`.
fn icu_support(initdb: &Path) -> IcuSupport {
    let tempdir = TempDir::new("icu-probe");
    let argv = icu_argv(&ICU_PROBE, &tempdir.join("probe"));
    let outcome = testkit::run(initdb, &argv).expect("run the ICU probe");
    icu_support_from_probe(&outcome).unwrap_or_else(|why| panic!("{}: {why}", initdb.display()))
}

/// Action: the reference `initdb` and its side of the branch, or `None` with
/// the skip already applied (`reference::find_or_skip`). The probe runs once
/// per process; every case below asks.
fn reference_icu() -> Option<(PathBuf, IcuSupport)> {
    static PROBED: OnceLock<IcuSupport> = OnceLock::new();
    let initdb = reference::find_or_skip("initdb")?;
    let support = *PROBED.get_or_init(|| icu_support(&initdb));
    Some((initdb, support))
}

/// What rinitdb does with the command line of a case in the ICU block.
/// rinitdb is a build without ICU (`docs/divergences.md`), so upstream would
/// never run these cases against it. What a C build without ICU writes for
/// the same line is pinned instead.
#[derive(Clone, Copy, Debug)]
enum OursWithoutIcu {
    /// It fails with this error before ICU is reached, so a build with ICU
    /// fails the same way.
    FailsFirst(&'static str),
    /// It reaches ICU and stops at [`ICU_NOT_SUPPORTED`].
    NotSupported,
}

impl OursWithoutIcu {
    fn stderr(self) -> &'static str {
        match self {
            Self::FailsFirst(error) => error,
            Self::NotSupported => ICU_NOT_SUPPORTED,
        }
    }
}

/// Pure: whether ours and the reference must agree byte for byte (stderr and
/// exit status) on an ICU case. They must when the line fails before ICU is
/// reached, or when the reference is a build without ICU as well. With ICU
/// on one side only, the two builds are on different sides of
/// 001_initdb.pl:114 and nothing obliges them to agree.
fn icu_case_is_byte_diffed(ours: OursWithoutIcu, reference: IcuSupport) -> bool {
    matches!(ours, OursWithoutIcu::FailsFirst(_)) || reference == IcuSupport::Without
}

/// One case of the `with_icu` block (001_initdb.pl:114-191).
///
/// Ours: rinitdb must fail with exactly `ours.stderr()`.
///
/// The reference, when it is built with ICU: the upstream assertion,
/// `stolen`, runs against it, as upstream runs it against a build with
/// `with_icu=yes`. When the byte diff does not apply
/// ([`icu_case_is_byte_diffed`]), that narrowing is printed
/// `SKIP (flagged, not silent)`. When the reference is built without ICU, the
/// case does not apply to it either (its `else` case is
/// `locale_provider_icu_fails_since_no_icu_support`), and the two builds must
/// then agree byte for byte.
fn icu_case(argv: &[OsString], ours: OursWithoutIcu, stolen: impl FnOnce(&Path)) {
    fails_with(argv, ours.stderr());

    let Some((initdb, support)) = reference_icu() else {
        return;
    };
    if support == IcuSupport::With {
        stolen(&initdb);
    }
    if icu_case_is_byte_diffed(ours, support) {
        gate_diagnostics(argv);
    } else {
        reference::announce_skip(&format!(
            "{}: the byte diff of {argv:?}: the reference initdb at {} is built with ICU and \
             rinitdb without, so they take different sides of `if ($ENV{{with_icu}} eq 'yes')` \
             (001_initdb.pl:114). The upstream assertion ran against the reference, and \
             rinitdb's stderr is pinned to `{ICU_NOT_SUPPORTED}`",
            reference::SKIP_FLAG,
            initdb.display()
        ));
    }
}

/// `testkit::command_fails_like` with a pattern that must compile.
fn fails_like(initdb: &Path, argv: &[OsString], pattern: &str) {
    let pattern = Pattern::new(pattern).expect("the stolen pattern compiles");
    testkit::command_fails_like(initdb, argv, &pattern);
}

/// The two builds tell themselves apart by [`ICU_PROBE`], and rinitdb is on
/// the side without ICU. This is the claim `docs/divergences.md` makes and
/// the ICU cases below rely on.
#[test]
fn rinitdb_is_a_build_without_icu() {
    assert_eq!(icu_support(Path::new(RINITDB)), IcuSupport::Without);
}

/// The probe reading and the byte-diff rule, on hand-made outcomes. No CI
/// lane has a reference built without ICU, so the `Without` side of the
/// reference is exercised here only.
#[test]
fn the_icu_probe_and_the_byte_diff_rule_read_both_builds() {
    let failed = |stderr: &str| {
        testkit::CommandOutcome::new(Some(1), b"stdout\n".to_vec(), stderr.as_bytes().to_vec())
    };
    let with = failed(
        "initdb: error: encoding mismatch\n\
         initdb: detail: The encoding you selected (SQL_ASCII) is not supported with the ICU \
         provider.\n",
    );
    let without = failed("initdb: error: ICU is not supported in this build\n");
    assert_eq!(icu_support_from_probe(&with), Ok(IcuSupport::With));
    assert_eq!(icu_support_from_probe(&without), Ok(IcuSupport::Without));
    assert!(icu_support_from_probe(&failed("initdb: error: something else\n")).is_err());
    assert!(icu_support_from_probe(&testkit::CommandOutcome::silent_success()).is_err());
    // A line that merely mentions the message is not the message.
    assert!(
        icu_support_from_probe(&failed(
            "initdb: error: ICU is not supported in this build, apparently\n"
        ))
        .is_err()
    );

    let first = OursWithoutIcu::FailsFirst("initdb: error: locale must be specified");
    assert!(icu_case_is_byte_diffed(first, IcuSupport::With));
    assert!(icu_case_is_byte_diffed(first, IcuSupport::Without));
    assert!(!icu_case_is_byte_diffed(
        OursWithoutIcu::NotSupported,
        IcuSupport::With
    ));
    assert!(icu_case_is_byte_diffed(
        OursWithoutIcu::NotSupported,
        IcuSupport::Without
    ));
}

/// `command_fails_like([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// "$tempdir/data2" ], qr/initdb: error: locale must be specified if provider
/// is icu/, 'locale provider ICU requires --icu-locale');` — 001_initdb.pl:116.
///
/// `setlocales` (`initdb.c:2471`) fails before ICU is reached, so both builds
/// write the same error and the byte diff always runs.
#[test]
fn locale_provider_icu_requires_icu_locale() {
    let tempdir = TempDir::new("icu-requires-locale");
    let argv = icu_argv(&[], &tempdir.join("data2"));
    icu_case(
        &argv,
        OursWithoutIcu::FailsFirst("initdb: error: locale must be specified if provider is icu"),
        |initdb| {
            fails_like(
                initdb,
                &argv,
                "initdb: error: locale must be specified if provider is icu",
            );
        },
    );
}

/// `command_ok([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// '--icu-locale' => 'en', "$tempdir/data3" ], 'option --icu-locale');`
/// — 001_initdb.pl:125.
#[test]
fn option_icu_locale() {
    let tempdir = TempDir::new("icu-locale");
    let argv = icu_argv(&["--icu-locale", "en"], &tempdir.join("data3"));
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        testkit::command_ok(initdb, &argv);
    });
}

/// `command_like([ 'initdb', '--no-sync', '--auth' => 'trust',
/// '--locale-provider' => 'icu', '--locale' => 'und', '--lc-collate' => 'C',
/// '--lc-ctype' => 'C', '--lc-messages' => 'C', '--lc-numeric' => 'C',
/// '--lc-monetary' => 'C', '--lc-time' => 'C', "$tempdir/data4" ],
/// qr/^\s+default collation:\s+und\n/ms, 'options --locale-provider=icu
/// --locale=und --lc-*=C');` — 001_initdb.pl:134. Perl's trailing `/ms` is the
/// leading `(?ms)` here (`testkit::pattern`).
#[test]
fn options_locale_provider_icu_locale_und_lc_c() {
    let tempdir = TempDir::new("icu-und");
    // Upstream's word order: `--auth` comes before `--locale-provider`.
    let mut argv = args(&["--no-sync", "--auth", "trust", "--locale-provider", "icu"]);
    argv.extend(args(&[
        "--locale",
        "und",
        "--lc-collate",
        "C",
        "--lc-ctype",
        "C",
        "--lc-messages",
        "C",
        "--lc-numeric",
        "C",
        "--lc-monetary",
        "C",
        "--lc-time",
        "C",
    ]));
    argv.push(OsString::from(tempdir.join("data4")));
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        let pattern = Pattern::new(r"(?ms)^\s+default collation:\s+und\n")
            .expect("the stolen pattern compiles");
        testkit::command_like(initdb, &argv, &pattern);
    });
}

/// `command_fails_like([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// '--icu-locale' => '@colNumeric=lower', "$tempdir/dataX" ], qr/could not
/// open collator for locale/, 'fails for invalid ICU locale');`
/// — 001_initdb.pl:151.
#[test]
fn fails_for_invalid_icu_locale() {
    let tempdir = TempDir::new("icu-invalid-locale");
    let argv = icu_argv(
        &["--icu-locale", "@colNumeric=lower"],
        &tempdir.join("dataX"),
    );
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        fails_like(initdb, &argv, "could not open collator for locale");
    });
}

/// `command_fails_like([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// '--encoding' => 'SQL_ASCII', '--icu-locale' => 'en', "$tempdir/dataX" ],
/// qr/error: encoding mismatch/, 'fails for encoding not supported by ICU');`
/// — 001_initdb.pl:161. The same command line is [`ICU_PROBE`].
#[test]
fn fails_for_encoding_not_supported_by_icu() {
    let tempdir = TempDir::new("icu-sql-ascii");
    let argv = icu_argv(&ICU_PROBE, &tempdir.join("dataX"));
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        fails_like(initdb, &argv, "error: encoding mismatch");
    });
}

/// `command_fails_like([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// '--icu-locale' => 'nonsense-nowhere', "$tempdir/dataX" ], qr/error: locale
/// "nonsense-nowhere" has unknown language "nonsense"/, 'fails for nonsense
/// language');` — 001_initdb.pl:172.
#[test]
fn fails_for_nonsense_language() {
    let tempdir = TempDir::new("icu-nonsense");
    let argv = icu_argv(
        &["--icu-locale", "nonsense-nowhere"],
        &tempdir.join("dataX"),
    );
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        fails_like(
            initdb,
            &argv,
            r#"error: locale "nonsense-nowhere" has unknown language "nonsense""#,
        );
    });
}

/// `command_fails_like([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// '--icu-locale' => '@colNumeric=lower', "$tempdir/dataX" ], qr/could not
/// open collator for locale "und-u-kn-lower": U_ILLEGAL_ARGUMENT_ERROR/,
/// 'fails for invalid collation argument');` — 001_initdb.pl:182. The same
/// command line as :151, held to the full message.
#[test]
fn fails_for_invalid_collation_argument() {
    let tempdir = TempDir::new("icu-invalid-collation");
    let argv = icu_argv(
        &["--icu-locale", "@colNumeric=lower"],
        &tempdir.join("dataX"),
    );
    icu_case(&argv, OursWithoutIcu::NotSupported, |initdb| {
        fails_like(
            initdb,
            &argv,
            r#"could not open collator for locale "und-u-kn-lower": U_ILLEGAL_ARGUMENT_ERROR"#,
        );
    });
}

/// `command_fails([ 'initdb', '--no-sync', '--locale-provider' => 'icu',
/// "$tempdir/data2" ], 'locale provider ICU fails since no ICU support');`
/// — 001_initdb.pl:194, the `$ENV{with_icu} ne 'yes'` branch, which is the one
/// that applies: rinitdb has no ICU dependency
/// (`rinitdb_is_a_build_without_icu`). The ICU block above it runs against a
/// reference built with ICU.
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
///
/// Nothing in the case is platform-specific; the guard is `create_plan`'s,
/// which is `cfg(unix)` like every other helper here that touches a real
/// filesystem. Without it this test target does not compile off Unix, and the
/// crate is deliberately kept buildable there (`layout.rs:360`,
/// `pg_config.rs`'s `cfg(windows)` arms).
#[cfg(unix)]
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

/// `command_ok([ 'initdb', '--no-data-checksums', $datadir_nochecksums ],
/// 'successful creation without data checksums');` — 001_initdb.pl:315-319,
/// followed by `command_like([ 'pg_controldata', $datadir_nochecksums ],
/// qr/Data page checksum version:.*0/,
/// 'checksums are disabled in control file');` — 001_initdb.pl:321-325.
///
/// The `command_ok` half needs a finished cluster and lands with it
/// (NAT-381 … NAT-387); what the case is *about* — that `--no-data-checksums`
/// puts a zero in `pg_control` where the default puts a one — is here in full.
/// The command line goes through the real parser, so the switch is the one
/// `initdb.c:3393` recognizes and not a constant this test chose.
#[test]
fn checksums_are_disabled_in_control_file() {
    let tempdir = TempDir::new("checksums-off");
    let datadir = tempdir.join("data_no_checksums");

    let argv = sync_argv(&["--no-data-checksums"], &datadir, &[]);
    let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(&argv) else {
        panic!("{argv:?} should be a cluster-creation command line");
    };
    assert!(options.no_data_checksums && !options.data_checksums);

    let switches = [rinitdb::control::ChecksumSwitch::NoDataChecksums];
    expand_control_file(&datadir, DataChecksums::resolve(switches));
    assert_data_page_checksum_version(&datadir, 0);
}

/// `command_fails([ 'pg_checksums', '--pgdata' => $datadir_nochecksums ],
/// "pg_checksums fails with data checksum disabled");` — 001_initdb.pl:327-332.
///
/// `pg_checksums` has no port here and is not in this project's scope, so this
/// case is the C tool or nothing: with PostgreSQL 18 on the box it is run
/// against the control file this port wrote and must fail; without it, the
/// gate prints `SKIP (flagged, not silent)`. There is no reader-side stand-in,
/// because what upstream is pinning is `pg_checksums`' refusal and not a field
/// value — `assert_data_page_checksum_version` above already pins the field.
#[test]
fn pg_checksums_fails_with_data_checksum_disabled() {
    let tempdir = TempDir::new("pg-checksums");
    let datadir = tempdir.join("data_no_checksums");
    expand_control_file(&datadir, DataChecksums::Disabled);

    let Some(pg_checksums) = reference::find("pg_checksums") else {
        reference::skip("pg_checksums");
        return;
    };
    let argv = [OsString::from("--pgdata"), OsString::from(&datadir)];
    testkit::command_fails(&pg_checksums, &argv);
}

/// The issue's second acceptance criterion: two expansions never share a
/// system identifier.
///
/// `BootStrapXLOG` derives one per `initdb` *process* (`xlog.c:5099`-`:5101`);
/// ADR-0002 makes an expansion an unpack, so a single process can make several
/// in the same microsecond, and `SystemIdentifier::generate` is what keeps them
/// apart (see its divergence note, and `docs/divergences.md`).
#[test]
fn two_expansions_never_share_a_system_identifier() {
    let tempdir = TempDir::new("sysid");

    let mut seen = BTreeSet::new();
    for which in 0..64 {
        let datadir = tempdir.join(&format!("data{which}"));
        let generated = expand_control_file(&datadir, DataChecksums::Enabled);
        // What is in the file, not just what the generator returned.
        let on_disk = testkit::read_control_file(&datadir).system_identifier;
        assert_eq!(on_disk, generated.get());
        assert!(seen.insert(on_disk), "system identifier {on_disk} repeated");
    }
}

/// `testkit`'s small reader and `rinitdb`'s whole-struct one must see the same
/// `pg_control`.
///
/// They are separate on purpose — `testkit` must not depend on the crate it
/// tests — and separate offsets are offsets that can drift apart. This is the
/// test that stops them: whatever the reader half of
/// `assert_data_page_checksum_version` reports is what the port itself wrote.
#[test]
fn the_two_control_file_readers_agree() {
    let tempdir = TempDir::new("readers");
    let datadir = tempdir.join("data");
    // Checksums *on*, so `data_checksum_version` is the one field of the four
    // that is nonzero in a template stand-in: a reader looking at the wrong
    // offset reads a zero and the comparison below catches it.
    expand_control_file(&datadir, DataChecksums::Enabled);

    let image = std::fs::read(testkit::control_file_path(&datadir)).expect("read pg_control");
    let full = ControlFile::parse(&image).expect("parse with rinitdb's reader");
    let small = testkit::read_control_file(&datadir);

    assert_eq!(small.system_identifier, full.system_identifier.get());
    assert_eq!(small.pg_control_version, full.pg_control_version);
    assert_eq!(small.catalog_version_no, full.catalog_version_no);
    assert_eq!(small.data_checksum_version, full.data_checksum_version);
    assert!(full.crc_is_valid());
}

/// Gate: the issue's first acceptance criterion, over a *real* `pg_control`.
///
/// Parsing and serializing a file C initdb wrote must give the bytes back
/// unchanged — which proves the field offsets, the native byte order, the
/// zeroed interior padding and the CRC all at once, and is the one check a
/// hand-built image cannot make (`control.rs`'s own round-trip test uses an
/// image this port synthesized, so it cannot catch a layout this port and
/// PostgreSQL disagree about).
///
/// The real file comes from `PGDROP_REF_PGDATA` if that names a PostgreSQL 18
/// data directory, otherwise from running the reference `initdb` into a
/// temporary one. With neither, `SKIP (flagged, not silent)`, or a failure
/// under `PGDROP_REQUIRE_REF`.
#[test]
fn a_real_control_file_round_trips_byte_for_byte() {
    let tempdir = TempDir::new("real-control");
    let datadir = if let Some(existing) = std::env::var_os(REF_PGDATA_ENV) {
        PathBuf::from(existing)
    } else {
        let Some(initdb) = reference::find("initdb") else {
            // Through `skip`, not `announce_skip`, so PGDROP_REQUIRE_REF
            // turns a missing reference into a failure here as in every gate.
            reference::skip("initdb");
            return;
        };
        let datadir = tempdir.join("data");
        let argv = args(&[
            "--no-sync",
            "--no-locale",
            "-A",
            "trust",
            "-U",
            "postgres",
            "-D",
        ])
        .into_iter()
        .chain([OsString::from(&datadir)])
        .collect::<Vec<_>>();
        let outcome = testkit::run(&initdb, &argv).expect("run the reference initdb");
        assert_eq!(
            outcome.status,
            Some(0),
            "reference initdb failed: {}",
            outcome.stderr_text()
        );
        datadir
    };

    let path = testkit::control_file_path(&datadir);
    let image = std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(
        image.len(),
        rinitdb::control::PG_CONTROL_FILE_SIZE,
        "a pg_control is PG_CONTROL_FILE_SIZE bytes"
    );

    let parsed = ControlFile::parse(&image).expect("parse a real pg_control");
    assert!(
        parsed.crc_is_valid(),
        "the reference cluster's own CRC does not check out"
    );
    assert_eq!(
        parsed.pg_control_version,
        rinitdb::control::PG_CONTROL_VERSION
    );
    assert_eq!(parsed.to_bytes().as_slice(), image.as_slice());
}

/// The whole `unix_socket_directories` line of a rendered `postgresql.conf`.
///
/// `setup_config` leaves it commented (`initdb.c:1370`, `mark_as_comment`), so
/// this is the `#`-prefixed assignment and its trailing comment, tabs and all.
#[cfg(unix)]
fn socket_directory_line(conf: &str) -> Option<&str> {
    conf.lines()
        .find(|line| line.starts_with("#unix_socket_directories"))
}

/// The value inside the quotes of that line.
#[cfg(unix)]
fn socket_directory_value(conf: &str) -> Option<String> {
    let line = socket_directory_line(conf)?;
    let (_, rest) = line.split_once('\'')?;
    rest.split_once('\'').map(|(value, _)| value.to_owned())
}

/// The `--with-socketdir=` argument of a `pg_config --configure` line, whose
/// switches come back each wrapped in single quotes.
#[cfg(unix)]
fn socketdir_switch(configure: &str) -> Option<String> {
    configure
        .split(['\'', ' ', '\n'])
        .find_map(|token| token.strip_prefix("--with-socketdir="))
        .map(str::to_owned)
}

/// Pure: the reference build's `DEFAULT_PGSOCKET_DIR` (`pg_config_manual.h:193`),
/// given what `pg_config` said (if it was there to ask) and the
/// `postgresql.conf` the reference `initdb` wrote.
///
/// It is a compile-time constant of the server the reference `initdb` belongs
/// to and not of this port: `configure --with-socketdir` moves it, and Debian
/// and Ubuntu build PostgreSQL with `/var/run/postgresql` where upstream's
/// default is `/tmp`. `docs/divergences.md` pre-declared the difference and
/// said "the byte-diff gate against it is what would surface it"; this is the
/// gate reading the constant instead of assuming its own.
///
/// `pg_config --configure` echoes the switch verbatim and is the one place a
/// built tree states it without also stating the rendering under test, so it
/// is asked first. PGDG ships `pg_config` in `postgresql-server-dev-18`, which
/// CI does not install, so the fallback is the `postgresql.conf` C initdb just
/// wrote — and only the string between its quotes is taken. The `#` prefix,
/// the ` = `, the quoting rule and the comment column are all still rendered
/// here and still diffed, exactly as `max_connections` is still rendered here
/// after its value is read off C's progress output.
#[cfg(unix)]
fn socket_directory_for(configured: Option<String>, reference_conf: &str) -> String {
    configured
        .or_else(|| socket_directory_value(reference_conf))
        .unwrap_or_else(|| rinitdb::pg_config::DEFAULT_PGSOCKET_DIR.to_owned())
}

/// `pg_config --configure`, when the reference installation ships `pg_config`.
#[cfg(unix)]
fn configured_socket_directory() -> Option<String> {
    let pg_config = reference::find("pg_config")?;
    let outcome = testkit::run(&pg_config, [OsString::from("--configure")]).ok()?;
    if outcome.status != Some(0) {
        return None;
    }
    // No switch means pg_config cannot answer, NOT that the build used the
    // compiled-in default. Upstream has no `--with-socketdir` at all: in
    // 18.6, `grep -rn socketdir configure configure.ac meson_options.txt
    // meson.build` is empty, and DEFAULT_PGSOCKET_DIR exists only at
    // pg_config_manual.h:193. Debian moves it by patching that header, so the
    // switch this parses is one no upstream build ever emits. Returning None
    // hands the question to the fallback, which reads the value out of the
    // postgresql.conf C actually wrote — the only source that can carry it.
    socketdir_switch(&outcome.stdout_text())
}

#[cfg(unix)]
#[test]
fn a_pg_config_that_names_no_socket_switch_defers_to_the_file_c_wrote() {
    // The bug this pins: treating "pg_config named no switch" as a positive
    // answer of DEFAULT_PGSOCKET_DIR made `socket_directory_for` short-circuit
    // before its fallback, so the gate rendered `/tmp` against a PGDG build
    // that writes `/var/run/postgresql`, and the whole retarget was a no-op.
    let c_wrote = "#unix_socket_directories = '/var/run/postgresql'\t# comma-separated\n";

    assert_eq!(socketdir_switch("--prefix=/usr --with-openssl"), None);
    assert_eq!(
        socket_directory_for(None, c_wrote),
        "/var/run/postgresql",
        "with no switch to go on, the value C wrote must win"
    );
    assert_eq!(
        socket_directory_for(Some("/var/run/postgresql".to_owned()), c_wrote),
        "/var/run/postgresql"
    );
    assert_eq!(
        socket_directory_for(None, "#port = 5432\n"),
        rinitdb::pg_config::DEFAULT_PGSOCKET_DIR,
        "only when neither source can answer is this port's constant assumed"
    );
}

/// `setup_config`'s `unix_socket_directories` fix-up (`initdb.c:1370`) redone
/// over `rendered`, for a build whose `DEFAULT_PGSOCKET_DIR` is `socketdir`.
///
/// It is the crate's own `replace_guc_value`, the same call `setup_config`
/// makes, so the quoting and the comment re-alignment are still this port's
/// and are still what the gate judges.
#[cfg(unix)]
fn retarget_socket_directory(rendered: &str, socketdir: &str) -> String {
    rinitdb::conf::join_lines(&rinitdb::conf::replace_guc_value(
        rinitdb::conf::split_lines(rendered),
        "unix_socket_directories",
        socketdir,
        true,
    ))
}

/// Action: run the reference `initdb` for one gate case and return its
/// progress output, which is where C announces every value it probed for.
#[cfg(unix)]
fn reference_progress(reference: &Path, argv: &[OsString]) -> String {
    let output = std::process::Command::new(reference)
        .args(argv)
        .env("TZ", "UTC")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run the reference initdb");
    assert!(
        output.status.success(),
        "reference initdb {argv:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("initdb progress is UTF-8")
}

/// The [`rinitdb::conf::Settings`] for one gate case: the four values
/// `test_config_settings` (`initdb.c:1118`) probed the machine for, read off
/// the lines where C announces each one, over the command line's own answers.
#[cfg(unix)]
fn probed_settings(
    progress: &str,
    options: &rinitdb::Options,
    plan: &rinitdb::CreatePlan,
    tag: &str,
) -> rinitdb::conf::Settings {
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

    rinitdb::conf::Settings {
        max_connections,
        // AV_SLOTS_FOR_CONNS(nconns), initdb.c:1135 — not announced.
        autovacuum_worker_slots: max_connections / 6,
        shared_buffers_blocks: shared_buffers_blocks(&shared_buffers),
        default_timezone: Some(announced("selecting default time zone ... ")),
        dynamic_shared_memory_type: announced(
            "selecting dynamic shared memory implementation ... ",
        ),
        auth: rinitdb::conf::AuthMethods::resolve(options),
        gucs: plan.gucs.clone(),
        perm: plan.perm,
        ..rinitdb::conf::Settings::default()
    }
}

/// `shared_buffers` is announced in the units it is written in, so it converts
/// straight back to the block count [`rinitdb::conf::Settings`] carries.
#[cfg(unix)]
fn shared_buffers_blocks(announced: &str) -> u32 {
    let kb_per_block = rinitdb::pg_config::BLCKSZ / 1024;
    match announced.strip_suffix("MB") {
        Some(mb) => mb.parse::<u32>().expect("shared_buffers MB") * 1024 / kb_per_block,
        None => {
            announced
                .strip_suffix("kB")
                .expect("shared_buffers is MB or kB")
                .parse::<u32>()
                .expect("shared_buffers kB")
                / kb_per_block
        }
    }
}

/// The `share_path` the reference `initdb` reads its samples from
/// (`get_share_path`, `initdb.c:2676`): the prefix layouts beside the binary
/// (stock, the Maven bundles, Homebrew), then the two distribution layouts
/// that move it — PGDG's Debian/Ubuntu `/usr/lib/postgresql/18/bin` reads
/// `/usr/share/postgresql/18`, Alpine's `/usr/libexec/postgresql18` reads
/// `/usr/share/postgresql18`.
#[cfg(unix)]
fn reference_share_dir(initdb: &Path) -> Option<PathBuf> {
    let bin = initdb.parent()?;
    let prefix = bin.parent()?;
    let mut candidates = vec![
        prefix.join("share/postgresql"),
        prefix.join("share/postgresql@18"),
        prefix.join("share"),
    ];
    if bin.ends_with("lib/postgresql/18/bin") {
        candidates.push(PathBuf::from("/usr/share/postgresql/18"));
    }
    if bin.ends_with("libexec/postgresql18") {
        candidates.push(PathBuf::from("/usr/share/postgresql18"));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("postgresql.conf.sample").is_file())
}

/// The three samples the reference `initdb` rendered from, when its
/// `share_path` can be found: `postgresql.conf.sample`, `pg_hba.conf.sample`,
/// `pg_ident.conf.sample`.
#[cfg(unix)]
fn reference_samples(initdb: &Path) -> Option<[String; 3]> {
    let share = reference_share_dir(initdb)?;
    let read = |name: &str| std::fs::read_to_string(share.join(name)).ok();
    Some([
        read("postgresql.conf.sample")?,
        read("pg_hba.conf.sample")?,
        read("pg_ident.conf.sample")?,
    ])
}

/// This port's `postgresql.conf` for the gate: its own rendering over
/// `sample`, with `setup_config`'s `unix_socket_directories` fix-up
/// (`initdb.c:1370`) redone for the reference build's constant.
#[cfg(unix)]
fn our_postgresql_conf(rendered: &str, sample: &str, socketdir: &str, tag: &str) -> String {
    let retargeted = retarget_socket_directory(rendered, socketdir);
    // Redoing the replacement over a line this port has already rewritten must
    // land exactly where C's single pass over the sample lands — same
    // quoting, same comment column. Without this the retarget could be
    // papering over the rendering instead of supplying the one constant.
    let single_pass = retarget_socket_directory(sample, socketdir);
    assert_eq!(
        socket_directory_line(&retargeted),
        socket_directory_line(&single_pass),
        "retargeting {socketdir} moved the line off where one pass puts it ({tag})"
    );
    retargeted
}

/// Gate: the four files `setup_config` writes, diffed byte for byte against
/// the ones C initdb writes, for `-A trust`, `-A md5` and
/// `--auth-host scram-sha-256` (the issue's three cases).
///
/// `setup_config` writes values it probed the machine for — the DSM
/// implementation, `max_connections`, `shared_buffers` and the time zone
/// (`test_config_settings`, `initdb.c:1118`). Probing is a separate stage and
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
    // Every file of every case is compared before anything is reported: a
    // first difference that stopped the run would hide the other three files
    // and the other two auth cases, which is how the socket-directory hunk
    // below kept `postgresql.auto.conf`, `pg_hba.conf`, `pg_ident.conf`,
    // `-A md5` and `--auth-host scram-sha-256` from ever being compared at all.
    let mut differences: Vec<String> = Vec::new();

    // Render from the samples C initdb itself read whenever they can be found,
    // so both renderings start from the same bytes and the diff is about the
    // rendering. A distribution may patch its samples: Alpine moves
    // `unix_socket_directories` in the sample itself, and its re-aligned
    // comment tab is not reproducible from the pristine sample by any retarget.
    // The embedded samples are pinned to PostgreSQL 18.6 by their own unit
    // test (`conf.rs`), so nothing about the product is taken from the
    // reference here.
    let reference_samples = reference_samples(&reference);
    let samples = if let Some([conf, hba, ident]) = &reference_samples {
        rinitdb::conf::ConfSamples {
            postgresql_conf: conf,
            pg_hba_conf: hba,
            pg_ident_conf: ident,
        }
    } else {
        reference::announce_skip(&format!(
            "{}: the reference initdb at {} has no share directory with \
             postgresql.conf.sample beside it, so this port's embedded \
             samples stand in and a distribution patch to them would show \
             up as a diff",
            reference::SKIP_FLAG,
            reference.display()
        ));
        rinitdb::conf::ConfSamples::EMBEDDED
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
        let progress = reference_progress(&reference, &argv);

        // The same command line, pointed at a directory that does not exist,
        // so `validate` sees what it saw before C initdb built the cluster.
        let mut plan_argv = common;
        plan_argv.push(OsString::from(tempdir.join("mine")));
        let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(&plan_argv) else {
            panic!("{plan_argv:?} should be a cluster-creation command line");
        };
        let plan = create_plan(&plan_argv);
        let settings = probed_settings(&progress, &options, &plan, tag);

        let slurp = |name: &str| -> String {
            let bytes = testkit::slurp_file(&datadir.join(name), None)
                .unwrap_or_else(|err| panic!("slurp C initdb's {name} ({tag}): {err}"));
            String::from_utf8(bytes).expect("a config file is UTF-8")
        };

        // The last value `setup_config` writes that this port cannot know:
        // the reference server's own DEFAULT_PGSOCKET_DIR.
        let socketdir = socket_directory_for(
            configured_socket_directory(),
            &slurp(rinitdb::conf::CONF_FILES[0]),
        );

        for (name, ours) in rinitdb::conf::render_all_from(samples, &settings) {
            let ours = if name == rinitdb::conf::CONF_FILES[0] {
                our_postgresql_conf(&ours, samples.postgresql_conf, &socketdir, tag)
            } else {
                ours
            };
            let theirs = slurp(name);
            if let Some(diff) = testkit::diff::unified(&theirs, &ours, "C initdb", "rinitdb") {
                differences.push(format!("{name} differs from C initdb's ({tag})\n{diff}"));
            }
        }
    }

    assert!(
        differences.is_empty(),
        "{}\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The one hunk of [`the_configuration_files_match_reference_initdb`] that
/// needs no reference binary: how the `unix_socket_directories` line comes out
/// for the two builds that exist in the wild.
///
/// `replace_guc_value`'s indentation loop (`initdb.c:593`) tabs to the comment
/// column the sample used, which is 40, but never closer than one space. A tab
/// fits after the 33-column `'/tmp'`; a single space is all that fits after
/// the 48-column `'/var/run/postgresql'`. So the whitespace difference between
/// a stock build's line and Debian's is upstream's own arithmetic, and any fix
/// that special-cased it would be the wrong fix.
#[cfg(unix)]
#[test]
fn the_socket_directory_line_is_rendered_for_whatever_build_wrote_it() {
    let sample = rinitdb::conf::POSTGRESQL_CONF_SAMPLE;
    assert_eq!(
        socket_directory_line(&retarget_socket_directory(sample, "/tmp")),
        Some("#unix_socket_directories = '/tmp'\t# comma-separated list of directories")
    );
    assert_eq!(
        socket_directory_line(&retarget_socket_directory(sample, "/var/run/postgresql")),
        Some(
            "#unix_socket_directories = '/var/run/postgresql' \
             # comma-separated list of directories"
        )
    );
}

/// Retargeting a line this port has already rendered must land where one pass
/// over the pristine sample lands — the invariant the gate asserts per run,
/// checked here for both builds without a reference binary.
#[cfg(unix)]
#[test]
fn retargeting_a_rendered_line_lands_where_one_pass_lands() {
    let ours = rinitdb::conf::render_postgresql_conf(
        rinitdb::conf::POSTGRESQL_CONF_SAMPLE,
        &rinitdb::conf::Settings::default(),
    );
    for socketdir in ["/tmp", "/var/run/postgresql", "/run/postgresql"] {
        assert_eq!(
            socket_directory_line(&retarget_socket_directory(&ours, socketdir)),
            socket_directory_line(&retarget_socket_directory(
                rinitdb::conf::POSTGRESQL_CONF_SAMPLE,
                socketdir
            )),
            "{socketdir}"
        );
    }
}

/// The two ways the reference build's constant is discovered, over the exact
/// shapes each source produces.
#[cfg(unix)]
#[test]
fn the_reference_socket_directory_is_read_not_assumed() {
    // `pg_config --configure` quotes every switch it echoes.
    assert_eq!(
        socketdir_switch(
            "'--build=x86_64-linux-gnu' '--with-socketdir=/var/run/postgresql' '--with-gssapi'\n"
        )
        .as_deref(),
        Some("/var/run/postgresql")
    );
    // A stock build passes no such switch.
    assert_eq!(socketdir_switch("'--prefix=/usr/local/pgsql'\n"), None);

    // The fallback reads only what is between the quotes, from either build's
    // line — including Debian's, whose comment is one space away.
    for socketdir in ["/tmp", "/var/run/postgresql"] {
        let conf = retarget_socket_directory(rinitdb::conf::POSTGRESQL_CONF_SAMPLE, socketdir);
        assert_eq!(socket_directory_value(&conf).as_deref(), Some(socketdir));
        assert_eq!(socket_directory_for(None, &conf), socketdir);
        // A `pg_config` answer outranks the file.
        assert_eq!(
            socket_directory_for(Some("/run/postgresql".to_owned()), &conf),
            "/run/postgresql"
        );
    }
}

// --- default time zone (Linear NAT-385) -------------------------------------

/// `grep -E '^(log_)?timezone'` over a rendered or written `postgresql.conf`.
fn timezone_lines(conf: &str) -> Vec<&str> {
    conf.lines()
        .filter(|line| line.starts_with("timezone") || line.starts_with("log_timezone"))
        .collect()
}

/// The timezone database C initdb would search: its own `share/timezone` when
/// the installation carries one, else the system database a
/// `--with-system-tzdata` build (Debian's, Ubuntu's) is pointed at.
fn reference_tzdir(initdb: &Path) -> Option<PathBuf> {
    let prefix = initdb.parent()?.parent()?;
    for candidate in ["share/postgresql/timezone", "share/timezone"] {
        let dir = prefix.join(candidate);
        if dir.is_dir() {
            return Some(dir);
        }
    }
    None
}

/// `select_default_timezone` over the real machine must name a zone the reader
/// accepts, and — with `TZ` out of the way, which is the case `001_initdb.pl`
/// goes out of its way to run (`t/001_initdb.pl:42`) — it must be the zone
/// `/etc/localtime` points at.
///
/// This is the reader-side stand-in for the gate below: it pins the answer on a
/// machine that has a timezone database but no PostgreSQL 18 to compare with.
/// No timezone database at all → `SKIP (flagged, not silent)`.
#[cfg(unix)]
#[test]
fn the_default_time_zone_is_the_one_etc_localtime_names() {
    let Some(src) = rinitdb::RealTzSource::from_env() else {
        reference::announce_skip(&format!(
            "{}: no timezone database at {} or any system location; \
             select_default_timezone has nothing to search",
            reference::SKIP_FLAG,
            rinitdb::findtimezone::TZDIR_ENV
        ));
        return;
    };
    let chosen = rinitdb::select_default_timezone(&src);

    if let Some(tz) = std::env::var_os("TZ") {
        // findtimezone.c:1769 — TZ wins outright when it names a zone.
        let tz = tz.to_string_lossy().into_owned();
        assert_eq!(
            chosen,
            Some(tz.clone()),
            "TZ={tz} names a zone, so it is the answer"
        );
        return;
    }

    let chosen = chosen.expect("a machine with a timezone database has a default zone");
    assert!(
        rinitdb::findtimezone::TzSource::read_tzfile(&src, &chosen).is_some()
            || rinitdb::tz::parse(&chosen, false).is_some(),
        "the chosen zone {chosen:?} is neither a file in {} nor a POSIX TZ string",
        src.tzdir().display()
    );

    let (Ok(target), Ok(system)) = (
        std::fs::read_link(rinitdb::findtimezone::TZDEFAULT),
        std::fs::read(rinitdb::findtimezone::TZDEFAULT),
    ) else {
        reference::announce_skip(&format!(
            "{}: {} is not a readable symlink, so the brute-force scan chose \
             {chosen:?} and there is no second opinion on this machine",
            reference::SKIP_FLAG,
            rinitdb::findtimezone::TZDEFAULT
        ));
        return;
    };

    // `check_system_link_file` walks the target left to right, skipping the
    // first component, and takes the *first* tail that names the zone
    // (`findtimezone.c:544`). Anything shorter is a different spelling of the
    // same instant — on this machine the brute-force scan answers "UTC" where
    // the symlink answers "Etc/UTC", because `zone_name_pref` prefers the
    // bare name — so a suffix test would pass for the wrong reason. Compare
    // against the exact tail instead.
    let target = target.to_string_lossy().into_owned();
    let components: Vec<&str> = target.split('/').filter(|c| !c.is_empty()).collect();
    let expected = (1..components.len())
        .map(|i| components[i..].join("/"))
        .find(|tail| {
            rinitdb::findtimezone::TzSource::read_tzfile(&src, tail).as_deref() == Some(&system[..])
        });
    let Some(expected) = expected else {
        reference::announce_skip(&format!(
            "{}: no tail of {target} names a file in {} with {}'s bytes, so the \
             database and the symlink disagree and {chosen:?} cannot be checked \
             against it",
            reference::SKIP_FLAG,
            src.tzdir().display(),
            rinitdb::findtimezone::TZDEFAULT
        ));
        return;
    };
    assert_eq!(
        chosen,
        expected,
        "{} points at {target}, whose zone is {expected:?}",
        rinitdb::findtimezone::TZDEFAULT
    );
}

/// Gate: the issue's acceptance criterion. `grep -E '^(log_)?timezone'` over
/// the `postgresql.conf` C initdb writes and over the one this port renders
/// must be identical, byte for byte.
///
/// The first case is `001_initdb.pl`'s own: `TZ` deleted, which is what that
/// file says it exists for ("make sure we run one successful test without a TZ
/// setting so we test initdb's time zone setting code", `t/001_initdb.pl:42`)
/// — on this machine that exercises the `/etc/localtime` shortcut. The others
/// pin the `TZ` arm against a named zone, a POSIX-style name and a
/// GMT-offset name.
///
/// Both sides are pointed at the same timezone database, because the answer is
/// a property of that database and not of the code: C searches its own
/// `share/timezone`, and `PGRUST_TZDIR` is how this port is aimed at the same
/// one.
///
/// Missing reference binary → `SKIP (flagged, not silent)`.
#[cfg(unix)]
#[test]
fn the_time_zone_lines_match_reference_initdb() {
    let Some(reference) = reference::find("initdb") else {
        reference::skip("initdb");
        return;
    };
    let Some(tzdir) = reference_tzdir(&reference) else {
        reference::announce_skip(&format!(
            "{}: the reference initdb at {} has no share/timezone beside it, \
             so there is no database both sides can be aimed at",
            reference::SKIP_FLAG,
            reference.display()
        ));
        return;
    };

    for (tag, tz) in [
        ("tz-unset", None),
        ("tz-named", Some("America/New_York")),
        ("tz-posix", Some("EST5EDT")),
        ("tz-offset", Some("Etc/GMT+5")),
    ] {
        let tempdir = TempDir::new(tag);
        let datadir = tempdir.join("data");
        let argv = args(&[
            "--no-sync",
            "--no-locale",
            "-A",
            "trust",
            "-U",
            "postgres",
            "-D",
        ])
        .into_iter()
        .chain([OsString::from(&datadir)])
        .collect::<Vec<_>>();

        let mut command = std::process::Command::new(&reference);
        command.args(&argv).stdin(std::process::Stdio::null());
        match tz {
            Some(tz) => command.env("TZ", tz),
            None => command.env_remove("TZ"),
        };
        let output = command.output().expect("run the reference initdb");
        assert!(
            output.status.success(),
            "reference initdb {argv:?} failed ({tag}): {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let theirs = std::fs::read_to_string(datadir.join("postgresql.conf"))
            .expect("read C initdb's postgresql.conf");

        // The same question, asked of this port against the same database.
        let source = TzdirSource {
            tzdir: tzdir.clone(),
            tz: tz.map(str::to_owned),
        };
        let settings = rinitdb::conf::Settings {
            default_timezone: rinitdb::select_default_timezone(&source),
            ..rinitdb::conf::Settings::default()
        };
        let ours =
            rinitdb::conf::render_postgresql_conf(rinitdb::conf::POSTGRESQL_CONF_SAMPLE, &settings);

        assert_eq!(
            timezone_lines(&theirs),
            timezone_lines(&ours),
            "the time zone lines differ from C initdb's ({tag})"
        );
    }
}

/// A [`rinitdb::TzSource`] aimed at a chosen directory with a chosen `TZ`, so
/// the gate can ask this port the question C initdb was asked without changing
/// the test process's own environment.
#[cfg(unix)]
struct TzdirSource {
    tzdir: PathBuf,
    tz: Option<String>,
}

#[cfg(unix)]
impl rinitdb::TzSource for TzdirSource {
    fn read_tzfile(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.tzdir.join(name)).ok()
    }

    fn zone_names(&self) -> Vec<String> {
        fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with('.') {
                    continue;
                }
                let sub = if prefix.is_empty() {
                    name.to_owned()
                } else {
                    format!("{prefix}/{name}")
                };
                if entry.path().is_dir() {
                    walk(&entry.path(), &sub, out);
                } else {
                    out.push(sub);
                }
            }
        }
        let mut names = Vec::new();
        walk(&self.tzdir, "", &mut names);
        names.sort_unstable();
        names
    }

    fn read_link(&self, linkname: &str) -> Option<String> {
        std::fs::read_link(linkname)
            .ok()
            .and_then(|t| t.to_str().map(str::to_owned))
    }

    fn read_path(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn tz_env(&self) -> Option<String> {
        self.tz.clone()
    }

    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
    }
}
