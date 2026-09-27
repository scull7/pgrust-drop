//! Port of `src/interfaces/libpq/t/005_negotiate_encryption.pl` (PostgreSQL
//! REL_18_6), driving this crate's `Connection` where upstream drives
//! `psql`.
//!
//! Every connection attempt is judged the way upstream judges it: the
//! server logs the negotiation (`trace_connection_negotiation`,
//! `log_connections`), [`parse_log_events`] condenses the new log lines to
//! the EVENTS of the test tables, and the outcome is what `SELECT
//! current_enc()` returned, or `fail`. The tables are upstream's, verbatim.
//!
//! What is here: the blocks a client without GSSAPI support can run — "Run
//! tests with GSS and SSL disabled in the server" (`:227`-`:281`), "Run
//! tests with GSS disabled and SSL enabled in the server" (`:284`-`:360`)
//! and "Test negotiation over unix domain sockets" (`:584`-`:600`). The
//! first block's table depends on whether the client has SSL (`:231`,
//! `:255`), which is `rlibpq`'s `tls` feature (ADR-0006), and the second is
//! skipped without it (`:289`) — both exactly as upstream decides them.
//! Upstream skips the two blocks that need Kerberos (`:367`-`:368`,
//! `:478`-`:481`) for a client built without GSSAPI, and so does this port,
//! by not having them: GSSAPI is feature-gated off (pgrust #40).
//!
//! Three deliberate differences from the Perl, none of which touches what
//! is compared:
//! - upstream connects with `host=enc-test-localhost.postgresql.example.com
//!   hostaddr=127.0.0.1` (`:99`-`:100`, `:655`); `rlibpq` does not read `hostaddr`
//!   yet, so this port says `host=127.0.0.1`. Both dial the same address, and
//!   no row here verifies a certificate against the host name. (It does
//!   change SNI: upstream's client sends the name, this port's sends none,
//!   as for any IP literal; the server's certificate is the same either way);
//! - the port is in the connection string, where `$node->psql` puts it in
//!   `PGPORT`;
//! - upstream runs only under `PG_TEST_EXTRA=libpq_encryption` (`:80`)
//!   because the server listens on TCP. It listens on 127.0.0.1 alone, as
//!   upstream's does, and the other live gates here already do the same.
//!
//! Without the reference tools every test prints `SKIP (flagged, not
//! silent)` and passes; with `PGDROP_REQUIRE_REF=1` a missing reference
//! fails instead.

#![allow(clippy::doc_markdown)]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use rlibpq::conninfo::{Env, parse_conninfo};
use rlibpq::{Build, Connection, ExecStatus};

mod common;

use common::{Cluster, only};

/// `$hostaddr`, `:100`.
const HOSTADDR: &str = "127.0.0.1";

/// `$servercidr`, `:101`.
const SERVERCIDR: &str = "127.0.0.1/32";

/// `:221`-`:225`.
const ALL_TEST_USERS: [&str; 5] = ["testuser", "ssluser", "nossluser", "gssuser", "nogssuser"];
const ALL_GSSENCMODES: [&str; 3] = ["disable", "prefer", "require"];
const ALL_SSLMODES: [&str; 4] = ["disable", "allow", "prefer", "require"];
const ALL_SSLNEGOTIATIONS: [&str; 2] = ["postgres", "direct"];

/// `:105`-`:113`, less `listen_addresses`, which `Cluster` passes itself,
/// and `ssl = off`, `:147`, which upstream writes when the client has SSL and
/// is the server's default otherwise.
const CONF: &str = "
# Capturing the EVENTS that occur during tests requires these settings
log_connections = 'receipt,authentication,authorization'
log_disconnections = on
trace_connection_negotiation = on
lc_messages = 'C'
ssl = off
";

/// `:157`-`:162`, the users every block connects as.
const USERS: [&str; 6] = [
    "localuser",
    "testuser",
    "ssluser",
    "nossluser",
    "gssuser",
    "nogssuser",
];

