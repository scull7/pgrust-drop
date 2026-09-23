//! The first WAL segment of a cluster expanded from the template.
//!
//! The template is shipped without its `pg_wal` (`crate::image`), so a new
//! cluster starts the way `pg_resetwal` leaves one: a single segment holding
//! a single shutdown checkpoint record, the one `pg_control` points at. This
//! is `WriteEmptyXLOG` (`src/bin/pg_resetwal/pg_resetwal.c:1117`) and
//! `XLogFileName` (`src/include/access/xlog_internal.h:166`), with the
//! checkpoint placed by [`crate::control::for_new_cluster`].
//!
//! Data / Calculations / Actions: [`segment`] and [`segment_file_name`] are
//! pure; the caller writes the bytes.
//!
//! # Layout
//!
//! Every struct below is written as raw memory in the machine's byte order,
//! like `pg_control` (`crate::control`), for the same 64-bit,
//! `MAXIMUM_ALIGNOF` 8 build:
//!
//! ```text
//! XLogPageHeaderData (xlog_internal.h:36)      XLogLongPageHeaderData (:61)
//!   0 u16 xlp_magic                              24 u64 xlp_sysid
//!   2 u16 xlp_info                               32 u32 xlp_seg_size
//!   4 u32 xlp_tli                                36 u32 xlp_xlog_blcksz
//!   8 u64 xlp_pageaddr                           40 = SizeOfXLogLongPHD (:69)
//!  16 u32 xlp_rem_len, 4 bytes of padding
//!
//! XLogRecord (xlogrecord.h:41), at SizeOfXLogLongPHD
//!   0 u32 xl_tot_len   4 u32 xl_xid   8 u64 xl_prev
//!  16 u8 xl_info  17 u8 xl_rmid  18 2 bytes of padding  20 u32 xl_crc
//!  24 = SizeOfXLogRecord (:55), then XLR_BLOCK_ID_DATA_SHORT, the data
//!       length as one byte, and the CheckPoint itself
//! ```

use crate::control::{ControlFile, SIZE_OF_XLOG_LONG_PHD, SIZEOF_CHECK_POINT, segment_of};
use crate::crc32c::crc32c;

/// `XLOG_PAGE_MAGIC` (`src/include/access/xlog_internal.h:34`).
pub const XLOG_PAGE_MAGIC: u16 = 0xD118;

/// `XLP_LONG_HEADER` (`src/include/access/xlog_internal.h:76`).
pub const XLP_LONG_HEADER: u16 = 0x0002;

/// `XLOG_BLCKSZ` in a stock build: `--with-wal-blocksize` defaults to 8 kB
/// (`configure.ac:333`, `:340`).
pub const XLOG_BLCKSZ: usize = 8192;

/// `SizeOfXLogRecord` (`src/include/access/xlogrecord.h:55`).
pub const SIZE_OF_XLOG_RECORD: usize = 24;

/// `offsetof(XLogRecord, xl_crc)`.
const XL_CRC: usize = 20;

/// `SizeOfXLogRecordDataHeaderShort` (`src/include/access/xlogrecord.h:219`).
pub const SIZE_OF_XLOG_RECORD_DATA_HEADER_SHORT: usize = 2;

/// `XLR_BLOCK_ID_DATA_SHORT` (`src/include/access/xlogrecord.h:243`).
pub const XLR_BLOCK_ID_DATA_SHORT: u8 = 255;

/// `XLOG_CHECKPOINT_SHUTDOWN` (`src/include/catalog/pg_control.h:68`).
pub const XLOG_CHECKPOINT_SHUTDOWN: u8 = 0x00;

/// `RM_XLOG_ID`, the first `PG_RMGR` entry
/// (`src/include/access/rmgrlist.h:28`).
pub const RM_XLOG_ID: u8 = 0;

/// The checkpoint record's `xl_tot_len`: header, short data header and the
/// `CheckPoint` (`pg_resetwal.c:1147`).
pub const CHECKPOINT_RECORD_LEN: usize =
    SIZE_OF_XLOG_RECORD + SIZE_OF_XLOG_RECORD_DATA_HEADER_SHORT + SIZEOF_CHECK_POINT;

