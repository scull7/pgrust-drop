//! The single-user session after expansion (NAT-383, ADR-0002): what the
//! template cannot carry, applied by `postgres --single`.
//!
//! C `initdb` runs every post-bootstrap step in one standalone backend
//! (`initialize_data_directory`, `initdb.c:3110`-`:3150`). The template
//! already holds the result of all of them for the options it was minted
//! with, so what is left here is the difference between those options and
//! this command line — [`fixup_script`] — and the one session that applies
//! it, started the way C starts it ([`BACKEND_OPTIONS`], [`run`]).
//!
//! What the script covers so far: the superuser's name (`-U`). The password,
//! collation stamping and the template databases' freeze are NAT-383's later
//! slices; until they land, a script is empty exactly when `-U` names the
//! template's own superuser, and no server is looked for or run then
//! (`docs/divergences.md`).
//!
//! Data / Calculations / Actions: [`SqlStatement`] and [`Server`] are data;
//! [`fixup_script`], [`resolve_server`] (over a [`ServerProbe`]) and
//! [`wait_result_to_str`] are pure; [`find_server`] and [`run`] are the
//! Actions.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::cluster::TEMPLATE_SUPERUSER;
use crate::error::InitdbError;
use crate::help::{PG_VERSION, PROGNAME};
use crate::strerror::strerror;
use crate::validate::CreatePlan;

/// `backend_options` (`initdb.c:226`), word by word.
pub const BACKEND_OPTIONS: [&str; 10] = [
    "--single",
    "-F",
    "-O",
    "-j",
    "-c",
    "search_path=pg_catalog",
    "-c",
    "exit_on_error=true",
    "-c",
    "log_checkpoints=false",
];

/// The database the session opens (`initdb.c:3113`).
pub const DATABASE: &str = "template1";

/// `BOOTSTRAP_SUPERUSERID` (`src/include/catalog/pg_authid.dat:22`): the
/// superuser the template was minted with, whatever it is called.
pub const BOOTSTRAP_SUPERUSERID: u32 = 10;

/// Names a `postgres` to run when there is none beside this program. Not an
/// upstream variable: C `initdb` only ever looks beside itself.
pub const SERVER_ENV: &str = "PGDROP_POSTGRES";

/// One statement for the session, without its terminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlStatement(String);

impl SqlStatement {
    /// The statement's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// What `PG_CMD_PUTS` writes for it (`initdb.c:334`): under `-j` a
    /// statement ends at a semicolon followed by an empty line.
    #[must_use]
    pub fn to_input(&self) -> String {
        format!("{};\n\n", self.0)
    }
}

/// Pure: `escape_quotes` (`initdb.c:406`), which is
/// `escape_single_quotes_ascii` (`src/port/quotes.c:33`) with
/// `SQL_STR_DOUBLE(ch, true)` (`src/include/c.h:1152`): every `'` and `\`
/// doubled, for an `E''` literal.
#[must_use]
pub fn escape_quotes(src: &str) -> String {
    let mut out = String::with_capacity(src.len() * 2);
    for ch in src.chars() {
        if matches!(ch, '\'' | '\\') {
            out.push(ch);
        }
        out.push(ch);
    }
    out
}

/// Pure: the statements that make the expanded template the cluster `plan`
/// asks for, in the order the session runs them. Empty when the template
/// already is that cluster.
///
/// `-U`: C writes the name into `postgres.bki` (`initdb.c:1587`); here the
/// template's superuser is renamed. `ALTER ROLE … RENAME` refuses the session
/// user ("session user cannot be renamed", `src/backend/commands/user.c:1373`;
/// pgrust's port refuses it too), and a standalone backend's session user is
/// [`BOOTSTRAP_SUPERUSERID`] (`InitializeSessionUserIdStandalone`,
/// `src/backend/utils/init/miscinit.c:891`), so the row is updated directly.
/// Every catalog that refers to the superuser does so by OID — ownership,
/// and the ACLs `setup_privileges` (`initdb.c:1806`) wrote naming it — so the
/// one row is the whole rename. The `pg_` prefix was refused before this
/// (`initdb.c:3478`, [`crate::validate`]); the literal is escaped as
/// `setup_auth` escapes the password (`initdb.c:1649`).
#[must_use]
pub fn fixup_script(plan: &CreatePlan) -> Vec<SqlStatement> {
    let mut script = Vec::new();
    if let Some(name) = plan
        .username
        .as_deref()
        .filter(|name| *name != TEMPLATE_SUPERUSER)
    {
        script.push(SqlStatement(format!(
            "UPDATE pg_authid SET rolname = E'{}' WHERE oid = {BOOTSTRAP_SUPERUSERID}",
            escape_quotes(name)
        )));
    }
    script
}

