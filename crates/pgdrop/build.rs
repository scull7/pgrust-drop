//! Embed `share/` (NAT-408): every file under it except its `README.md`
//! becomes one `(relative path, bytes)` row of `$OUT_DIR/share_files.rs`,
//! sorted by path, through `include_bytes!`. The same pass derives the
//! extraction key `src/share.rs` names its cache directory after: the package
//! version and a 64-bit FNV-1a over every row, so a changed byte is a new
//! directory and never a stale one.
//!
//! FNV-1a is a cache key here, not a security boundary: the cache lives in
//! the user's own directory, as trusted as the binary it came from.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let share = manifest.join("share");
    println!("cargo:rerun-if-changed={}", share.display());

    let mut files = Vec::new();
    walk(&share, &share, &mut files);
    files.retain(|(relative, _)| relative != "README.md");
    files.sort();

    let mut hash = FNV_OFFSET;
    let mut table = String::from("&[\n");
    for (relative, absolute) in &files {
        let bytes = std::fs::read(absolute).expect("read a share file");
        for part in [
            relative.as_bytes(),
            &[0],
            &bytes.len().to_le_bytes(),
            &bytes,
        ] {
            hash = fnv1a(hash, part);
        }
        writeln!(
            table,
            "    ({relative:?}, include_bytes!({:?}) as &[u8]),",
            absolute.display().to_string()
        )
        .expect("write to a String");
    }
    table.push(']');

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let generated = format!(
        "/// Every embedded share file, `(path relative to share/, bytes)`, sorted by path.\n\
         pub static FILES: &[(&str, &[u8])] = {table};\n\
         /// The extraction key: package version and an FNV-1a digest of [`FILES`].\n\
         pub const KEY: &str = \"{}-{hash:016x}\";\n",
        std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"),
    );
    std::fs::write(out.join("share_files.rs"), generated).expect("write share_files.rs");
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Every regular file under `dir`, as (`/`-separated path relative to `root`,
/// absolute path). Paths are ASCII by construction (upstream's file names).
fn walk(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in std::fs::read_dir(dir).expect("read a share directory") {
        let path = entry.expect("read a share directory entry").path();
        if path.is_dir() {
            walk(root, &path, files);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("under share/")
                .components()
                .map(|c| c.as_os_str().to_str().expect("an ASCII share path"))
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}
