//! NAT-410: how long a throwaway cluster takes, and where the binary's bytes
//! go. The calculations are `pgdrop::measure`'s; this file is the edge that
//! spawns, signals and times.
//!
//! Run it in the profile under test, which is also what CI does:
//!
//! ```text
//! cargo test --all-features --bench startup      # dev profile, as CI builds
//! PGDROP_STARTUP_RUNS=50 cargo test --all-features --bench startup
//! ```
//!
//! `harness = false`, so it prints its report straight to stdout. It does not
//! assert on a budget yet: the budgets are set from the baselines this
//! records (NAT-410's next slice).
//!
//! The server runs as `<scratch>/bin/postgres`, a hard link to pgdrop, beside
//! a `<scratch>/share/timezone` linked to this machine's timezone database —
//! the one share file pgdrop does not embed yet — exactly as
//! `tests/template_boot.rs` runs it; `timezonesets` and `tsearch_data` come
//! from the embedded copy, extracted into `<scratch>/cache`. One unmeasured
//! warm-up run pays that extraction and the page cache's first read of the
//! binary; the report is over the runs after it.

#![cfg(unix)]
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pgdrop::measure::{self, Phase, Run, SizeBreakdown};

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// Runs measured when `PGDROP_STARTUP_RUNS` is unset.
const DEFAULT_RUNS: usize = 20;

/// How long any one phase may take before the bench gives up on it.
const PHASE_TIMEOUT: Duration = Duration::from_mins(1);

/// The server's port. It listens on a Unix socket only, in a directory of
/// its own, so the number only names the socket file.
const PORT: &str = "5432";

fn main() {
    let runs = std::env::var("PGDROP_STARTUP_RUNS")
        .ok()
        .map_or(DEFAULT_RUNS, |value| {
            value
                .parse()
                .expect("PGDROP_STARTUP_RUNS is a number of runs")
        });
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let binary = std::fs::metadata(PGDROP).expect("the pgdrop binary").len();
    print!(
        "{}",
        measure::render_size(profile, &SizeBreakdown::of_this_build(binary))
    );

    let scratch = Scratch::new();
    let postgres = install(&scratch.0);
    let _warm_up = one_run(&scratch.0, &postgres, 0);
    let measured: Vec<Run> = (1..=runs)
        .map(|n| one_run(&scratch.0, &postgres, n))
        .collect();
    print!("{}", measure::render_startup(profile, &measured));
}

/// A scratch directory under Cargo's target tmpdir — the same filesystem as
/// the pgdrop binary, so it can be hard-linked — removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgdrop-startup-{}", std::process::id()));
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

/// `<scratch>/bin/postgres` and `<scratch>/share/timezone`; see the header.
fn install(scratch: &Path) -> PathBuf {
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin).expect("create bin/");
    std::fs::create_dir_all(scratch.join("share")).expect("create share/");
    let postgres = bin.join("postgres");
    if std::fs::hard_link(PGDROP, &postgres).is_err() {
        std::fs::copy(PGDROP, &postgres).expect("copy pgdrop");
    }
    let tzdir = rinitdb::RealTzSource::from_env()
        .expect("a timezone database on this machine")
        .tzdir()
        .to_path_buf();
    std::os::unix::fs::symlink(tzdir, scratch.join("share/timezone")).expect("link timezone");
    postgres
}

/// Action: one cluster's life, timed phase by phase, then its directory
/// removed.
fn one_run(scratch: &Path, postgres: &Path, n: usize) -> Run {
    let pgdata = scratch.join(format!("data-{n}"));
    let socket_dir = scratch.join(format!("sock-{n}"));
    std::fs::create_dir_all(&socket_dir).expect("create the socket directory");
    let mut run = Run::default();

    let started = Instant::now();
    let mut initdb = Command::new(PGDROP);
    initdb
        .arg("initdb")
        .args(["-U", "postgres", "--no-sync"])
        .arg(&pgdata);
    expect_success(&mut initdb, "pgdrop initdb");
    run.set(Phase::Initdb, started.elapsed());

    let started = Instant::now();
    let mut server = Command::new(postgres)
        .arg("-D")
        .arg(&pgdata)
        .arg("-k")
        .arg(&socket_dir)
        .args(["-p", PORT, "-c", "listen_addresses="])
        .env_remove(pgdrop::share::SHAREDIR_VAR)
        .env_remove("PGRUST_TZDIR")
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(scratch.join(format!("server-{n}.log"))).expect("log"))
        .spawn()
        .expect("spawn postgres");
    wait_until_ready(&mut server, &pgdata, scratch, n);
    run.set(Phase::Ready, started.elapsed());

    let started = Instant::now();
    let mut psql = Command::new(PGDROP);
    psql.arg("psql")
        .args(["-X", "-A", "-t", "-h"])
        .arg(&socket_dir)
        .args(["-p", PORT, "-U", "postgres", "-c", "select 1", "postgres"]);
    let stdout = expect_success(&mut psql, "pgdrop psql");
    run.set(Phase::FirstSelect, started.elapsed());
    assert_eq!(stdout, b"1\n", "select 1 answered {stdout:?}");

    let started = Instant::now();
    fast_shutdown(&mut server);
    run.set(Phase::Stop, started.elapsed());

    std::fs::remove_dir_all(&pgdata).expect("remove the cluster");
    std::fs::remove_dir_all(&socket_dir).expect("remove the socket directory");
    run
}

/// Run `command` to completion: exit 0, or panic with its stderr. Its stdout.
fn expect_success(command: &mut Command, what: &str) -> Vec<u8> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("spawn {what}: {err}"));
    assert!(
        output.status.success(),
        "{what}: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// Poll `postmaster.pid` the way `pg_ctl start` does (`pg_ctl.c:597`), but
/// every millisecond rather than every 100: the wait is what is measured.
fn wait_until_ready(server: &mut Child, pgdata: &Path, scratch: &Path, n: usize) {
    let pidfile = pgdata.join("postmaster.pid");
    let deadline = Instant::now() + PHASE_TIMEOUT;
    loop {
        if std::fs::read_to_string(&pidfile)
            .is_ok_and(|text| measure::postmaster_ready(&text, server.id()))
        {
            return;
        }
        let log =
            || std::fs::read_to_string(scratch.join(format!("server-{n}.log"))).unwrap_or_default();
        if let Some(status) = server.try_wait().expect("poll postgres") {
            panic!("postgres exited during startup: {status}\n{}", log());
        }
        if Instant::now() > deadline {
            let _ = server.kill();
            panic!("postgres not ready after {PHASE_TIMEOUT:?}\n{}", log());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// `pg_ctl stop`'s default, fast shutdown: `SIGINT` (`pg_ctl.c:80`), sent to
/// this child's PID with kill(1) — pgdrop binds no libc and forbids `unsafe`,
/// and std can only send `SIGKILL` — then wait for the server to exit.
fn fast_shutdown(server: &mut Child) {
    let pid = OsString::from(server.id().to_string());
    let mut kill = Command::new("kill");
    kill.arg("-INT").arg(&pid);
    expect_success(&mut kill, "kill -INT");
    let deadline = Instant::now() + PHASE_TIMEOUT;
    loop {
        if let Some(status) = server.try_wait().expect("poll postgres") {
            assert!(status.success(), "postgres stopped with {status}");
            return;
        }
        if Instant::now() > deadline {
            let _ = server.kill();
            panic!("postgres still running {PHASE_TIMEOUT:?} after SIGINT");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
