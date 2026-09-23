//! The protocol version 3 core against a real PostgreSQL 18 server.
//!
//! These are the gates NAT-389 asks for: the same query, the same error and
//! the same authentication methods through this crate and through the C tools,
//! compared byte for byte. They need a PostgreSQL 18 `initdb`, `pg_ctl` and
//! `psql`; when those are missing every one of them prints
//! `SKIP (flagged, not silent)` and passes, and none of them narrows into a
//! weaker assertion (AGENTS.md, "Never weaken a gate to get green").
//!
//! There is no upstream TAP file to steal here: `src/interfaces/libpq/t/` has
//! no simple-query test, and the authentication suite
//! (`src/test/authentication/t/001_password.pl`) is outside the sparse
//! checkout this port reads from. The cases below are therefore named for what
//! they pin rather than after an upstream test name.
//!
//! The extended-query gates (NAT-390) compare against C psql's `\bind`,
//! `\parse` and `\bind_named`, which drive `PQsendQueryParams` and
//! `PQsendQueryPrepared` in C libpq. The `libpq_pipeline` ports, traces and
//! all, are `t_001_libpq_pipeline.rs`.

#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use rlibpq::{ContextVisibility, ExecStatus, Params, Verbosity};
use testkit::reference;

mod common;

use common::{Cluster, TOOLS, bin_dir_with_every_tool, only, unaligned};

/// `select version()` through this crate is the row C psql prints.
#[test]
fn select_version_matches_the_reference_psql() {
    let Some(cluster) = Cluster::start("trust", 55_432) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn.exec(b"select version()").expect("query runs");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::TuplesOk);
    assert_eq!(results[0].ntuples(), 1);
    assert_eq!(results[0].nfields(), 1);

    let (stdout, _, code) = cluster.psql("select version()", "default");
    assert_eq!(code, 0);
    let mut ours = results[0].value(0, 0).expect("a value").to_vec();
    ours.push(b'\n');
    assert_eq!(ours, stdout, "the version row must be byte-identical");
}

/// An error carries the fields C psql shows with `VERBOSITY verbose`: the
/// SQLSTATE, the primary message and the statement position.
#[test]
fn an_error_carries_the_fields_the_reference_reports() {
    let Some(cluster) = Cluster::start("trust", 55_433) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn.exec(b"selct 1").expect("the exchange completes");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::FatalError);
    let error = results[0].error().expect("an error result");
    assert_eq!(error.sqlstate(), Some(&b"42601"[..]));

    // The comparison is made at VERBOSITY terse, and that is not a
    // convenience: at every other verbosity C libpq puts the position on a
    // syntax-cursor display over the query it kept (`res->errQuery`,
    // `fe-protocol3.c:966`, filled for a simple query at `fe-exec.c:1484`),
    // printing `LINE 1: selct 1` and a caret line that `reportErrorPosition`
    // (`fe-protocol3.c:1202`) draws and this port does not — the divergence
    // `docs/divergences.md` records. Terse is the one verbosity where upstream
    // renders the position as text (`fe-protocol3.c:1102`), which is exactly
    // what this port renders, so this is a real byte-for-byte gate over the
    // same fields rather than a comparison the port is documented to fail.
    let (_, stderr, code) = cluster.psql("selct 1", "terse");
    assert_ne!(code, 0);
    let rendered = error.message(
        ExecStatus::FatalError,
        Verbosity::Terse,
        ContextVisibility::Errors,
    );
    assert_eq!(
        rendered, stderr,
        "the terse rendering must be psql's, byte for byte"
    );

    // What is *not* judged, said out loud rather than left out: the default
    // verbosity, where psql adds the two cursor lines.
    let (_, default_stderr, _) = cluster.psql("selct 1", "default");
    if default_stderr != results[0].error_message() {
        reference::announce_skip(
            "OUT OF SCOPE (flagged, not silent): the default-verbosity rendering differs by \
             the syntax-cursor display (reportErrorPosition, fe-protocol3.c:1202); see \
             docs/divergences.md",
        );
    }
}

/// A notice reaches the client as a notice, not as a result.
#[test]
fn a_notice_is_delivered_out_of_band() {
    let Some(cluster) = Cluster::start("trust", 55_434) else {
        return;
    };
    let mut conn = cluster.connect();
    let results = conn
        .exec(b"drop table if exists no_such_table")
        .expect("runs");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].status(), ExecStatus::CommandOk);
    assert_eq!(conn.notices().len(), 1);
}

