//! `pg_control`: the `ControlFileData` image, parsed and written back.
//!
//! Port of `src/include/catalog/pg_control.h` (the struct and its four size
//! constants), of `InitControlFile` / `WriteControlFile`
//! (`src/backend/access/transam/xlog.c:4200`, `:4235`) for which fields a new
//! cluster gets and how the CRC is taken, and of the system-identifier
//! derivation in `BootStrapXLOG` (`xlog.c:5098`-`:5101`).
//!
//! ADR-0002 expands a pre-minted template cluster, so the `pg_control` this
//! crate ships is C initdb's and must not stay as it is: every cluster needs
//! its own [`SystemIdentifier`] and the [`DataChecksums`] setting the command
//! line asked for. [`rewrite`] is that whole change as one pure function from
//! the template's bytes to the new cluster's bytes.
//!
//! # Layout
//!
//! `ControlFileData` is written to disk as raw memory — `WriteControlFile`
//! `memcpy`s the struct into a zeroed `PG_CONTROL_FILE_SIZE` buffer
//! (`xlog.c:4304`) — so the file *is* the C ABI's layout: native byte order,
//! natural alignment, and the compiler's interior padding. [`offset`] holds
//! every field's byte offset for a 64-bit build with `MAXIMUM_ALIGNOF` 8
//! (x86-64, aarch64, the platforms this crate targets), and [`PADDING`] holds
//! the runs no field owns. The two tile `[0, SIZEOF_CONTROL_FILE_DATA)`
//! exactly, which is what [`tests::the_fields_and_the_padding_tile_the_struct`]
//! checks and what makes a round trip byte-identical rather than approximate.
//!
//! A 32-bit or `MAXIMUM_ALIGNOF 4` build lays the struct out differently;
//! upstream cannot read such a file either (`maxAlign` is one of the
//! compatibility fields `ReadControlFile` rejects on, `xlog.c:4434`), so there
//! is one layout here and a build that does not match it is a build whose
//! clusters upstream would refuse too.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::crc32c;

/// `PG_CONTROL_VERSION` (`src/include/catalog/pg_control.h:25`).
pub const PG_CONTROL_VERSION: u32 = 1800;

/// `CATALOG_VERSION_NO` (`src/include/catalog/catversion.h:60`).
pub const CATALOG_VERSION_NO: u32 = 202_506_291;

/// `MOCK_AUTH_NONCE_LEN` (`src/include/catalog/pg_control.h:28`).
pub const MOCK_AUTH_NONCE_LEN: usize = 32;

/// `PG_CONTROL_FILE_SIZE` (`src/include/catalog/pg_control.h:256`): the
/// physical size of the file, constant across format changes.
pub const PG_CONTROL_FILE_SIZE: usize = 8192;

/// `PG_CONTROL_MAX_SAFE_SIZE` (`src/include/catalog/pg_control.h:247`): the
/// struct must fit in one disk sector so the write is atomic.
pub const PG_CONTROL_MAX_SAFE_SIZE: usize = 512;

/// `sizeof(ControlFileData)` for a 64-bit build — the number of bytes
/// `get_controlfile_by_exact_path` reads (`src/common/controldata_utils.c:101`)
/// and `WriteControlFile` copies (`xlog.c:4304`).
pub const SIZEOF_CONTROL_FILE_DATA: usize = 296;

/// `PG_DATA_CHECKSUM_VERSION` (`src/include/storage/bufpage.h:208`).
pub const PG_DATA_CHECKSUM_VERSION: u32 = 1;

/// `FLOATFORMAT_VALUE` (`src/include/catalog/pg_control.h:201`).
pub const FLOATFORMAT_VALUE: f64 = 1_234_567.0;

/// Byte offset of every `ControlFileData` field, in declaration order.
///
/// Taken from `src/include/catalog/pg_control.h:104` onwards under the C ABI
/// rules for a 64-bit build; the `checkPointCopy` group is the embedded
/// `CheckPoint` struct (`pg_control.h:35`) and its offsets are absolute, not
/// relative to the group.
pub mod offset {
    /// `uint64 system_identifier`.
    pub const SYSTEM_IDENTIFIER: usize = 0;
    /// `uint32 pg_control_version`.
    pub const PG_CONTROL_VERSION: usize = 8;
    /// `uint32 catalog_version_no`.
    pub const CATALOG_VERSION_NO: usize = 12;
    /// `DBState state` — a C `enum`, so a 4-byte `int`.
    pub const STATE: usize = 16;
    /// `pg_time_t time`.
    pub const TIME: usize = 24;
    /// `XLogRecPtr checkPoint`.
    pub const CHECK_POINT: usize = 32;

    /// `checkPointCopy.redo`.
    pub const CP_REDO: usize = 40;
    /// `checkPointCopy.ThisTimeLineID`.
    pub const CP_THIS_TIME_LINE_ID: usize = 48;
    /// `checkPointCopy.PrevTimeLineID`.
    pub const CP_PREV_TIME_LINE_ID: usize = 52;
    /// `checkPointCopy.fullPageWrites`.
    pub const CP_FULL_PAGE_WRITES: usize = 56;
    /// `checkPointCopy.wal_level`.
    pub const CP_WAL_LEVEL: usize = 60;
    /// `checkPointCopy.nextXid` — a `FullTransactionId`, which is a struct
    /// wrapping one `uint64` (`src/include/access/transam.h:65`).
    pub const CP_NEXT_XID: usize = 64;
    /// `checkPointCopy.nextOid`.
    pub const CP_NEXT_OID: usize = 72;
    /// `checkPointCopy.nextMulti`.
    pub const CP_NEXT_MULTI: usize = 76;
    /// `checkPointCopy.nextMultiOffset`.
    pub const CP_NEXT_MULTI_OFFSET: usize = 80;
    /// `checkPointCopy.oldestXid`.
    pub const CP_OLDEST_XID: usize = 84;
    /// `checkPointCopy.oldestXidDB`.
    pub const CP_OLDEST_XID_DB: usize = 88;
    /// `checkPointCopy.oldestMulti`.
    pub const CP_OLDEST_MULTI: usize = 92;
    /// `checkPointCopy.oldestMultiDB`.
    pub const CP_OLDEST_MULTI_DB: usize = 96;
    /// `checkPointCopy.time`.
    pub const CP_TIME: usize = 104;
    /// `checkPointCopy.oldestCommitTsXid`.
    pub const CP_OLDEST_COMMIT_TS_XID: usize = 112;
    /// `checkPointCopy.newestCommitTsXid`.
    pub const CP_NEWEST_COMMIT_TS_XID: usize = 116;
    /// `checkPointCopy.oldestActiveXid`.
    pub const CP_OLDEST_ACTIVE_XID: usize = 120;