/// `:173`-`:196`.
const CURRENT_ENC: &str = r"
CREATE FUNCTION current_enc() RETURNS text LANGUAGE plpgsql AS $$
DECLARE
  ssl_in_use bool;
  gss_in_use bool;
BEGIN
  ssl_in_use = (SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid());
  gss_in_use = (SELECT encrypted FROM pg_stat_gssapi WHERE pid = pg_backend_pid());

  raise log 'ssl %  gss %', ssl_in_use, gss_in_use;

  IF ssl_in_use AND gss_in_use THEN
    RETURN 'ssl+gss';   -- shouldn't happen
  ELSIF ssl_in_use THEN
    RETURN 'ssl';
  ELSIF gss_in_use THEN
    RETURN 'gss';
  ELSE
    RETURN 'plain';
  END IF;
END;
$$;
";

/// `:233`-`:250`, the table for a client built with SSL.
const SSL_DISABLED_IN_SERVER_WITH_SSL_CLIENT: &str = "
# USER      GSSENCMODE   SSLMODE      SSLNEGOTIATION EVENTS                      -> OUTCOME
testuser    disable      disable      postgres       connect, authok             -> plain
.           .            allow        postgres       connect, authok             -> plain
.           .            prefer       postgres       connect, sslreject, authok  -> plain
.           .            require      postgres       connect, sslreject          -> fail
.           .            .            direct         connect, directsslreject    -> fail
.           prefer       disable      postgres       connect, authok             -> plain
.           .            allow        postgres       connect, authok             -> plain
.           .            prefer       postgres       connect, sslreject, authok  -> plain
.           .            require      postgres       connect, sslreject          -> fail
.           .            .            direct         connect, directsslreject    -> fail

# sslnegotiation=direct is not accepted unless sslmode=require or stronger
*           *            disable      direct         -     -> fail
*           *            allow        direct         -     -> fail
*           *            prefer       direct         -     -> fail
";

/// `:255`-`:268`, the table for a client built without SSL.
const SSL_DISABLED_IN_SERVER_WITHOUT_SSL_CLIENT: &str = "
# USER      GSSENCMODE   SSLMODE      SSLNEGOTIATION EVENTS                      -> OUTCOME
testuser    disable      disable      postgres       connect, authok             -> plain
.           .            allow        postgres       connect, authok             -> plain
.           .            prefer       postgres       connect, authok             -> plain
.           prefer       disable      postgres       connect, authok             -> plain
.           .            allow        postgres       connect, authok             -> plain
.           .            prefer       postgres       connect, authok             -> plain

# Without SSL support, sslmode=require and sslnegotiation=direct are
# not accepted at all
*           *            require      *              -     -> fail
*           *            *            direct         -     -> fail
	";

/// `:291`-`:313`, "Run tests with GSS disabled and SSL enabled in the
/// server".
const SSL_ENABLED_IN_SERVER: &str = "
# USER      GSSENCMODE   SSLMODE      SSLNEGOTIATION EVENTS                                          -> OUTCOME
testuser    disable      disable      postgres       connect, authok                                 -> plain
.           .            allow        postgres       connect, authok                                 -> plain
.           .            prefer       postgres       connect, sslaccept, authok                      -> ssl
.           .            require      postgres       connect, sslaccept, authok                      -> ssl
.           .            .            direct         connect, directsslaccept, authok                -> ssl
ssluser     .            disable      postgres       connect, authfail                               -> fail
.           .            allow        postgres       connect, authfail, reconnect, sslaccept, authok -> ssl
.           .            prefer       postgres       connect, sslaccept, authok                      -> ssl
.           .            require      postgres       connect, sslaccept, authok                      -> ssl
.           .            .            direct         connect, directsslaccept, authok                -> ssl
nossluser   .            disable      postgres       connect, authok                                 -> plain
.           .            allow        postgres       connect, authok                                 -> plain
.           .            prefer       postgres       connect, sslaccept, authfail, reconnect, authok -> plain
.           .            require      postgres       connect, sslaccept, authfail                    -> fail
.           .            require      direct         connect, directsslaccept, authfail              -> fail

