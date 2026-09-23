//! The new cluster's `pg_control` and first WAL segment against the ones C
//! `pg_resetwal` writes for the same cluster (NAT-381, ADR-0002).
//!
//! There is no upstream test to steal: `rinitdb` regenerates the WAL of an
//! expanded template the way `pg_resetwal -f` regenerates a cluster's
//! (`src/bin/pg_resetwal/pg_resetwal.c:940`, `:894`, `:1117`), so the C tool
//! itself is the oracle. Mint a cluster with the reference `initdb`, keep its
//! `pg_control` in template form, let the reference `pg_resetwal -f` reset
//! it, and then:
//!
//! - `for_new_cluster` over the template form, given the four values
//!   `pg_resetwal` does not derive from the file (identifier, nonce, clock,
//!   checksums: it keeps the cluster's own), lands the checkpoint where
//!   `pg_resetwal` did, with the same `checkPointCopy`;
//! - `wal::segment` over `pg_resetwal`'s own `pg_control` is its segment
//!   file byte for byte.
//!
//! Without the reference tools the test prints `SKIP (flagged, not silent)`
//! and passes; `PGDROP_REQUIRE_REF=1` makes it fail instead.

#![cfg(unix)]
// Integration tests are their own crate; see the library root for why this lint is off.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::path::PathBuf;

use rinitdb::control::{ControlFile, DataChecksums, NewCluster, for_new_cluster};
use rinitdb::image::MINT_ARGS;
use rinitdb::wal;
use testkit::reference;

/// A directory of this test's own, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("pgdrop-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the test's temporary directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read_control(pgdata: &std::path::Path) -> ControlFile {
    let bytes = std::fs::read(testkit::control_file_path(pgdata)).expect("read pg_control");
    let control = ControlFile::parse(&bytes).expect("parse pg_control");
    assert!(control.crc_is_valid(), "{}", pgdata.display());
    control
}

#[test]
fn the_first_segment_is_the_one_pg_resetwal_writes() {
    let Some(initdb) = reference::find_or_skip("initdb") else {
        return;
    };
    let Some(pg_resetwal) = reference::find_or_skip("pg_resetwal") else {
        return;
    };
    let tempdir = TempDir::new("first-segment");
    let pgdata = tempdir.0.join("data");
    let mut args: Vec<OsString> = vec!["-D".into(), pgdata.clone().into()];
    args.extend(MINT_ARGS.iter().map(OsString::from));
    testkit::command_ok(&initdb, &args);

    let template = read_control(&pgdata).as_template();
    testkit::command_ok(
        &pg_resetwal,
        [
            OsString::from("-f"),
            OsString::from("-D"),
            pgdata.clone().into(),
        ],
    );
    let reset = read_control(&pgdata);

    let ours = for_new_cluster(
        &template,
        &NewCluster {
            system_identifier: reset.system_identifier,
            checksums: DataChecksums::from_version(reset.data_checksum_version),
            mock_authentication_nonce: reset.mock_authentication_nonce,
            now: reset.check_point_copy.time,
        },
    );
    assert_eq!(ours.check_point, reset.check_point, "checkPoint");
    assert_eq!(
        ours.check_point_copy, reset.check_point_copy,
        "checkPointCopy"
    );
    assert_eq!(ours.state, reset.state);
    assert_eq!(ours.min_recovery_point, reset.min_recovery_point);
    assert_eq!(ours.min_recovery_point_tli, reset.min_recovery_point_tli);
    assert_eq!(ours.backup_start_point, reset.backup_start_point);
    assert_eq!(ours.backup_end_point, reset.backup_end_point);
    assert_eq!(ours.backup_end_required, reset.backup_end_required);

    let name = wal::checkpoint_segment_file_name(&reset);
    assert_eq!(name, wal::checkpoint_segment_file_name(&ours));
    let theirs = std::fs::read(pgdata.join("pg_wal").join(&name)).expect("pg_resetwal's segment");
    let segment = wal::segment(&reset);
    assert_eq!(segment.len(), theirs.len(), "segment size");
    assert!(
        segment == theirs,
        "{name}: the first differing byte is at {:?}",
        segment.iter().zip(&theirs).position(|(a, b)| a != b)
    );
}