    /// `XLogRecPtr unloggedLSN`.
    pub const UNLOGGED_LSN: usize = 128;
    /// `XLogRecPtr minRecoveryPoint`.
    pub const MIN_RECOVERY_POINT: usize = 136;
    /// `TimeLineID minRecoveryPointTLI`.
    pub const MIN_RECOVERY_POINT_TLI: usize = 144;
    /// `XLogRecPtr backupStartPoint`.
    pub const BACKUP_START_POINT: usize = 152;
    /// `XLogRecPtr backupEndPoint`.
    pub const BACKUP_END_POINT: usize = 160;
    /// `bool backupEndRequired`.
    pub const BACKUP_END_REQUIRED: usize = 168;
    /// `int wal_level`.
    pub const WAL_LEVEL: usize = 172;
    /// `bool wal_log_hints`.
    pub const WAL_LOG_HINTS: usize = 176;
    /// `int MaxConnections`.
    pub const MAX_CONNECTIONS: usize = 180;
    /// `int max_worker_processes`.
    pub const MAX_WORKER_PROCESSES: usize = 184;
    /// `int max_wal_senders`.
    pub const MAX_WAL_SENDERS: usize = 188;
    /// `int max_prepared_xacts`.
    pub const MAX_PREPARED_XACTS: usize = 192;
    /// `int max_locks_per_xact`.
    pub const MAX_LOCKS_PER_XACT: usize = 196;
    /// `bool track_commit_timestamp`.
    pub const TRACK_COMMIT_TIMESTAMP: usize = 200;
    /// `uint32 maxAlign`.
    pub const MAX_ALIGN: usize = 204;
    /// `double floatFormat`.
    pub const FLOAT_FORMAT: usize = 208;
    /// `uint32 blcksz`.
    pub const BLCKSZ: usize = 216;
    /// `uint32 relseg_size`.
    pub const RELSEG_SIZE: usize = 220;
    /// `uint32 xlog_blcksz`.
    pub const XLOG_BLCKSZ: usize = 224;
    /// `uint32 xlog_seg_size`.
    pub const XLOG_SEG_SIZE: usize = 228;
    /// `uint32 nameDataLen`.
    pub const NAME_DATA_LEN: usize = 232;
    /// `uint32 indexMaxKeys`.
    pub const INDEX_MAX_KEYS: usize = 236;
    /// `uint32 toast_max_chunk_size`.
    pub const TOAST_MAX_CHUNK_SIZE: usize = 240;
    /// `uint32 loblksize`.
    pub const LOBLKSIZE: usize = 244;
    /// `bool float8ByVal`.
    pub const FLOAT8_BY_VAL: usize = 248;
    /// `uint32 data_checksum_version`.
    pub const DATA_CHECKSUM_VERSION: usize = 252;
    /// `bool default_char_signedness`.
    pub const DEFAULT_CHAR_SIGNEDNESS: usize = 256;
    /// `char mock_authentication_nonce[MOCK_AUTH_NONCE_LEN]`.
    pub const MOCK_AUTHENTICATION_NONCE: usize = 257;
    /// `pg_crc32c crc` — `offsetof(ControlFileData, crc)`, which is both where
    /// the checksum lives and how many bytes it is taken over.
    pub const CRC: usize = 292;
}

/// The interior padding runs, as `(offset, length)`.
///
/// The C compiler inserts these to align the field that follows; `memset`
/// inside `InitControlFile` (`xlog.c:4215`) zeroes them, so on a cluster's own
/// `pg_control` they are zero. They belong to no field, which is why they are
/// listed here rather than inferred: together with [`offset`] they account for
/// every byte the CRC is taken over.
pub const PADDING: [(usize, usize); 10] = [
    (20, 4),  // after `state`, aligning `time`
    (57, 3),  // after `checkPointCopy.fullPageWrites`, aligning `wal_level`
    (100, 4), // after `checkPointCopy.oldestMultiDB`, aligning `time`
    (124, 4), // tail of `CheckPoint`, rounding it up to 88 bytes
    (148, 4), // after `minRecoveryPointTLI`, aligning `backupStartPoint`
    (169, 3), // after `backupEndRequired`, aligning `wal_level`
    (177, 3), // after `wal_log_hints`, aligning `MaxConnections`
    (201, 3), // after `track_commit_timestamp`, aligning `maxAlign`
    (249, 3), // after `float8ByVal`, aligning `data_checksum_version`
    (289, 3), // after the nonce, aligning `crc`
];

/// `DBState` (`src/include/catalog/pg_control.h:89`).
///
/// `Unrecognized` keeps a value upstream would not write, so parsing and
/// writing an image back stays byte-identical whatever is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbState {
    /// `DB_STARTUP`.
    Startup,
    /// `DB_SHUTDOWNED` — what `InitControlFile` sets (`xlog.c:4219`).
    Shutdowned,
    /// `DB_SHUTDOWNED_IN_RECOVERY`.
    ShutdownedInRecovery,
    /// `DB_SHUTDOWNING`.
    Shutdowning,
    /// `DB_IN_CRASH_RECOVERY`.
    InCrashRecovery,
    /// `DB_IN_ARCHIVE_RECOVERY`.
    InArchiveRecovery,
    /// `DB_IN_PRODUCTION`.
    InProduction,
    /// A value outside the enum, kept as it was read.
    Unrecognized(u32),
}

impl DbState {
    /// The `DBState` enumerator for `value`.
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Startup,
            1 => Self::Shutdowned,
            2 => Self::ShutdownedInRecovery,
            3 => Self::Shutdowning,
            4 => Self::InCrashRecovery,
            5 => Self::InArchiveRecovery,
            6 => Self::InProduction,
            other => Self::Unrecognized(other),
        }
    }

    /// The `int` C stores for this enumerator.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Startup => 0,
            Self::Shutdowned => 1,
            Self::ShutdownedInRecovery => 2,
            Self::Shutdowning => 3,
            Self::InCrashRecovery => 4,
            Self::InArchiveRecovery => 5,
            Self::InProduction => 6,
            Self::Unrecognized(other) => other,
        }
    }
}

/// Whether a cluster's data pages carry checksums, and which version.
///
/// `initdb.c:167` starts `data_checksums` at `true`, so PostgreSQL 18 writes
/// checksummed clusters unless `--no-data-checksums` says otherwise; the
/// version number itself is `PG_DATA_CHECKSUM_VERSION`
/// (`src/include/storage/bufpage.h:208`), and zero means off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataChecksums {
    /// `--no-data-checksums`: `data_checksum_version` is 0.
    Disabled,
    /// `-k` / `--data-checksums`, and `initdb.c:167`'s default.
    #[default]
    Enabled,
}

impl DataChecksums {
    /// The `data_checksum_version` this setting writes into `pg_control`.
    #[must_use]
    pub const fn version(self) -> u32 {
        match self {
            Self::Disabled => 0,
            Self::Enabled => PG_DATA_CHECKSUM_VERSION,
        }
    }

    /// What a `data_checksum_version` read back out of a file means.
    ///
    /// `DataChecksumsEnabled()` (`xlog.c:4614`) is `> 0`, so any nonzero
    /// version is on.
    #[must_use]
    pub const fn from_version(version: u32) -> Self {
        if version == 0 {
            Self::Disabled
        } else {
            Self::Enabled
        }
    }
}

/// One appearance of a checksum switch on the command line.
///
/// `initdb.c` keeps a single `data_checksums` variable and assigns to it from
/// two getopt arms — `'k'` at `:3311` and `--no-data-checksums` at `:3393` — so
/// whichever switch came *last* is the one that decides. Resolving them
/// therefore needs the order they were given in, not just which were given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumSwitch {
    /// `-k` / `--data-checksums` (`initdb.c:3311`).
    DataChecksums,
    /// `--no-data-checksums` (`initdb.c:3393`).
    NoDataChecksums,
}

impl DataChecksums {
    /// Apply the checksum switches in command-line order, last one winning.
    ///
    /// With none given the answer is `initdb.c:167`'s initial value.
    #[must_use]
    pub fn resolve(switches: impl IntoIterator<Item = ChecksumSwitch>) -> Self {
        switches
            .into_iter()
            .fold(Self::default(), |_, switch| match switch {
                ChecksumSwitch::DataChecksums => Self::Enabled,
                ChecksumSwitch::NoDataChecksums => Self::Disabled,
            })
    }
}

/// A cluster's unique system identifier (`ControlFileData.system_identifier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SystemIdentifier(u64);

impl SystemIdentifier {
    /// `BootStrapXLOG`'s derivation (`xlog.c:5099`-`:5101`):
    ///
    /// ```c
    /// sysidentifier = ((uint64) tv.tv_sec) << 32;
    /// sysidentifier |= ((uint64) tv.tv_usec) << 12;
    /// sysidentifier |= getpid() & 0xFFF;
    /// ```
    ///
    /// The upper half is the second, the lower half the microsecond (which
    /// upstream's comment notes must fit in 20 bits) shifted clear of the low
    /// 12 bits of the process id.
    #[must_use]
    // `tv_sec` and `tv_usec` are the members of `struct timeval` upstream reads;
    // renaming them to please `clippy::similar_names` would lose that.
    #[allow(clippy::similar_names)]
    pub const fn from_boot_parts(tv_sec: u64, tv_usec: u64, pid: u32) -> Self {
        Self((tv_sec << 32) | (tv_usec << 12) | (pid as u64 & 0xFFF))
    }

