//! NAT-409: `pgdrop start` and `pgdrop stop`, end to end against pgrust.
//!
//! `start` has no upstream counterpart, but `stop` is `pg_ctl stop`, and the
//! assertions of `src/bin/pg_ctl/t/001_start_stop.pl` that carry over are
//! stolen here in that file's order, each citing its line: a second start
//! on a running data directory fails (`:60`), `stop` succeeds (`:62`). The
//! third, "second pg_ctl stop fails" (`:64`), is inverted on purpose: the
//! issue asks for an idempotent `stop` (`docs/divergences.md`).
//!
//! Every server runs on pgrust through this binary, so nothing here needs a
//! reference installation and nothing SKIPs. Each test points
//! `XDG_CACHE_HOME` into its scratch directory, where the server extracts
//! the embedded share files, and a guard stops whatever it started even
//! when an assertion fails.

#![cfg(unix)]
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A scratch directory under Cargo's target tmpdir, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgdrop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }

    fn pgdrop(&self, args: &[&str]) -> Output {
        Command::new(PGDROP)
            .args(args)
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env_remove("PGDATA")
            .env_remove("PGRUST_PGSHAREDIR")
            .env_remove("PGRUST_TZDIR")
            .stdin(Stdio::null())
            .output()
            .expect("run pgdrop")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stops the cluster in `datadir` when dropped, so a failed assertion does
/// not leave a server behind.
struct Running<'a> {
    scratch: &'a Scratch,
    started: Started,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self
            .scratch
            .pgdrop(&["stop", "--datadir", &self.started.datadir]);
    }
}

/// What `start --json` printed.
#[derive(Debug, Clone)]
struct Started {
    uri: String,
    pid: u32,
    datadir: String,
}

impl Started {
    /// The run directory: the socket's directory, which the default data
    /// directory sits in.
    fn run_dir(&self) -> String {
        let socket_dir = self
            .uri
            .strip_prefix("postgresql://postgres@")
            .and_then(|rest| rest.split_once(':'))
            .map(|(host, _)| host.replace("%2F", "/"))
            .expect("a Unix-socket URI");
        assert!(!socket_dir.contains('%'), "{socket_dir}");
        socket_dir
    }
}

/// `"key": value` from the one-line `--json` object. The values here never
/// hold a `"`, a `\` or a `,`.
fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let start = json.find(&format!("\"{key}\": ")).expect(key) + key.len() + 4;
    let rest = &json[start..];
    let end = rest.find([',', '}']).expect("end of value");
    rest[..end].trim_matches('"')
}

fn start<'a>(scratch: &'a Scratch, extra: &[&str]) -> Running<'a> {
    let mut argv = vec!["start", "--json"];
    argv.extend_from_slice(extra);
    let output = scratch.pgdrop(&argv);
    assert_success(&output, "pgdrop start");
    let json = String::from_utf8(output.stdout).expect("UTF-8");
    assert!(json.ends_with("}\n"), "{json:?}");
    Running {
        scratch,
        started: Started {
            uri: json_field(&json, "uri").to_owned(),
            pid: json_field(&json, "pid").parse().expect("a PID"),
            datadir: json_field(&json, "datadir").to_owned(),
        },
    }
}

