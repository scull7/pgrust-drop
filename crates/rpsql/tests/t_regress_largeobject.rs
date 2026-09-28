//! Port of `src/test/regress/sql/largeobject.sql` (PostgreSQL 18.6), for
//! NAT-396's acceptance: `\lo_import`, `\lo_export`, `\lo_list` and
//! `\lo_unlink`, the way `pg_regress` runs them.
//!
//! The script runs through `rpsql -X -a -q` against a PostgreSQL 18 cluster,
//! in a database of its own, and is compared byte for byte with
//! `expected/largeobject.out` and with C psql's output on the same cluster.
//! The cluster needs the reference `initdb` and `pg_ctl`; without them the
//! gate prints `SKIP (flagged, not silent)`. What the commands do without a
//! server is pinned by `rpsql::large_obj`'s unit tests.
//!
//! The file is cut into its sections by `regress::split`, and every section
//! is run, in order, except for what needs a backslash command that has not
//! landed on `main` yet. Each exception names its command and the issue that
//! owns it, so the gate widens by deleting a line here:
//!
//! - `\getenv` (NAT-403): the section that reads `PG_ABS_SRCDIR` and
//!   `PG_ABS_BUILDDIR` is not run; `abs_srcdir` and `abs_builddir` come in
//!   through `-v` instead, the values `\getenv` would have set.
//! - `\gset` (NAT-402): the two sections that store `lo_from_bytea`'s OID,
//!   and the one that comments on the first of them, are not run; the one
//!   that tests export/import reversibility stops before its `\gset`.
//! - `\dl` (NAT-401): the `\lo_list` section stops before it.
//! - Notices (NAT-402): `-- Test resource management` raises one and is
//!   not run.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

mod regress;

use std::path::{Path, PathBuf};

use regress::{
    Cluster, LARGEOBJECT_OUT, LARGEOBJECT_SQL, Section, TENK_DATA, first_difference, sha256_hex,
    split,
};
use testkit::reference;

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// The port the live gate's cluster listens on.
const LARGEOBJECT_PORT: u16 = 55_396;

/// Sections not run yet: their header, and the command they wait for.
const NOT_RUN: [(&str, &str); 5] = [
    (
        "-- directory paths are passed to us in environment variables",
        "\\getenv (NAT-403)",
    ),
    ("-- Copy to another large object.", "\\gset (NAT-402)"),
    // `COMMENT ON LARGE OBJECT :newloid`, the OID the `\gset` above stores.
    (
        "-- Add a comment to it, as well, for pg_dump/pg_upgrade testing.",
        "\\gset (NAT-402)",
    ),
    // Its `DO` block's `RAISE NOTICE`: notices are printed from NAT-402's
    // stack on.
    ("-- Test resource management", "notices (NAT-402)"),
    (
        "-- This object is left in the database for pg_dump test purposes",
        "\\gset (NAT-402)",
    ),
];

/// Sections run up to, not including, a line: their header, and that line.
const RUN_UP_TO: [(&str, &str); 2] = [
    // `\dl` (NAT-401).
    (
        "-- Test psql's \\lo_list et al (we assume no other LOs exist yet)",
        "\\dl\n",
    ),
    // `\gset` (NAT-402).
    (
        "-- This is a hack to test that export/import are reversible",
        "SELECT lo_from_bytea(0, lo_get(:newloid_1)) AS newloid_2\n",
    ),
];

#[test]
fn the_vendored_files_are_the_ones_postgresql_18_6_ships() {
    // ADR-0008: vendored bytes come from the tag or the tarball.
    assert_eq!(
        sha256_hex(LARGEOBJECT_SQL.as_bytes()),
        regress::LARGEOBJECT_SQL_SHA256,
        "crates/rpsql/tests/regress/largeobject.sql is not REL_18_6's"
    );
    assert_eq!(
        sha256_hex(LARGEOBJECT_OUT.as_bytes()),
        regress::LARGEOBJECT_OUT_SHA256,
        "crates/rpsql/tests/regress/expected/largeobject.out is not REL_18_6's"
    );
    let tenk = std::fs::read(TENK_DATA).expect("the vendored tenk.data reads");
    assert_eq!(
        sha256_hex(&tenk),
        regress::TENK_DATA_SHA256,
        "crates/rpsql/tests/regress/data/tenk.data is not REL_18_6's"
    );
}

