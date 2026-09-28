//! A runner for the stolen `src/test/regress/sql/*.sql` scripts against
//! pgrust: a server `pgdrop start` brings up, each script section run through
//! rpsql (`pgdrop psql`) and through the reference C psql, and both held to
//! the vendored expected output (NAT-404).
//!
//! The rpsql crate's gates (`crates/rpsql/tests/t_regress_*.rs`) run the same
//! scripts against a stock PostgreSQL 18 cluster: they prove rpsql. These
//! prove the product's other half. With C psql 18.6 as the client, a section
//! that differs from the expected output is pgrust's doing, so each gate
//! names two lists besides the sections it gates:
//!
//! - `not_yet`: sections rpsql cannot run yet, each with the command it
//!   needs and the issue that owns it. Skipped by rpsql, flagged; C psql
//!   still runs them, so pgrust is gated on them all the same.
//! - `pgrust`: sections where pgrust itself prints something else, each
//!   with what it does, recorded in `docs/divergences.md` ("pgrust-side
//!   divergences") to be filed upstream. Such a section must still differ
//!   from the expected output through C psql, so a pgrust that has caught up
//!   fails the gate until the entry goes; rpsql's output on it must equal C
//!   psql's.
//!
//! Every name in either list must head exactly one section, and sections are
//! skipped by name only, never by pattern, so no gate narrows quietly.
//!
//! The script files, their splitter and the diff report are the rpsql
//! crate's (`crates/rpsql/tests/regress/mod.rs`), compiled in here by path so
//! both crates gate the one vendored copy.
//!
//! A section is run the way `pg_regress` runs a whole file
//! (`src/test/regress/pg_regress_main.c:75`): `psql -X -a -q -d <db>` with the
//! script on stdin and stdout and stderr in one stream.

// Each integration test is its own crate and uses only part of this module.
#![allow(dead_code, clippy::doc_markdown)]

#[path = "../../../rpsql/tests/regress/mod.rs"]
pub mod regress;

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use regress::{Section, first_difference};
use testkit::reference;

/// The binary under test: `pgdrop start`, `pgdrop stop` and `pgdrop psql`.
pub const PGDROP: &str = env!("CARGO_BIN_EXE_pgdrop");

/// A section a gate does not hold to the expected output, by header, and
/// why: the command and owning issue (`not_yet`), or what pgrust does
/// (`pgrust`).
pub type Named<'a> = (&'a str, &'a str);

/// A pgrust server `pgdrop start` brought up for one gate, in a scratch
/// directory of its own, and stopped with it.
pub struct Server {
    scratch: PathBuf,
    /// The socket directory, `start`'s run directory.
    host: String,
    datadir: String,
}

