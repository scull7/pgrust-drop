//! NAT-407 acceptance: `pgdrop postgres` runs pgrust in-process, on mimalloc
//! and on a stack pgdrop sizes itself, so a bare `pgdrop initdb` plus
//! `pgdrop postgres --single` needs neither pgrust's README's
//! `ulimit -s 65520` nor its `RUST_MIN_STACK=33554432`.
//!
//! The environment is the test runner's, minus any `RUST_MIN_STACK` and with
//! the share directory variables removed, `XDG_CACHE_HOME` pointed into the
//! scratch directory (NAT-408's embedded share files, as
//! `tests/template_boot.rs` does). The soft stack rlimit is whatever the
//! runner has, 8 MiB on the CI lanes.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use pgdrop::stack::{Limit, stack_rlimit};
use testkit::Environment;

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A scratch directory under Cargo's target tmpdir, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgdrop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn server_env(scratch: &Path) -> Environment {
    Environment::inherited()
        .without_all([
            pgdrop::share::SHAREDIR_VAR,
            pgdrop::share::TZDIR_VAR,
            pgdrop::stack::RUST_MIN_STACK,
        ])
        .with("XDG_CACHE_HOME", scratch.join("cache"))
}

/// `pgdrop initdb -U postgres --no-sync <pgdata>`: exit 0, nothing on stderr.
fn pgdrop_initdb(pgdata: &Path) {
    let argv = [
        OsString::from("initdb"),
        OsString::from("-U"),
        OsString::from("postgres"),
        OsString::from("--no-sync"),
        pgdata.into(),
    ];
    let outcome = testkit::run(Path::new(PGDROP), &argv).expect("run pgdrop initdb");
    assert_eq!(outcome.status, Some(0), "stderr: {}", outcome.stderr_text());
    assert_eq!(outcome.stderr_text(), "");
}

/// `pgdrop postgres --single <options> -D <pgdata> postgres` with `input`:
/// exit 0; stdout and stderr.
fn single(pgdata: &Path, options: &[&str], env: &Environment, input: &str) -> (String, String) {
    let argv: Vec<OsString> = ["postgres", "--single"]
        .iter()
        .chain(options)
        .map(OsString::from)
        .chain([OsString::from("-D"), pgdata.into(), "postgres".into()])
        .collect();
    let outcome = testkit::run_in(Path::new(PGDROP), argv, input.as_bytes(), env)
        .expect("run pgdrop postgres --single");
    let (stdout, stderr) = (outcome.stdout_text(), outcome.stderr_text());
    assert_eq!(
        outcome.status,
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

/// Every value single-user mode printed for a column named `column`, in
/// order: the `printatt` lines (`src/backend/access/common/printtup.c:423`).
fn values<'a>(stdout: &'a str, column: &str) -> Vec<&'a str> {
    let marker = format!(": {column} = \"");
    stdout
        .lines()
        .filter_map(|line| line.split_once(&marker))
        .filter_map(|(_, rest)| rest.split_once("\"\t"))
        .map(|(value, _)| value)
        .collect()
}

/// The acceptance line: through the subcommand, not a `postgres` link, on a
/// cluster from a bare `pgdrop initdb`, `select version()` names pgrust.
#[test]
fn pgdrop_postgres_single_answers_select_version() {
    let scratch = Scratch::new("embedded-version");
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let (stdout, _) = single(
        &pgdata,
        &[],
        &server_env(&scratch.0),
        "select version() as v;\n",
    );
    let version = values(&stdout, "v");
    assert_eq!(version.len(), 1, "stdout: {stdout}");
    assert!(
        version[0].starts_with("PostgreSQL 18.6 (pgrust "),
        "{version:?}"
    );
}

/// `src/test/regress/sql/infinite_recurse.sql:4`-`:5` and `:27`, expected
/// `src/test/regress/expected/infinite_recurse.out:21`-`:24`: SQLSTATE 54001,
/// "stack depth limit exceeded" — the guard fires before the stack ends,
/// rather than the server dying of SIGSEGV. (Single-user mode has no
/// `\set VERBOSITY`; its error report goes to stderr in full, so the test
/// looks for the primary message there. The `powerpc64` skip at `:16`-`:20`
/// does not apply to the lanes this runs on.)
///
/// Run under pgrust's README's `-c max_stack_depth=60000`, which C's own
/// check refuses unless the stack rlimit is at least that plus
/// `STACK_DEPTH_SLOP` (`src/backend/utils/misc/stack_depth.c:163`): the
/// server accepting it proves pgdrop raised the rlimit, and the recursion
/// ending in 54001 proves the thread pg_main runs on has the stack that
/// rlimit promises.
#[test]
fn infinite_recurse() {
    const MAX_STACK_DEPTH_KB: u64 = 60000;
    const STACK_DEPTH_SLOP: u64 = 512 * 1024;
    if let Some((_, Limit::Bytes(hard))) = stack_rlimit()
        && hard < MAX_STACK_DEPTH_KB * 1024 + STACK_DEPTH_SLOP
    {
        eprintln!(
            "{}: the hard stack rlimit, {hard} bytes, is below max_stack_depth = {MAX_STACK_DEPTH_KB}kB",
            testkit::reference::SKIP_FLAG
        );
        return;
    }

    let scratch = Scratch::new("embedded-recurse");
    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let (stdout, stderr) = single(
        &pgdata,
        &["-c", "max_stack_depth=60000"],
        &server_env(&scratch.0),
        "show max_stack_depth;\n\
         create function infinite_recurse() returns int as 'select infinite_recurse()' language sql;\n\
         select infinite_recurse();\n",
    );
    assert_eq!(
        values(&stdout, "max_stack_depth"),
        ["60000kB"],
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("ERROR:  stack depth limit exceeded"),
        "stderr: {stderr}"
    );
}