/// Calculation: `section` up to, not including, the input line `stop`, and
/// its expected output up to `-a`'s echo of that line.
fn up_to<'a>(section: &Section<'a>, stop: &str) -> Section<'a> {
    let cut = |text: &'a str| -> &'a str {
        let mut at = 0;
        for line in text.split_inclusive('\n') {
            if line == stop {
                return &text[..at];
            }
            at += line.len();
        }
        panic!("{:?} has no line {stop:?}", section.header)
    };
    Section {
        sql: cut(section.sql),
        expected: cut(section.expected),
        ..section.clone()
    }
}

/// Calculation: the script this gate runs and the output it expects, the
/// sections of `largeobject.sql` in order less [`NOT_RUN`] and cut by
/// [`RUN_UP_TO`].
fn gated_script() -> (String, String) {
    let sections = split(LARGEOBJECT_SQL, LARGEOBJECT_OUT)
        .expect("the vendored largeobject.sql and largeobject.out split");
    let headers: Vec<&str> = sections.iter().map(|s| s.header).collect();
    for header in NOT_RUN
        .iter()
        .map(|(h, _)| h)
        .chain(RUN_UP_TO.iter().map(|(h, _)| h))
    {
        assert!(
            headers.contains(header),
            "no section of largeobject.sql opens with {header:?}"
        );
    }
    let (mut sql, mut expected) = (String::new(), String::new());
    for section in &sections {
        if NOT_RUN.iter().any(|(h, _)| *h == section.header) {
            continue;
        }
        let section = match RUN_UP_TO.iter().find(|(h, _)| *h == section.header) {
            Some((_, stop)) => up_to(section, stop),
            None => section.clone(),
        };
        sql.push_str(section.sql);
        expected.push_str(section.expected);
    }
    (sql, expected)
}

#[test]
fn the_gated_script_keeps_every_lo_command_on_main() {
    let (sql, expected) = gated_script();
    // The acceptance's commands, each run at least once.
    for command in ["\\lo_list\n", "\\lo_list+\n", "\\lo_unlink 42\n"] {
        assert!(sql.contains(command), "{command:?}");
    }
    assert_eq!(sql.matches("\\lo_import :filename\n").count(), 2);
    assert_eq!(sql.matches("\\lo_export :newloid :filename\n").count(), 1);
    assert_eq!(sql.matches("\\lo_unlink :newloid\n").count(), 1);
    // Nothing that waits on another issue is left in.
    for command in ["\\getenv", "\\gset", "\\dl"] {
        assert!(!sql.contains(command), "{command:?}");
    }
    assert!(expected.ends_with("DROP ROLE regress_lo_user;\n"));
}

/// Action: a fresh database named `dbname` on `cluster`, and a fresh
/// `abs_builddir` with the `results` directory `pg_regress` makes in it.
fn prepare(cluster: &Cluster, dbname: &str) -> PathBuf {
    let mut conn = cluster.connect();
    let created = conn
        .exec(format!("CREATE DATABASE {dbname}").as_bytes())
        .expect("CREATE DATABASE is sent");
    assert!(
        created
            .iter()
            .all(|r| r.status() == rlibpq::ExecStatus::CommandOk),
        "CREATE DATABASE {dbname} failed"
    );
    let _ = conn.terminate();
    let builddir =
        std::env::temp_dir().join(format!("rpsql-largeobject-{}-{dbname}", std::process::id()));
    let _ = std::fs::remove_dir_all(&builddir);
    std::fs::create_dir_all(builddir.join("results")).expect("the results directory is made");
    builddir
}

/// Action: the gated script through `psql` in database `dbname`.
fn run(cluster: &Cluster, psql: &Path, dbname: &str, script: &str) -> Vec<u8> {
    let builddir = prepare(cluster, dbname);
    let srcdir = Path::new(TENK_DATA)
        .parent()
        .and_then(Path::parent)
        .expect("tenk.data sits in data/");
    let output = cluster.run_script_with(
        psql,
        dbname,
        &[
            ("abs_srcdir", &srcdir.display().to_string()),
            ("abs_builddir", &builddir.display().to_string()),
        ],
        script,
    );
    // `\lo_export` wrote lotest2.txt from the object `\lo_import` made of
    // lotest.txt, which the server's `lo_export()` wrote: the round trip
    // must give back the same bytes. None of the two is in the output.
    let read = |name: &str| std::fs::read(builddir.join("results").join(name)).ok();
    let (server, client) = (read("lotest.txt"), read("lotest2.txt"));
    let _ = std::fs::remove_dir_all(&builddir);
    assert!(
        server.is_some() && server == client,
        "{}: \\lo_export did not write back what \\lo_import read",
        psql.display()
    );
    output
}

