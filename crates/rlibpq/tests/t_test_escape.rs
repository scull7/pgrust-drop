//! Port of `src/test/modules/test_escape/test_escape.c` and the TAP script
//! that drives it, `t/001_test_escape.pl` (PostgreSQL REL_18_6), against
//! this crate's escape functions.
//!
//! Upstream builds a C program over libpq, connects it to a fresh node's
//! `db_sql_ascii` database, and requires every TAP line it prints to be
//! `ok` with an empty stderr (`001_test_escape.pl:22`-`:51`). Here the
//! program's body is the harness below, run twice:
//!
//! - [`test_escape_at_each_encoding_without_a_server`] hands each vector's
//!   encoding straight to the pure functions, so the whole matrix runs on
//!   every machine;
//! - [`t_001_test_escape`] is upstream's run: a live `db_sql_ascii`, each
//!   encoding set with `PQsetClientEncoding` (`test_escape.c:865`) and read
//!   back from what the server reports, and every escape made through
//!   [`Connection`]. It needs the reference tools and prints
//!   `SKIP (flagged, not silent)` without them.
//!
//! Differences from the C, none of which touches what is compared:
//! - `appendStringLiteral` and `fmtId` (`test_escape.c:431`, `:436`) are
//!   `fe_utils/string_utils.c`, not libpq; they are rpsql's to port, so their
//!   two rows of `pe_test_escape_funcs` are absent;
//! - the "psql parse" check (`test_psql_parse`, `:577`) runs psql's
//!   encoding-aware lexer over each escaped string, and rpsql's scanner does
//!   not take a client encoding yet, so that one subtest is not run;
//! - `test_gb18030_json` (`:213`) exercises `pg_parse_json`, which is
//!   `src/common/jsonapi.c` and not an escape function ("this isn't an
//!   'escape' test", `:210`);
//! - the Valgrind markers are gone: an escape function receives the input
//!   as a slice of exactly `escape_len` bytes, so it cannot read past it;
//!   the `NEVER_ACCESS_STR` check that the result holds nothing from beyond
//!   the input is still made (`:765`);
//! - `PQescapeString` reads the process-wide encoding and
//!   `standard_conforming_strings` of the last connection that reported
//!   them (`fe-exec.c:60`-`:61`); this crate keeps no process-wide state, so
//!   the harness passes that same connection's values explicitly.
//!
//! The TAP lines are printed as upstream prints them (verbosity 0: only the
//! result line of a success, details before a failure), and the test fails
//! if any line is `not ok`.

#![allow(clippy::doc_markdown)]

use std::fmt::Write as _;

use rlibpq::{Connection, Encoding, EscapeError, ExecStatus, escape_string};

mod common;

use common::{Cluster, connect_to, only};

const NODE_PORT: u16 = 55_520;

/// `NEVER_ACCESS_STR`, `test_escape.c:35`.
const NEVER_ACCESS_STR: &[u8] = b"\xff never-to-be-touched";

/// Where the escape functions get the client encoding and
/// `standard_conforming_strings` from.
enum Escaper<'a> {
    /// Straight from the vector, for the run without a server.
    Fixed(Encoding),
    /// From what the server reported to this connection.
    Live(&'a Connection),
}

impl Escaper<'_> {
    fn client_encoding(&self) -> Encoding {
        match self {
            Escaper::Fixed(encoding) => *encoding,
            Escaper::Live(conn) => conn.client_encoding(),
        }
    }

    fn std_strings(&self) -> bool {
        match self {
            // PostgreSQL 18's default (and the only value since 9.1 unless
            // set otherwise).
            Escaper::Fixed(_) => true,
            Escaper::Live(conn) => conn.std_strings(),
        }
    }

    fn literal(&self, s: &[u8]) -> Result<Vec<u8>, EscapeError> {
        match self {
            Escaper::Fixed(encoding) => rlibpq::escape::escape_internal(s, *encoding, false),
            Escaper::Live(conn) => conn.escape_literal(s),
        }
    }

    fn identifier(&self, s: &[u8]) -> Result<Vec<u8>, EscapeError> {
        match self {
            Escaper::Fixed(encoding) => rlibpq::escape::escape_internal(s, *encoding, true),
            Escaper::Live(conn) => conn.escape_identifier(s),
        }
    }

    fn string_conn(&self, s: &[u8]) -> rlibpq::EscapedString {
        match self {
            Escaper::Fixed(encoding) => escape_string(s, *encoding, self.std_strings()),
            Escaper::Live(conn) => conn.escape_string_conn(s),
        }
    }
}

