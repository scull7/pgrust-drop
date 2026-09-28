//! `\encoding`, the `ENCODING` variable and `PrintNotifications` (NAT-403),
//! gated byte for byte against C psql.
//!
//! `001_basic.pl:110`-`:134` (ported in `t_001_basic.rs`) checks `ENCODING`
//! and the two notification lines by pattern. This gate runs further
//! scripts written here through rpsql and through C psql against one
//! PostgreSQL 18 cluster, and compares stdout, stderr and the exit status as
//! raw bytes. The one normalization is the notifying backend's PID, which
//! differs from one connection to the next: every `with PID <digits>.` is
//! rewritten to `with PID N.` on both sides. The cluster and C psql come from
//! the lane's reference installation; without them the gate prints
//! `SKIP (flagged, not silent)`, and CI's `PGDROP_REQUIRE_REF=1` turns that
//! into a failure.
//!
//! What the scripts reach: `ENCODING` as `SyncVariables` sets it
//! (`command.c:4590`), with and without `PGCLIENTENCODING`; `\encoding` with
//! no argument, with a name, with a spelling it cleans (`latin-1`), with one
//! the server refuses and inside an aborted transaction (`command.c:1620`);
//! its extra-argument warning; `SET client_encoding` tracked after the query
//! (`common.c:1292`); and notifications with and without a payload, an empty
//! payload, and a payload the server converts to LATIN1 (`common.c:746`).

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::io::Write as _;
use std::path::Path;
use std::process::{Output, Stdio};

use testkit::reference;

use regress::{Cluster, first_difference};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

const ENCODING_PORT: u16 = 55_416;

const SCRIPT: &str = "\\echo :ENCODING\n\
    \\encoding\n\
    \\encoding latin-1\n\
    \\encoding\n\
    \\echo :ENCODING\n\
    \\encoding klingon\n\
    \\encoding\n\
    \\encoding UTF8 extra\n\
    \\echo :ENCODING\n\
    set client_encoding = 'SQL_ASCII';\n\
    \\echo :ENCODING\n\
    reset client_encoding;\n\
    \\echo :ENCODING\n\
    begin;\n\
    select 1/0;\n\
    \\encoding LATIN2\n\
    \\echo :ENCODING\n\
    rollback;\n\
    listen foo;\n\
    notify foo;\n\
    notify foo, 'bar';\n\
    notify foo, '';\n\
    select pg_notify('foo', 'one'), pg_notify('foo', 'two');\n\
    \\encoding LATIN1\n\
    select pg_notify('foo', chr(233));\n\
    \\encoding UTF8\n\
    listen \"b\u{e9}r\";\n\
    notify \"b\u{e9}r\", 'x';\n\
    \\echo done\n";

/// One run: psql's arguments, the environment it adds, and its stdin.
type Run = (
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
    &'static str,
);

/// What each psql is run with.
const RUNS: &[Run] = &[
    (&[], &[], SCRIPT),
    (&["-v", "ON_ERROR_STOP=1"], &[], SCRIPT),
    (&[], &[("PGCLIENTENCODING", "LATIN1")], SCRIPT),
    (&["-c", "\\encoding", "-c", "\\encoding nosuch"], &[], ""),
    (
        &["-c", "\\echo :ENCODING"],
        &[("PGCLIENTENCODING", "EUC_JP")],
        "",
    ),
];

/// Action: one run of `psql`.
fn run(cluster: &Cluster, psql: &Path, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
    let mut command = cluster.command(psql);
    command
        .arg("-X")
        .args(args)
        .env_remove("PGCLIENTENCODING")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("psql starts");
    let mut input = child.stdin.take().expect("a stdin pipe");
    let stdin = stdin.to_owned();
    let feeder = std::thread::spawn(move || {
        let _ = input.write_all(stdin.as_bytes());
    });
    let output = child.wait_with_output().expect("psql's output is read");
    feeder.join().expect("stdin is fed");
    output
}

/// Every `with PID <digits>.` as `with PID N.`: the one thing that differs
/// between two connections' notifications.
fn normalize_pids(bytes: &[u8]) -> Vec<u8> {
    const MARK: &[u8] = b"with PID ";
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(at) = rest.windows(MARK.len()).position(|w| w == MARK) {
        out.extend_from_slice(&rest[..at + MARK.len()]);
        rest = &rest[at + MARK.len()..];
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            out.push(b'N');
        }
        rest = &rest[digits..];
    }
    out.extend_from_slice(rest);
    out
}

#[test]
fn the_pid_is_the_only_thing_normalized() {
    assert_eq!(
        normalize_pids(b"x with PID 123.\nwith PID 4.\nPID 5\n"),
        b"x with PID N.\nwith PID N.\nPID 5\n"
    );
    assert_eq!(normalize_pids(b"with PID .\n"), b"with PID .\n");
}

#[test]
fn encoding_and_notifications_match_c_psql() {
    let Some(cluster) = Cluster::start(ENCODING_PORT) else {
        return;
    };
    let Some(c_psql) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    for (args, env, stdin) in RUNS {
        let theirs = run(&cluster, &c_psql, args, env, stdin);
        let ours = run(&cluster, Path::new(RPSQL), args, env, stdin);
        let at = format!("psql -X {args:?} with {env:?}");
        if let Some(diff) = first_difference(
            &normalize_pids(&theirs.stdout),
            &normalize_pids(&ours.stdout),
        ) {
            panic!("{at}: stdout: {diff}");
        }
        if let Some(diff) = first_difference(&theirs.stderr, &ours.stderr) {
            panic!("{at}: stderr: {diff}");
        }
        assert_eq!(
            theirs.status.code(),
            ours.status.code(),
            "{at}: exit status"
        );
    }
}
