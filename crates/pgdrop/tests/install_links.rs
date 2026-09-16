//! `pgdrop install-links DIR` end to end (Linear NAT-416).
//!
//! The command has no upstream counterpart — PostgreSQL installs three
//! executables, not one multicall binary — so there is no stolen test to port
//! and no reference binary to diff against. What is checked here is the
//! Acceptance list: a link answers as the tool it is named after, a second run
//! leaves the links alone, `--force` replaces them, and anything that is not a
//! symlink in the way is an error naming the path.
//!
//! Unix only: the links this makes are symlinks, and so are the decoys the
//! tests put in their way.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pgdrop-install-links-{tag}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the test's temporary directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `pgdrop install-links [--force] DIR`.
fn install(dir: &Path, force: bool) -> testkit::CommandOutcome {
    let mut argv: Vec<std::ffi::OsString> = vec!["install-links".into()];
    if force {
        argv.push("--force".into());
    }
    argv.push(dir.as_os_str().to_owned());
    testkit::run(Path::new(PGDROP), &argv).expect("run pgdrop")
}

/// A symlink pointing somewhere that is not pgdrop, so replacing it shows.
fn decoy(link: &Path) {
    let _ = std::fs::remove_file(link);
    std::os::unix::fs::symlink(Path::new("/nonexistent/decoy"), link).expect("decoy symlink");
}

#[test]
fn an_installed_link_answers_as_the_tool_it_is_named_after() {
    let dir = TempDir::new("answers");
    let outcome = install(dir.path(), false);
    assert!(outcome.succeeded(), "{}", outcome.stderr_text());
    assert_eq!(outcome.stderr_text(), String::new());

    // The Acceptance line: `/tmp/bin/initdb --version` is initdb's own.
    let initdb = testkit::run(&dir.join("initdb"), ["--version"]).expect("run the initdb link");
    assert_eq!(initdb.stdout_text(), "initdb (PostgreSQL) 18.6\n");
    let psql = testkit::run(&dir.join("psql"), ["--version"]).expect("run the psql link");
    assert_eq!(psql.stdout_text(), "psql (PostgreSQL) 18.6\n");

    // All three names exist, and each is a symlink to this very binary.
    for name in ["initdb", "psql", "postgres"] {
        let link = dir.join(name);
        assert_eq!(
            std::fs::read_link(&link).expect("read_link"),
            Path::new(PGDROP),
            "{name}"
        );
    }
}

#[test]
fn a_second_run_without_force_leaves_the_links_untouched_and_exits_zero() {
    let dir = TempDir::new("rerun");
    assert!(install(dir.path(), false).succeeded());
    // Point one of them somewhere else: a run that "left it alone" must leave
    // this target, not quietly restore the right one.
    decoy(&dir.join("psql"));

    let again = install(dir.path(), false);
    assert_eq!(again.status, Some(0), "{}", again.stderr_text());
    assert_eq!(again.stderr_text(), String::new());
    let note = again.stdout_text();
    for name in ["initdb", "psql", "postgres"] {
        assert!(
            note.contains(&format!(
                "\"{}\" is already a symbolic link",
                dir.join(name).display()
            )),
            "{name} has no note:\n{note}"
        );
    }
    assert!(
        note.contains("--force"),
        "the note withholds the remedy:\n{note}"
    );
    assert_eq!(
        std::fs::read_link(dir.join("psql")).expect("read_link"),
        Path::new("/nonexistent/decoy")
    );
}

#[test]
fn force_replaces_a_link_that_points_elsewhere() {
    let dir = TempDir::new("force");
    assert!(install(dir.path(), false).succeeded());
    decoy(&dir.join("psql"));

    let forced = install(dir.path(), true);
    assert_eq!(forced.status, Some(0), "{}", forced.stderr_text());
    assert!(
        forced
            .stdout_text()
            .contains(&format!("replaced \"{}\"", dir.join("psql").display())),
        "{}",
        forced.stdout_text()
    );
    for name in ["initdb", "psql", "postgres"] {
        assert_eq!(
            std::fs::read_link(dir.join(name)).expect("read_link"),
            Path::new(PGDROP),
            "{name}"
        );
    }
}

#[test]
fn a_regular_file_in_the_way_is_an_error_naming_the_path() {
    for force in [false, true] {
        let dir = TempDir::new(if force { "occupied-force" } else { "occupied" });
        let psql = dir.join("psql");
        std::fs::write(&psql, b"#!/bin/sh\nexec /usr/bin/psql \"$@\"\n").expect("write psql");

        let outcome = install(dir.path(), force);
        assert_eq!(outcome.status, Some(1), "force={force}");
        assert_eq!(
            outcome.stderr_text(),
            format!(
                "pgdrop: error: refusing to replace \"{}\": it is a regular file, \
                 not a symbolic link\n",
                psql.display()
            ),
            "force={force}"
        );
        // The whole plan is refused: the file is untouched and nothing else
        // was installed beside it.
        assert_eq!(
            std::fs::read(&psql).expect("read psql"),
            b"#!/bin/sh\nexec /usr/bin/psql \"$@\"\n"
        );
        assert!(!dir.join("initdb").exists(), "force={force}");
        assert!(!dir.join("postgres").exists(), "force={force}");
        assert_eq!(outcome.stdout_text(), String::new(), "force={force}");
    }
}

#[test]
fn a_directory_that_does_not_exist_is_reported_not_created() {
    let dir = TempDir::new("missing");
    let missing = dir.join("nope");
    let outcome = install(&missing, false);
    assert_eq!(outcome.status, Some(1));
    assert_eq!(
        outcome.stderr_text(),
        format!(
            "pgdrop: error: could not create symbolic link \"{}\": No such file or directory\n",
            missing.join("initdb").display()
        )
    );
    assert!(!missing.exists());
}