/// A `postgres` to run the session with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    program: PathBuf,
    /// The word a multicall binary needs before the server's own arguments.
    applet: Option<&'static str>,
}

impl Server {
    /// A `postgres` executable.
    #[must_use]
    pub fn executable(program: PathBuf) -> Self {
        Self {
            program,
            applet: None,
        }
    }

    /// A multicall binary that is `postgres` when its first argument says so
    /// (`pgdrop postgres …`).
    #[must_use]
    pub fn multicall(program: PathBuf) -> Self {
        Self {
            program,
            applet: Some("postgres"),
        }
    }

    /// The program to execute.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Pure: the arguments after the program name: `backend_options`, no
    /// `extra_options` (`-d`, `-n` and `--debug` are not ported yet), then
    /// `template1` (`initdb.c:3112`). PGDATA reaches the server through the
    /// environment, as `setup_pgdata` exports it (`initdb.c:2642`).
    #[must_use]
    pub fn single_user_args(&self) -> Vec<OsString> {
        self.applet
            .into_iter()
            .chain(BACKEND_OPTIONS)
            .chain([DATABASE])
            .map(OsString::from)
            .collect()
    }

    /// Pure: the command as C's `popen` string names it (`initdb.c:3112`),
    /// for `could not execute command` (`:753`).
    #[must_use]
    pub fn command_text(&self) -> String {
        let applet = self
            .applet
            .map(|word| format!(" {word}"))
            .unwrap_or_default();
        format!(
            "\"{}\"{applet} {}  {DATABASE} >/dev/null",
            self.program.display(),
            BACKEND_OPTIONS.join(" ")
        )
    }
}

/// `PG_BACKEND_VERSIONSTR` (`src/include/port.h:145`): what `postgres -V`
/// must print for `find_other_exec` to take it.
#[must_use]
pub fn backend_version_line() -> String {
    format!("postgres (PostgreSQL) {PG_VERSION}\n")
}

/// Pure: `line` is [`backend_version_line`], or that line with one
/// parenthesized suffix before its newline — `postgres (PostgreSQL) 18.6
/// (Ubuntu 18.6-1.pgdg24.04+2)`. `configure.ac:40`'s `--with-extra-version`
/// appends that suffix to `PG_VERSION`, and Debian, Ubuntu and Homebrew build
/// PostgreSQL that way. C compares against its own compiled string, so a
/// distribution's `initdb` expects its own suffix; this port has none, so it
/// takes a server of its version from any build (`docs/divergences.md`). The
/// version itself is still compared: `18.6` never takes `17.2` or `18.60`.
#[must_use]
pub fn is_backend_version(line: &str) -> bool {
    let expected = backend_version_line();
    if line == expected {
        return true;
    }
    line.strip_prefix(expected.trim_end_matches('\n'))
        .and_then(|rest| rest.strip_prefix(" ("))
        .and_then(|rest| rest.strip_suffix(")\n"))
        .is_some_and(|extra| !extra.contains(['(', ')', '\n']))
}

/// What [`resolve_server`] asks of the filesystem and of a candidate.
pub trait ServerProbe {
    /// `validate_exec` (`src/common/exec.c`): a regular file this process
    /// may execute.
    fn is_executable(&self, path: &Path) -> bool;
    /// `pipe_read_line("\"path\" -V")`: the first line of its output,
    /// newline kept, or `None` when it could not be run or printed nothing.
    fn version_line(&self, path: &Path) -> Option<String>;
}

