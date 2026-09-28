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
//! the embedded share files, and `XDG_RUNTIME_DIR` and `HOME` there too, so
//! the current-cluster pointer is the test's own; it clears every variable
//! that names a cluster. A guard stops whatever it started even when an
//! assertion fails.

#![cfg(unix)]
#![allow(clippy::doc_markdown)]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant};

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

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(PGDROP);
        command.args(args);
        self.isolate(&mut command);
        command
    }

    /// This scratch's environment, and nothing on stdin.
    fn isolate(&self, command: &mut Command) {
        command
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_RUNTIME_DIR", self.runtime_dir())
            .env("HOME", self.0.join("home"))
            .env_remove("PGRUST_PGSHAREDIR")
            .env_remove("PGRUST_TZDIR")
            .env_remove("PGDROP_TEST_AUTO_PORTS")
            .stdin(Stdio::null());
        for name in [
            "PGDATA",
            "PGHOST",
            "PGHOSTADDR",
            "PGPORT",
            "PGUSER",
            "PGDATABASE",
            "PGSERVICE",
        ] {
            command.env_remove(name);
        }
    }

    fn runtime_dir(&self) -> PathBuf {
        self.0.join("run")
    }

    /// `$XDG_RUNTIME_DIR/pgdrop/current`.
    fn pointer_path(&self) -> PathBuf {
        self.runtime_dir().join("pgdrop").join("current")
    }

    /// The data directory the pointer names, `None` without one.
    fn pointer_datadir(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.pointer_path()).ok()?;
        let (_, datadir) = text.split_once("\ndatadir=")?;
        Some(datadir.strip_suffix('\n')?.to_owned())
    }

    /// `sh -c SCRIPT` with `pgdrop` on the `PATH` and this scratch's
    /// environment.
    fn sh(&self, script: &str) -> Output {
        let bin = Path::new(PGDROP).parent().expect("the binary's directory");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![bin.to_path_buf()];
        paths.extend(std::env::split_paths(&path));
        let mut sh = Command::new("sh");
        sh.arg("-c").arg(script);
        self.isolate(&mut sh);
        sh.env("PATH", std::env::join_paths(paths).expect("a PATH"))
            .output()
            .expect("run sh")
    }

    fn pgdrop(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run pgdrop")
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
    Running {
        scratch,
        started: parse_started(&json),
    }
}

fn parse_started(json: &str) -> Started {
    assert!(json.ends_with("}\n"), "{json:?}");
    let started = Started {
        uri: json_field(json, "uri").to_owned(),
        pid: json_field(json, "pid").parse().expect("a PID"),
        datadir: json_field(json, "datadir").to_owned(),
    };
    // `"port"` is the URI's.
    let port = json_field(json, "port");
    assert!(
        started.uri.ends_with(&format!(":{port}/postgres")),
        "{json:?}"
    );
    started
}

/// A `start --foreground --json` still running: its process, and what it
/// printed once the server was ready. Dropping it stops the cluster and
/// then `start` itself.
struct Attached<'a> {
    scratch: &'a Scratch,
    child: Child,
    started: Started,
    stderr: PathBuf,
}

impl Attached<'_> {
    /// `start` exits on its own; its status, within a minute.
    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for pgdrop start") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "pgdrop start --foreground did not exit; stderr:\n{}",
                self.stderr()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }
}

impl Started {
    /// The TCP port of a `--port auto` URI.
    fn tcp_port(&self) -> u16 {
        self.uri
            .strip_prefix("postgresql://postgres@127.0.0.1:")
            .and_then(|rest| rest.strip_suffix("/postgres"))
            .expect("a TCP URI")
            .parse()
            .expect("a port")
    }
}

impl Drop for Attached<'_> {
    fn drop(&mut self) {
        let _ = self
            .scratch
            .pgdrop(&["stop", "--datadir", &self.started.datadir]);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `start --foreground --json`, returned once it has printed its line. Its
/// stderr, which the server's log goes to, is a file in the scratch
/// directory.
fn start_attached<'a>(scratch: &'a Scratch, tag: &str, extra: &[&str]) -> Attached<'a> {
    let mut argv = vec!["start", "--foreground", "--json"];
    argv.extend_from_slice(extra);
    let stderr = scratch.0.join(format!("{tag}.stderr"));
    let mut child = scratch
        .command(&argv)
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&stderr).expect("create the stderr file"))
        .spawn()
        .expect("run pgdrop start --foreground");
    let mut line = String::new();
    BufReader::new(child.stdout.take().expect("stdout"))
        .read_line(&mut line)
        .expect("read what start printed");
    assert!(
        !line.is_empty(),
        "start --foreground printed nothing; stderr:\n{}",
        std::fs::read_to_string(&stderr).unwrap_or_default()
    );
    Attached {
        scratch,
        child,
        started: parse_started(&line),
        stderr,
    }
}