fn assert_success(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what}: {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `select 1` through `pgdrop psql` on the cluster's socket. (`pgdrop psql
/// "$uri"` needs rpsql to expand a URI given as the database name, which it
/// does not do yet.)
fn select_1(scratch: &Scratch, started: &Started) {
    let run_dir = started.run_dir();
    let output = scratch.pgdrop(&[
        "psql", "-X", "-A", "-t", "-h", &run_dir, "-p", "5432", "-U", "postgres", "-c", "select 1",
        "postgres",
    ]);
    assert_success(&output, "pgdrop psql");
    assert_eq!(output.stdout, b"1\n");
}

fn postmaster_pid(datadir: &str) -> Option<u32> {
    let text = std::fs::read_to_string(Path::new(datadir).join("postmaster.pid")).ok()?;
    text.lines().next()?.parse().ok()
}

#[test]
fn start_answers_select_1_and_stop_removes_everything_twice() {
    let scratch = Scratch::new("start-stop");
    let running = start(&scratch, &[]);
    let started = running.started.clone();
    let run_dir = started.run_dir();
    assert_eq!(started.datadir, format!("{run_dir}/data"));
    assert_eq!(postmaster_pid(&started.datadir), Some(started.pid));
    select_1(&scratch, &started);

    // 001_start_stop.pl:62, 'pg_ctl stop'.
    let stop = scratch.pgdrop(&["stop", "--datadir", &started.datadir]);
    assert_success(&stop, "pgdrop stop");
    assert!(stop.stdout.is_empty() && stop.stderr.is_empty(), "{stop:?}");
    assert!(!Path::new(&run_dir).exists(), "{run_dir} is still there");

    // 001_start_stop.pl:64 is 'second pg_ctl stop fails'; `stop` is
    // idempotent instead.
    let again = scratch.pgdrop(&["stop", "--datadir", &started.datadir]);
    assert_success(&again, "second pgdrop stop");
    drop(running);
}

#[test]
fn a_second_start_on_a_running_datadir_fails_and_keep_keeps_it() {
    let scratch = Scratch::new("second-start");
    let datadir = scratch.0.join("data");
    let datadir = datadir.to_str().expect("UTF-8");
    let running = start(&scratch, &["--datadir", datadir, "--keep"]);
    let started = running.started.clone();
    assert_eq!(started.datadir, datadir);

    // 001_start_stop.pl:60, 'second pg_ctl start fails'.
    let second = scratch.pgdrop(&["start", "--datadir", datadir]);
    assert!(!second.status.success(), "{second:?}");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("the server exited during startup"),
        "{stderr}"
    );
    // The failed start touched nothing of the running cluster's.
    assert_eq!(postmaster_pid(datadir), Some(started.pid));
    select_1(&scratch, &started);

    let stop = scratch.pgdrop(&["stop", "--datadir", datadir]);
    assert_success(&stop, "pgdrop stop");
    assert!(Path::new(datadir).join("PG_VERSION").is_file(), "--keep");
    assert!(!Path::new(datadir).join("postmaster.pid").exists());
    assert!(!Path::new(datadir).join("pgdrop.start").exists());
    assert!(Path::new(&started.run_dir()).join("server.log").is_file());
    std::fs::remove_dir_all(started.run_dir()).expect("remove the kept run directory");
    drop(running);
}

/// Review of PR #99: `start` removes, on failure and on `stop`, only a data
/// directory it created. A directory of someone's files is refused by
/// initdb and left as it was; an empty one is minted into, and `stop`
/// leaves it and its cluster, taking only the record.
#[test]
fn a_failed_start_never_removes_a_directory_it_did_not_create() {
    let scratch = Scratch::new("not-mine");
    let full = scratch.0.join("full");
    std::fs::create_dir(&full).expect("create a directory");
    std::fs::write(full.join("notes.txt"), "mine\n").expect("write a file");
    let failed = scratch.pgdrop(&["start", "--datadir", full.to_str().expect("UTF-8")]);
    assert!(!failed.status.success(), "{failed:?}");
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(stderr.contains("initdb failed"), "{stderr}");
    assert_eq!(
        std::fs::read_to_string(full.join("notes.txt"))
            .ok()
            .as_deref(),
        Some("mine\n"),
        "{stderr}"
    );

    let empty = scratch.0.join("empty");
    std::fs::create_dir(&empty).expect("create a directory");
    let empty = empty.to_str().expect("UTF-8");
    let running = start(&scratch, &["--datadir", empty]);
    let started = running.started.clone();
    select_1(&scratch, &started);
    let stop = scratch.pgdrop(&["stop", "--datadir", empty]);
    assert_success(&stop, "pgdrop stop");
    assert!(Path::new(empty).join("PG_VERSION").is_file());
    assert!(!Path::new(empty).join("pgdrop.start").exists());
    assert!(!Path::new(&started.run_dir()).exists());
    drop(running);
}

/// The issue's acceptance: two concurrent starts never collide.
#[test]
fn two_concurrent_starts_never_collide() {
    let scratch = Scratch::new("concurrent");
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| start(&scratch, &[]));
        let two = scope.spawn(|| start(&scratch, &[]));
        (
            one.join().expect("first start"),
            two.join().expect("second start"),
        )
    });
    assert_ne!(one.started.run_dir(), two.started.run_dir());
    assert_ne!(one.started.pid, two.started.pid);
    select_1(&scratch, &one.started);
    select_1(&scratch, &two.started);
    for running in [&one, &two] {
        let stop = scratch.pgdrop(&["stop", "--datadir", &running.started.datadir]);
        assert_success(&stop, "pgdrop stop");
        assert!(!Path::new(&running.started.run_dir()).exists());
    }
}

#[test]
fn start_refuses_a_bad_command_line() {
    let scratch = Scratch::new("refuse");
    for (args, message) in [
        (&["--set", "fsync"][..], "--set fsync requires a value"),
        (&["--set", "port=1"][..], "--set cannot change port"),
        (&["--foreground"][..], "--foreground is not implemented yet"),
    ] {
        let mut argv = vec!["start"];
        argv.extend_from_slice(args);
        let output = scratch.pgdrop(&argv);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with("pgdrop: error: ") && stderr.contains(message),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn stop_without_a_datadir_says_so() {
    let scratch = Scratch::new("no-datadir");
    let output = scratch.pgdrop(&["stop"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "pgdrop: error: no data directory specified and environment variable PGDATA unset\n"
    );
}
