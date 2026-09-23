//! Build-time constants `initdb.c` inherits from `pg_config.h` /
//! `pg_config_manual.h`.
//!
//! C initdb bakes these in at configure time, so the cluster it writes depends
//! on how the server it belongs to was built. This port has no configure step
//! and targets a stock PostgreSQL 18.6 build, so each value below is upstream's
//! default with its defining line cited. A distribution build (Debian's
//! `--with-pgport`, a non-default `--with-pgsocketdir`, a rebuilt `BLCKSZ`)
//! would choose differently; that is a property of *that* server, not a
//! divergence in this port, and the byte-diff gate against such a build is what
//! would surface it.

/// `src/include/pg_config.h.in:31` — the default `--with-blocksize`, in bytes.
///
/// `setup_config` only ever uses `BLCKSZ / 1024`, the block size in kB.
pub const BLCKSZ: u32 = 8192;

/// `src/include/pg_config.h.in:43` — the default `--with-pgport`, as a string.
///
/// Upstream writes it into the file verbatim (`DEF_PGPORT_STR`), so it is a
/// string here too and never a number that has been formatted back.
///
/// Must equal `crates/rlibpq/src/pg_config.rs`'s `DEF_PGPORT_STR`: a cluster
/// `pgdrop initdb` writes and the server `pgdrop psql` dials by default are
/// the same configure-time number. Pinned by this module's tests and by
/// `crates/pgdrop/tests/pg_config_agreement.rs`.
pub const DEF_PGPORT_STR: &str = "5432";

/// `src/include/pg_config_manual.h:193` — where AF_UNIX sockets go by default.
///
/// Must equal `crates/rlibpq/src/pg_config.rs`'s `DEFAULT_PGSOCKET_DIR`, arm
/// for arm, or a cluster `pgdrop initdb` creates puts its socket somewhere
/// `pgdrop psql` does not look. Pinned by this module's tests and by
/// `crates/pgdrop/tests/pg_config_agreement.rs`.
#[cfg(not(windows))]
pub const DEFAULT_PGSOCKET_DIR: &str = "/tmp";
/// `src/include/pg_config_manual.h:195` — Windows has no standard location.
#[cfg(windows)]
pub const DEFAULT_PGSOCKET_DIR: &str = "";

/// `src/include/access/xlog_internal.h:91`.
pub const DEFAULT_MIN_WAL_SEGS: u32 = 5;

/// `src/include/access/xlog_internal.h:92`.
pub const DEFAULT_MAX_WAL_SEGS: u32 = 64;

/// `src/include/pg_config_manual.h:20` — `DEFAULT_XLOG_SEG_SIZE` in megabytes,
/// which is `initdb.c:169`'s initial `wal_segment_size_mb`.
pub const DEFAULT_WAL_SEGMENT_SIZE_MB: u32 = 16;

/// `src/include/pg_config_manual.h:156` / `:160` — never enabled by default.
pub const DEFAULT_BACKEND_FLUSH_AFTER: u32 = 0;

/// `src/include/pg_config_manual.h:157` — 64 where `sync_file_range()` exists.
#[cfg(target_os = "linux")]
pub const DEFAULT_BGWRITER_FLUSH_AFTER: u32 = 64;
/// `src/include/pg_config_manual.h:161` — 0 without `HAVE_SYNC_FILE_RANGE`.
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_BGWRITER_FLUSH_AFTER: u32 = 0;

/// `src/include/pg_config_manual.h:158` — 32 where `sync_file_range()` exists.
#[cfg(target_os = "linux")]
pub const DEFAULT_CHECKPOINT_FLUSH_AFTER: u32 = 32;
/// `src/include/pg_config_manual.h:162` — 0 without `HAVE_SYNC_FILE_RANGE`.
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_CHECKPOINT_FLUSH_AFTER: u32 = 0;

/// The three `#if DEFAULT_*_FLUSH_AFTER > 0` blocks of `setup_config`
/// (`initdb.c:1376`, `:1383`, `:1390`), in upstream order, as data.
///
/// Each is written as a commented-out assignment of its compile-time default,
/// measured in blocks and rendered in kB, and only when that default is
/// nonzero — which on a platform without `sync_file_range()` means not at all.
pub const FLUSH_AFTER_DEFAULTS: [(&str, u32); 3] = [
    ("backend_flush_after", DEFAULT_BACKEND_FLUSH_AFTER),
    ("bgwriter_flush_after", DEFAULT_BGWRITER_FLUSH_AFTER),
    ("checkpoint_flush_after", DEFAULT_CHECKPOINT_FLUSH_AFTER),
];

/// `#ifdef WIN32` at `initdb.c:1396`: Windows gets `update_process_title = off`.
pub const UPDATE_PROCESS_TITLE_OFF: bool = cfg!(windows);

#[cfg(test)]
mod tests {
    use super::*;

    /// `setup_config` divides by this and never by a literal 1024 of its own.
    #[test]
    fn the_block_size_is_a_whole_number_of_kilobytes() {
        assert_eq!(BLCKSZ % 1024, 0);
        assert_eq!(BLCKSZ / 1024, 8);
    }

    /// `pretty_wal_size(DEFAULT_MAX_WAL_SEGS)` must come out as upstream's
    /// documented `1GB`, and `DEFAULT_MIN_WAL_SEGS` as `80MB`.
    #[test]
    fn the_wal_defaults_are_the_sizes_the_sample_file_documents() {
        assert_eq!(DEFAULT_WAL_SEGMENT_SIZE_MB * DEFAULT_MAX_WAL_SEGS, 1024);
        assert_eq!(DEFAULT_WAL_SEGMENT_SIZE_MB * DEFAULT_MIN_WAL_SEGS, 80);
    }

    /// This crate's half of the cross-crate agreement: the two values
    /// `rlibpq` also defines, pinned to the stock build's literals
    /// (`src/include/pg_config.h.in:43`, `src/include/pg_config_manual.h:193`
    /// / `:195`). The other half is `rlibpq`'s
    /// `the_constants_rinitdb_also_defines_are_the_stock_build_values`
    /// (`crates/rlibpq/src/pg_config.rs`); `crates/pgdrop/tests/pg_config_agreement.rs`
    /// compares the two directly.
    ///
    /// If one of these ever legitimately stops being the stock value, change
    /// both halves together; the `pgdrop` test is the one that must survive.
    #[test]
    fn the_constants_rlibpq_also_defines_are_the_stock_build_values() {
        assert_eq!(DEF_PGPORT_STR, "5432");
        #[cfg(not(windows))]
        assert_eq!(DEFAULT_PGSOCKET_DIR, "/tmp");
        #[cfg(windows)]
        assert_eq!(DEFAULT_PGSOCKET_DIR, "");
    }

    /// The table is the three `#if` blocks, in `setup_config` order.
    #[test]
    fn the_flush_after_table_is_upstreams_in_upstream_order() {
        assert_eq!(
            FLUSH_AFTER_DEFAULTS.map(|(name, _)| name),
            [
                "backend_flush_after",
                "bgwriter_flush_after",
                "checkpoint_flush_after"
            ]
        );
    }
}
