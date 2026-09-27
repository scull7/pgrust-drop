//! The C ABI seen from C: every symbol `abi::SHIMS` lists links from a C
//! program, and each answers through the vendored `libpq-fe.h` what C libpq
//! built without SSL, OpenSSL or GSSAPI answers with no connection.

mod common;

use std::fmt::Write as _;

use common::{build, crate_dir, newest_archive, run};
use pq::abi::SHIMS;

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
         freed\n"
    );
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
