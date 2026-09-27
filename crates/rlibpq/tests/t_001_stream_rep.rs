//! Port of the `target_session_attrs` tests in
//! `src/test/recovery/t/001_stream_rep.pl` (PostgreSQL REL_18_6,
//! `:117`-`:247`), driving this crate's `Connection` where upstream drives
//! `psql`. The rest of that file tests the server's replication, not libpq,
//! and is not ported.
//!
//! A primary and two hot standbys, the second cascading from the first
//! (`:10`-`:43`). Upstream takes their base backups with `pg_basebackup`
//! from the running primary; here the primary is stopped and copied
//! (`backup_fs_cold`, `Cluster.pm:864`), which leaves the gate needing only
//! `initdb`, `pg_ctl` and `psql`. A cold copy of the primary is the same
//! system as a base backup of standby 1, so standby 2 still streams from
//! standby 1. What the tests look at — whether a server is in hot standby —
//! is the same either way.
//!
//! `test_target_session_attrs` runs `psql --dbname <connstr> --command
//! 'SHOW port;'` and expects exit status 0 with the target's port on
//! stdout, or status 2 (psql's "connection failed") with no target. Here
//! that is a `PQconnectdb`, and on success a `PQexec` of the same command.
//!
//! Without the reference tools the test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ConnectionError, ExecStatus, Filesystem};

mod common;

use common::{Cluster, only};

/// `init(allows_streaming => 1)`, `Cluster.pm:699`-`:716`, with the
/// `log_statement = all` of `:688`.
const ALLOWS_STREAMING: &str = "log_statement = all
wal_level = replica
max_wal_senders = 10
max_replication_slots = 10
autovacuum_worker_slots = 3
wal_log_hints = on
hot_standby = on
shared_buffers = 1MB
max_connections = 10
max_wal_size = 128MB
";

/// A node and the name upstream's messages call it by.
struct Node<'a> {
    cluster: &'a Cluster,
    name: &'static str,
}

/// `test_target_session_attrs`, `001_stream_rep.pl:122`: connect to
/// `node1`, then `node2`, using `target_session_attrs=mode`; expect to reach
/// `target_node` (`None` for failure) with `status`.
fn test_target_session_attrs(
    node1: &Node<'_>,
    node2: &Node<'_>,
    target_node: Option<&Node<'_>>,
    mode: &str,
    status: i32,
) {
    // :143-:146
    let connstr = format!(
        "host={},{} port={},{} target_session_attrs={mode}",
        node1.cluster.dir.display(),
        node2.cluster.dir.display(),
        node1.cluster.port,
        node2.cluster.port,
    );
    // Upstream's psql connects as the OS user; the gate's clusters were
    // made with `-U gateuser`.
    let connstr = format!("{connstr} user=gateuser dbname=postgres");

    // psql exits 2 when the connection fails, 0 when `SHOW port;` ran.
    let (ret, stdout, stderr) = match connect(&connstr) {
        Ok(mut conn) => {
            let result = only(conn.exec(b"SHOW port;").expect("SHOW port;"));
            assert_eq!(result.status(), ExecStatus::TuplesOk);
            conn.terminate().expect("Terminate is sent");
            let port = String::from_utf8_lossy(result.value(0, 0).unwrap_or_default());
            (0, port.into_owned(), String::new())
        }
        Err(err) => (2, String::new(), err.to_string()),
    };

    if status == 0 {
        let target = target_node.expect("a target to connect to");
        assert!(
            status == ret && stdout == target.cluster.port.to_string(),
            "connect to node {} with mode \"{mode}\" and {},{} listed \
             (ret = {ret}, stdout = {stdout}, stderr = {stderr})",
            target.name,
            node1.name,
            node2.name,
        );
    } else {
        // :169-:172
        println!("status = {status}");
        println!("ret = {ret}");
        println!("stdout = {stdout}");
        println!("stderr = {stderr}");
        assert!(
            status == ret && target_node.is_none(),
            "fail to connect with mode \"{mode}\" and {},{} listed",
            node1.name,
            node2.name,
        );
    }
}

