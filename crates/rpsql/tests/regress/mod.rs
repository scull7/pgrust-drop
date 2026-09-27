//! The harness for the stolen `src/test/regress/sql/psql.sql`: the vendored
//! script and its expected output, the splitter that cuts both into
//! sections, and a PostgreSQL 18 cluster to run a section against.
//!
//! `psql.sql` is one 2,000-line script. Gated whole, one wrong byte anywhere
//! fails everything and the diff is unreviewable, so it is cut into the
//! sections its own comment headers mark, and each section is gated against
//! the matching slice of `expected/psql.out` (NAT-400's Method).
//!
//! A section is run the way `pg_regress` runs the whole file
//! (`pg_regress_main.c:75`): `psql -X -a -q -d <db>` with the script on stdin
//! and stdout and stderr in one stream.

// Each integration test is its own crate and uses only part of this module.
#![allow(dead_code, clippy::doc_markdown)]

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use testkit::reference;

/// `src/test/regress/sql/psql.sql` at `REL_18_6`, byte for byte.
pub const PSQL_SQL: &str = include_str!("psql.sql");
/// `src/test/regress/expected/psql.out` at `REL_18_6`, byte for byte.
pub const PSQL_OUT: &str = include_str!("expected/psql.out");

/// The SHA-256 of each file as PostgreSQL 18.6 ships it (tag `REL_18_6`,
/// commit `724edf9bde9d356724ad384a2e196edc3c9f80f7`); see `README.md` here.
pub const PSQL_SQL_SHA256: &str =
    "8454d0e81f90bae39f6ffd213cd2c921e2bcfa8c2984cd3112e90840585dc457";
/// See [`PSQL_SQL_SHA256`].
pub const PSQL_OUT_SHA256: &str =
    "588bf1582a4deff3708e37f9b51c7879f83ca8be103656f0df6990d8257e8dc7";

/// One section of `psql.sql` and the slice of `psql.out` it produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<'a> {
    /// The header: the first line of the comment that opens the section,
    /// e.g. `-- show all pset options`. It is also the section's first line
    /// in both files, because `-a` echoes it.
    pub header: &'a str,
    /// The script, header line included.
    pub sql: &'a str,
    /// What `pg_regress` expects the script to print.
    pub expected: &'a str,
    /// 1-based line of the header in `psql.sql`.
    pub sql_line: usize,
    /// 1-based line of the header in `psql.out`.
    pub out_line: usize,
}

/// Why the two files do not split into the same sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitError(pub String);

/// Calculation: cut `sql` and `out` into [`Section`]s.
///
/// A section starts at a comment line (`--`) that opens the file or follows
/// an empty line: that is how `psql.sql` separates its topics, and a comment
/// that follows a statement directly is a note within one. `psql -a` echoes
/// every input line, comments included (`mainloop.c:360`), except an empty
/// one outside a quote, which is skipped before it is echoed
/// (`mainloop.c:222`), so each header reappears verbatim in `out`; it is found by
/// searching forward from the previous section's header, which keeps a
/// repeated header (there are several `-- errors`) matched in order.
///
/// Both files are cut losslessly: the sections' `sql` concatenate back to
/// `sql` and their `expected` to `out`.
///
/// # Errors
/// A header of `sql` with no echo in `out` after the previous one.
pub fn split<'a>(sql: &'a str, out: &'a str) -> Result<Vec<Section<'a>>, SplitError> {
    let sql_lines: Vec<&str> = sql.split_inclusive('\n').collect();
    let out_lines: Vec<&str> = out.split_inclusive('\n').collect();

    // (sql index, out index) of each section's first line.
    let mut starts: Vec<(usize, usize)> = Vec::new();
    let mut out_from = 0;
    for (i, line) in sql_lines.iter().enumerate() {
        let opens = line.starts_with("--") && (i == 0 || sql_lines[i - 1] == "\n");
        if !opens {
            continue;
        }
        let found = out_lines[out_from..]
            .iter()
            .position(|l| l == line)
            .map(|p| p + out_from)
            .ok_or_else(|| {
                SplitError(format!(
                    "psql.sql:{}: header {:?} is not echoed in psql.out after line {}",
                    i + 1,
                    line.trim_end(),
                    out_from
                ))
            })?;
        starts.push((i, found));
        out_from = found + 1;
    }
    if starts.first() != Some(&(0, 0)) {
        return Err(SplitError(
            "psql.sql and psql.out must both open with a header comment".to_string(),
        ));
    }

    // Byte offset of each line start, so a section is a slice, not a copy.
    let offsets = |lines: &[&str]| -> Vec<usize> {
        let mut at = 0;
        let mut v: Vec<usize> = lines
            .iter()
            .map(|l| {
                let start = at;
                at += l.len();
                start
            })
            .collect();
        v.push(at);
        v
    };
    let sql_at = offsets(&sql_lines);
    let out_at = offsets(&out_lines);

    let sections = starts
        .iter()
        .enumerate()
        .map(|(k, &(s, o))| {
            let (s_end, o_end) = starts
                .get(k + 1)
                .copied()
                .unwrap_or((sql_lines.len(), out_lines.len()));
            Section {
                header: sql_lines[s].trim_end_matches('\n'),
                sql: &sql[sql_at[s]..sql_at[s_end]],
                expected: &out[out_at[o]..out_at[o_end]],
                sql_line: s + 1,
                out_line: o + 1,
            }
        })
        .collect();
    Ok(sections)
}

