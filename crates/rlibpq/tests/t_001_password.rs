//! Port of the `.pgpass` block of `src/test/authentication/t/001_password.pl`
//! (PostgreSQL REL_18_6, `:563`-`:639`), run against a live reference cluster
//! twice per `test_conn`: through the reference `psql`, as upstream runs it,
//! and through this crate's `ConnInfo::add_defaults` and `Connection`.
//!
//! The rest of `001_password.pl` tests the server's authentication, not
//! libpq's password file, and is not ported here. What the block needs from
//! it is set up first, as upstream has it by `:563`: `log_connections` at
//! `all` (`:68`, `:112`), `log_min_messages = debug2` (`:70`), the roles
//! `scram_role` and `md5_role` (`:131`-`:141`) and `"scram,role"` (`:150`-`:153`),
//! and the database `regex_testdb` (`:203`), all with password `pass`.
//!
//! Differences from the Perl, none of which touches what is compared:
//! - the environment is a value handed to `add_defaults` and to
//!   `Connection::connect`, not `%ENV`: `PGHOST`, `PGPORT` (`Cluster.pm:1718`),
//!   `PGDATABASE` (`Cluster.pm:158`), `PGPASSFILE` (`:568`), and a `HOME`
//!   with no `.pgpass` in it; `PGPASSWORD` and `PGCHANNELBINDING` are unset
//!   (`:566`-`:567`) by never being set;
//! - `reset_pg_hba`'s `$node->reload` (`:37`) waits for the postmaster to
//!   log `received SIGHUP, reloading configuration files`
//!   (`postmaster.c:2005`) before going on, so the next connection meets the
//!   new `pg_hba.conf`; upstream does not wait;
//! - `connect_ok`'s "no stderr" for this crate is "no password-file
//!   warning", the one thing it writes to stderr itself.
//!
//! `nat_393_the_password_file_warnings_match_c_psql` is not upstream's: it
//! is NAT-393's acceptance ("a 0644 passfile produces the exact warning and
//! is ignored"), diffed against the reference `psql` byte for byte.
//!
//! Without the reference tools each test prints `SKIP (flagged, not silent)`
//! and passes; with `PGDROP_REQUIRE_REF=1` a missing reference fails instead.

#![allow(clippy::doc_markdown)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Connection, ExecStatus, Filesystem, PasswordLookup};
use testkit::Pattern;

mod common;

use common::{Cluster, only};

/// The node `001_password.pl` runs on.
const NODE_PORT: u16 = 55_520;

/// The node the NAT-393 warning gate runs on.
const WARNING_PORT: u16 = 55_521;

/// The block's node: the cluster, and the environment every `test_conn`
/// runs in.
struct Node {
    cluster: Cluster,
    env: Env,
}

impl Node {
    fn start(port: u16) -> Option<Self> {
        let cluster = Cluster::start_configured(
            "trust",
            port,
            "",
            "log_connections = all\nlog_min_messages = debug2\n",
        )?;
        let home = cluster.dir.join("home");
        std::fs::create_dir_all(&home).expect("home");
        let env = Env::empty()
            .with("PGHOST", utf8(&cluster.dir))
            .with("PGPORT", cluster.port.to_string())
            .with("PGDATABASE", "postgres")
            .with("HOME", utf8(&home));
        Some(Node { cluster, env })
    }

    fn data(&self) -> PathBuf {
        self.cluster.dir.join("data")
    }

    fn logfile(&self) -> PathBuf {
        self.cluster.dir.join("log")
    }

    /// `safe_psql` as the superuser, while `pg_hba.conf` still says `trust`.
    fn safe_sql(&self, sql: &str) {
        let mut conn = self.cluster.connect();
        for result in conn.exec(sql.as_bytes()).expect("PQexec") {
            assert_eq!(result.status(), ExecStatus::CommandOk, "{sql}: {result:?}");
        }
    }

    /// `reset_pg_hba`, `:26`: replace `pg_hba.conf` with one line (written
    /// with a continuation, as upstream's is) and reload.
    fn reset_pg_hba(&self, database: &str, role: &str, hba_method: &str) {
        let hba = self.data().join("pg_hba.conf");
        std::fs::remove_file(&hba).expect("unlink pg_hba.conf");
        // append_conf, Cluster.pm: the text and a newline.
        std::fs::write(&hba, format!("local {database} {role}\\\n {hba_method}\n"))
            .expect("pg_hba.conf");
        self.reload();
    }

