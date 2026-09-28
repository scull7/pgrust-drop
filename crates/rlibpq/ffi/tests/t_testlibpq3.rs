//! `src/test/examples/testlibpq3.c` (PostgreSQL REL_18_6), upstream's
//! example of out-of-line parameters and binary I/O, over the C ABI; and
//! `tests/c/params.c`, which drives the extended-query calls and the result
//! metadata past what the example reaches.
//!
//! `tests/c/testlibpq3.c` and `tests/c/testlibpq3.sql` are upstream's files,
//! unmodified (their sha256s are pinned in `upstream_files.rs`). The SQL is
//! loaded with the reference `psql`, as the example's header comment
//! (`testlibpq3.c:8`-`:16`) asks, and the program is compiled against the
//! vendored `libpq-fe.h` and linked with this crate's `libpq.a`. Upstream
//! checks no output; the example's header states the output it expects
//! (`:18`-`:28`), and the expected text here is that, byte for byte as the
//! `printf`s of `show_binary_results` (`:102`-`:110`) write it.
//!
//! `params.c`'s expected text is what the C source answers, cited per case
//! below. Both programs were also run, unchanged, against C libpq (PGDG
//! PostgreSQL 18.6's `libpq.so.5`) and printed exactly this; that
//! side-by-side run is not a gate here (see the PR for NAT-395 slice 4).
//!
//! The live gates need the reference tools; without them they print `SKIP
//! (flagged, not silent)` and pass, and with `PGDROP_REQUIRE_REF=1` a missing
//! reference fails instead.

#![allow(clippy::doc_markdown)]

mod common;
#[path = "../../tests/common/mod.rs"]
mod live;

use common::{build, crate_dir, run};
use live::Cluster;

/// The whole of `testlibpq3.c`'s `main` over `testlibpq3.sql`'s table: a
/// text parameter with binary results, then a binary `int4` parameter with
/// binary results, each printed by `show_binary_results`, which finds its
/// columns with `PQfnumber`. The example prints `tuple 0` both times: each
/// result has one row, and `i` (`testlibpq3.c:73`, `:102`) counts that
/// result's rows.
#[test]
fn testlibpq3_prints_both_binary_results_as_its_header_states() {
    let Some(cluster) = Cluster::start("trust", 55_492) else {
        return;
    };
    let (_, stderr, code) = cluster.psql_script(include_str!("c/testlibpq3.sql"));
    assert_eq!(String::from_utf8_lossy(&stderr), "");
    assert_eq!(code, 0, "testlibpq3.sql loads");
    let program = build(
        "testlibpq3",
        &[crate_dir().join("tests/c/testlibpq3.c")],
        &[],
    );

    let outcome = run(&program, &[&cluster.conninfo()]);

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(String::from_utf8_lossy(&outcome.stdout), TESTLIBPQ3_OUT);
    assert_eq!(outcome.status, Some(0));
}

const TESTLIBPQ3_OUT: &str = "tuple 0: got\n\
     \x20i = (4 bytes) 1\n\
     \x20t = (11 bytes) 'joe's place'\n\
     \x20b = (5 bytes) \\000\\001\\002\\003\\004\n\
     \n\
     tuple 0: got\n\
     \x20i = (4 bytes) 2\n\
     \x20t = (8 bytes) 'ho there'\n\
     \x20b = (5 bytes) \\004\\003\\002\\001\\000\n\
     \n";

/// `tests/c/params.c` against a live server. What C answers, case by case:
///
/// - `PQprepare` is a Parse and a Sync, so its result is COMMAND_OK with an
///   empty tag (`fe-exec.c:2323`, `PQsendPrepare` `:1553`).
/// - `PQdescribePrepared` reports the parameter types the server settled
///   (`getParamDescriptions`, `fe-protocol3.c:690`) and the columns; out of
///   range, `PQparamtype` and `PQftype` answer 0 with a notice
///   (`check_param_number`, `fe-exec.c:3579`; `check_field_number`, `:3541`).
/// - `PQfnumber` folds its argument as an identifier (`:3620`): `Sum` finds
///   nothing, `"Sum"` finds column 0; `""` and NULL are -1 (`:3636`-`:3639`).
/// - A NULL entry of `paramValues` is SQL NULL, and result format 1 makes
///   every column binary (`PQsendQueryGuts`, `:1774`).
/// - A binary `int4` parameter is `paramLengths[0]` bytes (`:1856`-`:1865`).
/// - `PQftable`/`PQftablecol` name a plain column's table and attribute
///   number and are 0 for an expression; `PQfsize`/`PQfmod` are `typlen` and
///   `atttypmod` (`:3717`-`:3781`).
/// - `PQcmdTuples` reads the count out of `INSERT 0 2`, `SELECT 2`,
///   `UPDATE 2` and `DELETE 1`, and nothing out of `CREATE TABLE`
///   (`:3853`); `PQoidValue` and `PQoidStatus` read the OID of an `INSERT`
///   tag (`:3824`, `:3796`).
/// - `PQresultErrorField` answers each field the server sent, NULL for one it
///   did not and for a result with no error (`:3497`).
/// - A NULL command or statement name, a parameter count outside 0..65535 and
///   a binary parameter with no length are refused with NULL and
///   `libpq_append_conn_error`'s message (`:1524`, `:1529`, `:1565`, `:1570`,
///   `:1575`, `:1664`, `:1863`), and the connection stays in step.
/// - `PQdescribePortal` describes a cursor's portal; the unnamed portal, for
///   a NULL name (`:2610`-`:2612`), does not exist.
/// - A binary COPY's result has `PQbinaryTuples` 1 (`getCopyStart`,
///   `fe-protocol3.c:1719`).
#[test]
fn pq_exec_params_and_result_metadata_answer_as_c_libpq_does() {
    let Some(cluster) = Cluster::start("trust", 55_493) else {
        return;
    };
    let program = build(
        "params",
        &[crate_dir().join("tests/c/params.c")],
        &["-Wall", "-Werror"],
    );

    let outcome = run(&program, &[&cluster.conninfo()]);

    assert_eq!(
        String::from_utf8_lossy(&outcome.stderr),
        "parameter number 2 is out of range 0..1\n\
         parameter number -1 is out of range 0..1\n\
         column number 2 is out of range 0..1\n\
         column number 4 is out of range 0..3\n\
         column number 4 is out of range 0..3\n"
    );
    assert_eq!(String::from_utf8_lossy(&outcome.stdout), PARAMS_OUT);
    assert_eq!(outcome.status, Some(0));
}

