//! `fe-print.c` side by side with C libpq: `PQprint`, `PQdisplayTuples` and
//! `PQprintTuples` over the same results, byte for byte.
//!
//! Upstream has no test of its own for these; its one caller is
//! `isolationtester`'s `printResultSet` (`src/test/isolation/
//! isolationtester.c:1113`), whose rendering the unit tests in
//! `rlibpq::print` check against `src/test/isolation/expected/`. This gate
//! covers the rest of the option space: the reference PostgreSQL 18 libpq is
//! linked into a small C program, generated below, that runs each query in
//! [`QUERIES`] and prints its result under every combination of
//! `PQprintOpt`'s six flags, three field separators, and with and without
//! `tableOpt` / `caption` / `fieldName`, then through `PQdisplayTuples` and
//! `PQprintTuples` with each of their arguments varied. rlibpq runs the same
//! queries on the same server and renders the same matrix; the two stdouts
//! must be identical, and C's stderr empty.
//!
//! The program's stdin is not a terminal and its stdout is a pipe, so
//! `PQprint`'s pager (`fe-print.c:150`) is never taken there either; a set
//! `pager` flag is still part of the matrix.
//!
//! It needs the reference tools, the reference libpq next to them (found by
//! [`libpq_candidates`]) and a C compiler (`$CC`, else `cc`). Without them the
//! gate prints `SKIP (flagged, not silent)` and passes; with
//! `PGDROP_REQUIRE_REF=1` it fails instead.

#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rlibpq::{PrintOpt, QueryResult, display_tuples, print, print_tuples};
use testkit::reference::{self, RefPolicy};

mod common;

use common::{Cluster, connect_to};

const NODE_PORT: u16 = 55_550;

/// The results both sides print. The cluster's database is `SQL_ASCII`, so
/// the non-ASCII bytes pass through as sent.
const QUERIES: [&str; 6] = [
    // Numeric, text, NULL, empty and borderline-numeric columns; each column
    // changes its mind (or not) on the second row.
    "SELECT * FROM (VALUES \
       (1, 'one'::text, 1.5, NULL::int, ''::text, '1e5', 'E1', '-3 '), \
       (22, 'twenty two', -0.25, 7, 'x', '2', '3', '4.') \
     ) AS t(n, word, num, maybe, empty, sci, e_first, trail)",
    // A column that looks numeric, then not, then numeric again: expanded
    // output decides row by row.
    "SELECT v AS \"a much longer column name\" FROM (VALUES ('12'), ('abc'), ('34')) AS s(v)",
    // No rows.
    "SELECT 1 AS a, 'b' AS b WHERE false",
    // One row of no fields.
    "SELECT",
    // Multibyte names and values, HTML metacharacters, a newline, and
    // non-ASCII digits.
    "SELECT 'naïve <b>&amp;</b>' AS \"ünïcode\", E'two\\nlines' AS text, \
       '١٢' AS arabic_digits, 'é1' AS \"é\"",
    // Nothing but NULLs.
    "SELECT NULL AS a, NULL AS b",
];

/// `fieldSep`s.
const SEPS: [&str; 3] = ["|", "", " :: "];
/// `fieldName` when replacements are given: one shorter than any real name,
/// one empty (keep the real name), one longer.
const FIELD_NAMES: [&str; 3] = ["x", "", "renamed_column"];
const TABLE_OPT: &str = "border=1";
const CAPTION: &str = "the <caption>";
/// `PQdisplayTuples`'s `fieldSep`s; `None` is `NULL`.
const DISPLAY_SEPS: [Option<&str>; 3] = [None, Some("|"), Some("")];
/// `PQprintTuples`'s `colWidth`s.
const COL_WIDTHS: [usize; 3] = [0, 3, 12];

/// Calculation: `s` as a C string literal, every byte that is not a letter
/// or digit written as an octal escape (which, unlike `\x`, cannot swallow
/// the byte after it).
fn c_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "\\{byte:03o}");
        }
    }
    out.push('"');
    out
}

fn c_array(items: impl IntoIterator<Item = String>) -> String {
    items.into_iter().collect::<Vec<_>>().join(", ")
}