    /// `$node->reload`, then the wait described in the module comment.
    fn reload(&self) {
        const RELOADING: &str = "received SIGHUP, reloading configuration files";
        let reloads = || slurp(&self.logfile(), 0).matches(RELOADING).count();
        let before = reloads();
        let out = Command::new(self.cluster.bin.join("pg_ctl"))
            .arg("--pgdata")
            .arg(self.data())
            .arg("reload")
            .output()
            .expect("pg_ctl reload runs");
        assert!(out.status.success(), "pg_ctl reload failed");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while reloads() == before {
            assert!(std::time::Instant::now() < deadline, "no reload logged");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The reference `psql` as `Cluster.pm`'s `psql` runs it for
    /// `connect_ok` / `connect_fails` (`Cluster.pm:2557`, `:2639`): `-XAtq`,
    /// `-w`, the SQL on stdin.
    fn c_psql(&self, connstr: &str, sql: &str) -> (i32, String) {
        use std::io::Write as _;
        let mut command = Command::new(self.cluster.bin.join("psql"));
        command
            .args(["--no-psqlrc", "--no-align", "--tuples-only", "--quiet"])
            .args(["--dbname", connstr, "--file", "-", "-w"])
            .env_clear()
            .env("LC_ALL", "C");
        for key in ["PGHOST", "PGPORT", "PGDATABASE", "PGPASSFILE", "HOME"] {
            if let Some(value) = self.env.get(key) {
                command.env(key, String::from_utf8(value.to_vec()).expect("utf-8"));
            }
        }
        let mut child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("reference psql runs");
        child
            .stdin
            .take()
            .expect("a stdin pipe")
            .write_all(sql.as_bytes())
            .expect("the SQL is written");
        let out = child.wait_with_output().expect("reference psql exits");
        let stderr = String::from_utf8(out.stderr).expect("utf-8 stderr");
        (out.status.code().unwrap_or(-1), stderr)
    }

    /// This crate's side of `connect_ok` / `connect_fails`: the conninfo
    /// `psql -d` would build, filled in and connected, then `sql` run. The
    /// password-file lookup's warning comes back with the outcome, being
    /// what `Connection::connect` writes to stderr.
    fn rlibpq(&self, connstr: &str, sql: &str) -> (Result<(), String>, Option<String>) {
        let mut info = parse_conninfo(connstr.as_bytes()).expect("conninfo parses");
        if let Err(err) = info.add_defaults(&self.env, &Filesystem) {
            return (Err(err.to_string()), None);
        }
        let warning = PasswordLookup::new(&info, &self.env, &Filesystem)
            .warning
            .map(|warning| warning.to_string());
        let outcome = match Connection::connect(&info, &self.env, &Filesystem) {
            Err(err) => Err(err.to_string()),
            Ok(mut conn) => {
                let result = only(conn.exec(sql.as_bytes()).expect("PQexec"));
                match result.status() {
                    ExecStatus::TuplesOk => Ok(()),
                    _ => Err(format!("{result:?}")),
                }
            }
        };
        (outcome, warning)
    }

    /// `test_conn`, `:43`, with `connect_ok` / `connect_fails`'s log checks
    /// (`log_check`, `Cluster.pm:2955`) — run through the reference `psql`
    /// and then through this crate, each against its own stretch of the
    /// server log.
    fn test_conn(
        &self,
        connstr: &str,
        method: &str,
        expected_res: i32,
        log_like: &[&str],
        log_unlike: &[&str],
    ) {
        let status_string = if expected_res == 0 {
            "success"
        } else {
            "failed"
        };
        let testname =
            format!("authentication {status_string} for method {method}, connstr {connstr}");
        let sql = format!("SELECT $$connected with {connstr}$$");

        let log_location = log_size(&self.logfile());
        let (ret, stderr) = self.c_psql(connstr, &sql);
        if expected_res == 0 {
            assert_eq!(ret, 0, "C psql: {testname}: {stderr}");
            assert_eq!(stderr, "", "C psql: {testname}: no stderr");
        } else {
            assert_ne!(ret, 0, "C psql: {testname}");
        }
        self.log_check(
            &format!("C psql: {testname}"),
            log_location,
            log_like,
            log_unlike,
        );

        let log_location = log_size(&self.logfile());
        let (outcome, warning) = self.rlibpq(connstr, &sql);
        if expected_res == 0 {
            assert_eq!(outcome, Ok(()), "rlibpq: {testname}");
            assert_eq!(warning, None, "rlibpq: {testname}: no stderr");
        } else {
            assert!(outcome.is_err(), "rlibpq: {testname}");
        }
        self.log_check(
            &format!("rlibpq: {testname}"),
            log_location,
            log_like,
            log_unlike,
        );
    }

    fn log_check(&self, test_name: &str, offset: usize, log_like: &[&str], log_unlike: &[&str]) {
        if log_like.is_empty() && log_unlike.is_empty() {
            return;
        }
        let log_contents = slurp(&self.logfile(), offset);
        for regex in log_like {
            let pattern = Pattern::new(regex).expect("pattern compiles");
            assert!(
                pattern.is_match(&log_contents),
                "{test_name}: log matches {regex}"
            );
        }
        for regex in log_unlike {
            let pattern = Pattern::new(regex).expect("pattern compiles");
            assert!(
                !pattern.is_match(&log_contents),
                "{test_name}: log does not match {regex}"
            );
        }
    }
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

fn log_size(path: &Path) -> usize {
    std::fs::metadata(path).map_or(0, |metadata| {
        usize::try_from(metadata.len()).expect("log size")
    })
}

/// `slurp_file($file, $offset)`.
fn slurp(path: &Path, offset: usize) -> String {
    let bytes = std::fs::read(path).expect("the log is read");
    String::from_utf8_lossy(&bytes[offset.min(bytes.len())..]).into_owned()
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

fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// What `001_password.pl` has set up by `:563`: the three roles and the
/// database the block connects as and to.
fn roles_and_database(node: &Node) {
    node.safe_sql(
        "SET password_encryption='scram-sha-256'; CREATE ROLE scram_role LOGIN PASSWORD 'pass';",
    );
    node.safe_sql("SET password_encryption='md5'; CREATE ROLE md5_role LOGIN PASSWORD 'pass';");
    node.safe_sql(
        "SET password_encryption='scram-sha-256'; CREATE ROLE \"scram,role\" LOGIN PASSWORD 'pass';",
    );
    node.safe_sql("CREATE database regex_testdb;");
}

/// `001_password.pl:563`-`:639`, in upstream's order.
#[test]
#[allow(clippy::too_many_lines)]
fn test_pgpass_processing() {
    let Some(mut node) = Node::start(NODE_PORT) else {
        return;
    };
    roles_and_database(&node);

    // Test .pgpass processing; but use a temp file, don't overwrite the real one!
    let pgpassfile = node.cluster.dir.join("pgpass");

    node.env = node.env.clone().with("PGPASSFILE", utf8(&pgpassfile));

    let _ = std::fs::remove_file(&pgpassfile);
    append_to_file(
        &pgpassfile,
        "\n# This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file. This very long comment is just here to exercise handling of long lines in the file.\n*:*:postgres:scram_role:pass:this is not part of the password.\n",
    );
    chmod(&pgpassfile, 0o600);

    node.reset_pg_hba("all", "all", "password");
    node.test_conn("user=scram_role", "password from pgpass", 0, &[], &[]);
    node.test_conn("user=md5_role", "password from pgpass", 2, &[], &[]);

    append_to_file(
        &pgpassfile,
        "\n*:*:*:scram_role:p\\ass\n*:*:*:scram,role:p\\ass\n",
    );

    node.test_conn("user=scram_role", "password from pgpass", 0, &[], &[]);

    // Testing with regular expression for username.  The third regexp matches.
    node.reset_pg_hba("all", "/^.*nomatch.*$, baduser, /^scr.*$", "password");
    node.test_conn(
        "user=scram_role",
        "password, matching regexp for username",
        0,
        &[r#"connection authenticated: identity="scram_role" method=password"#],
        &[],
    );

    // The third regex does not match anymore.
    node.reset_pg_hba("all", "/^.*nomatch.*$, baduser, /^sc_r.*$", "password");
    node.test_conn(
        "user=scram_role",
        "password, non matching regexp for username",
        2,
        &[],
        &["connection authenticated:"],
    );

    // Test with a comma in the regular expression.  In this case, the use of
    // double quotes is mandatory so as this is not considered as two elements
    // of the user name list when parsing pg_hba.conf.
    node.reset_pg_hba("all", "\"/^.*m,.*e$\"", "password");
    node.test_conn(
        "user=scram,role",
        "password, matching regexp for username",
        0,
        &[r#"connection authenticated: identity="scram,role" method=password"#],
        &[],
    );

    // Testing with regular expression for dbname. The third regex matches.
    node.reset_pg_hba("/^.*nomatch.*$, baddb, /^regex_t.*b$", "all", "password");
    node.test_conn(
        "user=scram_role dbname=regex_testdb",
        "password, matching regexp for dbname",
        0,
        &[r#"connection authenticated: identity="scram_role" method=password"#],
        &[],
    );

    // The third regexp does not match anymore.
    node.reset_pg_hba("/^.*nomatch.*$, baddb, /^regex_t.*ba$", "all", "password");
    node.test_conn(
        "user=scram_role dbname=regex_testdb",
        "password, non matching regexp for dbname",
        2,
        &[],
        &["connection authenticated:"],
    );

    std::fs::remove_file(&pgpassfile).expect("unlink");
}

/// NAT-393's acceptance, diffed against the reference `psql`: a password
/// file with group or world access, and one that is not a plain file, are
/// ignored with `passwordFromFile`'s exact warnings (`fe-connect.c:7950`,
/// `:7960`), which are the first line of C's stderr; and a password from the
/// file that the server refuses is reported with `pgpassfileWarning`'s line
/// (`fe-connect.c:8065`). psql's own `psql: error: connection to server …
/// failed: ` prefix is not rlibpq's, so the error is compared from there on.
#[test]
fn nat_393_the_password_file_warnings_match_c_psql() {
    let Some(mut node) = Node::start(WARNING_PORT) else {
        return;
    };
    roles_and_database(&node);
    node.reset_pg_hba("all", "all", "password");

    let pgpassfile = node.cluster.dir.join("pgpass");
    std::fs::write(&pgpassfile, "*:*:*:scram_role:pass\n").expect("pgpass");
    node.env = node.env.clone().with("PGPASSFILE", utf8(&pgpassfile));
    let sql = "SELECT 1";

    // The file works at 0600, for both.
    chmod(&pgpassfile, 0o600);
    let (ret, stderr) = node.c_psql("user=scram_role", sql);
    assert_eq!((ret, stderr.as_str()), (0, ""), "C psql at 0600");
    assert_eq!(
        node.rlibpq("user=scram_role", sql),
        (Ok(()), None),
        "rlibpq at 0600"
    );

    // 0644, and a directory in its place.
    let directory = node.cluster.dir.join("pgpass.d");
    std::fs::create_dir_all(&directory).expect("mkdir");
    chmod(&pgpassfile, 0o644);
    for passfile in [&pgpassfile, &directory] {
        node.env = node.env.clone().with("PGPASSFILE", utf8(passfile));
        let (ret, c_stderr) = node.c_psql("user=scram_role", sql);
        assert_ne!(ret, 0, "C psql ignores {}", passfile.display());
        let (outcome, warning) = node.rlibpq("user=scram_role", sql);
        let warning = warning.expect("rlibpq warns");
        let c_warning = c_stderr.split_inclusive('\n').next().unwrap_or_default();
        assert_eq!(warning, c_warning, "the warning, byte for byte");
        let error = outcome.expect_err("rlibpq ignores the file");
        assert_eq!(
            after_failed(&c_stderr),
            error,
            "the error after the warning"
        );
    }

    // A wrong password, from a file at 0600.
    chmod(&pgpassfile, 0o600);
    std::fs::write(&pgpassfile, "*:*:*:scram_role:wrong\n").expect("pgpass");
    node.env = node.env.clone().with("PGPASSFILE", utf8(&pgpassfile));
    let (ret, c_stderr) = node.c_psql("user=scram_role", sql);
    assert_ne!(ret, 0, "C psql is refused");
    let (outcome, warning) = node.rlibpq("user=scram_role", sql);
    assert_eq!(warning, None);
    let error = outcome.expect_err("rlibpq is refused");
    assert!(
        error.ends_with(&format!(
            "\npassword retrieved from file \"{}\"\n",
            pgpassfile.display()
        )),
        "{error:?}"
    );
    assert_eq!(after_failed(&c_stderr), error, "pgpassfileWarning's line");
}

/// C psql's stderr from the first `failed: ` on: what libpq left in
/// `PQerrorMessage` after its per-host `connection to server … failed: `
/// prefix (`emitHostIdentityInfo`, `fe-connect.c:2416`), which rlibpq does
/// not build.
fn after_failed(c_stderr: &str) -> &str {
    const FAILED: &str = "failed: ";
    let start = c_stderr
        .find(FAILED)
        .expect("psql reports a failed connection");
    &c_stderr[start + FAILED.len()..]
}