/// Pure: `XLogFileName` (`src/include/access/xlog_internal.h:166`), the
/// name of segment `segno` on timeline `tli`.
#[must_use]
pub fn segment_file_name(tli: u32, segno: u64, wal_segsz_bytes: u32) -> String {
    // XLogSegmentsPerXLogId (:100).
    let per_id = 0x1_0000_0000 / u64::from(wal_segsz_bytes);
    format!("{tli:08X}{:08X}{:08X}", segno / per_id, segno % per_id)
}

/// Pure: the name of the segment `control`'s checkpoint record is in.
#[must_use]
pub fn checkpoint_segment_file_name(control: &ControlFile) -> String {
    segment_file_name(
        control.check_point_copy.this_time_line_id,
        segment_of(control.check_point_copy.redo, control.xlog_seg_size),
        control.xlog_seg_size,
    )
}

/// Pure: `WriteEmptyXLOG`'s first page (`pg_resetwal.c:1117`): a long page
/// header, then the shutdown checkpoint record for `control`'s
/// `checkPointCopy`, then zeroes.
///
/// `control.check_point_copy.redo` must be the first record of a segment,
/// `SizeOfXLogLongPHD` past its start, which is where
/// [`crate::control::for_new_cluster`] puts it.
#[must_use]
pub fn first_page(control: &ControlFile) -> [u8; XLOG_BLCKSZ] {
    let cp = &control.check_point_copy;
    let mut page = [0u8; XLOG_BLCKSZ];

    // pg_resetwal.c:1133-:1140.
    put(&mut page, 0, &XLOG_PAGE_MAGIC.to_ne_bytes());
    put(&mut page, 2, &XLP_LONG_HEADER.to_ne_bytes());
    put(&mut page, 4, &cp.this_time_line_id.to_ne_bytes());
    put(
        &mut page,
        8,
        &(cp.redo - SIZE_OF_XLOG_LONG_PHD).to_ne_bytes(),
    );
    put(
        &mut page,
        24,
        &control.system_identifier.get().to_ne_bytes(),
    );
    put(&mut page, 32, &control.xlog_seg_size.to_ne_bytes());
    #[allow(clippy::cast_possible_truncation)] // 8192 fits.
    put(&mut page, 36, &(XLOG_BLCKSZ as u32).to_ne_bytes());

    // pg_resetwal.c:1143-:1155. xl_prev and xl_xid stay zero.
    #[allow(clippy::cast_possible_truncation)] // 40 fits.
    let at = SIZE_OF_XLOG_LONG_PHD as usize;
    #[allow(clippy::cast_possible_truncation)] // 114 fits.
    put(&mut page, at, &(CHECKPOINT_RECORD_LEN as u32).to_ne_bytes());
    page[at + 16] = XLOG_CHECKPOINT_SHUTDOWN;
    page[at + 17] = RM_XLOG_ID;
    let data = at + SIZE_OF_XLOG_RECORD;
    page[data] = XLR_BLOCK_ID_DATA_SHORT;
    #[allow(clippy::cast_possible_truncation)] // 88 fits.
    {
        page[data + 1] = SIZEOF_CHECK_POINT as u8;
    }
    put(&mut page, data + 2, &cp.to_bytes());

    // pg_resetwal.c:1157-:1161: the CRC runs over the data after the header,
    // then over the header up to xl_crc.
    let record = &page[at..at + CHECKPOINT_RECORD_LEN];
    let mut covered = Vec::with_capacity(CHECKPOINT_RECORD_LEN);
    covered.extend_from_slice(&record[SIZE_OF_XLOG_RECORD..]);
    covered.extend_from_slice(&record[..XL_CRC]);
    let crc = crc32c(&covered);
    put(&mut page, at + XL_CRC, &crc.to_ne_bytes());
    page
}

/// Pure: the whole segment file `WriteEmptyXLOG` writes, [`first_page`]
/// followed by zeroes up to `xlog_seg_size` bytes (`pg_resetwal.c:1183`).
///
/// # Panics
/// When `xlog_seg_size` does not fit in `usize`, which no 32- or 64-bit
/// target allows.
#[must_use]
pub fn segment(control: &ControlFile) -> Vec<u8> {
    let size = usize::try_from(control.xlog_seg_size).expect("a segment size fits in usize");
    let mut segment = vec![0u8; size.max(XLOG_BLCKSZ)];
    segment[..XLOG_BLCKSZ].copy_from_slice(&first_page(control));
    segment
}

