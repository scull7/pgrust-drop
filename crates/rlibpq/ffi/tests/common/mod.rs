//! Compile a C program against the vendored `libpq-fe.h`, link it with this
//! crate's `libpq.a`, and run it.
//!
//! No reference binary is involved, so nothing here skips: the C compiler is
//! the one every lane already needs (Rust links through it, and `ring` builds
//! with it), and a missing one fails the test.

// Each integration test is its own crate and uses only part of this module.
#![allow(dead_code, clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use testkit::CommandOutcome;

/// This crate's directory, which holds `include/` and `tests/c/`.
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Calculation: the libraries a C program linking a Rust `staticlib` needs
/// besides the archive — what `rustc --print native-static-libs` prints for
/// the target, less what every C compiler driver links anyway.
pub const fn native_static_libs() -> &'static [&'static str] {
    if cfg!(target_vendor = "apple") {
        &["-lSystem", "-lc", "-lm"]
    } else if cfg!(target_env = "musl") {
        &["-lc"]
    } else {
        &[
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]
    }
}

/// Calculation: of the `libpq-<hash>.a` archives in a `deps` directory, the
/// one written last, given each candidate's name and modification time.
///
/// Cargo builds this crate's archive as a dependency of the test that is
/// running, into the test binary's own `deps` directory, and does not copy
/// it anywhere stabler. Two archives sit there only when the crate was built
/// under two configurations; the one cargo just (re)built for this run is the
/// newest.
pub fn newest_archive<T: Ord>(candidates: impl IntoIterator<Item = (String, T)>) -> Option<String> {
    candidates
        .into_iter()
        .filter(|(name, _)| {
            name.starts_with("libpq-") && Path::new(name).extension().is_some_and(|ext| ext == "a")
        })
        .max_by(|a, b| a.1.cmp(&b.1))
        .map(|(name, _)| name)
}

/// Action: the `libpq.a` cargo built for this test run.
pub fn libpq_archive() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    let deps = exe.parent().expect("the test binary is in deps/");
    let candidates = std::fs::read_dir(deps)
        .expect("deps/ is readable")
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.file_name().into_string().ok()?, modified))
        });
    let name =
        newest_archive(candidates).unwrap_or_else(|| panic!("no libpq-*.a in {}", deps.display()));
    deps.join(name)
}

/// Action: compile `sources` against `include/` and `tests/c/`, link them
/// with `libpq.a`, and return the program. `$CC` picks the compiler, `cc` by
/// default.
pub fn build(program: &str, sources: &[PathBuf], cflags: &[&str]) -> PathBuf {
    let out_dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rlibpq-ffi");
    std::fs::create_dir_all(&out_dir).expect("create the output directory");
    let out = out_dir.join(program);
    let cc = std::env::var_os("CC").unwrap_or_else(|| OsString::from("cc"));
    let status = Command::new(&cc)
        .args(cflags)
        .arg("-I")
        .arg(crate_dir().join("include"))
        .arg("-I")
        .arg(crate_dir().join("tests/c"))
        .arg("-o")
        .arg(&out)
        .args(sources)
        .arg(libpq_archive())
        .args(native_static_libs())
        .status()
        .unwrap_or_else(|err| panic!("{}: {err}", cc.to_string_lossy()));
    assert!(status.success(), "{program} did not build: {status}");
    out
}

/// Action: run `program` with `args`.
pub fn run(program: &Path, args: &[&str]) -> CommandOutcome {
    testkit::run(program, args).expect("the program runs")
}

/// `chomp`, as `Utils.pm:426`-`:427` applies it to both streams of
/// `run_command`: one trailing newline removed.
pub fn chomp(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