    /// The identifier an image already carries.
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    /// The same derivation from a `SystemTime`'s distance from the Unix epoch,
    /// which is what `gettimeofday` reports (`xlog.c:5098`).
    #[must_use]
    pub const fn from_unix_time(since_epoch: Duration, pid: u32) -> Self {
        Self::from_boot_parts(
            since_epoch.as_secs(),
            since_epoch.subsec_micros() as u64,
            pid,
        )
    }

    /// The `uint64` written to `pg_control`.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The last identifier [`SystemIdentifier::generate`] handed out, so a second
/// cluster made in the same microsecond cannot be given the same one.
static LAST_GENERATED: AtomicU64 = AtomicU64::new(0);

impl SystemIdentifier {
    /// Action: the identifier `BootStrapXLOG` would derive right now.
    ///
    /// `gettimeofday` and `getpid` become `SystemTime::now()` and
    /// `std::process::id()`; the value is `xlog.c:5099`-`:5101`'s exactly,
    /// with one addition.
    ///
    /// **Divergence.** Upstream runs `BootStrapXLOG` once per `initdb`
    /// process, so it never has to ask what two calls a microsecond apart
    /// would produce — they would produce the same identifier, since the
    /// second and the microsecond are the only varying parts and the pid is
    /// fixed within a process. ADR-0002 makes a cluster an unpack rather than
    /// a fork-and-exec, so one process can expand several in far less than a
    /// microsecond, and the issue's own acceptance criterion is that two
    /// expansions never share an identifier. Each result is therefore forced
    /// past the previous one: when the clock has moved on, that is upstream's
    /// value unchanged; when it has not, it is the previous value plus one,
    /// which spends the low bits `xlog.c:5094` calls "a little extra
    /// uniqueness" and leaves the second and microsecond — the part upstream
    /// documents as readable — intact.
    #[must_use]
    pub fn generate() -> Self {
        let pid = std::process::id();
        loop {
            let since_epoch = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO);
            let candidate = Self::from_unix_time(since_epoch, pid).get();
            let previous = LAST_GENERATED.load(Ordering::Relaxed);
            let next = candidate.max(previous.saturating_add(1));
            if LAST_GENERATED
                .compare_exchange(previous, next, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return Self(next);
            }
        }
    }
}

/// A `CheckPoint` (`src/include/catalog/pg_control.h:35`), the copy of the last
/// checkpoint record `pg_control` keeps for disaster recovery.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheckPoint {
    /// `XLogRecPtr redo`.
    pub redo: u64,
    /// `TimeLineID ThisTimeLineID`.
    pub this_time_line_id: u32,
    /// `TimeLineID PrevTimeLineID`.
    pub prev_time_line_id: u32,
    /// `bool fullPageWrites`.
    pub full_page_writes: bool,
    /// `int wal_level`.
    pub wal_level: i32,
    /// `FullTransactionId nextXid`.
    pub next_xid: u64,
    /// `Oid nextOid`.
    pub next_oid: u32,
    /// `MultiXactId nextMulti`.
    pub next_multi: u32,
    /// `MultiXactOffset nextMultiOffset`.
    pub next_multi_offset: u32,
    /// `TransactionId oldestXid`.
    pub oldest_xid: u32,
    /// `Oid oldestXidDB`.
    pub oldest_xid_db: u32,
    /// `MultiXactId oldestMulti`.
    pub oldest_multi: u32,
    /// `Oid oldestMultiDB`.
    pub oldest_multi_db: u32,
    /// `pg_time_t time`.
    pub time: i64,
    /// `TransactionId oldestCommitTsXid`.
    pub oldest_commit_ts_xid: u32,
    /// `TransactionId newestCommitTsXid`.
    pub newest_commit_ts_xid: u32,
    /// `TransactionId oldestActiveXid`.
    pub oldest_active_xid: u32,
}

/// `ControlFileData` (`src/include/catalog/pg_control.h:104`), field for field.
///
/// `crc` is the value that was in the file; [`ControlFile::to_bytes`] always
/// recomputes it, exactly as `WriteControlFile` does, so the field is what
/// [`ControlFile::crc_is_valid`] compares against and never an input to a
/// write.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // The C struct has five `bool` fields.
pub struct ControlFile {
    /// `uint64 system_identifier`.
    pub system_identifier: SystemIdentifier,
    /// `uint32 pg_control_version`.
    pub pg_control_version: u32,
    /// `uint32 catalog_version_no`.
    pub catalog_version_no: u32,
    /// `DBState state`.
    pub state: DbState,
    /// `pg_time_t time`.
    pub time: i64,
    /// `XLogRecPtr checkPoint`.
    pub check_point: u64,
    /// `CheckPoint checkPointCopy`.
    pub check_point_copy: CheckPoint,
    /// `XLogRecPtr unloggedLSN`.
    pub unlogged_lsn: u64,
    /// `XLogRecPtr minRecoveryPoint`.
    pub min_recovery_point: u64,
    /// `TimeLineID minRecoveryPointTLI`.
    pub min_recovery_point_tli: u32,
    /// `XLogRecPtr backupStartPoint`.
    pub backup_start_point: u64,
    /// `XLogRecPtr backupEndPoint`.
    pub backup_end_point: u64,
    /// `bool backupEndRequired`.
    pub backup_end_required: bool,
    /// `int wal_level`.
    pub wal_level: i32,
    /// `bool wal_log_hints`.
    pub wal_log_hints: bool,
    /// `int MaxConnections`.
    pub max_connections: i32,
    /// `int max_worker_processes`.
    pub max_worker_processes: i32,
    /// `int max_wal_senders`.
    pub max_wal_senders: i32,
    /// `int max_prepared_xacts`.
    pub max_prepared_xacts: i32,
    /// `int max_locks_per_xact`.
    pub max_locks_per_xact: i32,
    /// `bool track_commit_timestamp`.
    pub track_commit_timestamp: bool,
    /// `uint32 maxAlign`.
    pub max_align: u32,
    /// `double floatFormat`.
    pub float_format: f64,
    /// `uint32 blcksz`.
    pub blcksz: u32,
    /// `uint32 relseg_size`.
    pub relseg_size: u32,
    /// `uint32 xlog_blcksz`.
    pub xlog_blcksz: u32,
    /// `uint32 xlog_seg_size`.
    pub xlog_seg_size: u32,
    /// `uint32 nameDataLen`.
    pub name_data_len: u32,
    /// `uint32 indexMaxKeys`.
    pub index_max_keys: u32,
    /// `uint32 toast_max_chunk_size`.
    pub toast_max_chunk_size: u32,
    /// `uint32 loblksize`.
    pub loblksize: u32,
    /// `bool float8ByVal`.
    pub float8_by_val: bool,
    /// `uint32 data_checksum_version`.
    pub data_checksum_version: u32,
    /// `bool default_char_signedness`.
    pub default_char_signedness: bool,
    /// `char mock_authentication_nonce[MOCK_AUTH_NONCE_LEN]`.
    pub mock_authentication_nonce: [u8; MOCK_AUTH_NONCE_LEN],
    /// `pg_crc32c crc`, as read. A write always recomputes it.
    pub crc: u32,
}

/// Why a `pg_control` image could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ControlFileError {
    /// Fewer than `sizeof(ControlFileData)` bytes.
    ///
    /// The shape of `get_controlfile_by_exact_path`'s short read
    /// (`src/common/controldata_utils.c:119`); the message it prints names the
    /// path, which is the caller's to add.
    #[error("read {found} of {SIZEOF_CONTROL_FILE_DATA}")]
    ShortRead {
        /// How many bytes the image actually held.
        found: usize,
    },
}

// Readers. `pg_control` is raw struct memory, so every scalar is in the
// machine's own byte order — upstream relies on exactly that
// (`pg_control.h:189`: a file from a different-endian machine is caught by
// `pg_control_version` looking wrong, not by an endianness field).
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