/// The section of the vendored `psql.sql` whose header is `header`.
///
/// # Panics
/// When the vendored files do not split, or no section has that header.
pub fn section(header: &str) -> Section<'static> {
    split(PSQL_SQL, PSQL_OUT)
        .expect("the vendored psql.sql and psql.out split")
        .into_iter()
        .find(|s| s.header == header)
        .unwrap_or_else(|| panic!("no section of psql.sql opens with {header:?}"))
}

/// The run of consecutive sections from the one headed `first` through the
/// one headed `last`, as one [`Section`] keyed by `first`: for sections that
/// only pass in order, because each starts from the `\pset` state the one
/// before it leaves. The sections tile both files, so the run is a slice of
/// each.
///
/// # Panics
/// As [`section`], or when `last` does not come after `first`.
pub fn sections(first: &str, last: &str) -> Section<'static> {
    let all = split(PSQL_SQL, PSQL_OUT).expect("the vendored psql.sql and psql.out split");
    let at = |header: &str| {
        all.iter()
            .position(|s| s.header == header)
            .unwrap_or_else(|| panic!("no section of psql.sql opens with {header:?}"))
    };
    let (from, to) = (at(first), at(last));
    assert!(from <= to, "{last:?} comes before {first:?}");
    let offset = |pick: fn(&Section<'static>) -> &'static str| -> (usize, usize) {
        let start: usize = all[..from].iter().map(|s| pick(s).len()).sum();
        let len: usize = all[from..=to].iter().map(|s| pick(s).len()).sum();
        (start, start + len)
    };
    let (sql_start, sql_end) = offset(|s| s.sql);
    let (out_start, out_end) = offset(|s| s.expected);
    Section {
        header: all[from].header,
        sql: &PSQL_SQL[sql_start..sql_end],
        expected: &PSQL_OUT[out_start..out_end],
        sql_line: all[from].sql_line,
        out_line: all[from].out_line,
    }
}

/// The part of `section` from its first line that reads `line` on: the same
/// cut in both files, since `-a` echoes that line too. It is for a section
/// whose head needs what this port does not have yet (`\d`, for one), and
/// whose tail runs on its own once the state the head left is restored.
///
/// # Panics
/// When either file has no such line in `section`.
pub fn tail(section: &Section<'static>, line: &str) -> Section<'static> {
    let cut = |text: &'static str, what: &str| -> (&'static str, usize) {
        let mut at = 0;
        for (n, l) in text.split_inclusive('\n').enumerate() {
            if l.trim_end_matches('\n') == line {
                return (&text[at..], n);
            }
            at += l.len();
        }
        panic!("{what} of {:?} has no line {line:?}", section.header);
    };
    let (sql, sql_skip) = cut(section.sql, "psql.sql");
    let (expected, out_skip) = cut(section.expected, "psql.out");
    Section {
        header: section.header,
        sql,
        expected,
        sql_line: section.sql_line + sql_skip,
        out_line: section.out_line + out_skip,
    }
}

/// The C tools a cluster is started with. `psql` is not among them: the
/// section gate compares rpsql against `psql.out` with only a server, and
/// against C psql when that is present too.
pub const CLUSTER_TOOLS: [&str; 2] = ["initdb", "pg_ctl"];

