//! Port of `src/interfaces/libpq/t/006_service.pl` (PostgreSQL REL_18_6),
//! driving this crate's `ConnInfo::add_defaults` and `Connection` where
//! upstream drives `psql` through `connect_ok` / `connect_fails`.
//!
//! The service files are real files in a temporary directory, read through
//! [`Filesystem`], and every `connect_ok` row connects to a live server whose
//! address only the service file knows: the environment names the dummy
//! node, which is never started (`:16`-`:21`), exactly as upstream's does.
//! The pure half of the same cases, with no server, is
//! `rlibpq::service::tests`.
//!
//! Differences from the Perl, none of which touches what is compared:
//! - the environment is a value handed to `add_defaults`, not `%ENV`, so
//!   upstream's `local $ENV{…}` scopes become rebindings of `env`;
//! - `connect_fails` matches its pattern against the error `add_defaults`
//!   returns, which is what libpq leaves in `PQerrorMessage` and what psql
//!   prints after `psql: error: `; `expected_stdout` on a `connect_fails`
//!   (`:89`, `:138`) is not checked upstream either (`Cluster.pm:2639`-`:2659`,
//!   which reads only `expected_stderr`);
//! - the dummy node is the environment `_get_env` builds for it
//!   (`Cluster.pm:1718`: `PGHOST`, `PGPORT`) without an `initdb` of its own,
//!   which nothing here reads;
//! - the user is `PGUSER=gateuser`, the superuser `Cluster` creates, where
//!   upstream's is the OS user `initdb` defaults to;
//! - `$node->teardown_node` (`:145`) is `Cluster`'s `Drop`.
//!
//! Without the reference tools the test prints `SKIP (flagged, not silent)`
//! and passes; with `PGDROP_REQUIRE_REF=1` a missing reference fails instead.

#![allow(clippy::doc_markdown)]

use std::path::Path;

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ExecStatus, Filesystem};
use testkit::Pattern;

mod common;

use common::{Cluster, only, unaligned};

/// The started node.
const NODE_PORT: u16 = 55_510;

/// The dummy node's port: nothing listens on it (`:20`-`:21`).
const DUMMY_PORT: u16 = 55_511;

/// `connect_ok`, `Cluster.pm:2557`: the connection succeeds and `sql`'s
/// output matches `expected_stdout`.
fn connect_ok(env: &Env, connstr: &str, test_name: &str, sql: &str, expected_stdout: &str) {
    let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
    if let Err(err) = info.add_defaults(env, &Filesystem) {
        panic!("{test_name}: {err}");
    }
    let mut conn = Connection::connect(&info, env, &Filesystem)
        .unwrap_or_else(|err| panic!("{test_name}: {err}"));
    let result = only(conn.exec(sql.as_bytes()).expect("PQexec"));
    assert_eq!(result.status(), ExecStatus::TuplesOk, "{test_name}");
    let stdout = String::from_utf8(unaligned(&result)).expect("utf-8");
    let pattern = Pattern::new(expected_stdout).expect("pattern compiles");
    assert!(
        pattern.is_match(&stdout),
        "{test_name}: stdout matches: {stdout:?}"
    );
}

/// `connect_fails`, `Cluster.pm:2639`: the connection fails and its error
/// matches `expected_stderr`.
fn connect_fails(env: &Env, connstr: &str, test_name: &str, expected_stderr: &str) {
    let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
    let error = match info.add_defaults(env, &Filesystem) {
        Err(err) => err.to_string(),
        Ok(()) => match Connection::connect(&info, env, &Filesystem) {
            Err(err) => err.to_string(),
            Ok(_) => panic!("{test_name}: connected"),
        },
    };
    let pattern = Pattern::new(expected_stderr).expect("pattern compiles");
    assert!(
        pattern.is_match(&error),
        "{test_name}: stderr matches: {error:?}"
    );
}

fn append_to_file(path: &Path, text: &str) {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .expect("the file is appended to");
}