/// Pure: `setup_bin_paths` (`initdb.c:2648`), then this port's two
/// fallbacks.
///
/// 1. `postgres` beside this program, as `find_other_exec` looks for it
///    (`src/common/exec.c:309`): used when it is executable and prints
///    [`backend_version_line`]; a different version is C's error
///    (`initdb.c:2664`), not a reason to look further.
/// 2. The program [`SERVER_ENV`] names, held to the same two checks.
/// 3. `embedded`, the server built into the running binary (pgdrop).
///
/// `my_exec` is `find_my_exec`'s answer, `None` when it had none; C then
/// names the program `progname` (`initdb.c:2658`).
///
/// # Errors
/// C's two `setup_bin_paths` failures, and [`InitdbError::ServerOverrideUnusable`].
pub fn resolve_server(
    my_exec: Option<&Path>,
    override_path: Option<&Path>,
    embedded: Option<&Server>,
    probe: &dyn ServerProbe,
) -> Result<Server, InitdbError> {
    let full_path =
        || my_exec.map_or_else(|| PROGNAME.to_owned(), |path| path.display().to_string());
    if let Some(sibling) = my_exec
        .and_then(Path::parent)
        .map(|dir| dir.join("postgres"))
        .filter(|sibling| probe.is_executable(sibling))
    {
        match probe.version_line(&sibling) {
            Some(line) if is_backend_version(&line) => return Ok(Server::executable(sibling)),
            Some(_) => {
                return Err(InitdbError::ServerWrongVersion {
                    full_path: full_path(),
                });
            }
            // find_other_exec's -1: nothing usable here.
            None => {}
        }
    }
    if let Some(path) = override_path {
        return if probe.is_executable(path)
            && probe
                .version_line(path)
                .is_some_and(|line| is_backend_version(&line))
        {
            Ok(Server::executable(path.to_path_buf()))
        } else {
            Err(InitdbError::ServerOverrideUnusable {
                path: path.display().to_string(),
            })
        };
    }
    embedded
        .cloned()
        .ok_or_else(|| InitdbError::ServerNotFound {
            full_path: full_path(),
        })
}

/// The real filesystem and real processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealServerProbe;

impl ServerProbe for RealServerProbe {
    fn is_executable(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && may_execute(&meta))
    }

    fn version_line(&self, path: &Path) -> Option<String> {
        let output = Command::new(path)
            .arg("-V")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let end = text.find('\n').map_or(text.len(), |at| at + 1);
        (end > 0).then(|| text[..end].to_owned())
    }
}

