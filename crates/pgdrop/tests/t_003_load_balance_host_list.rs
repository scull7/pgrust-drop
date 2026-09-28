//! NAT-394: `src/interfaces/libpq/t/003_load_balance_host_list.pl`
//! (PostgreSQL REL_18_6) against three pgrust clusters started by
//! `pgdrop start`, the connections made by rlibpq.
//!
//! `crates/rlibpq/tests/t_003_load_balance_host_list.rs` runs the same port
//! against the C server and SKIPs without it; here every server is pgrust
//! through this binary, so nothing needs a reference installation and
//! nothing SKIPs. The cases, their order and their names are upstream's,
//! each citing its line.
//!
//! Each `pgdrop start` puts its socket in a run directory of its own, so the
//! three nodes are upstream's `own_host => 1` (`:14`-`:15`) all three, and
//! share the socket name `.s.PGSQL.5432`: the host list differs, the port
//! list is `5432,5432,5432`. `--set log_statement=all` is what
//! `PostgreSQL::Test::Cluster` writes into every node's configuration
//! (`Cluster.pm:688`), and it is how upstream reads back which node a
//! connection reached: each `SELECT` lands in exactly one server log, here
//! `<run>/server.log`.
//!
//! `connect_ok` / `connect_fails` (`Cluster.pm`) run `psql -c $sql`; here
//! they are a `PQconnectdb` and a `PQexec`, as in the rlibpq port.

#![cfg(unix)]
#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ConnectionError, ExecStatus, Filesystem, QueryResult};

const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// The scratch directory `XDG_CACHE_HOME` points into, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgdrop-t003-{}", std::process::id()));
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

/// One node: a cluster `pgdrop start --json` started, stopped when dropped
/// so a failed assertion leaves no server behind.
struct Node<'a> {
    scratch: &'a Scratch,
    name: &'static str,
    datadir: String,
    /// The run directory: the node's socket directory, holding its log.
    host: String,
}

