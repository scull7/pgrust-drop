//! The template image against a cluster the reference C `initdb` minted
//! (NAT-381, ADR-0002).
//!
//! There is no upstream test to steal for this: the image is this port's own
//! bridge while pgrust has no `--boot`. What can be proved against C is that
//! a real PostgreSQL 18.6 cluster survives read → strip → pack → parse →
//! expand byte for byte, and that the expansion composes with the tree
//! `rinitdb::layout` has already made. Without a reference `initdb` the test
//! prints `SKIP (flagged, not silent)` and passes; `PGDROP_REQUIRE_REF=1`
//! makes it fail instead.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use rinitdb::image::{self, Entry, MINT_ARGS, Node};
use testkit::reference;

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("pgdrop-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create the test's temporary directory");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Mint a template with the reference `initdb` and the recorded recipe.
fn mint(initdb: &Path, dir: &Path) {
    let mut args: Vec<OsString> = vec!["-D".into(), dir.into()];
    args.extend(MINT_ARGS.iter().map(OsString::from));
    testkit::command_ok(initdb, &args);
}

/// The paths of `entries`, in canonical order, with what each holds.
fn listing(mut entries: Vec<Entry<Vec<u8>>>) -> Vec<(String, Option<Vec<u8>>)> {
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    entries
        .into_iter()
        .map(|entry| {
            let contents = match entry.node {
                Node::Dir => None,
                Node::File(contents) => Some(contents),
            };
            (entry.path.to_string(), contents)
        })
        .collect()
}

#[test]
fn a_reference_cluster_round_trips_through_the_image() {
    let Some(initdb) = reference::find_or_skip("initdb") else {
        return;
    };
    let tempdir = TempDir::new("image-mint");
    let minted = tempdir.join("tpl");
    mint(&initdb, &minted);

    let stripped = image::strip(image::read_tree(&minted).expect("read the minted cluster"));
    let packed = image::pack(&stripped).expect("pack the minted cluster");
    let entries = image::parse(&packed).expect("parse what was packed");

    // What strip must have removed, against a real cluster.
    let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
    for gone in [
        "PG_VERSION",
        "postgresql.conf",
        "pg_hba.conf",
        "global/pg_control",
    ] {
        assert!(minted.join(gone).is_file(), "C initdb wrote {gone}");
        assert!(!paths.contains(&gone), "{gone} is stripped");
    }
    assert!(
        entries
            .iter()
            .all(|entry| !(entry.path.as_str().starts_with("pg_wal/")
                && matches!(entry.node, Node::File(_)))),
        "no WAL file is packed"
    );
    for kept in [
        "global/pg_filenode.map",
        "base/1/PG_VERSION",
        "pg_xact/0000",
    ] {
        assert!(paths.contains(&kept), "{kept} is kept");
    }

    let target = tempdir.join("expanded");
    // The target itself is layout's to make, at PGDATA's mode.
    std::fs::create_dir(&target).expect("create the target");
    std::fs::set_permissions(
        &target,
        std::os::unix::fs::PermissionsExt::from_mode(testkit::files::PGDATA_DIR_MODE),
    )
    .expect("chmod the target");
    let started = Instant::now();
    let expanded = image::expand(&entries, &target, rinitdb::DataDirPerm::OWNER)
        .expect("expand into an empty directory");
    let elapsed = started.elapsed();
    eprintln!(
        "template image: {} bytes packed, {} dirs + {} files ({} bytes) expanded in {elapsed:?} (debug build)",
        packed.len(),
        expanded.dirs,
        expanded.files,
        expanded.bytes
    );

    assert_eq!(
        listing(image::read_tree(&target).expect("read the expansion")),
        listing(stripped),
        "the expansion is the stripped cluster, byte for byte"
    );
    testkit::check_mode_recursive_ok(
        &target,
        testkit::files::PGDATA_DIR_MODE,
        testkit::files::PGDATA_FILE_MODE,
        &[],
    );
}

/// The expansion lands on top of the tree `layout` has made — `global`,
/// `base/1`, `pg_wal/…` and a top-level `PG_VERSION` already there — without
/// colliding with any of it.
#[test]
fn the_image_expands_over_rinitdbs_layout() {
    let Some(initdb) = reference::find_or_skip("initdb") else {
        return;
    };
    let tempdir = TempDir::new("image-layout");
    let minted = tempdir.join("tpl");
    mint(&initdb, &minted);
    let packed = image::pack(&image::strip(
        image::read_tree(&minted).expect("read the minted cluster"),
    ))
    .expect("pack");
    let entries = image::parse(&packed).expect("parse");

    let pgdata = tempdir.join("data");
    let argv: Vec<OsString> = vec![pgdata.clone().into()];
    let rinitdb::Invocation::Init(options) = rinitdb::cli::plan(&argv) else {
        panic!("a plain PGDATA is a cluster-creation command line");
    };
    let Ok(rinitdb::Plan::Create(plan)) =
        rinitdb::validate(&options, &rinitdb::Environment::default(), &rinitdb::RealFs)
    else {
        panic!("validate {argv:?}");
    };
    rinitdb::layout::apply(&rinitdb::layout::layout(&plan, None))
        .unwrap_or_else(|err| panic!("layout: {}", err.render()));

    image::expand(&entries, &pgdata, plan.perm)
        .unwrap_or_else(|err| panic!("expand over the layout: {}", err.render()));

    // Everything C initdb made is there, except the stripped files, which a
    // later step writes; the one of them layout has already written is its
    // top-level PG_VERSION.
    for entry in image::read_tree(&minted).expect("read the minted cluster") {
        let path = entry.path.as_str();
        let here = pgdata.join(path);
        match entry.node {
            Node::Dir => assert!(here.is_dir(), "{path} is a directory"),
            Node::File(_) if !image::keeps(&entry) => {
                assert!(
                    path == "PG_VERSION" || !here.exists(),
                    "{path} is not from the image"
                );
            }
            Node::File(contents) => assert_eq!(
                std::fs::read(&here).expect("read an expanded file"),
                contents,
                "{path}"
            ),
        }
    }
    assert_eq!(
        std::fs::read(pgdata.join("PG_VERSION")).expect("layout's PG_VERSION"),
        b"18\n"
    );
    testkit::check_mode_recursive_ok(
        &pgdata,
        testkit::files::PGDATA_DIR_MODE,
        testkit::files::PGDATA_FILE_MODE,
        &[],
    );
}