fn connect(connstr: &str) -> Result<Connection, ConnectionError> {
    let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
    info.add_defaults(&Env::empty(), &Filesystem)
        .expect("no service to look up");
    Connection::connect(&info)
}

#[test]
fn target_session_attrs() {
    // :11-:18 — Initialize primary node.
    let Some(primary) = Cluster::start_node("pri-", "trust", 55_530, "", ALLOWS_STREAMING) else {
        return;
    };

    // :21-:22, :35-:37 — Take backup (cold, see the module comment).
    primary.stop();
    let backup = primary.backup_fs_cold("my_backup");
    primary.start_again();

    // :24-:28 — Create streaming standby linking to primary.
    let standby_1 = Cluster::start_standby("sb1-", 55_531, &backup, &primary);

    // :39-:43 — Create second standby node linking to standby 1.
    let standby_2 = Cluster::start_standby("sb2-", 55_532, &backup, &standby_1);

    let node_primary = Node {
        cluster: &primary,
        name: "primary",
    };
    let node_standby_1 = Node {
        cluster: &standby_1,
        name: "standby_1",
    };
    let node_standby_2 = Node {
        cluster: &standby_2,
        name: "standby_2",
    };

    // :117-:118 — Tests for connection parameter target_session_attrs.
    println!("testing connection parameter \"target_session_attrs\"");

    let (p, s1, s2) = (&node_primary, &node_standby_1, &node_standby_2);
    #[rustfmt::skip]
    let cases: [(&Node<'_>, &Node<'_>, Option<&Node<'_>>, &str, i32); 17] = [
        // :182 — Connect to primary in "read-write" mode with primary,standby1 list.
        (p, s1, Some(p), "read-write", 0),
        // :186 — Connect to primary in "read-write" mode with standby1,primary list.
        (s1, p, Some(p), "read-write", 0),
        // :190 — Connect to primary in "any" mode with primary,standby1 list.
        (p, s1, Some(p), "any", 0),
        // :194 — Connect to standby1 in "any" mode with standby1,primary list.
        (s1, p, Some(s1), "any", 0),
        // :198 — Connect to primary in "primary" mode with primary,standby1 list.
        (p, s1, Some(p), "primary", 0),
        // :202 — Connect to primary in "primary" mode with standby1,primary list.
        (s1, p, Some(p), "primary", 0),
        // :206 — Connect to standby1 in "read-only" mode with primary,standby1 list.
        (p, s1, Some(s1), "read-only", 0),
        // :210 — Connect to standby1 in "read-only" mode with standby1,primary list.
        (s1, p, Some(s1), "read-only", 0),
        // :214 — Connect to primary in "prefer-standby" mode with primary,primary list.
        (p, p, Some(p), "prefer-standby", 0),
        // :218 — Connect to standby1 in "prefer-standby" mode with primary,standby1 list.
        (p, s1, Some(s1), "prefer-standby", 0),
        // :222 — Connect to standby1 in "prefer-standby" mode with standby1,primary list.
        (s1, p, Some(s1), "prefer-standby", 0),
        // :226 — Connect to standby1 in "standby" mode with primary,standby1 list.
        (p, s1, Some(s1), "standby", 0),
        // :230 — Connect to standby1 in "standby" mode with standby1,primary list.
        (s1, p, Some(s1), "standby", 0),
        // :234 — Fail to connect in "read-write" mode with standby1,standby2 list.
        (s1, s2, None, "read-write", 2),
        // :238 — Fail to connect in "primary" mode with standby1,standby2 list.
        (s1, s2, None, "primary", 2),
        // :242 — Fail to connect in "read-only" mode with primary,primary list.
        (p, p, None, "read-only", 2),
        // :246 — Fail to connect in "standby" mode with primary,primary list.
        (p, p, None, "standby", 2),
    ];
    for (node1, node2, target_node, mode, status) in cases {
        test_target_session_attrs(node1, node2, target_node, mode, status);
    }
}