fn put(page: &mut [u8], at: usize, bytes: &[u8]) {
    page[at..at + bytes.len()].copy_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{
        DataChecksums, MOCK_AUTH_NONCE_LEN, NewCluster, PG_CONTROL_FILE_SIZE, SystemIdentifier,
        for_new_cluster,
    };

    fn a_control() -> ControlFile {
        let mut template = ControlFile::parse(&[0u8; PG_CONTROL_FILE_SIZE]).unwrap();
        template.xlog_seg_size = 16 * 1024 * 1024;
        template.check_point_copy.redo = 0x0175_B1F0;
        template.check_point_copy.this_time_line_id = 1;
        template.check_point_copy.prev_time_line_id = 1;
        template.check_point_copy.next_xid = 758;
        template.check_point_copy.next_oid = 13589;
        for_new_cluster(
            &template,
            &NewCluster {
                system_identifier: SystemIdentifier::from_raw(0x6A_B1_C2_D3_E4_F5_06_17),
                checksums: DataChecksums::Enabled,
                mock_authentication_nonce: [0; MOCK_AUTH_NONCE_LEN],
                now: 1_790_000_000,
            },
        )
    }

    #[test]
    fn segment_names_are_xlog_file_names() {
        let sixteen = 16 * 1024 * 1024;
        assert_eq!(segment_file_name(1, 1, sixteen), "000000010000000000000001");
        // 256 segments of 16 MB make one "xlog id".
        assert_eq!(
            segment_file_name(1, 0x100, sixteen),
            "000000010000000100000000"
        );
        assert_eq!(
            segment_file_name(0xA, 0x1FF, sixteen),
            "0000000A00000001000000FF"
        );
        assert_eq!(
            checkpoint_segment_file_name(&a_control()),
            "000000010000000000000002"
        );
    }

    #[test]
    fn the_page_header_names_the_segment_and_the_cluster() {
        let control = a_control();
        let page = first_page(&control);
        assert_eq!(&page[0..2], &0xD118u16.to_ne_bytes());
        assert_eq!(&page[2..4], &2u16.to_ne_bytes());
        assert_eq!(&page[4..8], &1u32.to_ne_bytes());
        assert_eq!(&page[8..16], &0x0200_0000u64.to_ne_bytes());
        assert_eq!(&page[16..24], &[0; 8], "xlp_rem_len and padding");
        assert_eq!(
            &page[24..32],
            &control.system_identifier.get().to_ne_bytes()
        );
        assert_eq!(&page[32..36], &(16u32 * 1024 * 1024).to_ne_bytes());
        assert_eq!(&page[36..40], &8192u32.to_ne_bytes());
    }

    #[test]
    fn the_record_is_one_shutdown_checkpoint_with_a_valid_crc() {
        let control = a_control();
        let page = first_page(&control);
        let record = &page[40..40 + CHECKPOINT_RECORD_LEN];
        assert_eq!(CHECKPOINT_RECORD_LEN, 114);
        assert_eq!(&record[0..4], &114u32.to_ne_bytes());
        assert_eq!(&record[4..16], &[0; 12], "xl_xid and xl_prev");
        assert_eq!(record[16], 0x00, "XLOG_CHECKPOINT_SHUTDOWN");
        assert_eq!(record[17], 0, "RM_XLOG_ID");
        assert_eq!(&record[18..20], &[0, 0]);
        assert_eq!(record[24], 255);
        assert_eq!(record[25], 88);
        assert_eq!(&record[26..], &control.check_point_copy.to_bytes());

        let mut covered = record[24..].to_vec();
        covered.extend_from_slice(&record[..20]);
        assert_eq!(&record[20..24], &crc32c(&covered).to_ne_bytes());

        assert!(page[40 + CHECKPOINT_RECORD_LEN..].iter().all(|&b| b == 0));
    }

    #[test]
    fn the_segment_is_the_first_page_then_zeroes() {
        let control = a_control();
        let segment = segment(&control);
        assert_eq!(segment.len(), 16 * 1024 * 1024);
        assert_eq!(&segment[..XLOG_BLCKSZ], &first_page(&control));
        assert!(segment[XLOG_BLCKSZ..].iter().all(|&b| b == 0));
    }
}
