//! Port of `src/interfaces/libpq/t/004_load_balance_dns.pl` (PostgreSQL
//! REL_18_6), driving this crate's `Connection` where upstream drives
//! `psql`.
//!
//! One host name with three addresses, so the shuffle under test is the
//! per-host address shuffle (`fe-connect.c:3116`), not the host-list one
//! `t_003_load_balance_host_list` covers. Three clusters share one port on
//! 127.0.0.1, 127.0.0.2 and 127.0.0.3 (`:66`-`:74`), and the hosts file
//! maps `pg-loadbalancetest` to all three.
//!
//! Gated exactly as upstream gates it, and for upstream's reasons: it runs
//! only when `PG_TEST_EXTRA` names `load_balance` (`:9`), only on Linux —
//! Windows, the other OS upstream allows (`:37`), is not a target here — and
//! only when the hosts file carries the three lines (`:57`-`:64`), which
//! needs root to add, so the test never adds them itself. Each of those
//! skips prints `SKIP (flagged, not silent)` with upstream's reason.
//! Past the gate, the reference tools are needed as for every live gate.
//!
//! `connect_ok` and the log reading are the same as in
//! `t_003_load_balance_host_list`.

#![allow(clippy::doc_markdown)]

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ConnectionError, ExecStatus, Filesystem};
use testkit::reference::{SKIP_FLAG, announce_skip};

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

/// Calculation: `:58`-`:59` — how many `127.0.0.[1-3] pg-loadbalancetest`
/// lines the hosts file has.
fn hosts_count(hosts_content: &str) -> usize {
    ["127.0.0.1", "127.0.0.2", "127.0.0.3"]
        .iter()
        .map(|ip| {
            hosts_content
                .matches(&format!("{ip} pg-loadbalancetest"))
                .count()
        })
        .sum()
}

/// Calculation: Perl's `$haystack =~ /\bword\b/` for a `word` of word
/// characters.
fn names_word(haystack: &str, word: &str) -> bool {
    let is_word = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let bytes = haystack.as_bytes();
    haystack.match_indices(word).any(|(at, _)| {
        let end = at + word.len();
        (at == 0 || !is_word(bytes[at - 1])) && (end == bytes.len() || !is_word(bytes[end]))
    })
}

/// The reason upstream would `plan skip_all`, if any (`:9`-`:64`).
fn skip_reason() -> Option<&'static str> {
    // :9
    let extra = std::env::var("PG_TEST_EXTRA").unwrap_or_default();
    if !names_word(&extra, "load_balance") {
        return Some("Potentially unsafe test load_balance not enabled in PG_TEST_EXTRA");
    }
    // :37-:44
    if !cfg!(target_os = "linux") {
        return Some("load_balance test only supported on Linux and Windows");
    }
    // :46-:64
    let hosts_content = std::fs::read_to_string("/etc/hosts").unwrap_or_default();
    if hosts_count(&hosts_content) != 3 {
        return Some("hosts file was not prepared for DNS load balance test");
    }
    None
}

#[test]
fn pg_test_extra_is_matched_on_word_boundaries() {
    assert!(names_word("load_balance", "load_balance"));
    assert!(names_word("ssl load_balance kerberos", "load_balance"));
    assert!(names_word("ssl,load_balance", "load_balance"));
    assert!(!names_word("load_balancer", "load_balance"));
    assert!(!names_word("xload_balance", "load_balance"));
    assert!(!names_word("", "load_balance"));
}

#[test]
fn the_hosts_file_check_counts_upstreams_three_lines() {
    let prepared = "127.0.0.1 localhost\n127.0.0.1 pg-loadbalancetest\n\
                    127.0.0.2 pg-loadbalancetest\n127.0.0.3 pg-loadbalancetest\n";
    assert_eq!(hosts_count(prepared), 3);
    assert_eq!(hosts_count("127.0.0.1 localhost\n"), 0);
    assert_eq!(hosts_count("127.0.0.4 pg-loadbalancetest\n"), 0);
}

#[test]
fn load_balance_dns() {
    if let Some(reason) = skip_reason() {
        announce_skip(&format!("{SKIP_FLAG}: {reason}"));
        return;
    }

    // :66-:82 — `use_tcp`, one port, `own_host` hands out 127.0.0.2 and .3.
    let port = 55_520;
    let Some(node1) = Cluster::start_node("node1-", "trust", port, "127.0.0.1", CONF) else {
        return;
    };
    let Some(node2) = Cluster::start_node("node2-", "trust", port, "127.0.0.2", CONF) else {
        return;
    };
    let Some(node3) = Cluster::start_node("node3-", "trust", port, "127.0.0.3", CONF) else {
        return;
    };
    let base = format!("host=pg-loadbalancetest port={port} user=gateuser dbname=postgres");

    // :86 — load_balance_hosts=disable should always choose the first one.
    connect_ok(
        &format!("{base} load_balance_hosts=disable"),
        "load_balance_hosts=disable connects to the first node",
        "SELECT 'connect1'",
        Some(&node1),
    );

    // :96 — "(2/3)^50 = 1.56832855e-9".
    for _ in 1..=50 {
        connect_ok(
            &format!("{base} load_balance_hosts=random"),
            "repeated connections with random load balancing",
            "SELECT 'connect2'",
            None,
        );
    }

    // :104-:117
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

    // :119-:120
    node1.stop();
    node2.stop();

    // :122 — load_balance_hosts=disable should continue trying hosts until
    // it finds a working one.
    connect_ok(
        &format!("{base} load_balance_hosts=disable"),
        "load_balance_hosts=disable continues until it connects to the a working node",
        "SELECT 'connect3'",
        Some(&node3),
    );

    // :130 — also with load_balance_hosts=random we continue to the next
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
