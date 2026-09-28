//! The C ABI seen from C: every symbol `abi::SHIMS` lists links from a C
//! program, and each answers through the vendored `libpq-fe.h` what C libpq
//! built without SSL, OpenSSL or GSSAPI answers with no connection, and
//! `PQconninfoOption` arrays reach C field by field as `PQconninfoOptions[]`
//! (`fe-connect.c:200`) defines them.

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use common::{build, crate_dir, newest_archive, run};
use pq::abi::SHIMS;
use rlibpq::CONNINFO_OPTIONS;
use testkit::{CommandOutcome, Environment, run_in};

/// Calculation: a C program that takes the address of every symbol in
/// `names` and prints how many it took. It declares them itself rather than
/// through `libpq-fe.h`, because `PQfreeNotify` is a macro there: what it
/// proves is that each name resolves in `libpq.a`, and nothing about types.
fn symbol_table_program(names: &[&str]) -> String {
    let mut c = String::from("#include <stdio.h>\n\n");
    for name in names {
        let _ = writeln!(c, "extern void {name}(void);");
    }
    c.push_str("\nint\nmain(void)\n{\n\tvoid\t\t(*const table[]) (void) = {\n");
    for name in names {
        let _ = writeln!(c, "\t\t{name},");
    }
    c.push_str("\t};\n\n\tprintf(\"%d\\n\", (int) (sizeof(table) / sizeof(table[0])));\n");
    c.push_str("\treturn 0;\n}\n");
    c
}

#[test]
fn every_shim_in_the_matrix_links_from_c() {
    let names: Vec<&str> = SHIMS.iter().map(|shim| shim.name).collect();
    let source = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rlibpq-ffi-symbols.c");
    std::fs::write(&source, symbol_table_program(&names)).expect("write the program");

    let program = build("symbols", &[source], &[]);
    let outcome = run(&program, &[]);

    assert_eq!(outcome.status, Some(0));
    assert_eq!(outcome.stdout, format!("{}\n", names.len()).into_bytes());
}

