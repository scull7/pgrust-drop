//! Build-time constants `fe-connect.c` inherits from `pg_config.h` and from
//! the `#ifdef`s at the top of the file.
//!
//! C libpq bakes these in when it is configured, so two libpq builds can hand
//! `PQconndefaults` different answers. This port has no configure step; each
//! value below says which arm it takes and why.

/// `src/include/pg_config.h.in:43`, whose default is 5432
/// (`configure.ac:157`), as the string `PQconninfoOptions[]` stores it
/// (`fe-connect.c:237`).
pub const DEF_PGPORT_STR: &str = "5432";

/// `src/include/pg_config.h.in:609`, whose default is `postgres`
/// (`configure.ac:890`).
pub const PG_KRB_SRVNAM: &str = "postgres";

/// `src/include/common/scram-common.h:24` — `SCRAM_SHA_256_KEY_LEN`, which is
/// `PG_SHA256_DIGEST_LENGTH`. Only ever used here as the `dispsize` of the two
/// `scram_*_key` options, which is `SCRAM_MAX_KEY_LEN * 2`.
pub const SCRAM_MAX_KEY_LEN: usize = 32;

/// `USE_SSL` — false here, because this crate has no TLS backend yet.
///
/// ADR-0006 puts `rustls` behind a `tls` feature that NAT-392 adds; until then
/// the `#else` arms at `fe-connect.c:125` and `:133` are the honest ones. A
/// distribution libpq is built with SSL and so answers `prefer` where we answer
/// `disable` — recorded in `docs/divergences.md`.
pub const USE_SSL: bool = false;

/// `ENABLE_GSS` — false here; GSSAPI is feature-gated work that no issue in
/// this milestone opens (`fe-connect.c:141`'s arm).
pub const ENABLE_GSS: bool = false;

/// `fe-connect.c:121`.
pub const DEFAULT_OPTION: &str = "";

/// `fe-connect.c:123` with `USE_SSL`, `:125` without.
pub const DEFAULT_CHANNEL_BINDING: &str = if USE_SSL { "prefer" } else { "disable" };

/// `fe-connect.c:127`.
pub const DEFAULT_TARGET_SESSION_ATTRS: &str = "any";

/// `fe-connect.c:128`.
pub const DEFAULT_LOAD_BALANCE_HOSTS: &str = "disable";

/// `fe-connect.c:130` with `USE_SSL`, `:133` without.
pub const DEFAULT_SSL_MODE: &str = if USE_SSL { "prefer" } else { "disable" };

/// `fe-connect.c:136`.
pub const DEFAULT_SSL_NEGOTIATION: &str = "postgres";

/// `fe-connect.c:139` with `ENABLE_GSS`, `:141` without.
pub const DEFAULT_GSS_MODE: &str = if ENABLE_GSS { "prefer" } else { "disable" };

/// `pg_config_manual.h` — `DEFAULT_PGSOCKET_DIR`, the compiled-in directory
/// `DefaultHost` (`fe-connect.c:110`) falls back to when `host` is unset. A
/// stock build uses `/tmp`; a distribution build often does not, which is the
/// same stock-build-constant divergence `crates/rinitdb/src/pg_config.rs`
/// records for `initdb`.
pub const DEFAULT_PGSOCKET_DIR: &str = "/tmp";

#[cfg(test)]
mod tests {
    use super::*;

    /// The pin for the `docs/divergences.md` entry: a build without TLS and
    /// without GSSAPI takes the `#else` arm of all three, and changing that is
    /// NAT-392's job, not a quiet edit.
    #[test]
    fn the_ssl_and_gss_defaults_are_the_arms_a_build_without_them_takes() {
        const { assert!(!USE_SSL) };
        const { assert!(!ENABLE_GSS) };
        assert_eq!(DEFAULT_SSL_MODE, "disable");
        assert_eq!(DEFAULT_CHANNEL_BINDING, "disable");
        assert_eq!(DEFAULT_GSS_MODE, "disable");
    }

    #[test]
    fn the_scram_key_dispsize_is_twice_the_key_length() {
        assert_eq!(SCRAM_MAX_KEY_LEN * 2, 64);
    }
}