// A C `int` / `int64` is the same four or eight bytes whichever way it is read.
#[allow(clippy::cast_possible_wrap)]
fn i32_at(image: &[u8], at: usize) -> i32 {
    u32_at(image, at) as i32
}

#[allow(clippy::cast_possible_wrap)]
fn i64_at(image: &[u8], at: usize) -> i64 {
    u64_at(image, at) as i64
}

fn f64_at(image: &[u8], at: usize) -> f64 {
    f64::from_bits(u64_at(image, at))
}

/// A C `bool` is one byte; `stdbool.h` writes 0 or 1 and every reader treats
/// nonzero as true.
fn bool_at(image: &[u8], at: usize) -> bool {
    image[at] != 0
}

fn put_u32(image: &mut [u8], at: usize, value: u32) {
    image[at..at + 4].copy_from_slice(&value.to_ne_bytes());
}

fn put_u64(image: &mut [u8], at: usize, value: u64) {
    image[at..at + 8].copy_from_slice(&value.to_ne_bytes());
}

#[allow(clippy::cast_sign_loss)] // Same four bytes; the sign is the reader's business.
fn put_i32(image: &mut [u8], at: usize, value: i32) {
    put_u32(image, at, value as u32);
}

#[allow(clippy::cast_sign_loss)] // Same eight bytes.
fn put_i64(image: &mut [u8], at: usize, value: i64) {
    put_u64(image, at, value as u64);
}

fn put_f64(image: &mut [u8], at: usize, value: f64) {
    put_u64(image, at, value.to_bits());
}

fn put_bool(image: &mut [u8], at: usize, value: bool) {
    image[at] = u8::from(value);
}

/// Write the embedded `checkPointCopy` group at its absolute offsets.
fn put_check_point(image: &mut [u8], cp: &CheckPoint) {
    put_u64(image, offset::CP_REDO, cp.redo);
    put_u32(image, offset::CP_THIS_TIME_LINE_ID, cp.this_time_line_id);
    put_u32(image, offset::CP_PREV_TIME_LINE_ID, cp.prev_time_line_id);
    put_bool(image, offset::CP_FULL_PAGE_WRITES, cp.full_page_writes);
    put_i32(image, offset::CP_WAL_LEVEL, cp.wal_level);
    put_u64(image, offset::CP_NEXT_XID, cp.next_xid);
    put_u32(image, offset::CP_NEXT_OID, cp.next_oid);
    put_u32(image, offset::CP_NEXT_MULTI, cp.next_multi);
    put_u32(image, offset::CP_NEXT_MULTI_OFFSET, cp.next_multi_offset);
    put_u32(image, offset::CP_OLDEST_XID, cp.oldest_xid);
    put_u32(image, offset::CP_OLDEST_XID_DB, cp.oldest_xid_db);
    put_u32(image, offset::CP_OLDEST_MULTI, cp.oldest_multi);
    put_u32(image, offset::CP_OLDEST_MULTI_DB, cp.oldest_multi_db);
    put_i64(image, offset::CP_TIME, cp.time);
    put_u32(
        image,
        offset::CP_OLDEST_COMMIT_TS_XID,
        cp.oldest_commit_ts_xid,
    );
    put_u32(
        image,
        offset::CP_NEWEST_COMMIT_TS_XID,
        cp.newest_commit_ts_xid,
    );
    put_u32(image, offset::CP_OLDEST_ACTIVE_XID, cp.oldest_active_xid);
}

impl ControlFile {
    /// Read a `pg_control` image.
    ///
    /// `get_controlfile_by_exact_path` (`src/common/controldata_utils.c:101`)
    /// reads `sizeof(ControlFileData)` bytes and ignores the rest of the file,
    /// so anything past [`SIZEOF_CONTROL_FILE_DATA`] is ignored here too. The
    /// CRC is *checked*, never enforced: upstream hands the verdict back
    /// through `crc_ok_p` (`controldata_utils.c:142`) and lets the caller
    /// decide, which is [`ControlFile::crc_is_valid`] here.
    ///
    /// # Errors
    ///
    /// [`ControlFileError::ShortRead`] when the image is smaller than the
    /// struct.
    pub fn parse(image: &[u8]) -> Result<Self, ControlFileError> {
        if image.len() < SIZEOF_CONTROL_FILE_DATA {
            return Err(ControlFileError::ShortRead { found: image.len() });
        }
        let mut nonce = [0u8; MOCK_AUTH_NONCE_LEN];
        nonce.copy_from_slice(
            &image[offset::MOCK_AUTHENTICATION_NONCE
                ..offset::MOCK_AUTHENTICATION_NONCE + MOCK_AUTH_NONCE_LEN],
        );
        Ok(Self {
            system_identifier: SystemIdentifier::from_raw(u64_at(image, offset::SYSTEM_IDENTIFIER)),
            pg_control_version: u32_at(image, offset::PG_CONTROL_VERSION),
            catalog_version_no: u32_at(image, offset::CATALOG_VERSION_NO),
            state: DbState::from_u32(u32_at(image, offset::STATE)),
            time: i64_at(image, offset::TIME),
            check_point: u64_at(image, offset::CHECK_POINT),
            check_point_copy: CheckPoint {
                redo: u64_at(image, offset::CP_REDO),
                this_time_line_id: u32_at(image, offset::CP_THIS_TIME_LINE_ID),
                prev_time_line_id: u32_at(image, offset::CP_PREV_TIME_LINE_ID),
                full_page_writes: bool_at(image, offset::CP_FULL_PAGE_WRITES),
                wal_level: i32_at(image, offset::CP_WAL_LEVEL),
                next_xid: u64_at(image, offset::CP_NEXT_XID),
                next_oid: u32_at(image, offset::CP_NEXT_OID),
                next_multi: u32_at(image, offset::CP_NEXT_MULTI),
                next_multi_offset: u32_at(image, offset::CP_NEXT_MULTI_OFFSET),
                oldest_xid: u32_at(image, offset::CP_OLDEST_XID),
                oldest_xid_db: u32_at(image, offset::CP_OLDEST_XID_DB),
                oldest_multi: u32_at(image, offset::CP_OLDEST_MULTI),
                oldest_multi_db: u32_at(image, offset::CP_OLDEST_MULTI_DB),
                time: i64_at(image, offset::CP_TIME),
                oldest_commit_ts_xid: u32_at(image, offset::CP_OLDEST_COMMIT_TS_XID),
                newest_commit_ts_xid: u32_at(image, offset::CP_NEWEST_COMMIT_TS_XID),
                oldest_active_xid: u32_at(image, offset::CP_OLDEST_ACTIVE_XID),
            },
            unlogged_lsn: u64_at(image, offset::UNLOGGED_LSN),
            min_recovery_point: u64_at(image, offset::MIN_RECOVERY_POINT),
            min_recovery_point_tli: u32_at(image, offset::MIN_RECOVERY_POINT_TLI),
            backup_start_point: u64_at(image, offset::BACKUP_START_POINT),
            backup_end_point: u64_at(image, offset::BACKUP_END_POINT),
            backup_end_required: bool_at(image, offset::BACKUP_END_REQUIRED),
            wal_level: i32_at(image, offset::WAL_LEVEL),
            wal_log_hints: bool_at(image, offset::WAL_LOG_HINTS),
            max_connections: i32_at(image, offset::MAX_CONNECTIONS),
            max_worker_processes: i32_at(image, offset::MAX_WORKER_PROCESSES),
            max_wal_senders: i32_at(image, offset::MAX_WAL_SENDERS),
            max_prepared_xacts: i32_at(image, offset::MAX_PREPARED_XACTS),
            max_locks_per_xact: i32_at(image, offset::MAX_LOCKS_PER_XACT),
            track_commit_timestamp: bool_at(image, offset::TRACK_COMMIT_TIMESTAMP),
            max_align: u32_at(image, offset::MAX_ALIGN),
            float_format: f64_at(image, offset::FLOAT_FORMAT),
            blcksz: u32_at(image, offset::BLCKSZ),
            relseg_size: u32_at(image, offset::RELSEG_SIZE),
            xlog_blcksz: u32_at(image, offset::XLOG_BLCKSZ),
            xlog_seg_size: u32_at(image, offset::XLOG_SEG_SIZE),
            name_data_len: u32_at(image, offset::NAME_DATA_LEN),
            index_max_keys: u32_at(image, offset::INDEX_MAX_KEYS),
            toast_max_chunk_size: u32_at(image, offset::TOAST_MAX_CHUNK_SIZE),
            loblksize: u32_at(image, offset::LOBLKSIZE),
            float8_by_val: bool_at(image, offset::FLOAT8_BY_VAL),
            data_checksum_version: u32_at(image, offset::DATA_CHECKSUM_VERSION),
            default_char_signedness: bool_at(image, offset::DEFAULT_CHAR_SIGNEDNESS),
            mock_authentication_nonce: nonce,
            crc: u32_at(image, offset::CRC),
        })
    }

