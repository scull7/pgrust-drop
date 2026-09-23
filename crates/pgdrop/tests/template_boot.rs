//! NAT-381 acceptance: a cluster `pgdrop initdb` expands from the embedded
//! template, with its rewritten `pg_control` and regenerated first WAL
//! segment, boots under pgrust `postgres --single -D <dir>`, and `select 1`
//! works. The same cluster shape boots under the reference C `postgres` too,
//! as a second oracle (`crates/rinitdb/tests/expanded_cluster.rs` does that
//! on its own; here both servers meet the one binary users run).
//!
//! This lives in pgdrop because pgdrop is where pgrust is linked (AGPL-3.0,
//! ADR-0003); rinitdb stays MIT and never reaches pgrust
//! (`scripts/check-license-wall.sh`).
//!
//! pgrust reads `timezonesets` and `timezone` from `<bindir>/../share` of the
//! executable it runs as (`find_my_exec`), and pgdrop does not embed those
//! yet (NAT-408). So the server runs as a hard link named `postgres` in a
//! scratch `bin/`, beside a `share/` of the test's own: an empty
//! `timezonesets/Default` (zero abbreviations, which `load_tzoffsets` takes)
//! and a link to this machine's timezone database. The pgrust half needs
//! nothing else and always runs. Without a reference installation the C
//! half prints `SKIP (flagged, not silent)` and passes;
//! `PGDROP_REQUIRE_REF=1` makes it fail instead.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use testkit::reference;

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A scratch directory under Cargo's target tmpdir — the same filesystem as
/// the pgdrop binary, so it can be hard-linked — removed when the test ends.
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

/// `<scratch>/bin/postgres`, a hard link to pgdrop (so `argv[0]` selects the
/// applet and `find_my_exec` lands in `<scratch>/bin`), and `<scratch>/share`
/// with the two things pgrust reads at startup: `timezonesets/Default`, empty,
/// and `timezone`, linked to the timezone database rinitdb reads too.
fn install(scratch: &Path) -> PathBuf {
    let bin = scratch.join("bin");
    let timezonesets = scratch.join("share/timezonesets");
    std::fs::create_dir_all(&bin).expect("create bin/");
    std::fs::create_dir_all(&timezonesets).expect("create share/timezonesets/");
    let postgres = bin.join("postgres");
    if std::fs::hard_link(PGDROP, &postgres).is_err() {
        std::fs::copy(PGDROP, &postgres).expect("copy pgdrop");
    }
    std::fs::write(timezonesets.join("Default"), b"").expect("write timezonesets/Default");
    let tzdir = rinitdb::RealTzSource::from_env()
        .expect("a timezone database on this machine")
        .tzdir()
        .to_path_buf();
    std::os::unix::fs::symlink(tzdir, scratch.join("share/timezone")).expect("link timezone");
    postgres
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

/// `<postgres> --single -D <pgdata> postgres` with `select 1`: exit 0, and
/// single-user mode's `printatt` line for the value
/// (`src/backend/access/common/printtup.c:423`). `version()` rides along so
/// the test can tell which server answered.
fn select_one(postgres: &Path, pgdata: &Path) -> String {
    let argv = [
        OsString::from("--single"),
        OsString::from("-D"),
        pgdata.into(),
        OsString::from("postgres"),
    ];
    let outcome = testkit::run_with_stdin(postgres, argv, b"select 1 as one, version() as v;\n")
        .expect("run postgres --single");
    let stdout = outcome.stdout_text();
    assert_eq!(
        outcome.status,
        Some(0),
        "{}\nstdout: {stdout}\nstderr: {}",
        postgres.display(),
        outcome.stderr_text()
    );
    assert!(
        stdout.contains("\t 1: one = \"1\"\t"),
        "{}\nstdout: {stdout}\nstderr: {}",
        postgres.display(),
        outcome.stderr_text()
    );
    stdout
        .lines()
        .find_map(|line| line.split_once("\t 2: v = \""))
        .and_then(|(_, rest)| rest.split_once("\"\t"))
        .map(|(version, _)| version.to_owned())
        .unwrap_or_default()
}

#[test]
fn the_expanded_template_boots_under_pgrust_single_user_mode() {
    let scratch = Scratch::new("template-boot");
    let pgrust = install(&scratch.0);

    let pgdata = scratch.0.join("data");
    pgdrop_initdb(&pgdata);
    let version = select_one(&pgrust, &pgdata);
    assert!(version.contains("(pgrust "), "{version}");

    // The second oracle: the reference C server, on a cluster of its own
    // from the same binary.
    let Some(reference_postgres) = reference::find_or_skip("postgres") else {
        return;
    };
    let c_pgdata = scratch.0.join("data-c");
    pgdrop_initdb(&c_pgdata);
    let version = select_one(&reference_postgres, &c_pgdata);
    assert!(
        version.starts_with("PostgreSQL 18.6") && !version.contains("pgrust"),
        "{version}"
    );
}