/// Calculation: the C program. Its loops are [`rust_side`]'s, in the same
/// order, and it starts with a line giving `PQlibVersion()`.
// Most of it is the C source, one template.
#[allow(clippy::too_many_lines)]
fn c_program() -> String {
    let queries = c_array(QUERIES.iter().map(|q| c_literal(q)));
    let seps = c_array(SEPS.iter().map(|s| c_literal(s)));
    let names = c_array(FIELD_NAMES.iter().map(|s| c_literal(s)));
    let display_seps = c_array(
        DISPLAY_SEPS
            .iter()
            .map(|s| s.map_or_else(|| "NULL".to_owned(), c_literal)),
    );
    let widths = c_array(COL_WIDTHS.iter().map(ToString::to_string));
    let table_opt = c_literal(TABLE_OPT);
    let caption = c_literal(CAPTION);
    // The declarations are libpq-fe.h's (REL_18_6: pqbool :245, PQprintOpt
    // :247, PQconnectdb :331, PQfinish :343, PQstatus :407, PQerrorMessage
    // :414, PQexec :486, PQresultStatus :582, PQclear :623, PQprint :664,
    // PQdisplayTuples :671, PQprintTuples :678, PQlibVersion :709), written
    // out so that no libpq headers need be installed; the two enums are
    // compared as the ints CONNECTION_OK (:84) and PGRES_TUPLES_OK (:128)
    // are.
    format!(
        r#"#include <stdio.h>
#include <string.h>

typedef struct pg_conn PGconn;
typedef struct pg_result PGresult;
typedef char pqbool;
typedef struct _PQprintOpt
{{
	pqbool header, align, standard, html3, expanded, pager;
	char *fieldSep;
	char *tableOpt;
	char *caption;
	char **fieldName;
}} PQprintOpt;

extern PGconn *PQconnectdb(const char *conninfo);
extern int PQstatus(const PGconn *conn);
extern char *PQerrorMessage(const PGconn *conn);
extern PGresult *PQexec(PGconn *conn, const char *query);
extern int PQresultStatus(const PGresult *res);
extern void PQclear(PGresult *res);
extern void PQfinish(PGconn *conn);
extern void PQprint(FILE *fout, const PGresult *res, const PQprintOpt *po);
extern void PQdisplayTuples(const PGresult *res, FILE *fp, int fillAlign,
							const char *fieldSep, int printHeader, int quiet);
extern void PQprintTuples(const PGresult *res, FILE *fout, int PrintAttNames,
						  int TerseOutput, int colWidth);
extern int PQlibVersion(void);

static const char *const queries[] = {{{queries}}};
static char *const seps[] = {{{seps}}};
static char *names[] = {{{names}, NULL}};
static const char *const display_seps[] = {{{display_seps}}};
static const int widths[] = {{{widths}}};

int
main(int argc, char **argv)
{{
	PGconn *conn;

	if (argc != 2)
		return 2;
	conn = PQconnectdb(argv[1]);
	if (PQstatus(conn) != 0)	/* CONNECTION_OK */
	{{
		fprintf(stderr, "%s", PQerrorMessage(conn));
		return 1;
	}}
	printf("PQlibVersion %d\n", PQlibVersion());
	for (int q = 0; q < (int) (sizeof(queries) / sizeof(queries[0])); q++)
	{{
		PGresult *res = PQexec(conn, queries[q]);

		if (PQresultStatus(res) != 2)	/* PGRES_TUPLES_OK */
		{{
			fprintf(stderr, "query %d: %s", q, PQerrorMessage(conn));
			return 1;
		}}
		for (int bits = 0; bits < 64; bits++)
			for (int s = 0; s < (int) (sizeof(seps) / sizeof(seps[0])); s++)
				for (int extras = 0; extras < 2; extras++)
				{{
					PQprintOpt po;

					memset(&po, 0, sizeof(po));
					po.header = bits & 1;
					po.align = (bits >> 1) & 1;
					po.standard = (bits >> 2) & 1;
					po.html3 = (bits >> 3) & 1;
					po.expanded = (bits >> 4) & 1;
					po.pager = (bits >> 5) & 1;
					po.fieldSep = seps[s];
					if (extras)
					{{
						po.tableOpt = {table_opt};
						po.caption = {caption};
						po.fieldName = names;
					}}
					printf("=== %d PQprint %d %d %d\n", q, bits, s, extras);
					PQprint(stdout, res, &po);
				}}
		for (int fill = 0; fill < 2; fill++)
			for (int s = 0; s < (int) (sizeof(display_seps) / sizeof(display_seps[0])); s++)
				for (int header = 0; header < 2; header++)
					for (int quiet = 0; quiet < 2; quiet++)
					{{
						printf("=== %d PQdisplayTuples %d %d %d %d\n", q, fill, s, header, quiet);
						PQdisplayTuples(res, stdout, fill, display_seps[s], header, quiet);
					}}
		for (int names_ = 0; names_ < 2; names_++)
			for (int terse = 0; terse < 2; terse++)
				for (int w = 0; w < (int) (sizeof(widths) / sizeof(widths[0])); w++)
				{{
					printf("=== %d PQprintTuples %d %d %d\n", q, names_, terse, w);
					PQprintTuples(res, stdout, names_, terse, widths[w]);
				}}
		PQclear(res);
	}}
	PQfinish(conn);
	return 0;
}}
"#
    )
}