# sslnegotiation=direct is not accepted unless sslmode=require or stronger
*           *            disable      direct         -     -> fail
*           *            allow        direct         -     -> fail
*           *            prefer       direct         -     -> fail
";

/// `:270`-`:276`, appended to whichever table was picked.
const GSSENCMODE_REQUIRE: &str = "
testuser    require      *            *              - -> fail
";

/// `:137`-`:144`: upstream's test certificate, vendored from the tag
/// (`tests/ssl/README.md`).
const SERVER_CRT: &[u8] = include_bytes!("ssl/server-cn-only.crt");
const SERVER_KEY: &[u8] = include_bytes!("ssl/server-cn-only.key");

/// Start a cluster configured as `:103`-`:217` leaves it — SSL and GSS off
/// in the server — or `None` when the reference tools are absent.
fn setup(port: u16) -> Option<Cluster> {
    let cluster = Cluster::start_configured("trust", port, HOSTADDR, CONF)?;
    let data = cluster.dir.join("data");

    // :135-:148 — installed when the client has SSL. Upstream copies them
    // before the server starts; with `ssl = off` nothing reads them until
    // the SSL block turns it on, so after is the same.
    if Build::THIS.use_ssl {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(data.join("server.crt"), SERVER_CRT).expect("server.crt is written");
        let key = data.join("server.key");
        std::fs::write(&key, SERVER_KEY).expect("server.key is written");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))
            .expect("failed to change permissions on server keys");
    }

    let mut admin = cluster.connect();
    for user in USERS {
        exec_ok(&mut admin, &format!("CREATE USER {user};"));
    }
    exec_ok(&mut admin, CURRENT_ENC);
    let loaded = conf_load_time(&mut admin);
    drop(admin);

    // :200-:216, without the hostgssenc line, which upstream writes only
    // with Kerberos (:213).
    let mut hba = format!(
        "
# TYPE        DATABASE        USER            ADDRESS                 METHOD             OPTIONS
local         postgres        localuser                               trust
host          postgres        testuser        {SERVERCIDR}             trust
hostnossl     postgres        nossluser       {SERVERCIDR}             trust
hostnogssenc  postgres        nogssuser       {SERVERCIDR}             trust
"
    );
    // :209-:211
    if Build::THIS.use_ssl {
        use std::fmt::Write as _;
        write!(
            hba,
            "
hostssl       postgres        ssluser         {SERVERCIDR}             trust
"
        )
        .expect("a String takes any write");
    }
    std::fs::write(data.join("pg_hba.conf"), hba).expect("pg_hba.conf is written");
    reload(&cluster, &data, &loaded);
    Some(cluster)
}

/// `$node->adjust_conf('postgresql.conf', 'ssl', …)` then `$node->reload`
/// (`:316`-`:317`, `:358`-`:359`). Appending wins over the earlier line, as
/// the later of two settings does in `postgresql.conf`.
fn set_ssl(cluster: &Cluster, on: bool) {
    use std::io::Write as _;
    let data = cluster.dir.join("data");
    let loaded = conf_load_time(&mut localuser(cluster));
    std::fs::OpenOptions::new()
        .append(true)
        .open(data.join("postgresql.conf"))
        .and_then(|mut conf| writeln!(conf, "ssl = {}", if on { "on" } else { "off" }))
        .expect("postgresql.conf is appended to");
    reload(cluster, &data, &loaded);
}