impl<'a> Node<'a> {
    /// `$node->init; $node->start`.
    fn start(scratch: &'a Scratch, name: &'static str) -> Self {
        let output = scratch.pgdrop(&["start", "--json", "--set", "log_statement=all"]);
        assert!(
            output.status.success(),
            "{name}: pgdrop start: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let json = std::str::from_utf8(&output.stdout).expect("UTF-8");
        let uri = json_field(json, "uri");
        let host = uri
            .strip_prefix("postgresql://postgres@")
            .and_then(|rest| rest.split_once(':'))
            .map(|(host, _)| host.replace("%2F", "/"))
            .expect("a Unix-socket URI");
        assert!(!host.contains(['%', ',', ' ', '\'']), "{host}");
        Node {
            scratch,
            name,
            datadir: json_field(json, "datadir").to_owned(),
            host,
        }
    }

    /// `$node->log_content`.
    fn log_content(&self) -> String {
        String::from_utf8_lossy(
            &std::fs::read(Path::new(&self.host).join("server.log")).unwrap_or_default(),
        )
        .into_owned()
    }

    /// `$node->stop`: `pgdrop stop` is a fast shutdown, waited for.
    fn stop(&self) {
        let output = self.scratch.pgdrop(&["stop", "--datadir", &self.datadir]);
        assert!(
            output.status.success(),
            "{}: pgdrop stop: {output:?}",
            self.name
        );
    }
}

impl Drop for Node<'_> {
    fn drop(&mut self) {
        let _ = self.scratch.pgdrop(&["stop", "--datadir", &self.datadir]);
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

fn connect(connstr: &str) -> Result<Connection, ConnectionError> {
    let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
    info.add_defaults(&Env::empty(), &Filesystem)
        .expect("no service to look up");
    Connection::connect(&info)
}

fn only(results: Vec<QueryResult>) -> QueryResult {
    assert_eq!(results.len(), 1, "one result: {results:?}");
    results.into_iter().next().expect("one result")
}

/// `connect_ok`, `Cluster.pm`: connect with `connstr`, run `sql`, and when
/// `log_like` names a node, find `statement: <sql>` among the lines its log
/// gained meanwhile.
fn connect_ok(connstr: &str, test_name: &str, sql: &str, log_like: Option<&Node>) {
    let offset = log_like.map(|node| node.log_content().len());
    let mut conn = connect(connstr).unwrap_or_else(|err| panic!("{test_name}: {err}"));
    let result = only(conn.exec(sql.as_bytes()).expect(test_name));
    assert_eq!(result.status(), ExecStatus::TuplesOk, "{test_name}");
    conn.terminate().expect("Terminate is sent");
    if let (Some(node), Some(offset)) = (log_like, offset) {
        let expected = format!("statement: {sql}");
        let log = node.log_content();
        assert!(
            log[offset..].contains(&expected),
            "{test_name}: {} log matches {expected:?}:\n{log}",
            node.name
        );
    }
}

/// `connect_fails`, `Cluster.pm`, with `expected_stderr`.
fn connect_fails(connstr: &str, test_name: &str, expected_stderr: &str) {
    let err = match connect(connstr) {
        Ok(_) => panic!("{test_name}: connected"),
        Err(err) => err.to_string(),
    };
    assert!(
        err.contains(expected_stderr),
        "{test_name}: stderr matches: {err}"
    );
}

/// How many times `statement: <sql>` is in `node`'s log.
fn occurrences(node: &Node, sql: &str) -> usize {
    node.log_content()
        .matches(&format!("statement: {sql}"))
        .count()
}

#[test]
fn load_balance_host_list() {
    let scratch = Scratch::new();

    // :13-:25
    let node1 = Node::start(&scratch, "node1");
    let node2 = Node::start(&scratch, "node2");
    let node3 = Node::start(&scratch, "node3");

    // :28-:29
    let hostlist = format!("{},{},{}", node1.host, node2.host, node3.host);
    let portlist = "5432,5432,5432";
    let base = format!("host={hostlist} port={portlist} user=postgres dbname=postgres");

    // :31
    connect_fails(
        &format!("{base} load_balance_hosts=doesnotexist"),
        "load_balance_hosts doesn't accept unknown values",
        "invalid load_balance_hosts value: \"doesnotexist\"",
    );

    // :37 — load_balance_hosts=disable should always choose the first one.
    connect_ok(
        &format!("{base} load_balance_hosts=disable"),
        "load_balance_hosts=disable connects to the first node",
        "SELECT 'connect1'",
        Some(&node1),
    );

    // :46 — "the chance of that not happening is so small that it's
    // negligible: (2/3)^50 = 1.56832855e-9".
    for _ in 1..=50 {
        connect_ok(
            &format!("{base} load_balance_hosts=random"),
            "repeated connections with random load balancing",
            "SELECT 'connect2'",
            None,
        );
    }

    // :54-:67
    let node1_occurrences = occurrences(&node1, "SELECT 'connect2'");
    let node2_occurrences = occurrences(&node2, "SELECT 'connect2'");
    let node3_occurrences = occurrences(&node3, "SELECT 'connect2'");
    let total_occurrences = node1_occurrences + node2_occurrences + node3_occurrences;

    assert!(
        node1_occurrences > 1,
        "received at least one connection on node1"
    );
    assert!(
        node2_occurrences > 1,
        "received at least one connection on node2"
    );
    assert!(
        node3_occurrences > 1,
        "received at least one connection on node3"
    );
    assert_eq!(
        total_occurrences, 50,
        "received 50 connections across all nodes"
    );

    // :69-:70. `pgdrop stop` also removes the run directory, so the host
    // list now names two socket directories that are gone rather than two
    // sockets nobody listens on; libpq moves on from either.
    node1.stop();
    node2.stop();

    // :72 — load_balance_hosts=disable should continue trying hosts until
    // it finds a working one.
    connect_ok(
        &format!("{base} load_balance_hosts=disable"),
        "load_balance_hosts=disable continues until it connects to the a working node",
        "SELECT 'connect3'",
        Some(&node3),
    );

    // :80 — also with load_balance_hosts=random we continue to the next
    // nodes if previous ones are down.
    for _ in 1..=5 {
        connect_ok(
            &format!("{base} load_balance_hosts=random"),
            "load_balance_hosts=random continues until it connects to the a working node",
            "SELECT 'connect4'",
            Some(&node3),
        );
    }
}