impl Server {
    /// Action: `pgdrop start --json`, with the server's share files
    /// extracted under the scratch directory's `XDG_CACHE_HOME`.
    ///
    /// # Panics
    /// When `start` fails or prints something that is not its JSON line.
    pub fn start(tag: &str) -> Self {
        let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("pgrust-regress-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("the scratch directory is created");
        let output = pgdrop(&scratch, &["start", "--json"]);
        assert!(
            output.status.success(),
            "pgdrop start: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let json = String::from_utf8(output.stdout).expect("start prints UTF-8");
        let started = Started::parse(&json);
        Self {
            scratch,
            host: started.host,
            datadir: started.datadir,
        }
    }

    /// Action: run `script` through `psql` (`None`: rpsql, as `pgdrop
    /// psql`) the way `pg_regress` does, and return stdout and stderr as the
    /// one stream `2>&1` makes of them.
    ///
    /// # Panics
    /// When psql cannot be started or its output read.
    pub fn run_script(&self, psql: Option<&Path>, script: &str) -> Vec<u8> {
        let mut command = Command::new(psql.unwrap_or(Path::new(PGDROP)));
        if psql.is_none() {
            command.arg("psql");
        }
        let (mut reader, writer) = std::io::pipe().expect("a pipe for 2>&1");
        command
            .args(["-X", "-a", "-q", "-d", "postgres"])
            .args(["-v", "HIDE_TABLEAM=on", "-v", "HIDE_TOAST_COMPRESSION=on"])
            // `pg_regress` names the server through the environment, too.
            .env("PGHOST", &self.host)
            .env("PGPORT", "5432")
            .env("PGUSER", "postgres")
            .env("LC_ALL", "C")
            .env_remove("PGDATABASE")
            .env_remove("PGOPTIONS")
            .env_remove("PGSERVICE")
            .env_remove("PSQLRC")
            .stdin(Stdio::piped())
            .stdout(writer.try_clone().expect("the pipe's write end clones"))
            .stderr(writer);
        let mut child = command.spawn().expect("psql starts");
        // The Command holds write ends of the pipe; drop it so the read below
        // sees end-of-file when psql exits.
        drop(command);

        let mut stdin = child.stdin.take().expect("a stdin pipe");
        let script = script.to_owned();
        let feeder = std::thread::spawn(move || {
            let _ = stdin.write_all(script.as_bytes());
        });
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .expect("psql's output is read");
        feeder.join().expect("the script is fed");
        child.wait().expect("psql exits");
        output
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = pgdrop(&self.scratch, &["stop", "--datadir", &self.datadir]);
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Action: `pgdrop <args>` with the scratch directory's XDG cache and no
/// server named by the caller's environment.
fn pgdrop(scratch: &Path, args: &[&str]) -> Output {
    Command::new(PGDROP)
        .args(args)
        .env("XDG_CACHE_HOME", scratch.join("cache"))
        .env_remove("PGDATA")
        .env_remove("PGRUST_PGSHAREDIR")
        .env_remove("PGRUST_TZDIR")
        .stdin(Stdio::null())
        .output()
        .expect("pgdrop runs")
}

/// What `start --json` printed that a gate needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Started {
    host: String,
    datadir: String,
}

impl Started {
    /// Calculation: the socket directory and data directory from the one
    /// line `{"uri": "postgresql://postgres@%2F…:5432/postgres", "pid": …,
    /// "datadir": "…"}`. Their values never hold a `"`, a `\` or a `,`.
    fn parse(json: &str) -> Self {
        let field = |key: &str| -> &str {
            let start = json.find(&format!("\"{key}\": ")).expect(key) + key.len() + 4;
            let rest = &json[start..];
            let end = rest.find([',', '}']).expect("end of value");
            rest[..end].trim_matches('"')
        };
        let host = field("uri")
            .strip_prefix("postgresql://postgres@")
            .and_then(|rest| rest.split_once(':'))
            .map(|(host, _)| host.replace("%2F", "/"))
            .expect("a Unix-socket URI");
        assert!(!host.contains('%'), "{host}");
        Self {
            host,
            datadir: field("datadir").to_owned(),
        }
    }
}

/// Calculation: the entry of `list` that names `section`.
fn named<'a>(list: &[Named<'a>], section: &Section<'_>) -> Option<Named<'a>> {
    list.iter().copied().find(|(h, _)| *h == section.header)
}

/// Check that every header of `list` heads exactly one of `sections`: a typo
/// cannot skip nothing, and a repeated header cannot skip two.
///
/// # Panics
/// On the first that does not.
pub fn assert_names_one_section_each(list: &[Named<'_>], sections: &[Section<'_>], what: &str) {
    for (header, _) in list {
        let count = sections.iter().filter(|s| s.header == *header).count();
        assert_eq!(
            count, 1,
            "{what} names {header:?}, which heads {count} sections"
        );
    }
}

/// Run `sections` in order against `server`, first through rpsql and then
/// through the reference C psql, each section in a session of its own, and
/// hold them to the expected output as the module documentation says.
///
/// Two passes, not one: a section run through rpsql and then again through
/// C psql would create its tables twice. A script ends by dropping what it
/// created, so C psql's pass starts from the database rpsql's started from.
///
/// # Panics
/// On the first section either psql gets wrong, or when `not_yet` or
/// `pgrust` name a header that does not head exactly one section.
pub fn gate(
    server: &Server,
    sections: &[Section<'_>],
    not_yet: &[Named<'_>],
    pgrust: &[Named<'_>],
) {
    assert_names_one_section_each(not_yet, sections, "not_yet");
    assert_names_one_section_each(pgrust, sections, "pgrust");

    let ours: Vec<Option<Vec<u8>>> = sections
        .iter()
        .map(|section| {
            if let Some((_, why)) = named(not_yet, section) {
                reference::announce_skip(&format!(
                    "{}: rpsql skips {:?} against pgrust: it needs {why}",
                    reference::SKIP_FLAG,
                    section.header
                ));
                return None;
            }
            let output = server.run_script(None, section.sql);
            if named(pgrust, section).is_none()
                && let Some(diff) = first_difference(section.expected.as_bytes(), &output)
            {
                panic!(
                    "rpsql on pgrust, sql:{} vs out:{} ({}): {diff}",
                    section.sql_line, section.out_line, section.header
                );
            }
            Some(output)
        })
        .collect();

    let Some(psql) = reference::find_or_skip("psql") else {
        return;
    };
    for (section, ours) in sections.iter().zip(&ours) {
        let theirs = server.run_script(Some(&psql), section.sql);
        if let Some((_, what)) = named(pgrust, section) {
            assert!(
                first_difference(section.expected.as_bytes(), &theirs).is_some(),
                "C psql on pgrust now prints the expected output of {:?}: \
                 pgrust no longer {what}; remove the entry",
                section.header
            );
            if let Some(ours) = ours
                && let Some(diff) = first_difference(&theirs, ours)
            {
                panic!("rpsql vs C psql on pgrust ({}): {diff}", section.header);
            }
        } else if let Some(diff) = first_difference(section.expected.as_bytes(), &theirs) {
            panic!(
                "C psql on pgrust, sql:{} vs out:{} ({}): {diff}\n\
                 pgrust prints something else here: record it in \
                 docs/divergences.md and name the section in `pgrust`",
                section.sql_line, section.out_line, section.header
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Started;

    #[test]
    fn start_json_names_the_socket_directory_and_the_datadir() {
        let json = "{\"uri\": \"postgresql://postgres@%2Ftmp%2Fpgdrop-1-a:5432/postgres\", \
                    \"pid\": 7, \"datadir\": \"/tmp/pgdrop-1-a/data\"}\n";
        assert_eq!(
            Started::parse(json),
            Started {
                host: "/tmp/pgdrop-1-a".to_owned(),
                datadir: "/tmp/pgdrop-1-a/data".to_owned(),
            }
        );
    }
}