    /// The `PG_CONTROL_FILE_SIZE` bytes `WriteControlFile` would write.
    ///
    /// `xlog.c:4303` zeroes the whole buffer and copies the struct into its
    /// front, so every byte past `sizeof(ControlFileData)` — and every interior
    /// padding byte, which `InitControlFile`'s `memset` already zeroed — is
    /// zero. The CRC over `offsetof(ControlFileData, crc)` bytes is recomputed
    /// here just as it is at `xlog.c:4290`.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; PG_CONTROL_FILE_SIZE] {
        let mut image = [0u8; PG_CONTROL_FILE_SIZE];
        put_u64(
            &mut image,
            offset::SYSTEM_IDENTIFIER,
            self.system_identifier.get(),
        );
        put_u32(
            &mut image,
            offset::PG_CONTROL_VERSION,
            self.pg_control_version,
        );
        put_u32(
            &mut image,
            offset::CATALOG_VERSION_NO,
            self.catalog_version_no,
        );
        put_u32(&mut image, offset::STATE, self.state.as_u32());
        put_i64(&mut image, offset::TIME, self.time);
        put_u64(&mut image, offset::CHECK_POINT, self.check_point);

        put_check_point(&mut image, &self.check_point_copy);

        put_u64(&mut image, offset::UNLOGGED_LSN, self.unlogged_lsn);
        put_u64(
            &mut image,
            offset::MIN_RECOVERY_POINT,
            self.min_recovery_point,
        );
        put_u32(
            &mut image,
            offset::MIN_RECOVERY_POINT_TLI,
            self.min_recovery_point_tli,
        );
        put_u64(
            &mut image,
            offset::BACKUP_START_POINT,
            self.backup_start_point,
        );
        put_u64(&mut image, offset::BACKUP_END_POINT, self.backup_end_point);
        put_bool(
            &mut image,
            offset::BACKUP_END_REQUIRED,
            self.backup_end_required,
        );
        put_i32(&mut image, offset::WAL_LEVEL, self.wal_level);
        put_bool(&mut image, offset::WAL_LOG_HINTS, self.wal_log_hints);
        put_i32(&mut image, offset::MAX_CONNECTIONS, self.max_connections);
        put_i32(
            &mut image,
            offset::MAX_WORKER_PROCESSES,
            self.max_worker_processes,
        );
        put_i32(&mut image, offset::MAX_WAL_SENDERS, self.max_wal_senders);
        put_i32(
            &mut image,
            offset::MAX_PREPARED_XACTS,
            self.max_prepared_xacts,
        );
        put_i32(
            &mut image,
            offset::MAX_LOCKS_PER_XACT,
            self.max_locks_per_xact,
        );
        put_bool(
            &mut image,
            offset::TRACK_COMMIT_TIMESTAMP,
            self.track_commit_timestamp,
        );
        put_u32(&mut image, offset::MAX_ALIGN, self.max_align);
        put_f64(&mut image, offset::FLOAT_FORMAT, self.float_format);
        put_u32(&mut image, offset::BLCKSZ, self.blcksz);
        put_u32(&mut image, offset::RELSEG_SIZE, self.relseg_size);
        put_u32(&mut image, offset::XLOG_BLCKSZ, self.xlog_blcksz);
        put_u32(&mut image, offset::XLOG_SEG_SIZE, self.xlog_seg_size);
        put_u32(&mut image, offset::NAME_DATA_LEN, self.name_data_len);
        put_u32(&mut image, offset::INDEX_MAX_KEYS, self.index_max_keys);
        put_u32(
            &mut image,
            offset::TOAST_MAX_CHUNK_SIZE,
            self.toast_max_chunk_size,
        );
        put_u32(&mut image, offset::LOBLKSIZE, self.loblksize);
        put_bool(&mut image, offset::FLOAT8_BY_VAL, self.float8_by_val);
        put_u32(
            &mut image,
            offset::DATA_CHECKSUM_VERSION,
            self.data_checksum_version,
        );
        put_bool(
            &mut image,
            offset::DEFAULT_CHAR_SIGNEDNESS,
            self.default_char_signedness,
        );
        image[offset::MOCK_AUTHENTICATION_NONCE
            ..offset::MOCK_AUTHENTICATION_NONCE + MOCK_AUTH_NONCE_LEN]
            .copy_from_slice(&self.mock_authentication_nonce);

        let crc = crc32c::crc32c(&image[..offset::CRC]);
        put_u32(&mut image, offset::CRC, crc);
        image
    }

    /// `*crc_ok_p` (`src/common/controldata_utils.c:142`): does the checksum in
    /// the file match the one over its contents?
    #[must_use]
    pub fn crc_is_valid(&self) -> bool {
        self.crc == crc32c::crc32c(&self.to_bytes()[..offset::CRC])
    }

    /// `DataChecksumsEnabled()` (`xlog.c:4611`).
    #[must_use]
    pub const fn data_checksums(&self) -> DataChecksums {
        DataChecksums::from_version(self.data_checksum_version)
    }
}

/// Make a template cluster's `pg_control` this cluster's.
///
/// ADR-0002 expands one pre-minted image per cluster, so the two fields
/// `InitControlFile` derives per cluster — `system_identifier` (`xlog.c:4217`)
/// and `data_checksum_version` (`xlog.c:4231`) — have to be replaced
/// afterwards, and the CRC `WriteControlFile` takes over the result
/// (`xlog.c:4290`) recomputed. Everything else in the template is the C
/// initdb's and stays untouched.
///
/// # Errors
///
/// [`ControlFileError::ShortRead`] when `image` is smaller than the struct.
pub fn rewrite(
    image: &[u8],
    system_identifier: SystemIdentifier,
    checksums: DataChecksums,
) -> Result<[u8; PG_CONTROL_FILE_SIZE], ControlFileError> {
    let mut control = ControlFile::parse(image)?;
    control.system_identifier = system_identifier;
    control.data_checksum_version = checksums.version();
    Ok(control.to_bytes())
}

/// `sizeof(CheckPoint)` on a 64-bit build (`pg_control.h:35`): the span of
/// `checkPointCopy` inside `ControlFileData`, tail padding included.
pub const SIZEOF_CHECK_POINT: usize = offset::UNLOGGED_LSN - offset::CP_REDO;

impl CheckPoint {
    /// The struct as raw memory: the bytes `WriteEmptyXLOG` `memcpy`s into
    /// its checkpoint record (`src/bin/pg_resetwal/pg_resetwal.c:1154`).
    #[must_use]
    pub fn to_bytes(&self) -> [u8; SIZEOF_CHECK_POINT] {
        let mut image = [0u8; offset::UNLOGGED_LSN];
        put_check_point(&mut image, self);
        let mut bytes = [0u8; SIZEOF_CHECK_POINT];
        bytes.copy_from_slice(&image[offset::CP_REDO..]);
        bytes
    }
}

/// `XLByteToSeg` (`src/include/access/xlog_internal.h:117`): the segment an
/// LSN falls in.
#[must_use]
pub const fn segment_of(lsn: u64, wal_segsz_bytes: u32) -> u64 {
    lsn / wal_segsz_bytes as u64
}

