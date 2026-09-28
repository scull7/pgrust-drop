//! NAT-519: every upstream `file:line` citation in the repo, re-anchored
//! against a pristine PostgreSQL 18.6 tree (tag `REL_18_6`,
//! `724edf9bde9d356724ad384a2e196edc3c9f80f7`, or the release tarball).
//!
//! The tree is named by `PGDROP_UPSTREAM_SRC`; `scripts/fetch-upstream-src.sh`
//! puts one there from the checksummed tarball. Without it the test prints
//! `SKIP (flagged, not silent)` and passes, unless
//! `PGDROP_REQUIRE_UPSTREAM_SRC=1`, which turns the skip into a failure.
//! pgrust's `crates/postgres-18.6-reference/` is refused: it is not upstream
//! (AGENTS.md), and it lacks `src/port/win32ver.rc`, which the tag has.
//!
//! The rules are in `testkit::citation`; this file is the edge that reads.

#![allow(clippy::doc_markdown)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use testkit::citation::{Checker, Upstream, is_excluded};
use testkit::reference::SKIP_FLAG;

const SRC_ENV: &str = "PGDROP_UPSTREAM_SRC";
const REQUIRE_ENV: &str = "PGDROP_REQUIRE_UPSTREAM_SRC";
const FLOOR: usize = 3000;

struct Tree {
    root: PathBuf,
    paths: BTreeSet<String>,
    cache: BTreeMap<String, Vec<String>>,
}

impl Upstream for Tree {
    fn paths(&self) -> &BTreeSet<String> {
        &self.paths
    }

    fn lines(&mut self, path: &str) -> Option<&[String]> {
        if !self.cache.contains_key(path) {
            let bytes = fs::read(self.root.join(path)).ok()?;
            let text = String::from_utf8_lossy(&bytes);
            self.cache
                .insert(path.to_owned(), text.lines().map(str::to_owned).collect());
        }
        self.cache.get(path).map(Vec::as_slice)
    }
}

/// Every regular file under `root`, `/`-separated and relative to it, except
/// what `skip` says no to.
fn walk(root: &Path, skip: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let entries = fs::read_dir(root.join(&rel))
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", root.join(&rel).display()));
        for entry in entries {
            let entry = entry.expect("dir entry");
            let child = rel.join(entry.file_name());
            let name = child.to_string_lossy().replace('\\', "/");
            let kind = entry.file_type().expect("file type");
            if kind.is_dir() {
                if !skip(&format!("{name}/")) {
                    stack.push(child);
                }
            } else if kind.is_file() && !skip(&name) {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

fn upstream_tree() -> Option<Tree> {
    let Some(root) = std::env::var_os(SRC_ENV).filter(|v| !v.is_empty()) else {
        assert!(
            std::env::var(REQUIRE_ENV).as_deref() != Ok("1"),
            "{REQUIRE_ENV}=1, so the citation lint may not be skipped; set {SRC_ENV} to a \
             pristine REL_18_6 tree (scripts/fetch-upstream-src.sh)"
        );
        eprintln!(
            "{SKIP_FLAG}: no upstream tree; set {SRC_ENV} to a pristine REL_18_6 tree \
             (scripts/fetch-upstream-src.sh)"
        );
        return None;
    };
    let root = PathBuf::from(root);
    let configure = fs::read_to_string(root.join("configure"))
        .unwrap_or_else(|e| panic!("{SRC_ENV}={}: configure: {e}", root.display()));
    assert!(
        configure.contains("PACKAGE_VERSION='18.6'"),
        "{SRC_ENV}={} is not PostgreSQL 18.6",
        root.display()
    );
    assert!(
        root.join("src/port/win32ver.rc").is_file(),
        "{SRC_ENV}={} lacks src/port/win32ver.rc: not a pristine REL_18_6 tree \
         (pgrust's reference tree is not upstream; see AGENTS.md)",
        root.display()
    );
    let sample = fs::read_to_string(root.join("src/backend/utils/misc/postgresql.conf.sample"))
        .expect("postgresql.conf.sample");
    assert!(
        !sample.contains("# PGRUST"),
        "{SRC_ENV}={} carries pgrust's postgresql.conf.sample: not upstream",
        root.display()
    );
    let paths = walk(&root, &|rel| rel == ".git/").into_iter().collect();
    Some(Tree {
        root,
        paths,
        cache: BTreeMap::new(),
    })
}

#[test]
fn every_upstream_citation_lands_where_it_says() {
    let Some(mut tree) = upstream_tree() else {
        return;
    };
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let files = walk(&repo, &is_excluded);
    let mut local: BTreeSet<String> = BTreeSet::new();
    for rel in &files {
        local.insert(rel.clone());
        if let Some(base) = rel.rsplit('/').next() {
            local.insert(base.to_owned());
        }
    }
    let mut checker = Checker::new(&mut tree, &local);
    let mut findings = Vec::new();
    for rel in &files {
        let bytes = fs::read(repo.join(rel)).expect("read repo file");
        if bytes.contains(&0) {
            continue;
        }
        findings.extend(checker.check(rel, &String::from_utf8_lossy(&bytes)));
    }
    let anchored = checker.anchored();
    eprintln!("{anchored} upstream citations checked against REL_18_6");
    // Guard against a lint that passes because it stopped recognising
    // citations: the repo had 3,370 when this was written, and only grows.
    assert!(
        anchored >= FLOOR,
        "only {anchored} upstream citations were found; the scanner has stopped seeing them"
    );
    let report: Vec<String> = findings.iter().map(ToString::to_string).collect();
    assert!(
        report.is_empty(),
        "{} citation(s) do not land in REL_18_6 (mark a deliberate exception with \
         `citation-lint: allow` and a reason on the same line):\n{}",
        report.len(),
        report.join("\n")
    );
}
