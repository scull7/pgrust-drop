//! Port of `src/interfaces/libpq/t/001_uri.pl` (PostgreSQL 18.6).
//!
//! Upstream runs the C helper `libpq_uri_regress` once per row of `@tests`
//! (`001_uri.pl:14`) and compares its stdout, its stderr and whether it
//! succeeded against the row. This runs *our* `libpq_uri_regress` the same way,
//! from the same table, in the environment `PostgreSQL::Test::Utils` scrubs
//! (`testkit::Environment::postgres_test`) — the expectations were written
//! against that environment and mean nothing outside it.
//!
//! `libpq_uri_regress_matches_the_c_helper` is the byte-diff gate over the same
//! table; it is skipped, flagged, when there is no C helper to gate against.

// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::path::Path;

use testkit::{Environment, Gate, run_in};

const LIBPQ_URI_REGRESS: &str = env!("CARGO_BIN_EXE_libpq_uri_regress");

/// One row of `@tests` (`001_uri.pl:10`): "the first element is the input
/// string, the second the expected stdout and the third the expected stderr.
/// Optionally, additional arguments may specify key/value pairs which will
/// override environment variables for the duration of the test."
struct UriTest {
    uri: &'static str,
    stdout: &'static str,
    stderr: &'static str,
    env: &'static [(&'static str, &'static str)],
}