/// `pe_test_escape_func.escape`: append the escaped form to `target`;
/// `Err` carries `PQerrorMessage` without its newline.
type EscapeFn = fn(&Escaper<'_>, &mut Vec<u8>, &[u8]) -> Result<(), String>;

fn error_text(err: EscapeError) -> String {
    let message = err.message();
    String::from_utf8_lossy(&message[..message.len() - 1]).into_owned()
}

/// `escape_literal`, `test_escape.c:251`.
fn escape_literal(ef: &Escaper<'_>, target: &mut Vec<u8>, s: &[u8]) -> Result<(), String> {
    let escaped = ef.literal(s).map_err(error_text)?;
    target.extend_from_slice(&escaped);
    Ok(())
}

/// `escape_identifier`, `test_escape.c:274`.
fn escape_identifier(ef: &Escaper<'_>, target: &mut Vec<u8>, s: &[u8]) -> Result<(), String> {
    let escaped = ef.identifier(s).map_err(error_text)?;
    target.extend_from_slice(&escaped);
    Ok(())
}

/// `escape_string_conn`, `test_escape.c:297`: the quotes are written, and
/// the escaped text between them, even when an error is reported.
fn escape_string_conn(ef: &Escaper<'_>, target: &mut Vec<u8>, s: &[u8]) -> Result<(), String> {
    let escaped = ef.string_conn(s);
    target.push(b'\'');
    target.extend_from_slice(&escaped.bytes);
    target.push(b'\'');
    escaped.error.map_or(Ok(()), |err| Err(error_text(err)))
}

/// `escape_string`, `test_escape.c:327`: `PQescapeString`, which cannot
/// report an error.
#[allow(clippy::unnecessary_wraps)] // an `EscapeFn`, as in C
fn escape_string_static(ef: &Escaper<'_>, target: &mut Vec<u8>, s: &[u8]) -> Result<(), String> {
    target.push(b'\'');
    target.extend_from_slice(&escape_string(s, ef.client_encoding(), ef.std_strings()).bytes);
    target.push(b'\'');
    Ok(())
}

/// `escape_replace`, `test_escape.c:350`: `s/'/''/`, what non-core drivers
/// do.
#[allow(clippy::unnecessary_wraps)] // an `EscapeFn`, as in C
fn escape_replace(_: &Escaper<'_>, target: &mut Vec<u8>, s: &[u8]) -> Result<(), String> {
    target.push(b'\'');
    for &c in s {
        if c == b'\'' {
            target.extend_from_slice(b"''");
        } else {
            target.push(c);
        }
    }
    target.push(b'\'');
    Ok(())
}

/// `pe_test_escape_func`, `test_escape.c:41`.
struct EscapeFunc {
    name: &'static str,
    reports_errors: bool,
    supports_only_valid: bool,
    supports_only_ascii_overlap: bool,
    escape: EscapeFn,
}

/// `pe_test_escape_funcs[]`, `test_escape.c:396`, less the two `fe_utils`
/// rows (see the module comment). Every remaining row has
/// `supports_input_length`.
const PE_TEST_ESCAPE_FUNCS: &[EscapeFunc] = &[
    EscapeFunc {
        name: "PQescapeLiteral",
        reports_errors: true,
        supports_only_valid: false,
        supports_only_ascii_overlap: false,
        escape: escape_literal,
    },
    EscapeFunc {
        name: "PQescapeIdentifier",
        reports_errors: true,
        supports_only_valid: false,
        supports_only_ascii_overlap: false,
        escape: escape_identifier,
    },
    EscapeFunc {
        name: "PQescapeStringConn",
        reports_errors: true,
        supports_only_valid: false,
        supports_only_ascii_overlap: false,
        escape: escape_string_conn,
    },
    EscapeFunc {
        name: "PQescapeString",
        reports_errors: false,
        supports_only_valid: false,
        supports_only_ascii_overlap: false,
        escape: escape_string_static,
    },
    EscapeFunc {
        name: "replace",
        reports_errors: false,
        supports_only_valid: true,
        supports_only_ascii_overlap: true,
        escape: escape_replace,
    },
];

/// `pe_test_vector`, `test_escape.c:76`: an encoding and the input bytes,
/// NULs included (`TV`/`TV_LEN`, `:443`-`:444`).
type TestVector = (&'static str, &'static [u8]);

/// `pe_test_vectors[]`, `test_escape.c:445`-`:551`, in order.
const PE_TEST_VECTORS: &[TestVector] = &[
    // expected to work sanity checks
    ("UTF-8", b"1"),
    ("UTF-8", b"'"),
    ("UTF-8", b"\""),
    ("UTF-8", b"'"),
    ("UTF-8", b"\""),
    ("UTF-8", b"\\"),
    ("UTF-8", b"\\'"),
    ("UTF-8", b"\\\""),
    // trailing multi-byte character, paddable in available space
    ("UTF-8", b"1\xC0"),
    ("UTF-8", b"1\xE0 "),
    ("UTF-8", b"1\xF0 "),
    ("UTF-8", b"1\xF0  "),
    ("UTF-8", b"1\xF0   "),
    // trailing multi-byte character, not enough space to pad
    ("UTF-8", b"1\xE0"),
    ("UTF-8", b"1\xF0"),
    ("UTF-8", b"\xF0"),
    // try to smuggle in something in invalid characters
    ("UTF-8", b"1\xE0'"),
    ("UTF-8", b"1\xE0\""),
    ("UTF-8", b"1\xF0'"),
    ("UTF-8", b"1\xF0\""),
    ("UTF-8", b"1\xF0'; "),
    ("UTF-8", b"1\xF0\"; "),
    ("UTF-8", b"1\xF0';;;;"),
    ("UTF-8", b"1\xF0  ';;;;"),
    ("UTF-8", b"1\xF0  \";;;;"),
    ("UTF-8", b"1\xE0'; \\l ; "),
    ("UTF-8", b"1\xE0\"; \\l ; "),
    // null byte handling
    ("UTF-8", b"some\0thing"),
    ("UTF-8", b"some\0"),
    ("UTF-8", b"some\xF0'\0"),
    ("UTF-8", b"some\xF0'\0'"),
    ("UTF-8", b"some\xF0ab\0'"),
    // GB18030's 4 byte encoding requires a 2nd byte limited values
    ("GB18030", b"\x90\x31"),
    ("GB18030", b"\\\x81\x5c'"),
    ("GB18030", b"\\\x81\x5c\""),
    ("GB18030", b"\\\x81\x5c\0'"),
    // \x81 indicates a 2 byte char. ' and " are not a valid second byte, but
    // that requires encoding verification to know. E.g. replace_string()
    // doesn't cope.
    ("GB18030", b"\\\x81';"),
    ("GB18030", b"\\\x81\";"),
    // \x81 indicates a 2 byte char. \ is a valid second character.
    ("GB18030", b"\\\x81\\';"),
    ("GB18030", b"\\\x81\\\";"),
    ("GB18030", b"\\\x81\0;"),
    ("GB18030", b"\\\x81\0'"),
    ("GB18030", b"\\\x81'\0"),
    ("SJIS", b"\xF0\x40;"),
    ("SJIS", b"\xF0';"),
    ("SJIS", b"\xF0\";"),
    ("SJIS", b"\xF0\0'"),
    ("SJIS", b"\\\xF0\\';"),
    ("SJIS", b"\\\xF0\\\";"),
    ("gbk", b"\x80';"),
    ("gbk", b"\x80"),
    ("gbk", b"\x80'"),
    ("gbk", b"\x80\""),
    ("gbk", b"\x80\\"),
    ("mule_internal", b"\\\x9c';\0;"),
    ("sql_ascii", b"1\xC0'"),
    // Testcases that are not null terminated for the specified input
    // length (TV_LEN): the slice is the first `len` bytes.
    ("gbk", b"\x80"),
    ("GB18030", b"\x80"),
    ("GB18030", b"\x80\0"),
    ("GB18030", b"\x80\x30"),
    ("GB18030", b"\x80\x30\0"),
    ("GB18030", b"\x80\x30\x30"),
    ("GB18030", b"\x80\x30\x30\0"),
    ("UTF-8", b"\xC3"),
    ("UTF-8", b"\xC3\xb6"),
];

/// `pe_test_config`'s counters and `report_result`, `test_escape.c:119`, at
/// verbosity 0.
#[derive(Default)]
struct Report {
    test_count: usize,
    failure_count: usize,
    out: String,
}

impl Report {
    fn result(&mut self, success: bool, testname: &str, details: &str, subname: &str, desc: &str) {
        self.test_count += 1;
        if !success {
            self.failure_count += 1;
            self.out.push_str(details);
        }
        writeln!(
            self.out,
            "{} {} - {testname}: {subname}: {desc}",
            if success { "ok" } else { "not ok" },
            self.test_count
        )
        .expect("a String takes any text");
    }

    /// `main`'s tail, `test_escape.c:967`-`:969`, and the TAP script's
    /// verdict: every line `ok`.
    fn finish(self, run: &str) {
        println!(
            "{}# {} failures\n1..{}",
            self.out, self.failure_count, self.test_count
        );
        assert!(self.test_count > 0, "{run}: no test ran");
        assert_eq!(
            self.failure_count, 0,
            "{run}: {} not ok, see above",
            self.failure_count
        );
    }
}

/// `escapify`, `test_escape.c:100`.
fn escapify(buf: &mut String, s: &[u8]) {
    for &c in s {
        match c {
            b'\n' => buf.push_str("\\n"),
            0 => buf.push_str("\\0"),
            c if !(b' '..=b'~').contains(&c) => {
                write!(buf, "\\x{c:2x}").expect("a String takes any text");
            }
            c => buf.push(char::from(c)),
        }
    }
}

/// `test_gb18030_page_multiple`, `test_escape.c:180`: a 128 KiB input whose
/// last byte starts a GB18030 character must be refused, not read past.
fn test_gb18030_page_multiple(report: &mut Report, ef: &Escaper<'_>) {
    let input_len = 0x20000;
    let mut input = vec![b'-'; input_len - 1];
    input.push(0xfe);

    let mut testname = format!(">repeat({}, {})", char::from(input[0]), input_len - 1);
    escapify(&mut testname, &input[input_len - 1..]);
    testname.push_str("< - GB18030 - PQescapeLiteral");

    report.result(
        ef.literal(&input).is_err(),
        &testname,
        "",
        "input validity vs escape success",
        "ok",
    );
}

/// `test_one_vector_escape`, `test_escape.c:634`. The two `input_encoding*`
/// names are upstream's.
#[allow(clippy::too_many_lines, clippy::similar_names)]
fn test_one_vector_escape(
    report: &mut Report,
    ef: &Escaper<'_>,
    tv: &TestVector,
    func: &EscapeFunc,
) {
    let (client_encoding, escape) = *tv;
    let encoding = ef.client_encoding();

    if func.supports_only_ascii_overlap && encoding_conflicts_ascii(encoding) {
        return;
    }

    let mut testname = String::from(">");
    escapify(&mut testname, escape);
    write!(testname, "< - {client_encoding} - {}", func.name).expect("a String takes any text");

    let mut details = format!("#\t input: {} bytes: ", escape.len());
    escapify(&mut details, escape);
    details.push('\n');
    writeln!(details, "#\t encoding: {client_encoding}").expect("a String takes any text");

    // check encoding of input, to compare with after the test
    let input_encoding_valid = encoding.verify_str(escape) == escape.len();
    writeln!(
        details,
        "#\t input encoding valid: {}",
        i32::from(input_encoding_valid)
    )
    .expect("a String takes any text");
    let strnlen = escape.iter().position(|&b| b == 0).unwrap_or(escape.len());
    let input_encoding0_valid = encoding.verify_str(&escape[..strnlen]) == strnlen;
    writeln!(
        details,
        "#\t input encoding valid till 0: {}",
        i32::from(input_encoding0_valid)
    )
    .expect("a String takes any text");
    writeln!(details, "#\t escape func: {}", func.name).expect("a String takes any text");

    if !input_encoding_valid && func.supports_only_valid {
        return;
    }

    // The input, followed by bytes the escape function must never see.
    let mut raw_buf = escape.to_vec();
    raw_buf.extend_from_slice(NEVER_ACCESS_STR);

    let mut escape_buf = Vec::new();
    let result = (func.escape)(ef, &mut escape_buf, &raw_buf[..escape.len()]);
    let escape_success = result.is_ok();
    if let Err(err) = &result {
        writeln!(details, "#\t escape error: {err}").expect("a String takes any text");
    }

    let escape_encoding_valid;
    if escape_buf.is_empty() {
        escape_encoding_valid = true;
    } else {
        write!(details, "#\t escaped string: {} bytes: ", escape_buf.len())
            .expect("a String takes any text");
        escapify(&mut details, &escape_buf);
        details.push('\n');

        escape_encoding_valid = encoding.verify_str(&escape_buf) == escape_buf.len();
        writeln!(
            details,
            "#\t escape encoding valid: {}",
            i32::from(escape_encoding_valid)
        )
        .expect("a String takes any text");

        // Verify that no data beyond the end of the input is included in
        // the escaped string (:760).
        let contains_never = !escape_buf
            .windows(NEVER_ACCESS_STR.len())
            .any(|w| w == NEVER_ACCESS_STR);
        report.result(
            contains_never,
            &testname,
            &details,
            "escaped data beyond end of input",
            if contains_never {
                "no"
            } else {
                "all secrets revealed"
            },
        );
    }

    // If the test reports errors, and the input was invalidly encoded,
    // escaping should fail (:776).
    if func.reports_errors {
        let mut ok = true;
        let mut resdesc = "ok";
        if escape_success {
            if !input_encoding0_valid {
                ok = false;
                resdesc = "invalid input escaped successfully";
            } else if !input_encoding_valid {
                resdesc = "invalid input escaped successfully, due to zero byte";
            }
        } else if input_encoding0_valid {
            ok = false;
            resdesc = "valid input failed to escape";
        } else if input_encoding_valid {
            resdesc = "valid input failed to escape, due to zero byte";
        }
        report.result(
            ok,
            &testname,
            &details,
            "input validity vs escape success",
            resdesc,
        );
    }

    // If the input is invalidly encoded, the output should also be
    // invalidly encoded (:814).
    {
        let mut ok = true;
        let mut resdesc = "ok";
        if input_encoding0_valid && !input_encoding_valid && escape_encoding_valid {
            resdesc = "invalid input produced valid output, due to zero byte";
        } else if input_encoding0_valid && !escape_encoding_valid {
            ok = false;
            resdesc = "valid input produced invalid output";
        } else if !input_encoding0_valid
            && (!func.reports_errors || escape_success)
            && escape_encoding_valid
        {
            ok = false;
            resdesc = "invalid input produced valid output";
        }
        report.result(
            ok,
            &testname,
            &details,
            "input and escaped encoding validity",
            resdesc,
        );
    }
}

/// `encoding_conflicts_ascii`, `test_escape.c:156`.
fn encoding_conflicts_ascii(encoding: Encoding) -> bool {
    encoding.is_client_only()
}

/// `test_one_vector`, `test_escape.c:863`, for an escaper already set to
/// the vector's encoding.
fn test_one_vector(report: &mut Report, ef: &Escaper<'_>, tv: &TestVector) {
    for func in PE_TEST_ESCAPE_FUNCS {
        test_one_vector_escape(report, ef, tv, func);
    }
}

/// The whole of `main` (`test_escape.c:957`-`:963`) with each encoding
/// taken from the vector rather than from a server.
#[test]
fn test_escape_at_each_encoding_without_a_server() {
    let mut report = Report::default();
    test_gb18030_page_multiple(&mut report, &Escaper::Fixed(Encoding::Gb18030));
    for tv in PE_TEST_VECTORS {
        let encoding = Encoding::from_name(tv.0.as_bytes()).expect("a known encoding");
        test_one_vector(&mut report, &Escaper::Fixed(encoding), tv);
    }
    report.finish("without a server");
}

/// `PQsetClientEncoding` as `test_one_vector` calls it (`:865`): a failure
/// ends the program.
fn set_client_encoding(conn: &mut Connection, name: &str) {
    let result = conn
        .set_client_encoding(name.as_bytes())
        .expect("the connection holds")
        .expect("the name fits and is not auto");
    assert_eq!(
        result.status(),
        ExecStatus::CommandOk,
        "failed to set encoding to {name}:\n{}",
        String::from_utf8_lossy(&result.error_message())
    );
    assert_eq!(
        Some(conn.client_encoding()),
        Encoding::from_name(name.as_bytes()),
        "the server reported {name} back"
    );
}

/// `001_test_escape.pl` with the C program's body run over [`Connection`].
#[test]
fn t_001_test_escape() {
    let Some(node) = Cluster::start("trust", NODE_PORT) else {
        return;
    };

    // $node->safe_psql('postgres', q(CREATE DATABASE db_sql_ascii ENCODING
    // "sql_ascii" TEMPLATE template0;)); (:14)
    let mut postgres = node.connect();
    let created = only(
        postgres
            .exec(b"CREATE DATABASE db_sql_ascii ENCODING \"sql_ascii\" TEMPLATE template0;")
            .expect("CREATE DATABASE runs"),
    );
    assert_eq!(created.status(), ExecStatus::CommandOk);

    // $node->connstr . " dbname=db_sql_ascii" (:17)
    let mut conn = connect_to(&format!("{} dbname=db_sql_ascii", node.conninfo()));

    let mut report = Report::default();
    set_client_encoding(&mut conn, "GB18030");
    test_gb18030_page_multiple(&mut report, &Escaper::Live(&conn));
    for tv in PE_TEST_VECTORS {
        set_client_encoding(&mut conn, tv.0);
        test_one_vector(&mut report, &Escaper::Live(&conn), tv);
    }
    report.finish("live");

    escaped_values_mean_the_input_to_the_server(&mut postgres);
}

/// Beyond upstream's harness, which only checks the *shape* of an escaped
/// string: the server reads each one back as the input it was made from.
/// A literal and a bytea literal from `PQescapeByteaConn` (hex) and
/// `PQescapeBytea` (escape format) go through the SQL parser; the bytea's
/// output comes back through `PQunescapeBytea`.
fn escaped_values_mean_the_input_to_the_server(conn: &mut Connection) {
    // The cluster is initdb'd under LC_ALL=C, so SQL_ASCII until told.
    set_client_encoding(conn, "UTF8");
    let text: &[u8] = "it's a \\ back'slash, \"quoted\", ünïcode".as_bytes();

    let mut query = b"SELECT ".to_vec();
    query.extend_from_slice(&conn.escape_literal(text).expect("valid UTF-8"));
    query.extend_from_slice(b", ");
    let mut quoted = b"'".to_vec();
    quoted.extend_from_slice(&conn.escape_string_conn(text).bytes);
    quoted.push(b'\'');
    query.extend_from_slice(&quoted);
    query.extend_from_slice(b" AS ");
    query.extend_from_slice(&conn.escape_identifier(b"a \"col\\\"").expect("ASCII"));
    let result = only(conn.exec(&query).expect("the query runs"));
    assert_eq!(
        result.status(),
        ExecStatus::TuplesOk,
        "{:?}",
        result.error()
    );
    assert_eq!(result.value(0, 0), Some(text));
    assert_eq!(result.value(0, 1), Some(text));
    assert_eq!(result.fname(1), Some(&b"a \"col\\\""[..]));

    let all: Vec<u8> = (0..=255).collect();
    for escaped in [
        conn.escape_bytea_conn(&all),
        rlibpq::escape_bytea(&all, conn.std_strings(), false),
    ] {
        let mut query = b"SELECT '".to_vec();
        query.extend_from_slice(&escaped);
        query.extend_from_slice(b"'::bytea");
        let result = only(conn.exec(&query).expect("the query runs"));
        assert_eq!(
            result.status(),
            ExecStatus::TuplesOk,
            "{:?}",
            result.error()
        );
        let value = result.value(0, 0).expect("one value");
        assert_eq!(rlibpq::unescape_bytea(value), all);
    }
}
