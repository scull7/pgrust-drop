//! `\i`, `\include`, `\ir`, `\include_relative` and `\cd` (NAT-403), gated
//! byte for byte against C psql.
//!
//! Upstream has no test that runs them: `psql.sql:1048`, `:1074` and `:1075`
//! name `\cd`, `\i` and `\ir` only inside a false `\if` branch, to show they
//! are skipped there. So the gate is the method's other half: scripts written
//! here, a tree of them in a directory of their own, run through rpsql and
//! through C psql against one PostgreSQL 18 cluster, stdout, stderr and the
//! exit status compared as raw bytes with no normalizer. The cluster and C
//! psql come from the lane's reference installation; without them the gate
//! prints `SKIP (flagged, not silent)`, and CI's `PGDROP_REQUIRE_REF=1` turns
//! that into a failure.
//!
//! What the scripts reach: a file run in place and the outer one reading on
//! at its own line (`mainloop.c:60`, `:662`); `\ir` beside the file being
//! read (`command.c:4942`); names canonicalized before they are opened and
//! reported (`command.c:4934`), `-f ./main.sql` included; a file that is not
//! there, a directory that cannot be read, `\q` inside an included file, a
//! server error inside one, `~` (`expand_tilde`), `\i -` reading on from
//! stdin, `\cd` with and without an argument, `-c '\i …'`, stdin with no
//! `-f`, `ON_ERROR_STOP` stopping the including file, and a read error
//! failing its loop (`mainloop.c:172`) under `-f`, `-c` and `ON_ERROR_STOP`.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use testkit::reference;

use regress::{Cluster, first_difference};

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

const INCLUDE_PORT: u16 = 55_496;

/// The tree the scripts run in: `(path, contents)`, relative to its root.
const TREE: &[(&str, &str)] = &[
    (
        "main.sql",
        "\\echo main start\n\
         select 'main' as m;\n\
         \\i sub/a.sql\n\
         \\ir sub/a.sql\n\
         \\include ./sub//../sub/b.sql\n\
         \\i nosuch.sql\n\
         \\i\n\
         \\include_relative sub/err.sql\n\
         \\i ~/h.sql\n\
         \\i -\n\
         \\cd sub\n\
         \\i b.sql\n\
         \\ir sub/b.sql\n\
         \\cd /nonexistent-rpsql-include-gate\n\
         \\cd\n\
         \\i h.sql\n\
         \\i ..\n\
         \\i h.sql extra\n\
         \\echo main end\n\
         select 1/0;\n",
    ),
    (
        "sub/a.sql",
        "\\echo in a\n\
         \\ir b.sql\n\
         \\warn a warns\n\
         \\ir ../sub/./nested/../c.sql\n",
    ),
    (
        "sub/b.sql",
        "select 'b' as b;\n\
         \\q\n\
         select 'not reached';\n",
    ),
    ("sub/c.sql", "\\nosuch\n"),
    (
        "sub/err.sql",
        "\n\
         select nosuchcolumn;\n\
         \\warn after the error\n",
    ),
    ("home/h.sql", "\\echo in home\n"),
    (
        "stop.sql",
        "\\i sub/err.sql\n\
         \\echo not reached\n",
    ),
    (
        "stopdir.sql",
        "\\i sub\n\
         \\echo not reached\n",
    ),
];

/// What each psql is run with: arguments, and what it reads on stdin.
const RUNS: &[(&[&str], &str)] = &[
    (&["-f", "./main.sql"], "select 'from stdin';\n"),
    (&["-c", "\\ir sub/a.sql", "-c", "\\i nosuch.sql"], ""),
    (&["-v", "ON_ERROR_STOP=1", "-f", "stop.sql"], ""),
    (&["-f", "sub"], ""),
    (&["-c", "\\i sub", "-c", "select 'next'"], ""),
    (&["-v", "ON_ERROR_STOP=1", "-f", "stopdir.sql"], ""),
    (
        &[],
        "\\i sub/a.sql\n\\i sub/err.sql\n\\i nosuch.sql\nselect 'last';\n",
    ),
];

/// A fresh copy of [`TREE`] for one psql, since `\cd` moves only that psql.
fn write_tree(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    for (name, text) in TREE {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

/// Action: one run of `psql` in `root`, with `HOME` inside it.
fn run(cluster: &Cluster, psql: &Path, root: &Path, args: &[&str], stdin: &str) -> Output {
    write_tree(root);
    let mut child = cluster
        .command(psql)
        .arg("-X")
        .args(args)
        .current_dir(root)
        .env("HOME", root.join("home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("psql starts");
    let mut input = child.stdin.take().expect("a stdin pipe");
    let stdin = stdin.to_owned();
    let feeder = std::thread::spawn(move || {
        let _ = input.write_all(stdin.as_bytes());
    });
    let output = child.wait_with_output().expect("psql's output is read");
    feeder.join().expect("stdin is fed");
    output
}

#[test]
fn include_and_cd_match_c_psql() {
    let Some(cluster) = Cluster::start(INCLUDE_PORT) else {
        return;
    };
    let Some(c_psql) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    // The same path for both, so that the absolute names `\cd` and `~`
    // produce are the same bytes in both outputs.
    let root: PathBuf =
        std::env::temp_dir().join(format!("rpsql-include-gate-{}", std::process::id()));
    for (args, stdin) in RUNS {
        let theirs = run(&cluster, &c_psql, &root, args, stdin);
        let ours = run(&cluster, Path::new(RPSQL), &root, args, stdin);
        let at = format!("psql -X {args:?} with stdin {stdin:?}");
        if let Some(diff) = first_difference(&theirs.stdout, &ours.stdout) {
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
    let _ = std::fs::remove_dir_all(&root);
}
