//! The multicall binary behaves like the tools it stands in for: the stolen
//! `program_*_ok` assertions pass through `pgdrop initdb` and through a
//! symlink named `initdb`.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

fn symlinked_as(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pgdrop-multicall-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let link = dir.join(name);
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(PGDROP, &link).expect("symlink");
    #[cfg(not(unix))]
    std::fs::copy(PGDROP, &link).expect("copy");
    link
}

#[test]
fn initdb_symlink_answers_like_initdb() {
    let link = symlinked_as("initdb");
    testkit::program_help_ok(&link);
    testkit::program_version_ok(&link);
    testkit::program_options_handling_ok(&link);

    let ours = testkit::run(&link, ["--version"]).expect("run");
    assert_eq!(ours.stdout_text(), "initdb (PostgreSQL) 18.6\n");
}

#[test]
fn initdb_subcommand_passes_arguments_through() {
    let pgdrop = Path::new(PGDROP);
    let version = testkit::run(pgdrop, ["initdb", "--version"]).expect("run");
    assert_eq!(version.stdout_text(), "initdb (PostgreSQL) 18.6\n");
    assert!(version.succeeded());

    // `--help` after other arguments is upstream's hint-and-exit-1, not pgdrop's help.
    let hint = testkit::run(pgdrop, ["initdb", "-D", "x", "--help"]).expect("run");
    assert_eq!(hint.status, Some(1));
    assert!(
        hint.stderr_text().contains("Try \"initdb --help\""),
        "{}",
        hint.stderr_text()
    );
}

#[test]
fn psql_symlink_reports_its_version() {
    let link = symlinked_as("psql");
    let ours = testkit::run(&link, ["--version"]).expect("run");
    assert_eq!(ours.stdout_text(), "psql (PostgreSQL) 18.6\n");
}

#[test]
fn root_help_and_unknown_subcommand() {
    let pgdrop = Path::new(PGDROP);
    let help = testkit::run(pgdrop, ["--help"]).expect("run");
    assert!(help.succeeded());
    assert!(
        help.stdout_text().contains("initdb"),
        "{}",
        help.stdout_text()
    );

    let bogus = testkit::run(pgdrop, ["bogus"]).expect("run");
    assert_eq!(bogus.status, Some(2));
    assert!(!bogus.stderr.is_empty());
}