/// `$node->reload`, `:217`, and then wait until the server has read the new
/// `pg_hba.conf`: `pg_ctl reload` only signals, and a row that raced the
/// reload would be judged against initdb's `pg_hba.conf`.
fn reload(cluster: &Cluster, data: &Path, before: &[u8]) {
    let out = Command::new(cluster.bin.join("pg_ctl"))
        .args(["-D".as_ref(), data.as_os_str()])
        .arg("reload")
        .output()
        .expect("reference pg_ctl runs");
    assert!(out.status.success(), "reference pg_ctl reload failed");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if conf_load_time(&mut localuser(cluster)) != before {
            return;
        }
        assert!(Instant::now() < deadline, "the server never reloaded");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `connstr => "user=localuser host=$unixdir"` (`:329`): the one role
/// `pg_hba.conf` lets in over the socket once setup has rewritten it.
fn localuser(cluster: &Cluster) -> Connection {
    connect(&format!(
        "user=localuser dbname=postgres host={} port={}",
        cluster.dir.display(),
        cluster.port
    ))
    .expect("localuser connects over the socket")
}

fn conf_load_time(conn: &mut Connection) -> Vec<u8> {
    let result = only(conn.exec(b"SELECT pg_conf_load_time()").expect("PQexec"));
    assert_eq!(result.status(), ExecStatus::TuplesOk, "{result:?}");
    result.value(0, 0).expect("one value").to_vec()
}

fn exec_ok(conn: &mut Connection, sql: &str) {
    let result = only(conn.exec(sql.as_bytes()).expect("PQexec"));
    assert_eq!(result.status(), ExecStatus::CommandOk, "{sql}: {result:?}");
}

/// `PQconnectdb(conninfo)`: `Err` carries libpq's message.
fn connect(conninfo: &str) -> Result<Connection, String> {
    let mut info = parse_conninfo(conninfo.as_bytes()).map_err(|err| err.to_string())?;
    info.add_defaults(&Env::empty());
    Connection::connect(&info).map_err(|err| err.to_string())
}

/// Collects every mismatch, so one run reports the whole matrix as `prove`
/// would rather than stopping at the first `is` that fails.
#[derive(Default)]
struct Tally {
    failures: Vec<String>,
}

impl Tally {
    fn assert_all_passed(&self) {
        assert!(
            self.failures.is_empty(),
            "{} negotiation row(s) differ from upstream's table:\n{}",
            self.failures.len(),
            self.failures.join("\n")
        );
    }
}

/// `test_matrix`, `:608`: the cube of users, `gssencmode`, `sslmode` and
/// `sslnegotiation`, each looked up in `expected`.
fn test_matrix(
    cluster: &Cluster,
    tally: &mut Tally,
    test_users: &[&str],
    gssencmodes: &[&str],
    sslmodes: &[&str],
    sslnegotiations: &[&str],
    expected: &BTreeMap<String, String>,
) {
    for test_user in test_users {
        for gssencmode in gssencmodes {
            for client_mode in sslmodes {
                for negotiation in sslnegotiations {
                    let key = format!("{test_user} {gssencmode} {client_mode} {negotiation}");
                    let expected_events = expected
                        .get(&key)
                        .map_or("<line missing from expected output table>", String::as_str);
                    connect_test(
                        cluster,
                        tally,
                        &format!(
                            "user={test_user} gssencmode={gssencmode} sslmode={client_mode} sslnegotiation={negotiation}"
                        ),
                        expected_events,
                    );
                }
            }
        }
    }
}

/// `connect_test`, `:645`: connect, run `SELECT current_enc()`, and compare
/// the log's events and the outcome with `expected_events_and_outcome`.
fn connect_test(
    cluster: &Cluster,
    tally: &mut Tally,
    connstr: &str,
    expected_events_and_outcome: &str,
) {
    let dbname = if connstr.contains("dbname=") {
        ""
    } else {
        "dbname=postgres "
    };
    let host = if connstr.contains("host=") {
        String::new()
    } else {
        format!("host={HOSTADDR} ")
    };
    let connstr_full = format!("{dbname}{host}port={} {connstr}", cluster.port);

    let logfile = cluster.dir.join("log");
    let log_location = std::fs::metadata(&logfile).map_or(0, |meta| meta.len());

    let (outcome, stderr) = match connect(&connstr_full) {
        Ok(mut conn) => match conn.exec(b"SELECT current_enc()") {
            Ok(results) => {
                let result = only(results);
                match (result.status(), result.value(0, 0)) {
                    (ExecStatus::TuplesOk, Some(value)) => {
                        (String::from_utf8_lossy(value).into_owned(), String::new())
                    }
                    _ => ("fail".to_string(), format!("{result:?}")),
                }
            }
            Err(err) => ("fail".to_string(), err.to_string()),
        },
        Err(err) => ("fail".to_string(), err),
    };

    let log = std::fs::read(&logfile).expect("the server log is readable");
    let start = usize::try_from(log_location).expect("the log fits in memory");
    let log_contents = String::from_utf8_lossy(&log[start.min(log.len())..]);
    let events = parse_log_events(&log_contents);

    let events_and_outcome = format!("{} -> {outcome}", events.join(", "));
    if events_and_outcome != expected_events_and_outcome {
        tally.failures.push(format!(
            " '{connstr}' -> {expected_events_and_outcome}\n     got: {events_and_outcome}\n     {stderr}"
        ));
    }
}

/// `parse_table`, `:697`: the table format described at `:27`-`:69`.
fn parse_table(table: &str) -> BTreeMap<String, String> {
    let mut expected = BTreeMap::new();
    let (mut user, mut gssencmode, mut sslmode, mut sslnegotiation) =
        (String::new(), String::new(), String::new(), String::new());
    for line in table.lines() {
        // Trim comments, then whitespace; skip what is left empty.
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }

        let (fields, outcome) = line
            .rsplit_once("->")
            .unwrap_or_else(|| panic!("could not parse line \"{line}\""));
        let mut words = fields.split_whitespace();
        let mut field = |slot: &mut String| {
            let word = words
                .next()
                .unwrap_or_else(|| panic!("could not parse line \"{line}\""));
            if word != "." {
                *slot = word.to_string();
            }
        };
        field(&mut user);
        field(&mut gssencmode);
        field(&mut sslmode);
        field(&mut sslnegotiation);
        // Normalize the whitespace in the "EVENTS -> OUTCOME" part.
        let events_text = words.collect::<Vec<_>>().join(" ");
        let events: Vec<&str> = events_text.split(',').map(str::trim).collect();
        let events_and_outcome = format!("{} -> {}", events.join(", "), outcome.trim());

        expand_expected_line(
            &mut expected,
            [&user, &gssencmode, &sslmode, &sslnegotiation],
            &events_and_outcome,
        );
    }
    expected
}

