//! Port of `src/bin/initdb/t/001_initdb.pl` (PostgreSQL 18.6), in upstream
//! order. Only the server-free assertions exist so far; each later chunk adds
//! the next block of the Perl file (Linear NAT-378 … NAT-386).

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::Path;

use testkit::{Gate, reference};

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
        println!("{}", reference::skip_message("initdb"));
        return;
    };
    for arg in ["--help", "--version"] {
        gate.clone().arg(arg).assert_clean();
    }
}
