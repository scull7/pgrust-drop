//! Mint the committed template image (NAT-381, ADR-0002).
//!
//! Run through `scripts/mint-template-image.sh`, which is the interface; this
//! is its pack step, with exactly two positional arguments and no options, so
//! it takes them from `argv` rather than through usage-rs.
//!
//! ```text
//! mint_template <initdb> <out-dir>
//! ```
//!
//! It refuses an `initdb` that is not PostgreSQL 18.6 or not linked against
//! musl ([`rinitdb::image::mint`]), mints two clusters with
//! [`rinitdb::image::MINT_ARGS`], strips and packs each, and refuses unless
//! the two images are byte-identical — a mint that is not reproducible is not
//! worth recording; the refusal names the first entry that differs. It keeps
//! each mint's `global/pg_control` in template form
//! ([`rinitdb::image::mint::template_control`]) and requires those to agree
//! too. Between
//! the two it measures what the host put into `pg_collation`, by running
//! [`rinitdb::image::mint::HOST_QUERY`] through the same `postgres` in
//! single-user mode on the first cluster, after it is packed (the query's
//! shutdown checkpoint writes to the directory). Then it writes
//! `<out-dir>/template.img`, `<out-dir>/template.control` and
//! `<out-dir>/template.manifest`.

// Crate attributes are the only level that outranks CI's `-W clippy::pedantic`;
// the library root says why this lint is off, and `sha256.rs` is included here.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use rinitdb::image::manifest::{MINT_LIBC, Manifest};
use rinitdb::image::{self, MAGIC, MINT_ARGS, mint};

// SHA-256 is `#[cfg(test)]` in the library, so it stays out of the shipped
// binary; this tool compiles the same file in by path.
#[path = "../src/sha256.rs"]
mod sha256;

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [initdb, out_dir] = args.as_slice() else {
        eprintln!("usage: mint_template <initdb> <out-dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(initdb), Path::new(out_dir)) {
        Ok(manifest) => {
            print!("{}", manifest.render());
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("mint_template: error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(initdb: &Path, out_dir: &Path) -> Result<Manifest, String> {
    let version = Command::new(initdb)
        .arg("--version")
        .output()
        .map_err(|err| format!("could not run \"{}\": {err}", initdb.display()))?;
    let version = String::from_utf8_lossy(&version.stdout).into_owned();
    mint::check_version(&version).map_err(|err| err.to_string())?;

    let postgres = initdb.with_file_name("postgres");
    for binary in [initdb, postgres.as_path()] {
        let elf = std::fs::read(binary)
            .map_err(|err| format!("could not read \"{}\": {err}", binary.display()))?;
        mint::check_musl(&binary.display().to_string(), &elf).map_err(|err| err.to_string())?;
    }

    let scratch = Scratch::new()?;
    let first_dir = scratch.0.join("first");
    let (first, first_control) = mint_and_pack(initdb, &first_dir)?;
    let host = host_facts(&postgres, &first_dir)?;
    let (second, second_control) = mint_and_pack(initdb, &scratch.0.join("second"))?;
    if let Some(difference) = mint::first_difference(&first, &second) {
        return Err(format!(
            "two mints packed to different images (first difference: {difference}); \
             the mint is not reproducible"
        ));
    }
    if first_control != second_control {
        return Err(
            "two mints left different pg_control files in template form; \
             the mint is not reproducible"
                .to_owned(),
        );
    }

    let manifest = Manifest {
        format: MAGIC[MAGIC.len() - 1],
        initdb: version.trim_end_matches('\n').to_owned(),
        libc: MINT_LIBC.to_owned(),
        icu: host.icu,
        collations: host.collations,
        options: MINT_ARGS.join(" "),
        bytes: first.len() as u64,
        sha256: sha256::digest_hex(&first),
        control: sha256::digest_hex(&first_control),
    };
    let write = |name: &str, bytes: &[u8]| {
        let path = out_dir.join(name);
        std::fs::write(&path, bytes)
            .map_err(|err| format!("could not write \"{}\": {err}", path.display()))
    };
    write("template.img", &first)?;
    write("template.control", &first_control)?;
    write("template.manifest", manifest.render().as_bytes())?;
    Ok(manifest)
}

/// `initdb -D <dir> MINT_ARGS`, then read, strip and pack `<dir>`, and keep
/// its `pg_control` in template form. Both are taken before anything else
/// runs a server on `<dir>`.
fn mint_and_pack(initdb: &Path, dir: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    let output = Command::new(initdb)
        .arg("-D")
        .arg(dir)
        .args(MINT_ARGS)
        .output()
        .map_err(|err| format!("could not run \"{}\": {err}", initdb.display()))?;
    if !output.status.success() {
        return Err(format!(
            "initdb failed ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let entries = image::read_tree(dir)
        .map_err(|err| format!("could not read \"{}\": {err}", dir.display()))?;
    let control_path = dir.join("global").join("pg_control");
    let control = std::fs::read(&control_path)
        .map_err(|err| format!("could not read \"{}\": {err}", control_path.display()))?;
    let control = mint::template_control(&control).map_err(|err| err.to_string())?;
    let packed = image::pack(&image::strip(entries)).map_err(|err| err.to_string())?;
    Ok((packed, control.to_vec()))
}

/// What the minting host put into `pg_collation`.
struct HostFacts {
    icu: String,
    collations: String,
}

/// `postgres --single -D <dir> postgres` with [`mint::HOST_QUERY`] on stdin,
/// and the two values it prints.
fn host_facts(postgres: &Path, dir: &Path) -> Result<HostFacts, String> {
    let mut child = Command::new(postgres)
        .arg("--single")
        .arg("-D")
        .arg(dir)
        .arg("postgres")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run \"{}\": {err}", postgres.display()))?;
    // Dropping stdin after the write is the EOF that ends the session.
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(mint::HOST_QUERY.as_bytes())
        .map_err(|err| format!("could not write to \"{}\": {err}", postgres.display()))?;
    let output = child
        .wait_with_output()
        .map_err(|err| format!("could not run \"{}\": {err}", postgres.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value = |column: &str| {
        mint::single_user_value(&stdout, column)
            .map(str::to_owned)
            .ok_or_else(|| {
                format!(
                    "postgres --single printed no \"{column}\" ({}):\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )
            })
    };
    Ok(HostFacts {
        icu: value("icu")?,
        collations: value("collations")?,
    })
}

/// A scratch directory, removed when the tool exits.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!("rinitdb-mint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .map_err(|err| format!("could not create \"{}\": {err}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
