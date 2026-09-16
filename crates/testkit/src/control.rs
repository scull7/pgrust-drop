//! Reading a cluster's `pg_control` without the C `pg_controldata`.
//!
//! Several `t/001_initdb.pl` assertions are `command_like(['pg_controldata',
//! $datadir], qr/…/)` — `001_initdb.pl:74` and `:323` check the data-checksum
//! version that way. `pg_controldata` is a PostgreSQL binary, so on a machine
//! without PostgreSQL 18 the byte-diff gate for those lines skips; the
//! assertion itself must still be made, which is what [`read_control_file`] is
//! for.
//!
//! This is deliberately the *small* reader: the handful of fields the stolen
//! assertions name, read straight out of the image the way `pg_controldata`'s
//! own `get_controlfile()` does (`src/common/controldata_utils.c:66`). The full
//! `ControlFileData` port — every field, plus writing one back — belongs to the
//! tool that writes clusters, and lives in `rinitdb::control`. Keeping the two
//! apart keeps `testkit` free of a dependency on the crate it tests;
//! `crates/rinitdb/tests/t_001_initdb.rs::the_two_control_file_readers_agree`
//! is the test that stops them drifting.

use std::path::{Path, PathBuf};

/// `XLOG_CONTROL_FILE` (`src/include/access/xlog_internal.h:150`), relative to
/// the data directory.
pub const XLOG_CONTROL_FILE: &str = "global/pg_control";

/// `sizeof(ControlFileData)` on a 64-bit build; `get_controlfile` reads exactly
/// this many bytes (`src/common/controldata_utils.c:99`).
const SIZEOF_CONTROL_FILE_DATA: usize = 296;

/// `offsetof(ControlFileData, system_identifier)`.
const OFF_SYSTEM_IDENTIFIER: usize = 0;
/// `offsetof(ControlFileData, pg_control_version)`.
const OFF_PG_CONTROL_VERSION: usize = 8;
/// `offsetof(ControlFileData, catalog_version_no)`.
const OFF_CATALOG_VERSION_NO: usize = 12;
/// `offsetof(ControlFileData, data_checksum_version)`.
const OFF_DATA_CHECKSUM_VERSION: usize = 252;

/// The fields of `pg_control` the stolen assertions read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlData {
    /// `Database system identifier` (`pg_controldata.c:242`).
    pub system_identifier: u64,
    /// `pg_control version number` (`pg_controldata.c:238`).
    pub pg_control_version: u32,
    /// `Catalog version number` (`pg_controldata.c:240`).
    pub catalog_version_no: u32,
    /// `Data page checksum version` (`pg_controldata.c:337`).
    pub data_checksum_version: u32,
}

impl ControlData {
    /// Pure: read the fields out of a `pg_control` image.
    ///
    /// `None` when the image is shorter than `sizeof(ControlFileData)`, which
    /// is the short read `get_controlfile_by_exact_path` calls a fatal error
    /// (`src/common/controldata_utils.c:113`).
    #[must_use]
    pub fn parse(image: &[u8]) -> Option<Self> {
        if image.len() < SIZEOF_CONTROL_FILE_DATA {
            return None;
        }
        Some(Self {
            system_identifier: u64_at(image, OFF_SYSTEM_IDENTIFIER),
            pg_control_version: u32_at(image, OFF_PG_CONTROL_VERSION),
            catalog_version_no: u32_at(image, OFF_CATALOG_VERSION_NO),
            data_checksum_version: u32_at(image, OFF_DATA_CHECKSUM_VERSION),
        })
    }

    /// The `Data page checksum version:` line `pg_controldata.c:337` prints,
    /// padded to upstream's column so a stolen `qr//` can be matched against it.
    #[must_use]
    pub fn data_page_checksum_version_line(&self) -> String {
        format!(
            "Data page checksum version:           {}",
            self.data_checksum_version
        )
    }
}

/// Where `pg_control` lives under `datadir`.
#[must_use]
pub fn control_file_path(datadir: &Path) -> PathBuf {
    datadir.join(XLOG_CONTROL_FILE)
}

/// Action: read `$datadir/global/pg_control`.
///
/// # Panics
///
/// If the file cannot be read or is shorter than `sizeof(ControlFileData)` —
/// both are `pg_fatal` in `get_controlfile_by_exact_path`, and a test that
/// asked for a cluster's control file has nothing to assert without it.
#[must_use]
pub fn read_control_file(datadir: &Path) -> ControlData {
    let path = control_file_path(datadir);
    let image = std::fs::read(&path).unwrap_or_else(|err| {
        panic!(
            "could not open file \"{}\" for reading: {err}",
            path.display()
        )
    });
    ControlData::parse(&image).unwrap_or_else(|| {
        panic!(
            "could not read file \"{}\": read {} of {SIZEOF_CONTROL_FILE_DATA}",
            path.display(),
            image.len()
        )
    })
}

fn u32_at(image: &[u8], at: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&image[at..at + 4]);
    u32::from_ne_bytes(bytes)
}

fn u64_at(image: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&image[at..at + 8]);
    u64::from_ne_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Pattern;

    fn an_image(sysid: u64, checksum_version: u32) -> Vec<u8> {
        let mut image = vec![0u8; SIZEOF_CONTROL_FILE_DATA];
        image[OFF_SYSTEM_IDENTIFIER..OFF_SYSTEM_IDENTIFIER + 8]
            .copy_from_slice(&sysid.to_ne_bytes());
        image[OFF_PG_CONTROL_VERSION..OFF_PG_CONTROL_VERSION + 4]
            .copy_from_slice(&1800u32.to_ne_bytes());
        image[OFF_DATA_CHECKSUM_VERSION..OFF_DATA_CHECKSUM_VERSION + 4]
            .copy_from_slice(&checksum_version.to_ne_bytes());
        image
    }

    #[test]
    fn the_fields_come_out_of_the_image() {
        let data = ControlData::parse(&an_image(0x0123_4567_89AB_CDEF, 1)).expect("parse");
        assert_eq!(data.system_identifier, 0x0123_4567_89AB_CDEF);
        assert_eq!(data.pg_control_version, 1800);
        assert_eq!(data.data_checksum_version, 1);
    }

    #[test]
    fn a_short_image_is_none() {
        assert_eq!(ControlData::parse(&[0u8; 295]), None);
        assert!(ControlData::parse(&[0u8; 296]).is_some());
    }

    /// The rendered line has to satisfy the stolen `qr//`s themselves —
    /// `001_initdb.pl:75` and `:324` — or reading the file instead of running
    /// `pg_controldata` would be answering a different question.
    #[test]
    fn the_rendered_line_matches_the_stolen_patterns() {
        let enabled = ControlData::parse(&an_image(1, 1)).expect("parse");
        let disabled = ControlData::parse(&an_image(1, 0)).expect("parse");

        let on =
            Pattern::new("Data page checksum version:.*1").expect("compile the stolen pattern");
        let off =
            Pattern::new("Data page checksum version:.*0").expect("compile the stolen pattern");

        assert!(on.is_match(&enabled.data_page_checksum_version_line()));
        assert!(off.is_match(&disabled.data_page_checksum_version_line()));
        assert!(!off.is_match(&enabled.data_page_checksum_version_line()));
        assert!(!on.is_match(&disabled.data_page_checksum_version_line()));
    }
}