/// The whole script, top to bottom, on one node: its three blocks share the
/// files and the environment it sets up first, in upstream's order.
#[test]
#[allow(clippy::too_many_lines)]
fn t_006_service() {
    let Some(node) = Cluster::start("trust", NODE_PORT) else {
        return;
    };
    // $node->connstr, Cluster.pm:280.
    let node_connstr = format!("port={} host={}", node.port, node.dir.display());

    // my $td = PostgreSQL::Test::Utils::tempdir; (:23)
    let td = node.dir.join("td");
    std::fs::create_dir_all(&td).expect("tempdir");

    // :25-:33
    let srvfile_valid = td.join("pg_service_valid.conf");
    append_to_file(&srvfile_valid, "[my_srv]\n");
    for param in node_connstr.split_whitespace() {
        append_to_file(&srvfile_valid, &format!("{param}\n"));
    }

    // :35-:38
    let srvfile_empty = td.join("pg_service_empty.conf");
    append_to_file(&srvfile_empty, "");

    // :43-:44
    let srvfile_missing = td.join("pg_service_missing.conf");

    // Cluster.pm:158 and :1718 for the dummy node, then :50 and :55.
    let base = Env::empty()
        .with("PGDATABASE", "postgres")
        .with("PGUSER", "gateuser")
        .with("PGHOST", node.dir.to_str().expect("utf-8 path"))
        .with("PGPORT", DUMMY_PORT.to_string())
        .with("PGSYSCONFDIR", td.to_str().expect("utf-8 path"))
        .with("PGSERVICEFILE", srvfile_empty.to_str().expect("utf-8 path"));

    // Checks combinations of service name and a valid service file. (:57)
    {
        let env = base
            .clone()
            .with("PGSERVICEFILE", srvfile_valid.to_str().expect("utf-8 path"));
        connect_ok(
            &env,
            "service=my_srv",
            "connection with correct \"service\" string and PGSERVICEFILE",
            "SELECT 'connect1_1'",
            "connect1_1",
        );

        connect_ok(
            &env,
            "postgres://?service=my_srv",
            "connection with correct \"service\" URI and PGSERVICEFILE",
            "SELECT 'connect1_2'",
            "connect1_2",
        );

        connect_fails(
            &env,
            "service=undefined-service",
            "connection with incorrect \"service\" string and PGSERVICEFILE",
            "definition of service \"undefined-service\" not found",
        );

        let env = env.with("PGSERVICE", "my_srv");
        connect_ok(
            &env,
            "",
            "connection with correct PGSERVICE and PGSERVICEFILE",
            "SELECT 'connect1_3'",
            "connect1_3",
        );

        let env = env.with("PGSERVICE", "undefined-service");
        connect_fails(
            &env,
            "",
            "connection with incorrect PGSERVICE and PGSERVICEFILE",
            "definition of service \"undefined-service\" not found",
        );
    }

    // Checks case of incorrect service file. (:93)
    {
        let env = base.clone().with(
            "PGSERVICEFILE",
            srvfile_missing.to_str().expect("utf-8 path"),
        );
        connect_fails(
            &env,
            "service=my_srv",
            "connection with correct \"service\" string and incorrect PGSERVICEFILE",
            "service file \".*pg_service_missing.conf\" not found",
        );
    }

    // Checks case of service file named "pg_service.conf" in PGSYSCONFDIR.
    // (:103)
    {
        // Create copy of valid file
        let srvfile_default = td.join("pg_service.conf");
        std::fs::copy(&srvfile_valid, &srvfile_default).expect("copy");

        let env = base.clone();
        connect_ok(
            &env,
            "service=my_srv",
            "connection with correct \"service\" string and pg_service.conf",
            "SELECT 'connect2_1'",
            "connect2_1",
        );

        connect_ok(
            &env,
            "postgres://?service=my_srv",
            "connection with correct \"service\" URI and default pg_service.conf",
            "SELECT 'connect2_2'",
            "connect2_2",
        );

        connect_fails(
            &env,
            "service=undefined-service",
            "connection with incorrect \"service\" string and default pg_service.conf",
            "definition of service \"undefined-service\" not found",
        );

        let env = env.with("PGSERVICE", "my_srv");
        connect_ok(
            &env,
            "",
            "connection with correct PGSERVICE and default pg_service.conf",
            "SELECT 'connect2_3'",
            "connect2_3",
        );

        let env = env.with("PGSERVICE", "undefined-service");
        connect_fails(
            &env,
            "",
            "connection with incorrect PGSERVICE and default pg_service.conf",
            "definition of service \"undefined-service\" not found",
        );

        // Remove default pg_service.conf.
        std::fs::remove_file(&srvfile_default).expect("unlink");
    }
}
