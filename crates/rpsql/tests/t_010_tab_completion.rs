//! Port of `src/bin/psql/t/010_tab_completion.pl` (PostgreSQL 18.6), the
//! parts that are not tab completion: the interactive session itself.
//!
//! Upstream starts psql on a pseudo-terminal (`interactive_psql`,
//! `Cluster.pm:2401`), types into it and waits for a pattern on its output
//! (`check_completion`, 010_tab_completion.pl:83), and quits with `\q`. Every
//! completion check in between (`SEL<tab>` …) waits for NAT-405's
//! `Completer`, a later issue, and `clear_query` (`:112`) for `\r`, which
//! NAT-402 adds; they are not ported yet, and are listed where they fall.
//!
//! The same session runs against rpsql and, where this lane has one, against
//! C psql. Past upstream's pattern checks, the history file each writes on
//! `\q` (`finishInput`, `input.c:535`) is diffed byte for byte, when C psql's
//! was written by GNU readline: libedit writes its own format, so on such a
//! lane that one diff is flagged as skipped.
//!
//! The cluster comes from the reference `initdb` and `pg_ctl`; without them
//! the test prints `SKIP (flagged, not silent)` and passes, and under
//! `PGDROP_REQUIRE_REF=1` (CI) it fails instead.

#![allow(clippy::doc_markdown)]

use std::fs::File;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod pty;
mod regress;

use regress::Cluster;
use testkit::pattern::Pattern;

const RPSQL: &str = env!("CARGO_BIN_EXE_rpsql");

/// Unique among this crate's live gates; each starts its own cluster.
const TAB_COMPLETION_PORT: u16 = 55_410;
const PASTE_PORT: u16 = 55_411;

/// `$PostgreSQL::Test::Utils::timeout_default` (`Utils.pm:172`-`:174`).
const TIMEOUT_DEFAULT_SECS: u64 = 180;
const TIMEOUT_DEFAULT: Duration = Duration::from_secs(TIMEOUT_DEFAULT_SECS);

/// `PostgreSQL::Test::BackgroundPsql` started interactive: psql on a
/// pseudo-terminal, its stderr on a pipe (`BackgroundPsql.pm`, `new`).
struct InteractivePsql {
    child: Child,
    master: File,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    /// How much of `stdout` earlier calls have consumed.
    seen: usize,
}

/// `$node->interactive_psql('postgres', history_file => $historyfile)`
/// (`Cluster.pm:2401`-`:2436`), then `wait_connect` (`BackgroundPsql.pm:147`).
fn interactive_psql(cluster: &Cluster, psql: &Path, history_file: &Path) -> InteractivePsql {
    let pty = pty::open().expect("a pseudo-terminal");
    let mut command = cluster.command(psql);
    command
        // Cluster.pm:2414-:2426.
        .env("PSQL_HISTORY", history_file)
        .env("INPUTRC", "/dev/null")
        .env_remove("TERM")
        .env_remove("LS_COLORS")
        // Cluster.pm:2428-:2431.
        .args([
            "--no-psqlrc",
            "--no-align",
            "--tuples-only",
            "--dbname",
            "postgres",
        ])
        .stdin(Stdio::from(
            pty.slave.try_clone().expect("the slave, twice"),
        ))
        .stdout(Stdio::from(pty.slave))
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("psql starts");
    // The parent's copies of the slave go with `command`, so that the master
    // reads end once psql has exited.
    drop(command);

    let stdout = Arc::new(Mutex::new(Vec::new()));
    let stderr = Arc::new(Mutex::new(Vec::new()));
    collect(pty.master.try_clone().expect("the master, twice"), &stdout);
    collect(child.stderr.take().expect("a stderr pipe"), &stderr);
    let mut session = InteractivePsql {
        child,
        master: pty.master,
        stdout,
        stderr,
        seen: 0,
    };

    // BackgroundPsql.pm:157-:173.
    let banner = "background_psql: ready";
    session.type_in(format!("\\echo '{banner}'\n\\warn '{banner}'\n").as_bytes());
    let banner_match = Pattern::new(&format!("{banner}\\r?\\n")).expect("the banner pattern");
    let deadline = Instant::now() + TIMEOUT_DEFAULT;
    while !(banner_match.is_match(&session.stdout_since(0))
        && banner_match.is_match(&String::from_utf8_lossy(&session.stderr.lock().unwrap())))
    {
        assert!(
            Instant::now() < deadline,
            "psql startup timed out; stdout {:?}, stderr {:?}",
            session.stdout_since(0),
            String::from_utf8_lossy(&session.stderr.lock().unwrap())
        );
        thread::sleep(Duration::from_millis(20));
    }
    session.seen = session.stdout.lock().unwrap().len();
    session.stderr.lock().unwrap().clear();
    session
}