/// `XLogSegNoOffsetToRecPtr` (`src/include/access/xlog_internal.h:103`).
#[must_use]
pub const fn segment_offset_to_lsn(segno: u64, offset: u64, wal_segsz_bytes: u32) -> u64 {
    segno * wal_segsz_bytes as u64 + offset
}

/// `SizeOfXLogLongPHD` (`src/include/access/xlog_internal.h:69`): the long
/// page header that opens a segment, and so the offset of its first record.
pub const SIZE_OF_XLOG_LONG_PHD: u64 = 40;

impl ControlFile {
    /// Pure: this `pg_control` with the fields no two clusters share zeroed —
    /// the system identifier, both timestamps and the mock authentication
    /// nonce — and everything else kept.
    ///
    /// This is the form the template's `pg_control` is committed in
    /// (`crates/rinitdb/image/template.control`). What it zeroes is either a
    /// fact about one cluster (`InitControlFile`, `xlog.c:4217`, `:4218`) or
    /// a wall-clock time (`update_controlfile`,
    /// `src/common/controldata_utils.c:197`, and the checkpoint's own), so
    /// two mints of the same catalogs agree on what is left, and
    /// [`for_new_cluster`] sets every field it zeroed.
    #[must_use]
    pub fn as_template(&self) -> Self {
        let mut template = *self;
        template.system_identifier = SystemIdentifier::from_raw(0);
        template.time = 0;
        template.check_point_copy.time = 0;
        template.mock_authentication_nonce = [0; MOCK_AUTH_NONCE_LEN];
        template
    }
}

/// What a new cluster's `pg_control` gets that its template cannot carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewCluster {
    /// `InitControlFile`'s `sysidentifier` (`xlog.c:4217`).
    pub system_identifier: SystemIdentifier,
    /// `-k` / `--no-data-checksums` (`xlog.c:4231`).
    pub checksums: DataChecksums,
    /// `InitControlFile`'s `pg_strong_random` nonce (`xlog.c:4210`).
    pub mock_authentication_nonce: [u8; MOCK_AUTH_NONCE_LEN],
    /// `time(NULL)`, for the file and for its checkpoint.
    pub now: i64,
}

/// Pure: the new cluster's `pg_control`, from the template's.
///
/// The template's `pg_wal` is not shipped (`crate::image::STRIPPED_FILES`),
/// so its checkpoint record is gone. The cluster gets a new one, alone in a
/// new first segment, the way `pg_resetwal` gives a cluster one:
///
/// - `FindEndOfXLOG` (`src/bin/pg_resetwal/pg_resetwal.c:940`) starts from
///   the segment of the redo pointer and, with no segment file left to push
///   it further, advances by exactly one. So every page LSN in the expanded
///   catalogs is behind the new checkpoint.
/// - `RewriteControlFile` (`:894`) puts redo and the checkpoint just past
///   that segment's long page header, stamps the checkpoint time, marks the
///   cluster shut down and clears the recovery and backup fields.
///
/// `RewriteControlFile` also forces `wal_level` and the `max_*` settings to
/// their defaults (`:917`), because it cannot know which server wrote the
/// file. This template's values are the ones C initdb's own server wrote
/// under the settings `postgresql.conf` is rendered with, so they are kept.
///
/// Then the fields `InitControlFile` derives per cluster (`xlog.c:4217`,
/// `:4218`, `:4231`) and `update_controlfile`'s timestamp
/// (`src/common/controldata_utils.c:197`) are set. [`ControlFile::to_bytes`]
/// takes the CRC.
#[must_use]
pub fn for_new_cluster(template: &ControlFile, new: &NewCluster) -> ControlFile {
    let mut control = *template;
    let seg_size = control.xlog_seg_size;
    let segno = segment_of(control.check_point_copy.redo, seg_size) + 1;
    let redo = segment_offset_to_lsn(segno, SIZE_OF_XLOG_LONG_PHD, seg_size);

    control.check_point_copy.redo = redo;
    control.check_point_copy.time = new.now;
    control.state = DbState::Shutdowned;
    control.check_point = redo;
    control.min_recovery_point = 0;
    control.min_recovery_point_tli = 0;
    control.backup_start_point = 0;
    control.backup_end_point = 0;
    control.backup_end_required = false;

    control.system_identifier = new.system_identifier;
    control.mock_authentication_nonce = new.mock_authentication_nonce;
    control.data_checksum_version = new.checksums.version();
    control.time = new.now;
    control
}