/// `expand_expected_line`, `:741`: a `*` stands for every value of its
/// column.
fn expand_expected_line(result: &mut BTreeMap<String, String>, fields: [&str; 4], expected: &str) {
    let columns: [&[&str]; 4] = [
        &ALL_TEST_USERS,
        &ALL_GSSENCMODES,
        &ALL_SSLMODES,
        &ALL_SSLNEGOTIATIONS,
    ];
    if let Some(wild) = fields.iter().position(|field| *field == "*") {
        for value in columns[wild] {
            let mut expanded = fields;
            expanded[wild] = value;
            expand_expected_line(result, expanded, expected);
        }
    } else {
        result.insert(fields.join(" "), expected.to_string());
    }
}

/// `parse_log_events`, `:795`: scrape the server log for the negotiation
/// events. No events at all is `-`.
fn parse_log_events(log_contents: &str) -> Vec<&'static str> {
    let mut events = Vec::new();
    for line in log_contents.lines() {
        if line.contains("connection received") {
            events.push(if events.is_empty() {
                "connect"
            } else {
                "reconnect"
            });
        }
        for (pattern, event) in [
            ("SSLRequest accepted", "sslaccept"),
            ("SSLRequest rejected", "sslreject"),
            ("direct SSL connection accepted", "directsslaccept"),
            ("direct SSL connection rejected", "directsslreject"),
            ("GSSENCRequest accepted", "gssaccept"),
            ("GSSENCRequest rejected", "gssreject"),
            ("no pg_hba.conf entry", "authfail"),
            ("connection authenticated", "authok"),
            (
                "error triggered for injection point backend-",
                "backenderror",
            ),
            ("protocol version 2 error triggered", "v2error"),
        ] {
            if line.contains(pattern) {
                events.push(event);
            }
        }
    }
    if events.is_empty() {
        events.push("-");
    }
    events
}