/// Append everything `from` yields to `into`, on a thread of its own. A
/// pseudo-terminal's master reports `EIO` once the slave is closed on Linux;
/// that is its end of file.
fn collect(mut from: impl Read + Send + 'static, into: &Arc<Mutex<Vec<u8>>>) {
    let into = Arc::clone(into);
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = from.read(&mut chunk) {
            if n == 0 {
                break;
            }
            into.lock().unwrap().extend_from_slice(&chunk[..n]);
        }
    });
}

impl InteractivePsql {
    fn type_in(&mut self, keys: &[u8]) {
        self.master.write_all(keys).expect("the keys are typed");
    }

    fn stdout_since(&self, from: usize) -> String {
        String::from_utf8_lossy(&self.stdout.lock().unwrap()[from..]).into_owned()
    }

    /// `query_until($until, $query)` (`BackgroundPsql.pm`): type `send`, wait
    /// for `until` on the output since the last call, and return that output.
    fn query_until(&mut self, until: &Pattern, send: &[u8]) -> Option<String> {
        self.type_in(send);
        let deadline = Instant::now() + TIMEOUT_DEFAULT;
        loop {
            let out = self.stdout_since(self.seen);
            if until.is_match(&out) {
                self.seen = self.stdout.lock().unwrap().len();
                return Some(out);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// `$h->quit` (`BackgroundPsql.pm`): `\q`, then wait for the exit.
    fn quit(mut self) -> ExitStatus {
        self.type_in(b"\\q\n");
        let deadline = Instant::now() + TIMEOUT_DEFAULT;
        loop {
            if let Some(status) = self.child.try_wait().expect("psql's status") {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                panic!("psql did not exit on \\q");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for InteractivePsql {
    /// A failed check must not leave psql waiting on its terminal.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// `check_completion($send, $pattern, $annotation)` (010_tab_completion.pl:83).
fn check_completion(
    h: &mut InteractivePsql,
    psql: &Path,
    send: &[u8],
    pattern: &str,
    annotation: &str,
) {
    let pattern = Pattern::new(pattern).expect("a supported pattern");
    let out = h.query_until(&pattern, send);
    assert!(
        out.is_some(),
        "{annotation}: {} did not print {} after {:?}; it printed {:?}",
        psql.display(),
        pattern.as_str(),
        String::from_utf8_lossy(send),
        h.stdout_since(h.seen)
    );
}

/// `clear_line()` (010_tab_completion.pl:121): control-U, then Enter.
fn clear_line(h: &mut InteractivePsql, psql: &Path) {
    check_completion(h, psql, b"\x15\n", "postgres=# ", "control-U works");
}

/// 010_tab_completion.pl:75-:81 and :426-:427 around what can be ported
/// today, against one psql. Returns the history file it wrote.
fn session(cluster: &Cluster, psql: &Path, ours: bool) -> Vec<u8> {
    // 010_tab_completion.pl:78, one file per psql.
    let historyfile: PathBuf = Path::new(env!("CARGO_TARGET_TMPDIR")).join(if ours {
        "010_rpsql_history.txt"
    } else {
        "010_psql_history.txt"
    });
    let _ = std::fs::remove_file(&historyfile);

    // 010_tab_completion.pl:81.
    let mut h = interactive_psql(cluster, psql, &historyfile);

    // :97-:109 SEL<tab>, and the completion checks from :112 on, wait for
    // tab completion; clear_query (:112) waits for `\r` (NAT-402).

    // Not upstream: a query and its answer, over two lines, so the history
    // file has an entry with a newline inside.
    check_completion(
        &mut h,
        psql,
        b"select\n",
        "postgres-# ",
        "a statement continues",
    );
    check_completion(
        &mut h,
        psql,
        b"42;\n",
        "(?s)42\\r?\\n.*postgres=# ",
        "the statement runs",
    );

    // clear_line (:121), as :417-:419 uses it after typing part of a line.
    // An editor that redraws the prompt as it goes can satisfy that pattern
    // before the control-U lands, so a statement after it proves the line
    // was really cleared: had it not been, psql would run the COPY instead.
    h.type_in(b"COPY foo FROM stdin WITH ( DEF)");
    clear_line(&mut h, psql);
    check_completion(
        &mut h,
        psql,
        b"select 1;\n",
        "(?s)\\n1\\r?\\n.*postgres=# ",
        "the line was cleared",
    );

    if ours {
        // Not upstream, and rpsql only: control-C at the prompt throws the
        // query away (`mainloop.c:108`). The terminal is not psql's
        // controlling one, so C psql, whose readline leaves the byte to the
        // terminal's ISIG, would never see it.
        check_completion(
            &mut h,
            psql,
            b"select\n",
            "postgres-# ",
            "a second statement",
        );
        check_completion(&mut h, psql, b"\x03", "postgres=# ", "control-C clears it");
        check_completion(
            &mut h,
            psql,
            b"select 7;\n",
            "(?s)7\\r?\\n.*postgres=# ",
            "and it is gone",
        );
    }

    // send psql an explicit \q to shut it down, else pty won't close properly
    // (010_tab_completion.pl:426-:427).
    let status = h.quit();
    assert!(status.success(), "{} returned {status}", psql.display());

    std::fs::read(&historyfile).expect("psql wrote its history file")
}

/// Not upstream, and rpsql only: lines pasted in one write, where each line's
/// first byte may arrive in the same read as the Enter before it. redox_liner's
/// `Context::read_line` dropped that byte with its per-line key iterator,
/// which turned `\warn cd` into `warn cd`, a query buffer line (NAT-405).
/// Lines of odd and even length both come after an Enter here, and the
/// multi-line statement still waits for its semicolon (`mainloop.c:420`).
#[test]
fn a_paste_keeps_the_first_byte_of_every_line() {
    let Some(cluster) = Cluster::start(PASTE_PORT) else {
        return;
    };
    let historyfile = Path::new(env!("CARGO_TARGET_TMPDIR")).join("010_rpsql_paste_history.txt");
    let _ = std::fs::remove_file(&historyfile);
    let rpsql = Path::new(RPSQL);
    let mut h = interactive_psql(&cluster, rpsql, &historyfile);

    check_completion(
        &mut h,
        rpsql,
        b"\\echo ab\n\\warn cd\n\\echo e\n\\echo fgh\nselect\n4 + 2;\n\\echo end\n",
        "(?s)\\nab\\r?\\n.*\\ne\\r?\\n.*\\nfgh\\r?\\n.*\\n6\\r?\\n.*\\nend\\r?\\n",
        "every pasted line keeps its first byte",
    );
    // `\warn cd` ran before `\echo end`; its stderr pipe may lag behind.
    let deadline = Instant::now() + TIMEOUT_DEFAULT;
    while h.stderr.lock().unwrap().as_slice() != b"cd\n" {
        assert!(
            Instant::now() < deadline,
            "\\warn cd did not reach stderr; it has {:?}",
            String::from_utf8_lossy(&h.stderr.lock().unwrap())
        );
        thread::sleep(Duration::from_millis(20));
    }
    let status = h.quit();
    assert!(status.success(), "rpsql returned {status}");
    assert_eq!(
        String::from_utf8_lossy(&std::fs::read(&historyfile).expect("a history file")),
        "\\echo 'background_psql: ready'\n\\warn 'background_psql: ready'\n\
         \\echo ab\n\\warn cd\n\\echo e\n\\echo fgh\nselect\u{1}4 + 2;\n\\echo end\n\\q\n",
        "the history file holds each pasted line whole"
    );
}

#[test]
fn interactive_session_without_completion() {
    let Some(cluster) = Cluster::start(TAB_COMPLETION_PORT) else {
        return;
    };

    let rpsql = Path::new(RPSQL);
    let ours = session(&cluster, rpsql, true);

    match cluster.reference_psql() {
        Some(psql) => {
            let theirs = session(&cluster, &psql, false);
            if theirs.starts_with(b"_HiStOrY_V2_") {
                testkit::reference::announce_skip(
                    "C psql is built on libedit, whose history file format rpsql does not write",
                );
                return;
            }
            // rpsql's control-C block adds one entry C psql's session did
            // not type — the query control-C threw away is not kept
            // (`mainloop.c:115`); everything else is the same keystrokes.
            let expected = [
                &theirs[..theirs.len() - b"\\q\n".len()],
                b"select 7;\n\\q\n",
            ]
            .concat();
            assert_eq!(
                String::from_utf8_lossy(&ours),
                String::from_utf8_lossy(&expected),
                "history file, rpsql (left) against C psql plus rpsql's extra entries (right)"
            );
        }
        None => testkit::reference::skip("psql"),
    }
}