const TESTS: [UriTest; 63] = [
    UriTest {
        uri: "postgresql://uri-user:secret@host:12345/db",
        stdout: "user='uri-user' password='secret' dbname='db' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://uri-user@host:12345/db",
        stdout: "user='uri-user' dbname='db' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://uri-user@host/db",
        stdout: "user='uri-user' dbname='db' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host:12345/db",
        stdout: "dbname='db' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db",
        stdout: "dbname='db' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://uri-user@host:12345/",
        stdout: "user='uri-user' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://uri-user@host/",
        stdout: "user='uri-user' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://uri-user@",
        stdout: "user='uri-user' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host:12345/",
        stdout: "host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host:12345",
        stdout: "host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db",
        stdout: "dbname='db' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://",
        stdout: "(local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://?hostaddr=127.0.0.1",
        stdout: "hostaddr='127.0.0.1' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://example.com?hostaddr=63.1.2.4",
        stdout: "host='example.com' hostaddr='63.1.2.4' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://%68ost/",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db?user=uri-user",
        stdout: "user='uri-user' dbname='db' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db?user=uri-user&port=12345",
        stdout: "user='uri-user' dbname='db' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db?u%73er=someotheruser&port=12345",
        stdout: "user='someotheruser' dbname='db' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host/db?u%7aer=someotheruser&port=12345",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid URI query parameter: "uzer""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host:12345?user=uri-user",
        stdout: "user='uri-user' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?user=uri-user",
        stdout: "user='uri-user' host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?  user = uri-user & port  = 12345 ",
        stdout: "user='uri-user' host='host' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?  user user  =  uri  & port = 12345 12 ",
        stdout: "",
        stderr: r#"libpq_uri_regress: unexpected spaces found in "  user user  ", use percent-encoded spaces (%20) instead"#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?  user  =  uri-user  & port = 12345 12 ",
        stdout: "",
        stderr: r#"libpq_uri_regress: unexpected spaces found in " 12345 12 ", use percent-encoded spaces (%20) instead"#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://[::1]:12345/db",
        stdout: "dbname='db' host='::1' port='12345' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://[::1]/db",
        stdout: "dbname='db' host='::1' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://[2001:db8::1234]/",
        stdout: "host='2001:db8::1234' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://[200z:db8::1234]/",
        stdout: "host='200z:db8::1234' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://[::1]",
        stdout: "host='::1' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://",
        stdout: "(local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres:///",
        stdout: "(local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres:///db",
        stdout: "dbname='db' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://uri-user@/db",
        stdout: "user='uri-user' dbname='db' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://?host=/path/to/socket/dir",
        stdout: "host='/path/to/socket/dir' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?uzer=",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid URI query parameter: "uzer""#,
        env: &[],
    },
    UriTest {
        uri: "postgre://",
        stdout: "",
        stderr: r#"libpq_uri_regress: missing "=" after "postgre://" in connection info string"#,
        env: &[],
    },
    UriTest {
        uri: "postgres://[::1",
        stdout: "",
        stderr: r#"libpq_uri_regress: end of string reached when looking for matching "]" in IPv6 host address in URI: "postgres://[::1""#,
        env: &[],
    },
    UriTest {
        uri: "postgres://[]",
        stdout: "",
        stderr: r#"libpq_uri_regress: IPv6 host address may not be empty in URI: "postgres://[]""#,
        env: &[],
    },
    UriTest {
        uri: "postgres://[::1]z",
        stdout: "",
        stderr: r#"libpq_uri_regress: unexpected character "z" at position 17 in URI (expected ":" or "/"): "postgres://[::1]z""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?zzz",
        stdout: "",
        stderr: r#"libpq_uri_regress: missing key/value separator "=" in URI query parameter: "zzz""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?value1&value2",
        stdout: "",
        stderr: r#"libpq_uri_regress: missing key/value separator "=" in URI query parameter: "value1""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?key=key=value",
        stdout: "",
        stderr: r#"libpq_uri_regress: extra key/value separator "=" in URI query parameter: "key""#,
        env: &[],
    },
    UriTest {
        uri: "postgres://host?dbname=%XXfoo",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid percent-encoded token: "%XXfoo""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://a%00b",
        stdout: "",
        stderr: r#"libpq_uri_regress: forbidden value %00 in percent-encoded value: "a%00b""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://%zz",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid percent-encoded token: "%zz""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://%1",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid percent-encoded token: "%1""#,
        env: &[],
    },
    UriTest {
        uri: "postgresql://%",
        stdout: "",
        stderr: r#"libpq_uri_regress: invalid percent-encoded token: "%""#,
        env: &[],
    },
    UriTest {
        uri: "postgres://@host",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://host:/",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://:12345/",
        stdout: "port='12345' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://otheruser@?host=/no/such/directory",
        stdout: "user='otheruser' host='/no/such/directory' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://otheruser@/?host=/no/such/directory",
        stdout: "user='otheruser' host='/no/such/directory' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://otheruser@:12345?host=/no/such/socket/path",
        stdout: "user='otheruser' host='/no/such/socket/path' port='12345' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://otheruser@:12345/db?host=/path/to/socket",
        stdout: "user='otheruser' dbname='db' host='/path/to/socket' port='12345' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://:12345/db?host=/path/to/socket",
        stdout: "dbname='db' host='/path/to/socket' port='12345' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://:12345?host=/path/to/socket",
        stdout: "host='/path/to/socket' port='12345' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgres://%2Fvar%2Flib%2Fpostgresql/dbname",
        stdout: "dbname='dbname' host='/var/lib/postgresql' (local)",
        stderr: "",
        env: &[],
    },
    UriTest {
        uri: "postgresql://host?sslmode=disable",
        stdout: "host='host' sslmode='disable' (inet)",
        stderr: "",
        env: &[("PGSSLROOTCERT", "system")],
    },
    UriTest {
        uri: "postgresql://host?sslmode=prefer",
        stdout: "host='host' sslmode='prefer' (inet)",
        stderr: "",
        env: &[("PGSSLROOTCERT", "system")],
    },
    UriTest {
        uri: "postgresql://host?sslmode=verify-full",
        stdout: "host='host' (inet)",
        stderr: "",
        env: &[("PGSSLROOTCERT", "system")],
    },
];

/// `local %ENV = %ENV; %ENV = (%ENV, %envvars);` (`001_uri.pl:258`, `:268`) on
/// top of the `Utils.pm` scrub every TAP test starts from.
fn environment(test: &UriTest) -> Environment {
    let mut environment = Environment::postgres_test("001_uri.pl");
    for (key, value) in test.env {
        environment = environment.with(*key, *value);
    }
    environment
}