/// Some execute bit is set (`validate_exec` asks `access(X_OK)`).
#[cfg(unix)]
fn may_execute(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn may_execute(_: &std::fs::Metadata) -> bool {
    true
}

/// Action: [`resolve_server`] over this process — its executable with
/// symbolic links resolved, as `find_my_exec` resolves them
/// (`normalize_exec_path`, `src/common/exec.c:241`), its [`SERVER_ENV`]
/// (unset or empty is absent) and the real filesystem.
///
/// # Errors
/// As [`resolve_server`].
pub fn find_server(embedded: Option<&Server>) -> Result<Server, InitdbError> {
    let my_exec = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let override_path = std::env::var_os(SERVER_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    resolve_server(
        my_exec.as_deref(),
        override_path.as_deref(),
        embedded,
        &RealServerProbe,
    )
}

/// Pure: `wait_result_to_str` (`src/common/wait_error.c:33`) for a child
/// that exited with `code` or was killed by `signal`.
///
/// For a signal C appends `pg_strsignal`'s text, which differs between libcs
/// and is out of reach without one; the number alone is written.
#[must_use]
pub fn wait_result_to_str(code: Option<i32>, signal: Option<i32>) -> String {
    match (code, signal) {
        (Some(126), _) => "command not executable".to_owned(),
        (Some(127), _) => "command not found".to_owned(),
        (Some(code), _) => format!("child process exited with exit code {code}"),
        (None, Some(signal)) => format!("child process was terminated by signal {signal}"),
        (None, None) => "child process exited with unrecognized status".to_owned(),
    }
}

/// Action: `PG_CMD_OPEN`, every statement of `script`, `PG_CMD_CLOSE`
/// (`initdb.c:3115`-`:3150`), with PGDATA exported as `setup_pgdata` does.
///
/// The server's stdout goes to `/dev/null` and its stderr is this process's,
/// as with C's `popen`. A failed exit is `pclose_check`'s error
/// (`src/common/exec.c:410`); a failed write, checked after it as
/// `check_ok` does (`initdb.c:2117`), is `could not write to child process`.
///
/// # Errors
/// Those three, as [`InitdbError`]s.
pub fn run(server: &Server, pgdata: &Path, script: &[SqlStatement]) -> Result<(), InitdbError> {
    let mut child = Command::new(server.program())
        .args(server.single_user_args())
        .env("PGDATA", pgdata)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|err| InitdbError::CouldNotExecuteCommand {
            command: server.command_text(),
            reason: strerror(&err),
        })?;
    let mut output_failed = None;
    if let Some(mut stdin) = child.stdin.take() {
        for statement in script {
            if let Err(err) = stdin.write_all(statement.to_input().as_bytes()) {
                output_failed = Some(err);
                break;
            }
        }
        // Dropping stdin is the end of input that ends the session.
    }
    let status = child
        .wait()
        .map_err(|err| InitdbError::ChildProcessFailed {
            reason: format!("pclose() failed: {}", strerror(&err)),
        })?;
    if !status.success() {
        return Err(InitdbError::ChildProcessFailed {
            reason: wait_result_to_str(status.code(), signal(status)),
        });
    }
    match output_failed {
        Some(err) => Err(InitdbError::CouldNotWriteToChildProcess {
            reason: strerror(&err),
        }),
        None => Ok(()),
    }
}

/// The signal that ended a child, if one did.
#[cfg(unix)]
fn signal(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn signal(_: std::process::ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn plan_for(username: Option<&str>) -> CreatePlan {
        let options = crate::cli::Options {
            pgdata: Some("/tmp/single-user-unit".to_owned()),
            ..crate::cli::Options::default()
        };
        let env = crate::validate::Environment {
            pgdata: None,
            effective_user: username.map(str::to_owned),
        };
        match crate::validate::validate(&options, &env, &crate::validate::RealFs) {
            Ok(crate::validate::Plan::Create(plan)) => plan,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_templates_own_superuser_needs_no_session() {
        assert_eq!(fixup_script(&plan_for(Some("postgres"))), []);
        assert_eq!(fixup_script(&plan_for(None)), []);
    }

    #[test]
    fn another_superuser_is_the_bootstrap_superuser_renamed() {
        let script = fixup_script(&plan_for(Some("alice")));
        assert_eq!(
            script
                .iter()
                .map(SqlStatement::to_input)
                .collect::<String>(),
            "UPDATE pg_authid SET rolname = E'alice' WHERE oid = 10;\n\n"
        );
        // escape_quotes doubles both, as for C's E'' password literal.
        let script = fixup_script(&plan_for(Some("o'brien\\x")));
        assert_eq!(
            script[0].as_str(),
            "UPDATE pg_authid SET rolname = E'o''brien\\\\x' WHERE oid = 10"
        );
    }

    #[test]
    fn escape_quotes_doubles_quotes_and_backslashes_only() {
        assert_eq!(escape_quotes("plain"), "plain");
        assert_eq!(escape_quotes("a'b\\c\"d"), "a''b\\\\c\"d");
        assert_eq!(escape_quotes("ünï'"), "ünï''");
    }

    #[test]
    fn the_session_is_started_as_initdb_starts_it() {
        // initdb.c:226 and :3112.
        let server = Server::executable(PathBuf::from("/usr/lib/postgresql/bin/postgres"));
        assert_eq!(
            server.single_user_args(),
            [
                "--single",
                "-F",
                "-O",
                "-j",
                "-c",
                "search_path=pg_catalog",
                "-c",
                "exit_on_error=true",
                "-c",
                "log_checkpoints=false",
                "template1",
            ]
        );
        assert_eq!(
            server.command_text(),
            "\"/usr/lib/postgresql/bin/postgres\" --single -F -O -j -c search_path=pg_catalog \
             -c exit_on_error=true -c log_checkpoints=false  template1 >/dev/null"
        );
        let multicall = Server::multicall(PathBuf::from("/opt/bin/pgdrop"));
        assert_eq!(multicall.single_user_args()[..2], ["postgres", "--single"]);
        assert!(
            multicall
                .command_text()
                .starts_with("\"/opt/bin/pgdrop\" postgres --single ")
        );
    }

    /// Paths that exist and are executable, and what each prints for `-V`.
    struct FakeProbe(HashMap<PathBuf, Option<&'static str>>);

    impl FakeProbe {
        fn new(entries: &[(&str, Option<&'static str>)]) -> Self {
            Self(
                entries
                    .iter()
                    .map(|(path, line)| (PathBuf::from(path), *line))
                    .collect(),
            )
        }
    }

    impl ServerProbe for FakeProbe {
        fn is_executable(&self, path: &Path) -> bool {
            self.0.contains_key(path)
        }
        fn version_line(&self, path: &Path) -> Option<String> {
            self.0.get(path).copied().flatten().map(str::to_owned)
        }
    }

    const OURS: Option<&str> = Some("postgres (PostgreSQL) 18.6\n");
    const OLDER: Option<&str> = Some("postgres (PostgreSQL) 17.2\n");
    const PGDG: Option<&str> = Some("postgres (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)\n");

    fn resolve(
        probe: &FakeProbe,
        override_path: Option<&str>,
        embedded: Option<&Server>,
    ) -> Result<Server, String> {
        resolve_server(
            Some(Path::new("/opt/pg/bin/initdb")),
            override_path.map(Path::new),
            embedded,
            probe,
        )
        .map_err(|err| err.render())
    }

    #[test]
    fn the_server_beside_initdb_comes_first_as_in_c() {
        let embedded = Server::multicall(PathBuf::from("/opt/pg/bin/pgdrop"));
        let probe = FakeProbe::new(&[
            ("/opt/pg/bin/postgres", OURS),
            ("/elsewhere/postgres", OURS),
        ]);
        assert_eq!(
            resolve(&probe, Some("/elsewhere/postgres"), Some(&embedded)),
            Ok(Server::executable(PathBuf::from("/opt/pg/bin/postgres")))
        );
    }

    #[test]
    fn a_sibling_of_another_version_is_cs_error() {
        let probe = FakeProbe::new(&[
            ("/opt/pg/bin/postgres", OLDER),
            ("/elsewhere/postgres", OURS),
        ]);
        assert_eq!(
            resolve(&probe, Some("/elsewhere/postgres"), None),
            Err(
                "initdb: error: program \"postgres\" was found by \"/opt/pg/bin/initdb\" but was \
                 not the same version as initdb"
                    .to_owned()
            )
        );
    }

    #[test]
    fn without_a_sibling_the_override_then_the_embedded_server() {
        let embedded = Server::multicall(PathBuf::from("/opt/pg/bin/pgdrop"));
        // A sibling that cannot answer -V is find_other_exec's -1: not there.
        let probe = FakeProbe::new(&[
            ("/opt/pg/bin/postgres", None),
            ("/elsewhere/postgres", OURS),
        ]);
        assert_eq!(
            resolve(&probe, Some("/elsewhere/postgres"), Some(&embedded)),
            Ok(Server::executable(PathBuf::from("/elsewhere/postgres")))
        );
        assert_eq!(resolve(&probe, None, Some(&embedded)), Ok(embedded.clone()));
        // An override that is not a usable postgres is an error, not a
        // reason to fall back.
        assert_eq!(
            resolve(&probe, Some("/nowhere/postgres"), Some(&embedded)),
            Err(
                "initdb: error: PGDROP_POSTGRES names \"/nowhere/postgres\", which is not a \
                 postgres executable of the same version as initdb"
                    .to_owned()
            )
        );
    }

    #[test]
    fn a_distribution_extra_version_is_still_our_version() {
        assert!(is_backend_version("postgres (PostgreSQL) 18.6\n"));
        assert!(is_backend_version(
            "postgres (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)\n"
        ));
        assert!(is_backend_version(
            "postgres (PostgreSQL) 18.6 (Homebrew)\n"
        ));
        for other in [
            "postgres (PostgreSQL) 17.2\n",
            "postgres (PostgreSQL) 18.60\n",
            "postgres (PostgreSQL) 18.6 (Ubuntu)",
            "postgres (PostgreSQL) 18.6 trailing words\n",
            "postgres (PostgreSQL) 18.6 (a) (b)\n",
            "pg_ctl (PostgreSQL) 18.6 (Ubuntu)\n",
        ] {
            assert!(!is_backend_version(other), "{other:?}");
        }
        let sibling = FakeProbe::new(&[("/opt/pg/bin/postgres", PGDG)]);
        assert_eq!(
            resolve(&sibling, None, None),
            Ok(Server::executable(PathBuf::from("/opt/pg/bin/postgres")))
        );
        let elsewhere = FakeProbe::new(&[("/usr/lib/postgresql/18/bin/postgres", PGDG)]);
        assert_eq!(
            resolve(
                &elsewhere,
                Some("/usr/lib/postgresql/18/bin/postgres"),
                None
            ),
            Ok(Server::executable(PathBuf::from(
                "/usr/lib/postgresql/18/bin/postgres"
            )))
        );
    }

    #[test]
    fn no_server_at_all_is_cs_error() {
        assert_eq!(
            resolve(&FakeProbe::new(&[]), None, None),
            Err(
                "initdb: error: program \"postgres\" is needed by initdb but was not found in \
                 the same directory as \"/opt/pg/bin/initdb\""
                    .to_owned()
            )
        );
        // find_my_exec failed: C names progname (initdb.c:2658).
        assert_eq!(
            resolve_server(None, None, None, &FakeProbe::new(&[])).map_err(|err| err.render()),
            Err(
                "initdb: error: program \"postgres\" is needed by initdb but was not found in \
                 the same directory as \"initdb\""
                    .to_owned()
            )
        );
    }

    #[test]
    fn wait_results_read_as_c_reads_them() {
        assert_eq!(
            wait_result_to_str(Some(1), None),
            "child process exited with exit code 1"
        );
        assert_eq!(
            wait_result_to_str(Some(126), None),
            "command not executable"
        );
        assert_eq!(wait_result_to_str(Some(127), None), "command not found");
        assert_eq!(
            wait_result_to_str(None, Some(9)),
            "child process was terminated by signal 9"
        );
    }
}