/// "Run tests with GSS and SSL disabled in the server", `:227`-`:281`.
#[test]
fn running_tests_with_ssl_and_gss_disabled_in_the_server() {
    let Some(cluster) = setup(55_500) else {
        return;
    };

    // :231 — the table depends on whether the client has SSL.
    let mut test_table = if Build::THIS.use_ssl {
        SSL_DISABLED_IN_SERVER_WITH_SSL_CLIENT
    } else {
        SSL_DISABLED_IN_SERVER_WITHOUT_SSL_CLIENT
    }
    .to_string();
    test_table.push_str(GSSENCMODE_REQUIRE);

    let mut tally = Tally::default();
    test_matrix(
        &cluster,
        &mut tally,
        &["testuser"],
        &ALL_GSSENCMODES,
        &ALL_SSLMODES,
        &ALL_SSLNEGOTIATIONS,
        &parse_table(&test_table),
    );
    tally.assert_all_passed();
}

/// "Run tests with GSS disabled and SSL enabled in the server", `:284`-
/// `:360`.
#[test]
fn running_tests_with_ssl_enabled_in_server() {
    // :289
    if !Build::THIS.use_ssl {
        println!("SKIP: SSL not supported by this build (005_negotiate_encryption.pl:289)");
        return;
    }
    let Some(cluster) = setup(55_502) else {
        return;
    };

    // :315-:317
    set_ssl(&cluster, true);

    let mut tally = Tally::default();
    test_matrix(
        &cluster,
        &mut tally,
        &["testuser", "ssluser", "nossluser"],
        &["disable"],
        &ALL_SSLMODES,
        &ALL_SSLNEGOTIATIONS,
        &parse_table(SSL_ENABLED_IN_SERVER),
    );

    // :324-:355
    if check_extension(&cluster, "injection_points") {
        for (point, expected) in [
            ("backend-initialize", "connect, backenderror -> fail"),
            ("backend-initialize-v2-error", "connect, v2error -> fail"),
            (
                "backend-ssl-startup",
                "connect, sslaccept, backenderror, reconnect, authok -> plain",
            ),
        ] {
            let mut local = localuser(&cluster);
            if point == "backend-initialize" {
                // :165 — upstream creates the extension during setup.
                exec_ok(
                    &mut local,
                    "CREATE EXTENSION IF NOT EXISTS injection_points;",
                );
            }
            let attach = format!("SELECT injection_points_attach('{point}', 'error');");
            let result = only(local.exec(attach.as_bytes()).expect("PQexec"));
            assert_eq!(
                result.status(),
                ExecStatus::TuplesOk,
                "{attach}: {result:?}"
            );
            drop(local);
            connect_test(
                &cluster,
                &mut tally,
                "user=testuser sslmode=prefer",
                expected,
            );
            restart(&cluster);
        }
    } else {
        println!(
            "note: injection_points is not installed in the reference server, so \
             005_negotiate_encryption.pl:324-:355 do not run, as upstream skips them"
        );
    }

    // :357-:359
    set_ssl(&cluster, false);
    tally.assert_all_passed();
}

/// `$node->check_extension`, `Cluster.pm`: is it in `pg_available_extensions`?
fn check_extension(cluster: &Cluster, name: &str) -> bool {
    let mut admin = localuser(cluster);
    let query = format!("SELECT count(*) FROM pg_available_extensions WHERE name = '{name}'");
    let result = only(admin.exec(query.as_bytes()).expect("PQexec"));
    result.value(0, 0) != Some(b"0")
}

