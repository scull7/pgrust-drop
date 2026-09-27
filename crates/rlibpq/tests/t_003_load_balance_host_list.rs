//! Port of `src/interfaces/libpq/t/003_load_balance_host_list.pl`
//! (PostgreSQL REL_18_6), driving this crate's `Connection` where upstream
//! drives `psql`.
//!
//! Three clusters, each with its own socket directory (`own_host => 1`,
//! `:14`-`:15`), and one connection string listing all three. Which server a
//! connection reached is read back from the servers' logs, as upstream reads
//! it: `PostgreSQL::Test::Cluster` sets `log_statement = all`
//! (`Cluster.pm:688`), so each `SELECT` lands in exactly one log.
//!
//! `connect_ok` / `connect_fails` (`Cluster.pm`) run `psql -c $sql` and
//! check its exit status, stderr and the new log lines; here they are a
//! `PQconnectdb` and a `PQexec`, and the error text is the connection
//! error's message, which is what psql prints after `psql: error: `.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ConnectionError, ExecStatus, Filesystem};

mod common;

use common::{Cluster, only};

const CONF: &str = "log_statement = all\n";

/// `connect_ok`, `Cluster.pm`: connect with `connstr`, run `sql`, and when
/// `log_like` names a node, find `statement: <sql>` among the lines its log
/// gained meanwhile.
fn connect_ok(connstr: &str, test_name: &str, sql: &str, log_like: Option<&Cluster>) {
    let offset = log_like.map(|node| node.log_content().len());
    let mut conn = connect(connstr).unwrap_or_else(|err| panic!("{test_name}: {err}"));
    let result = only(conn.exec(sql.as_bytes()).expect(test_name));
    assert_eq!(result.status(), ExecStatus::TuplesOk, "{test_name}");
    conn.terminate().expect("Terminate is sent");
    if let (Some(node), Some(offset)) = (log_like, offset) {
        let expected = format!("statement: {sql}");
        assert!(
            node.log_content()[offset..].contains(&expected),
            "{test_name}: log matches {expected:?}"
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

fn connect(connstr: &str) -> Result<Connection, ConnectionError> {
    let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
    info.add_defaults(&Env::empty(), &Filesystem)
        .expect("no service to look up");
    Connection::connect(&info)
}

/// How many times `statement: <sql>` is in `node`'s log.
fn occurrences(node: &Cluster, sql: &str) -> usize {
    node.log_content()
        .matches(&format!("statement: {sql}"))
        .count()
}

#[test]
fn load_balance_host_list() {
    // :13-:25
    let Some(node1) = Cluster::start_node("node1-", "trust", 55_510, "", CONF) else {
        return;
    };
    let Some(node2) = Cluster::start_node("node2-", "trust", 55_511, "", CONF) else {
        return;
    };
    let Some(node3) = Cluster::start_node("node3-", "trust", 55_512, "", CONF) else {
        return;
    };

    // :28-:29
    let hostlist = format!(
        "{},{},{}",
        node1.dir.display(),
        node2.dir.display(),
        node3.dir.display()
    );
    let portlist = format!("{},{},{}", node1.port, node2.port, node3.port);
    let base = format!("host={hostlist} port={portlist} user=gateuser dbname=postgres");

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

    // :69-:70
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