/// `params.c`'s stdout, case by case as above.
const PARAMS_OUT: &str = "-- prepare s1\n\
     status PGRES_COMMAND_OK cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- describe s1\n\
     nparams 2 paramtype 0 23 paramtype 1 20\n\
     field 0 Sum type 20 size 8 mod -1 format 0\n\
     field 1 two type 25 size -1 mod -1 format 0\n\
     paramtype 2 0 paramtype -1 0 ftype 2 0\n\
     fnumber Sum -1 \"Sum\" 0 sum -1 two 1 TWO 1 \"TWO\" -1 \"\" -1 NULL -1\n\
     -- describe s1\n\
     status PGRES_COMMAND_OK cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- exec s1 text\n\
     nparams 0\n\
     field 0 Sum type 20 size 8 mod -1 format 0\n\
     field 1 two type 25 size -1 mod -1 format 0\n\
     value \"42\" \"2\" binaryTuples 0\n\
     -- exec s1 text\n\
     status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" cmdTuples \"1\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- exec s1 binary\n\
     nparams 0\n\
     field 0 Sum type 20 size 8 mod -1 format 1\n\
     field 1 two type 25 size -1 mod -1 format 1\n\
     isnull 1 1 length 0 0 binaryTuples 1\n\
     -- exec s1 binary\n\
     status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" cmdTuples \"1\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- params binary int4\n\
     nparams 0\n\
     field 0 answer type 23 size 4 mod -1 format 0\n\
     value \"42\"\n\
     -- params binary int4\n\
     status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" cmdTuples \"1\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- create\n\
     status PGRES_COMMAND_OK cmdStatus \"CREATE TABLE\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- insert\n\
     status PGRES_COMMAND_OK cmdStatus \"INSERT 0 2\" cmdTuples \"2\" oidValue 0 oidStatus \"0\"\n\
     resultErrorMessage \"\"\n\
     -- select pt\n\
     nparams 0\n\
     field 0 name type 1043 size -1 mod 14 format 0\n\
     field 1 id type 23 size 4 mod -1 format 0\n\
     field 2 price type 1700 size -1 mod 327686 format 0\n\
     field 3 next type 23 size 4 mod -1 format 0\n\
     field 0 table pt tablecol 2\n\
     field 1 table pt tablecol 1\n\
     field 2 table pt tablecol 3\n\
     field 3 table 0 tablecol 0\n\
     ftable 4 0 ftablecol 4 0\n\
     -- select pt\n\
     status PGRES_TUPLES_OK cmdStatus \"SELECT 2\" cmdTuples \"2\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- update\n\
     status PGRES_COMMAND_OK cmdStatus \"UPDATE 2\" cmdTuples \"2\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- delete\n\
     status PGRES_COMMAND_OK cmdStatus \"DELETE 1\" cmdTuples \"1\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- division by zero\n\
     severity ERROR nonlocalized ERROR sqlstate 22012 primary division by zero schema NULL\n\
     -- division by zero\n\
     status PGRES_FATAL_ERROR cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"ERROR:  division by zero\n\
     \"\n\
     -- no error\n\
     sqlstate NULL\n\
     -- exec nosuch\n\
     status PGRES_FATAL_ERROR cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"ERROR:  prepared statement \"nosuch\" does not exist\n\
     \"\n\
     -- params NULL command\n\
     NULL errorMessage \"command string is a null pointer\n\
     \"\n\
     -- params -1\n\
     NULL errorMessage \"number of parameters must be between 0 and 65535\n\
     \"\n\
     -- params 65536\n\
     NULL errorMessage \"number of parameters must be between 0 and 65535\n\
     \"\n\
     -- prepare NULL name\n\
     NULL errorMessage \"statement name is a null pointer\n\
     \"\n\
     -- prepare NULL query\n\
     NULL errorMessage \"command string is a null pointer\n\
     \"\n\
     -- exec NULL name\n\
     NULL errorMessage \"statement name is a null pointer\n\
     \"\n\
     -- exec binary without length\n\
     NULL errorMessage \"length must be given for binary parameter\n\
     \"\n\
     -- select 1\n\
     status PGRES_TUPLES_OK cmdStatus \"SELECT 1\" cmdTuples \"1\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- describe c\n\
     nparams 0\n\
     field 0 x type 21 size 2 mod -1 format 0\n\
     field 1 y type 19 size 64 mod -1 format 0\n\
     -- describe c\n\
     status PGRES_COMMAND_OK cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"\"\n\
     -- describe NULL\n\
     status PGRES_FATAL_ERROR cmdStatus \"\" cmdTuples \"\" oidValue 0 oidStatus \"\"\n\
     resultErrorMessage \"ERROR:  portal \"\" does not exist\n\
     \"\n\
     -- copy binary\n\
     binaryTuples 1 nfields 1 fformat 1\n";