/// Action: rlibpq's rendering of the matrix [`c_program`] prints, without
/// its version line.
fn rust_side(conninfo: &str) -> Vec<u8> {
    let mut conn = connect_to(conninfo);
    let mut out = Vec::new();
    for (q, query) in QUERIES.iter().enumerate() {
        // PQexec returns the last result.
        let res: QueryResult = conn
            .exec(query.as_bytes())
            .expect("rlibpq runs the query")
            .pop()
            .expect("a result");
        let names: Vec<&[u8]> = FIELD_NAMES.iter().map(|s| s.as_bytes()).collect();
        for bits in 0..64 {
            for (s, sep) in SEPS.iter().enumerate() {
                for extras in 0..2 {
                    let flag = |bit: u32| (bits >> bit) & 1 == 1;
                    let with_extras = extras == 1;
                    let po = PrintOpt {
                        header: flag(0),
                        align: flag(1),
                        standard: flag(2),
                        html3: flag(3),
                        expanded: flag(4),
                        pager: flag(5),
                        field_sep: sep.as_bytes(),
                        table_opt: with_extras.then_some(TABLE_OPT.as_bytes()),
                        caption: with_extras.then_some(CAPTION.as_bytes()),
                        field_name: if with_extras { &names } else { &[] },
                    };
                    writeln!(out, "=== {q} PQprint {bits} {s} {extras}").unwrap();
                    print(&mut out, &res, &po).unwrap();
                }
            }
        }
        for fill in 0..2 {
            for (s, sep) in DISPLAY_SEPS.iter().enumerate() {
                for header in 0..2 {
                    for quiet in 0..2 {
                        writeln!(out, "=== {q} PQdisplayTuples {fill} {s} {header} {quiet}")
                            .unwrap();
                        let sep = sep.map(str::as_bytes);
                        display_tuples(&res, &mut out, fill == 1, sep, header == 1, quiet == 1)
                            .unwrap();
                    }
                }
            }
        }
        for names_ in 0..2 {
            for terse in 0..2 {
                for (w, &width) in COL_WIDTHS.iter().enumerate() {
                    writeln!(out, "=== {q} PQprintTuples {names_} {terse} {w}").unwrap();
                    print_tuples(&res, &mut out, names_ == 1, terse == 1, width).unwrap();
                }
            }
        }
    }
    out
}

/// Calculation: where the libpq of the PostgreSQL whose tools are in `bin`
/// may be, most likely first: `pg_config --libdir` if there is one
/// (Homebrew), then the layouts of the packages CI installs — Homebrew
/// (`../lib/postgresql`), Alpine (`/usr/libexec/postgresql18` → `/usr/lib`)
/// and PGDG (`/usr/lib/postgresql/18/bin` → `/usr/lib/<triplet>`) — and a
/// plain `../lib`.
fn libpq_candidates(bin: &Path, pg_config_libdir: Option<&Path>) -> Vec<PathBuf> {
    let relative = [
        "../lib",
        "../lib/postgresql",
        "../../lib",
        "../../../x86_64-linux-gnu",
        "../../../aarch64-linux-gnu",
    ];
    pg_config_libdir
        .map(Path::to_path_buf)
        .into_iter()
        .chain(relative.iter().map(|rel| bin.join(rel)))
        .flat_map(|dir| ["libpq.so.5", "libpq.5.dylib"].map(|name| dir.join(name)))
        .collect()
}