/// `chomp` (`001_uri.pl:275`): remove one trailing newline, if there is one.
fn chomp(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

/// `test_uri` (`001_uri.pl:255`), run for each row by the `foreach` at `:283`.
///
/// Upstream's `is_deeply(\%result, \%expect, $uri)` is one test result per row
/// that keeps stdout, stderr and the exit status distinguishable, and every row
/// is reported whatever the earlier ones did. One `#[test]` that collects all
/// the mismatches is the closest Rust equivalent: the URI is the upstream test
/// name and it names every failure below.
#[test]
fn test_uri() {
    let mut failures = Vec::new();

    for test in &TESTS {
        let outcome = run_in(
            Path::new(LIBPQ_URI_REGRESS),
            [test.uri],
            &[],
            &environment(test),
        )
        .unwrap_or_else(|err| panic!("could not run {LIBPQ_URI_REGRESS}: {err}"));

        let stdout = String::from_utf8_lossy(chomp(&outcome.stdout)).into_owned();
        let stderr = String::from_utf8_lossy(chomp(&outcome.stderr)).into_owned();
        // $expect{'exit'} = $expect{stderr} eq ''; (001_uri.pl:267)
        let expected_exit = test.stderr.is_empty();

        if stdout != test.stdout {
            failures.push(format!(
                "{}\n  stdout: expected {:?}\n          got      {:?}",
                test.uri, test.stdout, stdout
            ));
        }
        if stderr != test.stderr {
            failures.push(format!(
                "{}\n  stderr: expected {:?}\n          got      {:?}",
                test.uri, test.stderr, stderr
            ));
        }
        if outcome.succeeded() != expected_exit {
            failures.push(format!(
                "{}\n  exit:   expected success={expected_exit}, got status {:?}",
                test.uri, outcome.status
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} cases from 001_uri.pl failed:\n{}",
        failures.len(),
        TESTS.len(),
        failures.join("\n")
    );
}

/// Calculation: every URI the table holds more than once, with how many times,
/// in a stable order.
fn repeated_uris(tests: &[UriTest]) -> Vec<(&'static str, usize)> {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for test in tests {
        *counts.entry(test.uri).or_default() += 1;
    }
    counts.into_iter().filter(|(_, times)| *times > 1).collect()
}

/// The table above was copied from `001_uri.pl:14`; this is the pin that says
/// nothing was dropped on the way.
///
/// The row *count* needs no test: `TESTS` is declared `[UriTest; 63]`, so
/// `TESTS.len() == 63` is a compile-time property and an assertion on it
/// cannot fail. What the array type cannot give is which rows those 63 are,
/// and the transcription slip that survives a correct count is pasting one row
/// twice instead of advancing to the next — the count stays 63 while a row of
/// upstream's table is gone and its coverage with it.
///
/// Upstream repeats exactly one URI, `postgresql://host/db`, at `001_uri.pl:32`
/// and again at `:46`, so exactly one repeat is expected here and any other is
/// a row of upstream's table overwritten by its neighbour.
#[test]
fn the_stolen_table_repeats_only_the_row_upstream_repeats() {
    assert_eq!(
        repeated_uris(&TESTS),
        [("postgresql://host/db", 2)],
        "a URI repeated here that upstream does not repeat means a row was pasted over"
    );
}

/// Gate: every row of the stolen table through the C `libpq_uri_regress` and
/// through ours, byte for byte on stdout, stderr and the exit status.
///
/// The helper is a test program, not an installed one: it is built into
/// `src/interfaces/libpq/test/` of a PostgreSQL 18.6 source tree and no
/// package ships it, so `PGDROP_REF_BIN` has to point at that directory for
/// this gate to be live. Without it the gate prints `SKIP (flagged, not
/// silent)` and passes — it is never narrowed to something weaker.
#[test]
fn libpq_uri_regress_matches_the_c_helper() {
    let Some(gate) = Gate::for_tool_or_skip("libpq_uri_regress", LIBPQ_URI_REGRESS) else {
        return; // The flagged skip is already on stderr.
    };
    for test in &TESTS {
        gate.clone()
            .arg(test.uri)
            .with_env(environment(test))
            .assert_clean();
    }
}