/// Action: the POSIX `kill` utility, as `pgdrop stop` signals.
fn send(signal: &str, pid: u32) {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill {signal} {pid}");
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

/// A bare `pgdrop psql -X -A -t -c QUERY`: no host, port, user or
/// database, so the pointer decides where it goes.
fn bare_psql(scratch: &Scratch, query: &str) -> Output {
    scratch.pgdrop(&["psql", "-X", "-A", "-t", "-c", query])
}

/// `select 1` through `pgdrop psql "$uri"`, the URI `start` printed, which
/// psql expands as a connection string (`startup.c:277`). The environment
/// names another host and user, so only the URI can get it there.
fn select_1(scratch: &Scratch, started: &Started) {
    let output = scratch
        .command(&["psql", "-X", "-A", "-t", "-c", "select 1", &started.uri])
        .env("PGHOST", scratch.0.join("nowhere"))
        .env("PGUSER", "nobody")
        .output()
        .expect("run pgdrop psql");
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

/// Review of PR #99: a server that crashed leaves its `postmaster.pid`
/// naming a process that is gone. `stop` takes that as already stopped
/// (`pg_ctl stop` fails "could not send stop signal") and removes what
/// `start` recorded, twice over.
#[test]
fn stop_after_a_crash_is_already_stopped() {
    let scratch = Scratch::new("crashed");
    let datadir = scratch.0.join("data");
    let run_dir = scratch.0.join("pgdrop-crashed-run");
    std::fs::create_dir(&datadir).expect("create the data directory");
    std::fs::create_dir(&run_dir).expect("create the run directory");
    let mut gone = Command::new("true").spawn().expect("run true");
    let pid = gone.id();
    gone.wait().expect("wait for true");
    std::fs::write(datadir.join("postmaster.pid"), format!("{pid}\n")).expect("write");
    std::fs::write(
        datadir.join("pgdrop.start"),
        format!(
            "pgdrop start 1\nremove_datadir=yes\nremove_run_dir=yes\nrun_dir={}\n",
            run_dir.display()
        ),
    )
    .expect("write");
    let datadir = datadir.to_str().expect("UTF-8");
    for attempt in ["pgdrop stop", "second pgdrop stop"] {
        let stop = scratch.pgdrop(&["stop", "--datadir", datadir]);
        assert_success(&stop, attempt);
        assert!(stop.stderr.is_empty(), "{stop:?}");
        assert!(
            !Path::new(datadir).exists() && !run_dir.exists(),
            "{attempt}"
        );
    }
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

/// `--foreground`: `start` stays attached to the server it prints, forwards
/// SIGINT (Ctrl-C), SIGTERM and SIGHUP to it as a fast shutdown and SIGQUIT
/// as an immediate one (`postmaster.c:2056`-`:2062`), as pg_ctl forwards
/// SIGINT while it waits for a server (`pg_ctl.c:851`-`:872`), and once the
/// server has exited removes what `stop` would and exits 0. The server's log
/// is on `start`'s stderr, and none of it on stdout.
#[test]
fn foreground_forwards_shutdown_signals_and_cleans_up() {
    let scratch = Scratch::new("foreground");
    for (signal, request) in [
        ("-INT", "fast shutdown request"),
        ("-TERM", "fast shutdown request"),
        ("-HUP", "fast shutdown request"),
        ("-QUIT", "immediate shutdown request"),
    ] {
        let mut attached = start_attached(&scratch, signal, &[]);
        let started = attached.started.clone();
        assert_ne!(started.pid, attached.child.id(), "the server is a child");
        assert_eq!(postmaster_pid(&started.datadir), Some(started.pid));
        assert!(
            Path::new(&started.datadir).join("pgdrop.start").is_file(),
            "stop's record"
        );
        select_1(&scratch, &started);

        send(signal, attached.child.id());
        let status = attached.wait();
        assert!(
            status.success(),
            "{signal}: {status}\n{}",
            attached.stderr()
        );
        let run_dir = started.run_dir();
        assert!(!Path::new(&run_dir).exists(), "{signal}: {run_dir}");
        let stderr = attached.stderr();
        assert!(
            stderr.contains(&format!("received {request}")),
            "{signal}: {stderr}"
        );
    }
}

/// `pgdrop stop` works on a foreground cluster as on a detached one, and
/// `start` then exits 0; with `--keep` the data directory stays, without
/// `start`'s record, as it does after a detached `--keep`.
#[test]
fn foreground_ends_when_pgdrop_stop_stops_it() {
    let scratch = Scratch::new("foreground-stop");
    let datadir = scratch.0.join("data");
    let datadir = datadir.to_str().expect("UTF-8");
    let mut attached = start_attached(&scratch, "keep", &["--datadir", datadir, "--keep"]);
    let started = attached.started.clone();
    select_1(&scratch, &started);

    let stop = scratch.pgdrop(&["stop", "--datadir", datadir]);
    assert_success(&stop, "pgdrop stop");
    let status = attached.wait();
    assert!(status.success(), "{status}\n{}", attached.stderr());
    assert!(Path::new(datadir).join("PG_VERSION").is_file(), "--keep");
    assert!(!Path::new(datadir).join("postmaster.pid").exists());
    assert!(!Path::new(datadir).join("pgdrop.start").exists());
    std::fs::remove_dir_all(started.run_dir()).expect("remove the kept run directory");
}

/// The issue's acceptance, literally: `pgdrop start --json && pgdrop psql
/// -c 'select 1' && pgdrop stop` with no arguments. The pointer names the
/// cluster until `stop` takes it with everything else; `stop` again has
/// nothing to do.
#[test]
fn start_psql_stop_need_no_arguments() {
    let scratch = Scratch::new("bare");
    let output = scratch.sh("pgdrop start --json && pgdrop psql -X -c 'select 1' && pgdrop stop");
    assert_success(&output, "the acceptance line");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    let (json, psql) = stdout.split_at(stdout.find('\n').expect("a line") + 1);
    let started = parse_started(json);
    assert_eq!(
        psql, " ?column? \n----------\n        1\n(1 row)\n\n",
        "{stdout}"
    );
    assert!(!Path::new(&started.run_dir()).exists());
    assert!(!scratch.pointer_path().exists(), "stop clears the pointer");
    let again = scratch.pgdrop(&["stop"]);
    assert_success(&again, "a second bare pgdrop stop");
    assert!(again.stderr.is_empty(), "{again:?}");
}

/// `--env` is eval-able: libpq's variables reach the cluster, and `PGDATA`
/// stops it. The pointer's `0700` directory is created on the way.
#[test]
fn env_is_eval_able_and_names_the_cluster() {
    let scratch = Scratch::new("env");
    let output = scratch.sh("eval \"$(pgdrop start --env --port auto)\" && \
         echo \"$PGHOST $PGPORT $PGUSER $PGDATA\" && \
         pgdrop psql -X -A -t -c 'show port' && pgdrop stop && test ! -e \"$PGDATA\"");
    assert_success(&output, "eval pgdrop start --env");
    let stdout = String::from_utf8(output.stdout).expect("UTF-8");
    let mut lines = stdout.lines();
    let vars: Vec<&str> = lines.next().expect("the variables").split(' ').collect();
    assert_eq!(vars[0], "127.0.0.1", "{stdout}");
    assert_eq!(vars[2], "postgres", "{stdout}");
    assert_eq!(lines.next(), Some(vars[1]), "psql reached PGPORT: {stdout}");
    assert!(!scratch.pointer_path().exists());
    let dir = scratch.runtime_dir().join("pgdrop");
    let mode = std::fs::metadata(&dir).expect("the pointer's directory");
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777,
        0o700
    );
}

/// Two concurrent starts leave a whole pointer naming one of them, and a
/// bare psql reaches that one; neither cluster is disturbed. A start after
/// both is the one the pointer names.
#[test]
fn concurrent_starts_leave_the_pointer_naming_one_of_them() {
    let scratch = Scratch::new("pointer-race");
    let (one, two) = std::thread::scope(|scope| {
        let one = scope.spawn(|| start(&scratch, &["--port", "auto"]));
        let two = scope.spawn(|| start(&scratch, &["--port", "auto"]));
        (
            one.join().expect("first start"),
            two.join().expect("second start"),
        )
    });
    assert_ne!(one.started.tcp_port(), two.started.tcp_port());
    let named = scratch.pointer_datadir().expect("a pointer");
    let current = [&one, &two]
        .into_iter()
        .find(|running| running.started.datadir == named)
        .expect("the pointer names one of the two");
    let port = bare_psql(&scratch, "show port");
    assert_success(&port, "bare pgdrop psql");
    assert_eq!(
        String::from_utf8_lossy(&port.stdout),
        format!("{}\n", current.started.tcp_port())
    );
    select_1(&scratch, &one.started);
    select_1(&scratch, &two.started);

    let third = start(&scratch, &[]);
    assert_eq!(
        scratch.pointer_datadir(),
        Some(third.started.datadir.clone())
    );
    let bare = bare_psql(&scratch, "select 1");
    assert_success(&bare, "bare pgdrop psql");
    assert_eq!(bare.stdout, b"1\n");
    select_1(&scratch, &one.started);
    select_1(&scratch, &two.started);
}

/// A server killed with SIGKILL leaves the pointer naming it. A bare psql
/// says so rather than failing to connect; a bare stop takes it as already
/// stopped, removes what `start` made and the pointer, and says nothing the
/// second time either.
#[test]
fn a_stale_pointer_after_a_crash() {
    let scratch = Scratch::new("stale");
    let running = start(&scratch, &[]);
    let started = running.started.clone();
    send("-KILL", started.pid);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gone(started.pid) {
        assert!(Instant::now() < deadline, "the server survived SIGKILL");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(scratch.pointer_datadir(), Some(started.datadir.clone()));

    let psql = bare_psql(&scratch, "select 1");
    assert_eq!(psql.status.code(), Some(2), "{psql:?}");
    let stderr = String::from_utf8_lossy(&psql.stderr);
    assert_eq!(
        stderr,
        format!(
            "pgdrop: error: the cluster pgdrop start started last (data directory \"{}\") \
             is not running; start one with pgdrop start, or name a server with -h and -p \
             or PGHOST and PGPORT\n",
            started.datadir
        )
    );

    for attempt in ["bare pgdrop stop", "second bare pgdrop stop"] {
        let stop = scratch.pgdrop(&["stop"]);
        assert_success(&stop, attempt);
        assert!(stop.stderr.is_empty(), "{attempt}: {stop:?}");
        assert!(!Path::new(&started.run_dir()).exists(), "{attempt}");
        assert!(!scratch.pointer_path().exists(), "{attempt}");
    }
    drop(running);
}

/// Whether `pid` has exited: `kill -0` fails, or (Linux) it is a zombie
/// nobody reaps, as in a container whose PID 1 reaps nothing.
fn gone(pid: u32) -> bool {
    let zombie = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| Some(stat.rsplit_once(')')?.1.trim_start().starts_with('Z')))
        .unwrap_or(false);
    zombie
        || !Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status()
            .expect("run kill")
            .success()
}

/// `--datadir`, `PGDATA` and libpq's variables and flags each win over the
/// pointer, which only a stop of the cluster it names takes away.
#[test]
fn explicit_datadir_pgdata_and_pg_variables_override_the_pointer() {
    let scratch = Scratch::new("explicit");
    let one = start(&scratch, &[]);
    let two = start(&scratch, &[]);
    let three = start(&scratch, &[]);
    assert_eq!(
        scratch.pointer_datadir(),
        Some(three.started.datadir.clone())
    );

    // `--datadir` stops that cluster, not the current one.
    let stop = scratch.pgdrop(&["stop", "--datadir", &one.started.datadir]);
    assert_success(&stop, "pgdrop stop --datadir");
    assert!(!Path::new(&one.started.run_dir()).exists());
    assert_eq!(
        scratch.pointer_datadir(),
        Some(three.started.datadir.clone())
    );

    // So does `PGDATA`.
    let stop = scratch
        .command(&["stop"])
        .env("PGDATA", &two.started.datadir)
        .output()
        .expect("run pgdrop stop");
    assert_success(&stop, "PGDATA=… pgdrop stop");
    assert!(!Path::new(&two.started.run_dir()).exists());
    assert_eq!(
        scratch.pointer_datadir(),
        Some(three.started.datadir.clone())
    );
    select_1(&scratch, &three.started);

    // Any variable that names a server keeps a bare stop off the pointer.
    let stop = scratch
        .command(&["stop"])
        .env("PGHOST", three.started.run_dir())
        .output()
        .expect("run pgdrop stop");
    assert_eq!(stop.status.code(), Some(1), "{stop:?}");
    assert!(Path::new(&three.started.datadir).exists());

    // psql: a flag or a variable naming a server, user or database wins.
    let nowhere = scratch.0.join("nowhere");
    let nowhere = nowhere.to_str().expect("UTF-8");
    let flag = scratch.pgdrop(&["psql", "-X", "-h", nowhere, "-c", "select 1"]);
    let mut env_host = scratch.command(&["psql", "-X", "-c", "select 1"]);
    env_host.env("PGHOST", nowhere);
    let mut env_data = scratch.command(&["psql", "-X", "-c", "select 1"]);
    env_data.env("PGDATA", &three.started.datadir);
    for (what, output) in [
        ("-h", flag),
        ("PGHOST", env_host.output().expect("run pgdrop psql")),
        ("PGDATA", env_data.output().expect("run pgdrop psql")),
    ] {
        assert_eq!(output.status.code(), Some(2), "{what}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("psql: error: "), "{what}: {stderr}");
    }
    // A database named on the command line is not the pointer's either.
    let named = scratch.pgdrop(&["psql", "-X", "-At", "-c", "select 1", &three.started.uri]);
    assert_success(&named, "pgdrop psql URI");

    let bare = scratch.pgdrop(&["stop"]);
    assert_success(&bare, "bare pgdrop stop");
    assert!(!Path::new(&three.started.run_dir()).exists());
    assert!(!scratch.pointer_path().exists());
    drop((one, two, three));
}

/// `--port auto` on a port someone holds: the server cannot bind it and
/// exits, and `start` tries another. A test build tries the ports in
/// `PGDROP_TEST_AUTO_PORTS` before asking the kernel, so the first one is
/// deterministically taken; given only taken ones, `start` gives up.
#[test]
fn port_auto_retries_a_port_taken_before_the_server_bound_it() {
    let scratch = Scratch::new("port-auto");
    let held = TcpListener::bind("127.0.0.1:0").expect("hold a port");
    let taken = held.local_addr().expect("its address").port();

    let output = scratch
        .command(&["start", "--json", "--port", "auto"])
        .env("PGDROP_TEST_AUTO_PORTS", taken.to_string())
        .output()
        .expect("run pgdrop start");
    assert_success(&output, "pgdrop start --port auto");
    let running = Running {
        scratch: &scratch,
        started: parse_started(&String::from_utf8(output.stdout).expect("UTF-8")),
    };
    let port = running.started.tcp_port();
    assert_ne!(port, taken);
    let pointer = std::fs::read_to_string(scratch.pointer_path()).expect("the pointer");
    assert!(pointer.contains(&format!("\nport={port}\n")), "{pointer}");
    select_1(&scratch, &running.started);
    // The default data directory is `<run>/data`.
    let run_dir = Path::new(&running.started.datadir)
        .parent()
        .expect("the run directory");
    let log = std::fs::read_to_string(run_dir.join("server.log")).expect("the server log");
    assert!(
        log.contains("could not create any TCP/IP sockets"),
        "the first attempt's log is kept:\n{log}"
    );
    drop(running);

    let taken_list = vec![taken.to_string(); 8].join(",");
    let output = scratch
        .command(&["start", "--port", "auto"])
        .env("PGDROP_TEST_AUTO_PORTS", &taken_list)
        .output()
        .expect("run pgdrop start");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "pgdrop: error: --port auto found no free port: each of the 8 it picked was \
             taken before the server could listen on it (the last, {taken})\n"
        )
    );
    assert!(output.stdout.is_empty());
    drop(held);
}

#[test]
fn start_refuses_a_bad_command_line() {
    let scratch = Scratch::new("refuse");
    for (args, message) in [
        (&["--set", "fsync"][..], "--set fsync requires a value"),
        (&["--set", "port=1"][..], "--set cannot change port"),
        (
            &["--env", "--json"][..],
            "--env and --json cannot be used together",
        ),
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

/// With nowhere a pointer could be (no `XDG_RUNTIME_DIR`, no `HOME`), or
/// with a variable naming a server, a bare stop has no cluster: pg_ctl's
/// complaint (`pg_ctl.c:2442`).
#[test]
fn stop_without_a_datadir_says_so() {
    let scratch = Scratch::new("no-datadir");
    let output = scratch
        .command(&["stop"])
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("HOME")
        .output()
        .expect("run pgdrop stop");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "pgdrop: error: no database directory specified and environment variable PGDATA unset\n"
    );
}