/// `$node->restart`: `pg_ctl restart`, which clears every injection point.
fn restart(cluster: &Cluster) {
    let data = cluster.dir.join("data");
    let out = Command::new(cluster.bin.join("pg_ctl"))
        .args(["-D".as_ref(), data.as_os_str()])
        .arg("-w")
        .arg("-l")
        .arg(cluster.dir.join("log"))
        .arg("restart")
        .env("LC_ALL", "C")
        .output()
        .expect("reference pg_ctl runs");
    assert!(out.status.success(), "reference pg_ctl restart failed");
}

/// "Test negotiation over unix domain sockets", `:584`-`:600`: libpq
/// attempts neither SSL nor GSSAPI over a Unix socket.
#[test]
fn test_negotiation_over_unix_domain_sockets() {
    let Some(cluster) = setup(55_501) else {
        return;
    };
    let unixdir = cluster.dir.display().to_string();

    let mut tally = Tally::default();
    connect_test(
        &cluster,
        &mut tally,
        &format!("user=localuser gssencmode=prefer sslmode=prefer host={unixdir}"),
        "connect, authok -> plain",
    );
    connect_test(
        &cluster,
        &mut tally,
        &format!("user=localuser gssencmode=require sslmode=prefer host={unixdir}"),
        "- -> fail",
    );
    tally.assert_all_passed();
}

/// The helpers are a port too; these pin them against the Perl's reading of
/// the tables, so a matrix that passes is not passing because a row went
/// missing.
#[test]
fn the_tables_expand_as_parse_table_expands_them() {
    let mut table = SSL_DISABLED_IN_SERVER_WITHOUT_SSL_CLIENT.to_string();
    table.push_str(GSSENCMODE_REQUIRE);
    let expected = parse_table(&table);
    // Every cell of test_matrix's cube for testuser has a row.
    assert_eq!(
        expected
            .keys()
            .filter(|key| key.starts_with("testuser "))
            .count(),
        ALL_GSSENCMODES.len() * ALL_SSLMODES.len() * ALL_SSLNEGOTIATIONS.len()
    );
    assert_eq!(
        expected["testuser prefer allow postgres"],
        "connect, authok -> plain"
    );
    // A later line overrides an earlier one, as `%expected = (%expected,
    // %expanded)` does: `require` beats the `disable` rows' `direct`.
    assert_eq!(expected["testuser require disable postgres"], "- -> fail");
    assert_eq!(expected["testuser disable prefer direct"], "- -> fail");

    // test_matrix's cube for the SSL block: every row it looks up exists.
    let ssl_enabled = parse_table(SSL_ENABLED_IN_SERVER);
    for user in ["testuser", "ssluser", "nossluser"] {
        for sslmode in ALL_SSLMODES {
            for negotiation in ALL_SSLNEGOTIATIONS {
                assert!(
                    ssl_enabled.contains_key(&format!("{user} disable {sslmode} {negotiation}")),
                    "{user} {sslmode} {negotiation}"
                );
            }
        }
    }
    assert_eq!(
        ssl_enabled["ssluser disable allow postgres"],
        "connect, authfail, reconnect, sslaccept, authok -> ssl"
    );

    let with_ssl = parse_table(SSL_DISABLED_IN_SERVER_WITH_SSL_CLIENT);
    assert_eq!(
        with_ssl["testuser prefer require direct"],
        "connect, directsslreject -> fail"
    );
    assert_eq!(with_ssl["nogssuser require allow direct"], "- -> fail");
}

#[test]
fn the_log_events_are_the_ones_parse_log_events_scrapes() {
    let log = "\
LOG:  connection received: host=127.0.0.1 port=1
LOG:  SSLRequest rejected
FATAL:  no pg_hba.conf entry for host \"127.0.0.1\"
LOG:  connection received: host=127.0.0.1 port=2
LOG:  connection authenticated: user=\"testuser\" method=trust
LOG:  disconnection: session time: 0:00:00.001
";
    assert_eq!(
        parse_log_events(log),
        ["connect", "sslreject", "authfail", "reconnect", "authok"]
    );
    assert_eq!(parse_log_events("LOG:  checkpoint starting\n"), ["-"]);
}
