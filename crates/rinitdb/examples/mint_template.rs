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
//! worth recording. Then it writes `<out-dir>/template.img` and
//! `<out-dir>/template.manifest`.

// Crate attributes are the only level that outranks CI's `-W clippy::pedantic`;
// the library root says why this lint is off, and `sha256.rs` is included here.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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
    let first = mint_and_pack(initdb, &scratch.0.join("first"))?;
    let second = mint_and_pack(initdb, &scratch.0.join("second"))?;
    if first != second {
        return Err("two mints packed to different images; the mint is not reproducible".into());
    }

    let manifest = Manifest {
        format: MAGIC[MAGIC.len() - 1],
        initdb: version.trim_end_matches('\n').to_owned(),
        libc: MINT_LIBC.to_owned(),
        options: MINT_ARGS.join(" "),
        bytes: first.len() as u64,
        sha256: sha256::digest_hex(&first),
    };
    let write = |name: &str, bytes: &[u8]| {
        let path = out_dir.join(name);
        std::fs::write(&path, bytes)
            .map_err(|err| format!("could not write \"{}\": {err}", path.display()))
    };
    write("template.img", &first)?;
    write("template.manifest", manifest.render().as_bytes())?;
    Ok(manifest)
}

/// `initdb -D <dir> MINT_ARGS`, then read, strip and pack `<dir>`.
fn mint_and_pack(initdb: &Path, dir: &Path) -> Result<Vec<u8>, String> {
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
    image::pack(&image::strip(entries)).map_err(|err| err.to_string())
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