/// Past the probes, the NULL arms of every `PGconn` and `PGresult` shim
/// (`fe-connect.c:7575`, `:7638`; `fe-exec.c:3442`, `:3450`, `:3458`, `:3512`,
/// `:3520`, `:3541`, `:3556`, `:3783`; the metadata of `:3497`, `:3528`,
/// `:3620`, `:3717`-`:3772`, `:3796`, `:3824`, `:3853`, `:3946`, `:3957`;
/// and `PQexecStart`'s NULL `conn`, `:2365`), then a conninfo that does not
/// parse: `PQconnectdb` still returns a `PGconn`, `CONNECTION_BAD` with
/// `conninfo_parse`'s message (`fe-connect.c:6347`), and `PQexec`,
/// `PQexecParams` and `PQdescribePortal` on it send nothing
/// (`fe-exec.c:1706`), before any argument is checked. The accessors of
/// that `PGconn` read fields never filled (`fe-connect.c:7472`-`:7680`):
/// NULL, but `""` for `PQpass`, `PQhost` and `PQtty` and `DEF_PGPORT_STR`
/// for `PQport`; `PQconninfo` is every row with no value; and `PQreset`
/// fails with an empty message, the options never having been valid
/// (`pqClosePGconn`, `:5254`; `pqConnectDBStart`, `:2709`). Then each other
/// way in refuses options that do not parse: `PQconnectStart`,
/// `PQconnectdbParams` (`conninfo_array_parse`, `:6528`),
/// `PQconnectStartParams` with an expanded `dbname`, and `PQsetdbLogin`
/// with a `dbName` that is a connection string (`:2250`).
///
/// Every line past the SSL and GSSAPI probes is what C libpq 18.6 prints
/// for the same program (checked against PGDG's `libpq.so.5`).
#[test]
fn every_shim_answers_as_c_libpq_without_ssl_or_gssapi() {
    let program = build(
        "no_connection",
        &[crate_dir().join("tests/c/no_connection.c")],
        &["-Wall", "-Werror"],
    );
    let outcome = run(&program, &[]);

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        "PQlibVersion 180006\n\
         PQisthreadsafe 1\n\
         PQsslInUse 0\n\
         PQgetssl NULL\n\
         PQsslStruct NULL\n\
         PQsslAttribute NULL\n\
         PQsslAttributeNames {NULL}\n\
         PQgetSSLKeyPassHook_OpenSSL NULL\n\
         PQdefaultSSLKeyPassHook_OpenSSL 0\n\
         PQgssEncInUse 0\n\
         PQgetgssctx NULL\n\
         freed\n\
         PQstatus 1\n\
         PQerrorMessage connection pointer is NULL\n\
         PQexec NULL\n\
         PQresultStatus PGRES_FATAL_ERROR\n\
         PQresStatus PGRES_EMPTY_QUERY|PGRES_TUPLES_CHUNK|invalid ExecStatusType code\n\
         PQresultErrorMessage \"\"\n\
         PQntuples 0 PQnfields 0\n\
         PQfname NULL\n\
         PQcmdStatus NULL\n\
         PQgetvalue NULL PQgetlength 0 PQgetisnull 1\n\
         PQbinaryTuples 0 PQfnumber -1\n\
         PQftable 0 PQftablecol 0 PQfformat 0\n\
         PQftype 0 PQfsize 0 PQfmod 0\n\
         PQoidStatus \"\" PQoidValue 0 PQcmdTuples \"\"\n\
         PQnparams 0 PQparamtype 0\n\
         PQresultErrorField NULL\n\
         PQexecParams NULL PQprepare NULL PQexecPrepared NULL\n\
         PQdescribePrepared NULL PQdescribePortal NULL\n\
         PQdb NULL PQuser NULL PQoptions NULL\n\
         PQpass NULL PQhost NULL PQport NULL PQtty NULL\n\
         PQtransactionStatus 4 PQparameterStatus NULL PQserverVersion 0\n\
         PQsocket -1 PQbackendPID 0 PQconnectPoll 0\n\
         PQconninfo NULL\n\
         PQparameterStatus NULL PQresetStart 0 PQresetPoll 0\n\
         PQconnectdb set\n\
         PQstatus 1\n\
         PQerrorMessage missing \"=\" after \"bogus\" in connection info string\n\
         PQexec NULL\n\
         PQerrorMessage no connection to the server\n\
         PQexecParams NULL\n\
         PQerrorMessage no connection to the server\n\
         PQdescribePortal NULL\n\
         PQerrorMessage no connection to the server\n\
         PQdb NULL PQuser NULL PQoptions NULL\n\
         PQpass  PQhost  PQport 5432 PQtty \n\
         PQtransactionStatus 4 PQparameterStatus NULL PQserverVersion 0\n\
         PQsocket -1 PQbackendPID 0 PQconnectPoll 0\n\
         PQconninfo 50 rows, 0 set\n\
         PQparameterStatus NULL\n\
         PQreset PQstatus 1 PQerrorMessage \"\"\n\
         PQresetStart 0 PQresetPoll 0\n\
         PQconnectStart PQstatus 1 PQconnectPoll 0 \
         PQerrorMessage missing \"=\" after \"bogus\" in connection info string\n\
         PQconnectdbParams PQstatus 1 PQerrorMessage invalid connection option \"bogus\"\n\
         PQconnectStartParams PQstatus 1 PQerrorMessage invalid connection option \"bogus\"\n\
         PQsetdbLogin PQstatus 1 PQerrorMessage invalid connection option \"bogus\"\n"
    );
    assert_eq!(outcome.status, Some(0));
}

/// Action: `tests/c/conninfo.c`, built once for every test that runs it —
/// tests run in parallel, and two compilers writing one output would race.
fn conninfo_program() -> &'static Path {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        build(
            "conninfo",
            &[crate_dir().join("tests/c/conninfo.c")],
            &["-Wall", "-Werror"],
        )
    })
}

/// Action: run `program` with `args` in the scrubbed TAP environment plus
/// `env`, so no `PG*` variable of the caller's leaks into the defaults.
fn run_scrubbed(program: &Path, args: &[&str], env: &[(&str, &str)]) -> CommandOutcome {
    let mut environment = Environment::postgres_test("c_abi");
    for (key, value) in env {
        environment = environment.with(*key, *value);
    }
    run_in(program, args, &[], &environment).expect("the program runs")
}

/// Calculation: the row `conninfo.c` prints for option `index` holding `val`.
fn expected_row(index: usize, val: Option<&str>) -> String {
    let def = &CONNINFO_OPTIONS[index];
    format!(
        "{}|{}|{}|{}|{}|{}|{}",
        def.keyword,
        def.envvar.unwrap_or("(null)"),
        def.compiled.unwrap_or("(null)"),
        val.unwrap_or("(null)"),
        def.label,
        def.dispchar.as_str(),
        def.dispsize
    )
}

