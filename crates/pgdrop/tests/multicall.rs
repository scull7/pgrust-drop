//! The multicall binary behaves like the tools it stands in for: the stolen
//! `program_*_ok` assertions pass through `pgdrop initdb` and through a
//! symlink named `initdb`.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use testkit::normalize::EXTRA_VERSION;

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

/// NAT-376 acceptance: `pgdrop postgres --version` prints pgrust's version
/// string. The rest of the applet is still NAT-407's.
#[test]
fn postgres_subcommand_reports_pgrusts_version() {
    let pgdrop = Path::new(PGDROP);
    for flag in ["--version", "-V"] {
        let out = testkit::run(pgdrop, ["postgres", flag]).expect("run");
        assert!(out.succeeded(), "{flag}");
        assert_eq!(out.stdout_text(), pgdrop::postgres::VERSION_LINE, "{flag}");
        assert_eq!(out.stdout_text(), "postgres (PostgreSQL) 18.6\n", "{flag}");
        assert_eq!(out.stderr_text(), "", "{flag}");
    }
    let server = testkit::run(pgdrop, ["postgres", "-D", "x", "--version"]).expect("run");
    assert!(!server.succeeded());
    assert!(
        server.stderr_text().contains("NAT-407"),
        "{}",
        server.stderr_text()
    );
}

/// Through a `postgres` symlink, pgrust's version line is C's byte for byte
/// (`main.c:170`, `PG_BACKEND_VERSIONSTR`).
///
/// Normalized by `normalize::EXTRA_VERSION`, as `version_matches_c_psql` and
/// the `initdb` version gate are: PGDG and Homebrew build the reference with
/// `--with-extra-version`, which appends a parenthetical such as
/// `(Ubuntu 18.6-1.pgdg24.04+2)` or `(Homebrew)` to that one line. The
/// normalizer strips only a trailing parenthetical from that line shape; the
/// version number itself is still compared, so 18.6 and 18.5 still differ.
#[test]
fn postgres_symlink_version_matches_the_c_server() {
    let link = symlinked_as("postgres");
    let Some(gate) = testkit::Gate::for_tool_or_skip("postgres", link) else {
        return; // The flagged skip is already on stderr.
    };
    gate.arg("--version")
        .normalizer(EXTRA_VERSION)
        .assert_clean();
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