#[test]
fn libpq_candidates_cover_each_lanes_layout() {
    let pgdg = libpq_candidates(Path::new("/usr/lib/postgresql/18/bin"), None);
    assert!(pgdg.contains(&PathBuf::from(
        "/usr/lib/postgresql/18/bin/../../../x86_64-linux-gnu/libpq.so.5"
    )));
    let alpine = libpq_candidates(Path::new("/usr/libexec/postgresql18"), None);
    assert!(alpine.contains(&PathBuf::from(
        "/usr/libexec/postgresql18/../../lib/libpq.so.5"
    )));
    let libdir = Path::new("/opt/homebrew/opt/postgresql@18/lib/postgresql");
    let brew = libpq_candidates(
        Path::new("/opt/homebrew/opt/postgresql@18/bin"),
        Some(libdir),
    );
    assert_eq!(brew[1], libdir.join("libpq.5.dylib"));
}

/// Action: `pg_config --libdir` from `bin`, if it runs.
fn pg_config_libdir(bin: &Path) -> Option<PathBuf> {
    let out = Command::new(bin.join("pg_config"))
        .arg("--libdir")
        .output()
        .ok()?;
    let dir = String::from_utf8(out.stdout).ok()?;
    out.status.success().then(|| PathBuf::from(dir.trim_end()))
}

/// Action: a gate prerequisite that is not a reference tool is missing —
/// skip, or under `PGDROP_REQUIRE_REF=1` fail.
fn missing(what: &str) {
    match reference::policy() {
        RefPolicy::Require => panic!("PGDROP_REQUIRE_REF is set, but {what}"),
        RefPolicy::Skip => reference::announce_skip(&format!("{}: {what}", reference::SKIP_FLAG)),
    }
}

/// Action: compile [`c_program`] against `libpq` into `dir`.
fn compile(dir: &Path, libpq: &Path) -> Option<PathBuf> {
    let source = dir.join("fe_print.c");
    let program = dir.join("fe_print");
    std::fs::write(&source, c_program()).expect("write the C program");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_owned());
    let libdir = libpq.parent().expect("a library has a directory");
    let out = Command::new(&cc)
        .arg("-o")
        .arg(&program)
        .arg(&source)
        .arg(libpq)
        .arg(format!("-Wl,-rpath,{}", libdir.display()))
        .output();
    match out {
        Ok(out) if out.status.success() => Some(program),
        Ok(out) => panic!(
            "{cc} failed to build the C side:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(err) => {
            missing(&format!("no C compiler `{cc}` ran: {err}"));
            None
        }
    }
}

/// The first case whose output differs, for a readable failure.
fn first_difference(c: &[u8], rust: &[u8]) -> String {
    let split = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .split("=== ")
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let (c, rust) = (split(c), split(rust));
    for (c, rust) in c.iter().zip(&rust) {
        if c != rust {
            return format!("C libpq:\n=== {c}\nrlibpq:\n=== {rust}");
        }
    }
    format!("{} cases from C, {} from rlibpq", c.len(), rust.len())
}

#[test]
fn fe_print_matches_c_libpq() {
    let Some(cluster) = Cluster::start("trust", NODE_PORT) else {
        return;
    };
    let libdir = pg_config_libdir(&cluster.bin);
    let Some(libpq) = libpq_candidates(&cluster.bin, libdir.as_deref())
        .into_iter()
        .find(|path| path.is_file())
    else {
        missing(&format!(
            "no reference libpq was found beside {}",
            cluster.bin.display()
        ));
        return;
    };
    let Some(program) = compile(&cluster.dir, &libpq) else {
        return;
    };

    let conninfo = cluster.conninfo();
    let out = Command::new(&program)
        .arg(&conninfo)
        .stdin(Stdio::null())
        .output()
        .expect("the C side runs");
    assert!(
        out.status.success() && out.stderr.is_empty(),
        "the C side failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let newline = out
        .stdout
        .iter()
        .position(|&b| b == b'\n')
        .expect("a version line");
    let version = std::str::from_utf8(&out.stdout[..newline]).expect("ASCII");
    let version: u32 = version
        .strip_prefix("PQlibVersion ")
        .and_then(|v| v.parse().ok())
        .expect("PQlibVersion <n>");
    if !(180_000..190_000).contains(&version) {
        missing(&format!(
            "{} is libpq {version}, not PostgreSQL 18",
            libpq.display()
        ));
        return;
    }
    let _ = writeln!(
        std::io::stderr().lock(),
        "fe_print_matches_c_libpq: C side is {} (PQlibVersion {version})",
        libpq.display()
    );
    let c = &out.stdout[newline + 1..];

    let rust = rust_side(&conninfo);
    assert!(c == rust.as_slice(), "{}", first_difference(c, &rust));
}