/// A PostgreSQL 18 cluster started from the reference tools for one gate and
/// stopped with it. Trust authentication over its Unix socket only.
pub struct Cluster {
    bin: PathBuf,
    dir: PathBuf,
    port: u16,
}

impl Cluster {
    /// Start a cluster on `port`, or `None` — with the skip flagged, or the
    /// test failed under `PGDROP_REQUIRE_REF=1` — when the reference `initdb`
    /// or `pg_ctl` is missing.
    ///
    /// # Panics
    /// When the tools are there but `initdb` or `pg_ctl start` fails.
    pub fn start(port: u16) -> Option<Self> {
        let initdb = reference::find_or_skip(CLUSTER_TOOLS[0])?;
        let bin = initdb.parent()?.to_path_buf();
        if !bin.join(CLUSTER_TOOLS[1]).is_file() {
            reference::skip(CLUSTER_TOOLS[1]);
            return None;
        }

        let dir = std::env::temp_dir().join(format!("rpsql-regress-{port}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the cluster directory is created");
        let data = dir.join("data");

        // The C locale and UTF8, so that neither the machine's locale nor
        // its default encoding leaks into the output.
        let initdb = Command::new(bin.join("initdb"))
            .args(["-D".as_ref(), data.as_os_str()])
            .args([
                "-U",
                "regress",
                "--auth",
                "trust",
                "-E",
                "UTF8",
                "--no-sync",
            ])
            .env("LC_ALL", "C")
            .output()
            .expect("reference initdb runs");
        assert!(
            initdb.status.success(),
            "reference initdb failed: {}",
            String::from_utf8_lossy(&initdb.stderr)
        );

        let start = Command::new(bin.join("pg_ctl"))
            .args(["-D".as_ref(), data.as_os_str()])
            .arg("-w")
            .arg("-o")
            .arg(format!(
                "-p {port} -k {} -c listen_addresses=",
                dir.display()
            ))
            .args(["-l".as_ref(), dir.join("log").as_os_str()])
            .arg("start")
            .env("LC_ALL", "C")
            .output()
            .expect("reference pg_ctl runs");
        assert!(
            start.status.success(),
            "reference pg_ctl start failed: {}",
            String::from_utf8_lossy(&start.stderr)
        );

        Some(Self { bin, dir, port })
    }

    /// Action: run `script` through `psql` the way `pg_regress` does, and
    /// return stdout and stderr as the one stream `2>&1` makes of them.
    ///
    /// # Panics
    /// When `psql` cannot be started or its output read.
    pub fn run_script(&self, psql: &Path, script: &str) -> Vec<u8> {
        let (mut reader, writer) = std::io::pipe().expect("a pipe for 2>&1");
        let mut command = Command::new(psql);
        command
            .args(["-X", "-a", "-q", "-d", "postgres"])
            .args(["-v", "HIDE_TABLEAM=on", "-v", "HIDE_TOAST_COMPRESSION=on"])
            // `pg_regress` names the server through the environment, too.
            .env("PGHOST", &self.dir)
            .env("PGPORT", self.port.to_string())
            .env("PGUSER", "regress")
            .env("LC_ALL", "C")
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

    /// The reference `psql` beside `initdb`, if this lane's installation has
    /// one (the Maven bundles `scripts/fetch-ref-binaries.sh` fetches do not).
    pub fn reference_psql(&self) -> Option<PathBuf> {
        let psql = self.bin.join("psql");
        psql.is_file().then_some(psql)
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args(["-D".as_ref(), self.dir.join("data").as_os_str()])
            .args(["-m", "immediate", "stop"])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A readable account of where `ours` first leaves `expected`: the line
/// number within the section and both lines, escaped so trailing spaces and
/// control bytes show.
pub fn first_difference(expected: &[u8], ours: &[u8]) -> Option<String> {
    let exp: Vec<&[u8]> = expected.split_inclusive(|&b| b == b'\n').collect();
    let got: Vec<&[u8]> = ours.split_inclusive(|&b| b == b'\n').collect();
    let show = |l: Option<&&[u8]>| match l {
        Some(l) => format!("{:?}", String::from_utf8_lossy(l)),
        None => "<end of output>".to_string(),
    };
    (0..exp.len().max(got.len()))
        .find(|&i| exp.get(i) != got.get(i))
        .map(|i| {
            format!(
                "line {} of the section differs\n  expected: {}\n  got:      {}",
                i + 1,
                show(exp.get(i)),
                show(got.get(i))
            )
        })
}