/// `PQconninfoParse` fills only what the string sets (`use_defaults` false,
/// `fe-connect.c:6185`); every other field of every row is the static
/// table's, and the array ends at the table's end.
#[test]
fn pq_conninfo_parse_lays_every_row_out_as_the_table() {
    let outcome = run_scrubbed(
        conninfo_program(),
        &["host=h port=5433 user='a b'"],
        &[("PGDATABASE", "ignored")],
    );

    let mut expected = String::from("errmsg: (null)\n");
    for (index, def) in CONNINFO_OPTIONS.iter().enumerate() {
        let val = match def.keyword {
            "host" => Some("h"),
            "port" => Some("5433"),
            "user" => Some("a b"),
            _ => None,
        };
        expected.push_str(&expected_row(index, val));
        expected.push('\n');
    }
    expected.push_str("freed\n");

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(String::from_utf8_lossy(&outcome.stdout), expected);
    assert_eq!(outcome.status, Some(0));
}

/// `PQconninfoParse`'s failure: null, and a `malloc`'d message ending in the
/// newline `libpq_append_error` adds (`fe-connect.c:6186`-`:6187`,
/// `fe-misc.c:1539`), which `PQfreemem` releases; with a null `errmsg`, null
/// and nothing else.
#[test]
fn pq_conninfo_parse_reports_an_error_through_errmsg() {
    let outcome = run_scrubbed(conninfo_program(), &["host"], &[]);

    assert_eq!(String::from_utf8_lossy(&outcome.stderr), "");
    assert_eq!(
        String::from_utf8_lossy(&outcome.stdout),
        "error: missing \"=\" after \"host\" in connection info string\n\
         without errmsg: NULL\n"
    );
    assert_eq!(outcome.status, Some(1));
}

/// `PQconndefaults` fills defaults from the environment and the compiled-in
/// values (`conninfo_add_defaults`, `fe-connect.c:6624`). The values are
/// rlibpq's and are tested there; this pins that they reach C in the rows
/// `PQconninfoOptions[]` defines.
#[test]
fn pq_conndefaults_fills_rows_from_the_environment() {
    let outcome = run_scrubbed(conninfo_program(), &[], &[("PGHOST", "envhost")]);
    let stdout = String::from_utf8_lossy(&outcome.stdout);
    let rows: Vec<&str> = stdout.lines().collect();

    assert_eq!(rows.len(), CONNINFO_OPTIONS.len() + 1, "{stdout}");
    assert_eq!(rows.last(), Some(&"freed"));
    for (index, (row, def)) in rows.iter().zip(&CONNINFO_OPTIONS).enumerate() {
        match def.keyword {
            "host" => assert_eq!(*row, expected_row(index, Some("envhost"))),
            // A value it had no default for stays NULL.
            "password" => assert_eq!(*row, expected_row(index, None)),
            _ => {
                let fields: Vec<&str> = row.split('|').collect();
                let want = expected_row(index, None);
                let want: Vec<&str> = want.split('|').collect();
                assert_eq!(fields.len(), want.len(), "{row}");
                // Every field but `val` is the table's.
                assert_eq!(fields[..3], want[..3], "{row}");
                assert_eq!(fields[4..], want[4..], "{row}");
            }
        }
    }
    assert_eq!(outcome.status, Some(0));
}

#[test]
fn symbol_table_program_declares_and_counts_each_name() {
    let c = symbol_table_program(&["PQa", "PQb"]);
    assert!(c.contains("extern void PQa(void);\nextern void PQb(void);\n"));
    assert!(c.contains("\t\tPQa,\n\t\tPQb,\n\t};"));
}

#[test]
fn newest_archive_takes_the_latest_libpq_archive_only() {
    let candidates = [
        ("libpq-aaaa.a".to_owned(), 2),
        ("libpq-bbbb.a".to_owned(), 3),
        ("libpq-cccc.rlib".to_owned(), 9),
        ("libpqcomm-dddd.a".to_owned(), 9),
    ];
    assert_eq!(newest_archive(candidates), Some("libpq-bbbb.a".to_owned()));
    assert_eq!(newest_archive(Vec::<(String, u8)>::new()), None);
}
