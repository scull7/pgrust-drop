//! Port of `src/bin/initdb/t/001_initdb.pl` (PostgreSQL 18.6), in upstream
//! order. Only the server-free assertions exist so far; each later chunk adds
//! the next block of the Perl file (Linear NAT-378 … NAT-386).

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::Path;

use testkit::reference;

const RINITDB: &str = env!("CARGO_BIN_EXE_rinitdb");

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

/// Byte-diff gate: `--help` and `--version` against the C initdb when present.
#[test]
fn help_and_version_match_reference_initdb() {
    let Some(reference_bin) = reference::find("initdb") else {
        println!("{}", reference::skip_message("initdb"));
        return;
    };
    for arg in ["--help", "--version"] {
        let theirs = testkit::run(&reference_bin, [arg]).expect("run reference initdb");
        let ours = testkit::run(Path::new(RINITDB), [arg]).expect("run rinitdb");
        assert_eq!(
            theirs,
            ours,
            "initdb {arg} differs from {}",
            reference_bin.display()
        );
    }
}