/// `largeobject.sql`, less what waits on other issues, through rpsql and C
/// psql against a server.
#[test]
fn largeobject() {
    let Some(cluster) = Cluster::start(LARGEOBJECT_PORT) else {
        return;
    };
    let (sql, expected) = gated_script();

    let ours = run(&cluster, Path::new(RPSQL), "lo_rpsql", &sql);
    if let Some(diff) = first_difference(expected.as_bytes(), &ours) {
        panic!("rpsql vs largeobject.out: {diff}");
    }
    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = run(&cluster, &psql, "lo_psql", &sql);
            if let Some(diff) = first_difference(&theirs, &ours) {
                panic!("rpsql vs C psql: {diff}");
            }
        }
        None => reference::skip("psql"),
    }
}

/// The error paths `largeobject.sql` does not take, side by side with C
/// psql: the missing arguments (`command.c:2387`, `:2401`, `:2433`), an
/// unknown `\lo_` command, a file that cannot be opened, an object that
/// does not exist, the refusal inside an aborted transaction
/// (`large_obj.c:84`), the extra-argument warning, a comment that needs
/// escaping, and a `-c` action. OIDs differ between two databases, so
/// nothing here prints one.
#[test]
fn lo_errors_match_c_psql() {
    let Some(cluster) = Cluster::start(LARGEOBJECT_PORT + 1) else {
        return;
    };
    let Some(psql) = cluster.reference_psql() else {
        reference::skip("psql");
        return;
    };
    let script = concat!(
        "\\lo_import\n",
        "\\lo_export 42\n",
        "\\lo_unlink\n",
        "\\lo_frob\n",
        "\\lo_import /nonexistent/lotest.txt\n",
        "\\lo_unlink 4242\n",
        "\\lo_export 4242 :abs_builddir/results/none.txt\n",
        "\\lo_import :abs_srcdir/data/tenk.data 'it''s \\\\ commented'\n",
        "SELECT description FROM pg_description WHERE objoid = :LASTOID;\n",
        "\\set QUIET off\n",
        "\\lo_export :LASTOID :abs_builddir/results/tenk.txt\n",
        "\\set QUIET on\n",
        "\\lo_unlink :LASTOID extra arguments\n",
        "\\lo_list a b\n",
        "\\lo_list+ a b c\n",
        "BEGIN;\n",
        "SELECT 1/0;\n",
        "\\lo_unlink 1\n",
        "\\lo_import :abs_srcdir/data/tenk.data\n",
        "ROLLBACK;\n",
    );
    let srcdir = Path::new(TENK_DATA)
        .parent()
        .and_then(Path::parent)
        .expect("tenk.data sits in data/");
    let mut outputs = Vec::new();
    for (bin, dbname) in [(Path::new(RPSQL), "lo_err_rpsql"), (&psql, "lo_err_psql")] {
        let builddir = prepare(&cluster, dbname);
        outputs.push(cluster.run_script_with(
            bin,
            dbname,
            &[
                ("abs_srcdir", &srcdir.display().to_string()),
                ("abs_builddir", &builddir.display().to_string()),
            ],
            script,
        ));
        let exported = std::fs::read(builddir.join("results/tenk.txt")).ok();
        let _ = std::fs::remove_dir_all(&builddir);
        let tenk = std::fs::read(TENK_DATA).expect("the vendored tenk.data reads");
        assert!(
            exported.as_deref() == Some(tenk.as_slice()),
            "{}: \\lo_export did not write back tenk.data",
            bin.display()
        );
    }
    if let Some(diff) = first_difference(&outputs[1], &outputs[0]) {
        panic!("rpsql vs C psql: {diff}");
    }

    // `-c`, an `ACT_SINGLE_SLASH` action (`startup.c:392`): exit status and
    // both streams.
    let single = |bin: &Path| {
        cluster
            .command(bin)
            .args(["-X", "-c", "\\lo_unlink 4242", "-c", "\\lo_import"])
            .output()
            .expect("psql runs")
    };
    let (ours, theirs) = (single(Path::new(RPSQL)), single(&psql));
    assert_eq!(ours.status.code(), theirs.status.code());
    assert_eq!(
        String::from_utf8_lossy(&ours.stderr),
        String::from_utf8_lossy(&theirs.stderr)
    );
    assert_eq!(ours.stdout, theirs.stdout);
}