/// Action: `pg_strong_random` for the mock authentication nonce, in the
/// variant upstream builds without OpenSSL: read `/dev/urandom`
/// (`src/port/pg_strong_random.c:150`).
///
/// # Errors
/// The `open` or `read` failure. Upstream's caller turns one into `could not
/// generate secret authorization token` (`xlog.c:4213`).
pub fn strong_random_nonce() -> std::io::Result<[u8; MOCK_AUTH_NONCE_LEN]> {
    use std::io::Read as _;
    let mut nonce = [0u8; MOCK_AUTH_NONCE_LEN];
    // `read_exact` retries `EINTR` and short reads, as the C loop does.
    std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    Ok(nonce)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field as `(offset, width)`, in declaration order.
    ///
    /// Together with [`PADDING`] this must tile the struct; a field left out,
    /// or given the wrong width, shows up as a gap or an overlap.
    const FIELDS: [(usize, usize); 47] = [
        (offset::SYSTEM_IDENTIFIER, 8),
        (offset::PG_CONTROL_VERSION, 4),
        (offset::CATALOG_VERSION_NO, 4),
        (offset::STATE, 4),
        (offset::TIME, 8),
        (offset::CHECK_POINT, 8),
        (offset::CP_REDO, 8),
        (offset::CP_THIS_TIME_LINE_ID, 4),
        (offset::CP_PREV_TIME_LINE_ID, 4),
        (offset::CP_FULL_PAGE_WRITES, 1),
        (offset::CP_WAL_LEVEL, 4),
        (offset::CP_NEXT_XID, 8),
        (offset::CP_NEXT_OID, 4),
        (offset::CP_NEXT_MULTI, 4),
        (offset::CP_NEXT_MULTI_OFFSET, 4),
        (offset::CP_OLDEST_XID, 4),
        (offset::CP_OLDEST_XID_DB, 4),
        (offset::CP_OLDEST_MULTI, 4),
        (offset::CP_OLDEST_MULTI_DB, 4),
        (offset::CP_TIME, 8),
        (offset::CP_OLDEST_COMMIT_TS_XID, 4),
        (offset::CP_NEWEST_COMMIT_TS_XID, 4),
        (offset::CP_OLDEST_ACTIVE_XID, 4),
        (offset::UNLOGGED_LSN, 8),
        (offset::MIN_RECOVERY_POINT, 8),
        (offset::MIN_RECOVERY_POINT_TLI, 4),
        (offset::BACKUP_START_POINT, 8),
        (offset::BACKUP_END_POINT, 8),
        (offset::BACKUP_END_REQUIRED, 1),
        (offset::WAL_LEVEL, 4),
        (offset::WAL_LOG_HINTS, 1),
        (offset::MAX_CONNECTIONS, 4),
        (offset::MAX_WORKER_PROCESSES, 4),
        (offset::MAX_WAL_SENDERS, 4),
        (offset::MAX_PREPARED_XACTS, 4),
        (offset::MAX_LOCKS_PER_XACT, 4),
        (offset::TRACK_COMMIT_TIMESTAMP, 1),
        (offset::MAX_ALIGN, 4),
        (offset::FLOAT_FORMAT, 8),
        (offset::BLCKSZ, 4),
        (offset::RELSEG_SIZE, 4),
        (offset::XLOG_BLCKSZ, 4),
        (offset::XLOG_SEG_SIZE, 4),
        (offset::NAME_DATA_LEN, 4),
        (offset::INDEX_MAX_KEYS, 4),
        (offset::TOAST_MAX_CHUNK_SIZE, 4),
        (offset::LOBLKSIZE, 4),
    ];

    /// The last four fields, kept apart only so `FIELDS` stays a flat list of
    /// the scalars above the nonce.
    const TAIL_FIELDS: [(usize, usize); 4] = [
        (offset::FLOAT8_BY_VAL, 1),
        (offset::DATA_CHECKSUM_VERSION, 4),
        (offset::DEFAULT_CHAR_SIGNEDNESS, 1),
        (offset::MOCK_AUTHENTICATION_NONCE, MOCK_AUTH_NONCE_LEN),
    ];

    /// The byte positions no field owns: the ten padding runs.
    fn padding_positions() -> Vec<usize> {
        PADDING.iter().flat_map(|&(at, len)| at..at + len).collect()
    }

    /// A `pg_control` image with a different value in every field, canonical
    /// `0`/`1` in the C `bool` bytes, and zero in the padding — the shape a
    /// cluster's own file has, since `InitControlFile` memsets the struct.
    ///
    /// The values are a counter, so no two fields can be confused for one
    /// another and a field read from the wrong offset comes out wrong.
    fn a_full_image() -> [u8; PG_CONTROL_FILE_SIZE] {
        let bools = [
            offset::CP_FULL_PAGE_WRITES,
            offset::BACKUP_END_REQUIRED,
            offset::WAL_LOG_HINTS,
            offset::TRACK_COMMIT_TIMESTAMP,
            offset::FLOAT8_BY_VAL,
            offset::DEFAULT_CHAR_SIGNEDNESS,
        ];
        let padding = padding_positions();
        let mut image = [0u8; PG_CONTROL_FILE_SIZE];
        let mut counter: u8 = 1;
        for (at, byte) in image.iter_mut().enumerate().take(offset::CRC) {
            if padding.contains(&at) {
                continue;
            }
            if bools.contains(&at) {
                *byte = 1;
                continue;
            }
            *byte = counter;
            counter = counter.wrapping_add(1).max(1);
        }
        // `floatFormat` must stay a number the round trip can reproduce: an
        // arbitrary bit pattern could be a signalling NaN.
        put_f64(&mut image, offset::FLOAT_FORMAT, FLOATFORMAT_VALUE);
        let crc = crc32c::crc32c(&image[..offset::CRC]);
        put_u32(&mut image, offset::CRC, crc);
        image
    }

    #[test]
    fn the_fields_and_the_padding_tile_the_struct() {
        let mut owner = vec![0u8; SIZEOF_CONTROL_FILE_DATA];
        for (at, len) in FIELDS
            .into_iter()
            .chain(TAIL_FIELDS)
            .chain([(offset::CRC, 4)])
            .chain(PADDING)
        {
            for byte in &mut owner[at..at + len] {
                assert_eq!(*byte, 0, "byte {at}..{} is claimed twice", at + len);
                *byte = 1;
            }
        }
        assert!(
            owner.iter().all(|&claimed| claimed == 1),
            "some byte of ControlFileData belongs to no field and no padding run"
        );
    }

    #[test]
    fn the_struct_fits_in_one_sector_and_in_the_file() {
        // `StaticAssertDecl`, pg_control.h:261 and :263 — static there, so
        // static here too.
        const {
            assert!(SIZEOF_CONTROL_FILE_DATA <= PG_CONTROL_MAX_SAFE_SIZE);
            assert!(SIZEOF_CONTROL_FILE_DATA <= PG_CONTROL_FILE_SIZE);
        }
    }

    /// The acceptance criterion, over an image whose every field differs: parse
    /// then serialize must give back exactly the bytes that went in.
    #[test]
    fn a_round_trip_is_byte_identical() {
        let image = a_full_image();
        let parsed = ControlFile::parse(&image).expect("parse the image");
        assert!(parsed.crc_is_valid());
        assert_eq!(parsed.to_bytes(), image);
    }

    /// Non-vacuity for the test above: moving any single byte of the image must
    /// break the round trip, so no byte is silently unread.
    #[test]
    fn every_byte_of_the_struct_survives_the_round_trip() {
        let image = a_full_image();
        let padding = padding_positions();
        for at in 0..offset::CRC {
            if padding.contains(&at) {
                continue;
            }
            let mut moved = image;
            // Keep the C `bool` bytes canonical: 2 and 1 are both "true", so
            // flipping to 2 would be a difference C cannot see either.
            moved[at] = u8::from(moved[at] == 0);
            let crc = crc32c::crc32c(&moved[..offset::CRC]);
            put_u32(&mut moved, offset::CRC, crc);
            let parsed = ControlFile::parse(&moved).expect("parse the changed image");
            assert_eq!(parsed.to_bytes(), moved, "byte {at} did not survive");
            assert_ne!(
                parsed,
                ControlFile::parse(&image).expect("parse the original"),
                "byte {at} is not read by any field"
            );
        }
    }

    #[test]
    fn the_padding_is_zero_on_a_write() {
        let parsed = ControlFile::parse(&a_full_image()).expect("parse");
        let written = parsed.to_bytes();
        for (at, len) in PADDING {
            assert!(
                written[at..at + len].iter().all(|&byte| byte == 0),
                "padding at {at} is not zero"
            );
        }
        assert!(
            written[SIZEOF_CONTROL_FILE_DATA..].iter().all(|&b| b == 0),
            "the tail past sizeof(ControlFileData) is not zero"
        );
    }

    #[test]
    fn an_image_shorter_than_the_struct_is_a_short_read() {
        let short = [0u8; SIZEOF_CONTROL_FILE_DATA - 1];
        assert_eq!(
            ControlFile::parse(&short),
            Err(ControlFileError::ShortRead {
                found: SIZEOF_CONTROL_FILE_DATA - 1
            })
        );
        assert!(ControlFile::parse(&[0u8; SIZEOF_CONTROL_FILE_DATA]).is_ok());
    }

    #[test]
    fn a_damaged_image_is_read_but_reported() {
        let mut image = a_full_image();
        image[offset::CATALOG_VERSION_NO] ^= 0xFF;
        let parsed = ControlFile::parse(&image).expect("a bad CRC is not a read failure");
        assert!(!parsed.crc_is_valid());
        // And writing it back repairs the checksum, as WriteControlFile does.
        let repaired = ControlFile::parse(&parsed.to_bytes()).expect("parse the rewrite");
        assert!(repaired.crc_is_valid());
    }

    #[test]
    fn a_rewrite_changes_the_two_fields_and_nothing_else() {
        let image = a_full_image();
        let before = ControlFile::parse(&image).expect("parse");
        let sysid = SystemIdentifier::from_boot_parts(1_726_000_000, 123_456, 4242);
        let after = ControlFile::parse(
            &rewrite(&image, sysid, DataChecksums::Disabled).expect("rewrite the image"),
        )
        .expect("parse the rewrite");

        assert_eq!(after.system_identifier, sysid);
        assert_eq!(after.data_checksum_version, 0);
        assert_eq!(after.data_checksums(), DataChecksums::Disabled);
        assert!(after.crc_is_valid());

        // Everything else is the template's.
        let mut expected = before;
        expected.system_identifier = sysid;
        expected.data_checksum_version = 0;
        expected.crc = after.crc;
        assert_eq!(after, expected);
    }

    #[test]
    fn a_rewrite_with_checksums_on_writes_the_checksum_version() {
        let image = a_full_image();
        let rewritten = rewrite(
            &image,
            SystemIdentifier::from_raw(7),
            DataChecksums::Enabled,
        )
        .expect("rewrite");
        let parsed = ControlFile::parse(&rewritten).expect("parse");
        assert_eq!(parsed.data_checksum_version, PG_DATA_CHECKSUM_VERSION);
        assert_eq!(parsed.data_checksum_version, 1);
    }

    /// `xlog.c:5099`-`:5101`, checked field by field against the C expression.
    #[test]
    fn the_system_identifier_is_seconds_microseconds_and_the_low_pid_bits() {
        let sysid = SystemIdentifier::from_boot_parts(0x1234_5678, 999_999, 0x8765);
        assert_eq!(sysid.get() >> 32, 0x1234_5678);
        assert_eq!((sysid.get() >> 12) & 0xF_FFFF, 999_999);
        assert_eq!(sysid.get() & 0xFFF, 0x765);
    }

    /// Upstream's comment at `xlog.c:5093` says the microsecond "must fit in 20
    /// bits"; the largest one does, and it does not reach the seconds half.
    #[test]
    fn the_largest_microsecond_stays_clear_of_the_seconds() {
        let sysid = SystemIdentifier::from_boot_parts(1, 999_999, 0);
        assert_eq!(sysid.get() >> 32, 1);
        // The 20-bit microsecond field holds the largest microsecond intact.
        assert_eq!((sysid.get() >> 12) & 0xF_FFFF, 999_999);
    }

    /// The issue's second acceptance criterion, at the generator: two clusters
    /// expanded back to back must not share an identifier even when the clock
    /// has not ticked between them.
    ///
    /// Ten thousand calls in a tight loop take far less than ten thousand
    /// microseconds, so most of them see the second and the microsecond
    /// `xlog.c:5099`-`:5100` derive from unchanged — which is exactly the case
    /// upstream never meets, and exactly what the forcing in
    /// [`SystemIdentifier::generate`] is for. Without it this repeats.
    #[test]
    fn generate_never_repeats_within_a_process() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is past the epoch")
            .as_secs();

        let mut seen = std::collections::BTreeSet::new();
        let mut previous = 0u64;
        for _ in 0..10_000 {
            let identifier = SystemIdentifier::generate().get();
            assert!(identifier > previous, "identifiers must keep increasing");
            assert!(seen.insert(identifier), "{identifier} was handed out twice");
            previous = identifier;
        }

        // The forcing spends the low bits, never the readable half: the upper
        // 32 are still the second the cluster was made in.
        assert!(
            (identifier_seconds(previous)).abs_diff(now) <= 1,
            "the seconds half drifted away from the clock"
        );
    }

    fn identifier_seconds(identifier: u64) -> u64 {
        identifier >> 32
    }

    #[test]
    fn checksums_default_to_on_and_the_last_switch_wins() {
        use ChecksumSwitch::{DataChecksums as K, NoDataChecksums as NoK};
        // initdb.c:167 — `static bool data_checksums = true;`
        assert_eq!(DataChecksums::resolve([]), DataChecksums::Enabled);
        assert_eq!(DataChecksums::resolve([NoK]), DataChecksums::Disabled);
        assert_eq!(DataChecksums::resolve([K]), DataChecksums::Enabled);
        assert_eq!(DataChecksums::resolve([K, NoK]), DataChecksums::Disabled);
        assert_eq!(DataChecksums::resolve([NoK, K]), DataChecksums::Enabled);
    }

    #[test]
    fn a_nonzero_version_is_checksums_on() {
        assert_eq!(DataChecksums::from_version(0), DataChecksums::Disabled);
        assert_eq!(DataChecksums::from_version(1), DataChecksums::Enabled);
        assert_eq!(DataChecksums::from_version(99), DataChecksums::Enabled);
    }

    #[test]
    fn every_db_state_round_trips() {
        for value in 0..8u32 {
            assert_eq!(DbState::from_u32(value).as_u32(), value);
        }
        assert_eq!(DbState::from_u32(1), DbState::Shutdowned);
        assert_eq!(DbState::from_u32(7), DbState::Unrecognized(7));
    }

    #[test]
    fn a_check_point_is_the_check_point_copy_bytes() {
        let image = a_full_image();
        let control = ControlFile::parse(&image).unwrap();
        assert_eq!(SIZEOF_CHECK_POINT, 88);
        assert_eq!(
            control.check_point_copy.to_bytes().as_slice(),
            &image[offset::CP_REDO..offset::UNLOGGED_LSN]
        );
    }

    #[test]
    fn the_template_form_zeroes_exactly_the_per_cluster_fields() {
        let control = ControlFile::parse(&a_full_image()).unwrap();
        let template = control.as_template();
        assert_eq!(template.system_identifier.get(), 0);
        assert_eq!(template.time, 0);
        assert_eq!(template.check_point_copy.time, 0);
        assert_eq!(template.mock_authentication_nonce, [0; MOCK_AUTH_NONCE_LEN]);

        // Put the four back and nothing else differs.
        let mut restored = template;
        restored.system_identifier = control.system_identifier;
        restored.time = control.time;
        restored.check_point_copy.time = control.check_point_copy.time;
        restored.mock_authentication_nonce = control.mock_authentication_nonce;
        assert_eq!(restored.to_bytes(), control.to_bytes());
    }

    /// A template whose last checkpoint sits where C initdb's does, in the
    /// first segment.
    fn a_template() -> ControlFile {
        let mut template = ControlFile::parse(&a_full_image()).unwrap().as_template();
        template.xlog_seg_size = 16 * 1024 * 1024;
        template.check_point_copy.redo = 0x0175_B1F0;
        template.check_point = 0x0175_B1F0;
        template.state = DbState::InProduction;
        template.min_recovery_point = 7;
        template.min_recovery_point_tli = 7;
        template.backup_start_point = 7;
        template.backup_end_point = 7;
        template.backup_end_required = true;
        template
    }

    #[test]
    fn a_new_cluster_checkpoints_at_the_start_of_the_next_segment() {
        let template = a_template();
        let new = NewCluster {
            system_identifier: SystemIdentifier::from_raw(0x1234),
            checksums: DataChecksums::Disabled,
            mock_authentication_nonce: [9; MOCK_AUTH_NONCE_LEN],
            now: 1_790_000_000,
        };
        let control = for_new_cluster(&template, &new);

        // pg_resetwal.c:940 and :894: segment 1 holds the old redo, so the
        // new record opens segment 2, just past its long page header.
        assert_eq!(control.check_point_copy.redo, 0x0200_0028);
        assert_eq!(control.check_point, 0x0200_0028);
        assert_eq!(control.check_point_copy.time, 1_790_000_000);
        assert_eq!(control.time, 1_790_000_000);
        assert_eq!(control.state, DbState::Shutdowned);
        assert_eq!(control.min_recovery_point, 0);
        assert_eq!(control.min_recovery_point_tli, 0);
        assert_eq!(control.backup_start_point, 0);
        assert_eq!(control.backup_end_point, 0);
        assert!(!control.backup_end_required);

        assert_eq!(control.system_identifier.get(), 0x1234);
        assert_eq!(control.mock_authentication_nonce, [9; MOCK_AUTH_NONCE_LEN]);
        assert_eq!(control.data_checksum_version, 0);

        // Everything else is the template's.
        assert_eq!(control.wal_level, template.wal_level);
        assert_eq!(control.max_connections, template.max_connections);
        assert_eq!(
            control.check_point_copy.next_xid,
            template.check_point_copy.next_xid
        );
        assert_eq!(
            control.check_point_copy.next_oid,
            template.check_point_copy.next_oid
        );
        assert_eq!(control.catalog_version_no, template.catalog_version_no);
    }

    #[test]
    fn a_redo_on_a_segment_boundary_still_moves_one_segment_on() {
        let mut template = a_template();
        template.check_point_copy.redo = 0x0300_0000;
        let new = NewCluster {
            system_identifier: SystemIdentifier::from_raw(1),
            checksums: DataChecksums::Enabled,
            mock_authentication_nonce: [0; MOCK_AUTH_NONCE_LEN],
            now: 0,
        };
        let control = for_new_cluster(&template, &new);
        assert_eq!(control.check_point, 0x0400_0028);
        assert_eq!(control.data_checksum_version, PG_DATA_CHECKSUM_VERSION);
    }

    #[test]
    fn the_nonce_is_read_from_the_system() {
        let first = strong_random_nonce().unwrap();
        let second = strong_random_nonce().unwrap();
        assert_ne!(first, second, "two 256-bit draws should not collide");
    }
}
