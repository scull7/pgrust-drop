//! Sections of `src/test/regress/sql/psql.sql` (PostgreSQL 18.6), each piped
//! through C psql and through rpsql the way pg_regress runs the file —
//! `psql -X -a -q -d <db> < psql.sql` (`pg_regress_main.c:74`-`:75`) — against
//! one PostgreSQL 18 cluster started from the reference tools, with stdout,
//! stderr and the exit status compared byte for byte.
//!
//! The sections are vendored from REL_18_6 under `tests/regress/` (see its
//! README). A line that needs a command another Linear issue owns is cut from
//! the input *for both sides*, by exact text, with its owner named; the gate
//! is never narrowed any other way. Without the reference tools the gate
//! prints `SKIP (flagged, not silent)`; CI installs PostgreSQL 18 on every
//! lane and sets `PGDROP_REQUIRE_REF=1`, so there it runs.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::Command;

use testkit::Gate;
use testkit::env::Environment;
use testkit::reference;

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// A PostgreSQL 18 cluster from the reference tools, started for one gate
/// and stopped with it. It trusts local connections and listens on its Unix
/// socket only.
struct Cluster {
    bin: PathBuf,
    dir: PathBuf,
    port: u16,
}

impl Cluster {
    /// `None`, with the skip announced, when `initdb`, `pg_ctl` and `psql`
    /// are not all present in one reference installation.
    fn start(port: u16) -> Option<Self> {
        let Some(initdb) = reference::find("initdb") else {
            reference::skip("initdb");
            return None;
        };
        let bin = initdb.parent()?.to_path_buf();
        for tool in ["pg_ctl", "psql"] {
            if !bin.join(tool).is_file() {
                reference::skip(tool);
                return None;
            }
        }

        let dir = std::env::temp_dir().join(format!("rpsql-gate-{port}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        let data = dir.join("data");

        let initdb = Command::new(bin.join("initdb"))
            .args(["-D".as_ref(), data.as_os_str()])
            .args(["-U", "gateuser", "--auth", "trust", "--no-sync"])
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        assert!(
            initdb.status.success(),
            "reference initdb failed: {initdb:?}"
        );

        let start = Command::new(bin.join("pg_ctl"))
            .args(["-D".as_ref(), data.as_os_str()])
            .arg("-w")
            .arg("-o")
            .arg(format!(
                "-p {port} -k {} -c listen_addresses=",
                dir.display()
            ))
            .args(["-l".as_ref(), dir.join("log").as_os_str()])
            .arg("start")
            .env("LC_ALL", "C")
            .output()
            .ok()?;
        assert!(
            start.status.success(),
            "reference pg_ctl start failed: {start:?}"
        );

        Some(Self { bin, dir, port })
    }

    /// `psql -X -a -q` connected to this cluster, reading `script` on stdin,
    /// through the reference psql and through rpsql.
    fn regress_gate(&self, script: &str) -> Gate {
        Gate::new(self.bin.join("psql"), RPSQL)
            .with_env(Environment::postgres_test("pg_regress"))
            .with_args(["-X", "-a", "-q"])
            .arg("-h")
            .arg(self.dir.as_os_str())
            .with_args([
                "-p",
                &self.port.to_string(),
                "-U",
                "gateuser",
                "-d",
                "postgres",
            ])
            .with_stdin(script.as_bytes().to_vec())
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args(["-D".as_ref(), self.dir.join("data").as_os_str()])
            .args(["-m", "immediate", "stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `script` without the lines equal to one of `cut`, each of which must be
/// present exactly once — a cut that no longer matches is a failure, not a
/// silent widening.
fn without_lines(script: &str, cut: &[(&str, &str)]) -> String {
    for (line, owner) in cut {
        let hits = script.lines().filter(|l| l == line).count();
        assert_eq!(
            hits, 1,
            "cut line {line:?} ({owner}) must occur exactly once"
        );
    }
    script
        .split_inclusive('\n')
        .filter(|l| {
            !cut.iter()
                .any(|(line, _)| l.trim_end_matches('\n') == *line)
        })
        .collect()
}

/// psql.sql:908-1140, `-- tests for \if ... \endif`: variables, `\if`,
/// `\elif`, `\else`, `\endif`, and every command upstream has, skipped inside
/// a false branch.
#[test]
fn if_section_matches_c_psql() {
    let script = without_lines(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/regress/psql_if.sql"),
        )
        .expect("the vendored section"),
        &[
            // psql.sql:1034, in an active branch: `do_pset` is NAT-400's.
            (
                "\\pset fieldsep | `nosuchcommand` :foo :'foo' :\"foo\"",
                "NAT-400",
            ),
            // psql.sql:1121: `\sf` (`exec_command_sf_sv`, `command.c:2982`)
            // is not on any issue's slice yet; NAT-402's PR lists it.
            ("\\sf silly_function(int)", "unassigned"),
        ],
    );
    let Some(cluster) = Cluster::start(55_402) else {
        return;
    };
    cluster.regress_gate(&script).assert_clean();
}

#[test]
fn a_cut_line_that_is_not_there_fails_loudly() {
    let result = std::panic::catch_unwind(|| without_lines("a\nb\n", &[("c", "nobody")]));
    assert!(result.is_err());
    assert_eq!(without_lines("a\nb\nc\n", &[("b", "someone")]), "a\nc\n");
}