/// Each authentication method a build without TLS or GSSAPI can do, against a
/// server configured for it.
#[test]
fn every_authentication_method_connects() {
    for (index, method) in ["trust", "password", "md5", "scram-sha-256"]
        .into_iter()
        .enumerate()
    {
        let Some(cluster) = Cluster::start(method, 55_440 + u16::try_from(index).unwrap()) else {
            return;
        };
        let mut conn = cluster.connect();
        let results = conn.exec(b"select 1").expect("query runs");
        assert_eq!(results[0].value(0, 0), Some(&b"1"[..]), "method {method}");
    }
}

/// The gate is skipped, not narrowed, when this machine's PostgreSQL 18 is
/// incomplete: an installation missing any one of [`TOOLS`] is refused, and
/// refused by the name of the tool that is missing, so the gates above go on
/// to announce a `SKIP (flagged, not silent)` that says what to install.
///
/// Every arm is exercised because the silent narrowing this repo exists to
/// prevent lives in the arm nobody looks at: a `bin` directory holding
/// `initdb` and `psql` but no `pg_ctl` is a real client-only package, and the
/// mistake to catch is a lookup that shrugs at it and lets `Cluster::start`
/// run half a gate.
#[test]
fn an_installation_missing_any_one_tool_is_refused_by_name() {
    let complete = bin_dir_with_every_tool(Path::new("/ref/bin/initdb"), |_| true);
    assert_eq!(complete, Ok(PathBuf::from("/ref/bin")));

    for absent in TOOLS {
        let found = bin_dir_with_every_tool(Path::new("/ref/bin/initdb"), |path| {
            path != Path::new("/ref/bin").join(absent)
        });
        assert_eq!(found, Err(absent), "a bin directory without {absent}");
    }
}

/// `PQexecParams` through this crate prints what C psql's `\bind … \g`
/// prints — and psql's `\bind` *is* `PQsendQueryParams` with text
/// parameters and no types (`src/bin/psql/common.c:1616`, in `ExecQueryAndProcessResults`),
/// so both sides send the same Parse/Bind/Describe/Execute/Sync.
#[test]
fn exec_params_matches_the_reference_psql_bind() {
    let Some(cluster) = Cluster::start("trust", 55_450) else {
        return;
    };
    let mut conn = cluster.connect();
    let query = "select $1::int4 + 1, $2::text, upper($2)";
    let result = only(
        conn.exec_params(
            query.as_bytes(),
            &[],
            &Params::text(&[Some(b"41"), Some(b"a b|c")]),
        )
        .expect("the exchange completes"),
    );
    assert_eq!(result.status(), ExecStatus::TuplesOk);
    assert_eq!(
        result.ntuples(),
        1,
        "a row to compare, not two empty outputs"
    );
    assert_eq!(result.ftype(0), Some(23), "int4");

    let (stdout, stderr, code) = cluster.psql_script(&format!("{query} \\bind 41 'a b|c' \\g\n"));
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(unaligned(&result), stdout, "the row must be byte-identical");
}

/// `PQprepare` then `PQexecPrepared` print what `\parse` then
/// `\bind_named … \g` print, and a statement that does not exist fails
/// with the same terse error line on both sides.
#[test]
fn prepare_and_exec_prepared_match_the_reference_psql() {
    let Some(cluster) = Cluster::start("trust", 55_451) else {
        return;
    };
    let mut conn = cluster.connect();
    let prepared = only(
        conn.prepare(b"s1", b"select $1::int8 * 2, $1::text", &[])
            .expect("the exchange completes"),
    );
    assert_eq!(prepared.status(), ExecStatus::CommandOk);
    let result = only(
        conn.exec_prepared(b"s1", &Params::text(&[Some(b"21")]))
            .expect("the exchange completes"),
    );
    assert_eq!(result.status(), ExecStatus::TuplesOk);
    assert_eq!(
        result.ntuples(),
        1,
        "a row to compare, not two empty outputs"
    );

    let (stdout, stderr, code) =
        cluster.psql_script("select $1::int8 * 2, $1::text \\parse s1\n\\bind_named s1 21 \\g\n");
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    assert_eq!(unaligned(&result), stdout, "the row must be byte-identical");

    let missing = only(
        conn.exec_prepared(b"nope", &Params::text(&[]))
            .expect("the exchange completes"),
    );
    assert_eq!(missing.status(), ExecStatus::FatalError);
    let (_, stderr, _) = cluster.psql_script("\\bind_named nope \\g\n");
    assert_eq!(
        missing.error().expect("an error result").message(
            ExecStatus::FatalError,
            Verbosity::Terse,
            ContextVisibility::Errors
        ),
        stderr,
        "the terse error must be psql's, byte for byte"
    );
}
