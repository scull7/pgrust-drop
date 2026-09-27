//! The TLS and GSSAPI probes of `fe-secure.c`, as a C libpq built without
//! SSL and without GSSAPI answers them.
//!
//! `rlibpq` has no TLS backend yet (`rlibpq::pg_config::USE_SSL`; ADR-0006
//! puts `rustls` behind the `tls` feature NAT-392 adds) and no GSSAPI, so
//! these are upstream's own dummy arms — `#ifndef USE_SSL`
//! (`fe-secure.c:449`-`:476`), `#ifndef USE_OPENSSL` (`:482`-`:501`) and
//! `#ifndef ENABLE_GSS` (`:504`-`:518`) — plus the two no-ops and
//! `PQsslInUse` that every build has. The assertions below stop the build the
//! moment either capability appears, because every answer here would then be
//! wrong.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use rlibpq::pg_config::{ENABLE_GSS, USE_SSL};

use crate::PGconn;

const _: () = assert!(
    !USE_SSL,
    "rlibpq has TLS now: PQsslAttribute and its siblings must answer from the connection"
);
const _: () = assert!(
    !ENABLE_GSS,
    "rlibpq has GSSAPI now: PQgetgssctx and PQgssEncInUse must answer from the connection"
);

/// `PQsslKeyPassHook_OpenSSL_type`, `libpq-fe.h:828`.
#[allow(non_camel_case_types)]
pub type PQsslKeyPassHook_OpenSSL_type =
    Option<unsafe extern "C" fn(buf: *mut c_char, size: c_int, conn: *mut PGconn) -> c_int>;

/// `PQsslAttributeNames`' answer without SSL: a list holding only its
/// terminating `NULL` (`fe-secure.c:472`). `Option<&T>` is a nullable
/// pointer with `None` as `NULL`, and unlike a raw pointer it may sit in a
/// `static`.
static NO_SSL_ATTRIBUTES: [Option<&'static c_char>; 1] = [None];

/// `PQsslInUse`, `fe-secure.c:103`: `conn->ssl_in_use`, which a build without
/// SSL never sets, so 0 for every connection and for `NULL`.
#[unsafe(no_mangle)]
pub extern "C" fn PQsslInUse(_conn: *mut PGconn) -> c_int {
    0
}

/// `PQinitSSL`, `fe-secure.c:117`: a no-op in every build.
#[unsafe(no_mangle)]
pub extern "C" fn PQinitSSL(_do_init: c_int) {}

/// `PQinitOpenSSL`, `fe-secure.c:129`: a no-op in every build.
#[unsafe(no_mangle)]
pub extern "C" fn PQinitOpenSSL(_do_ssl: c_int, _do_crypto: c_int) {}

/// `PQgetssl`, `fe-secure.c:452`.
#[unsafe(no_mangle)]
pub extern "C" fn PQgetssl(_conn: *mut PGconn) -> *mut c_void {
    ptr::null_mut()
}

/// `PQsslStruct`, `fe-secure.c:458`.
#[unsafe(no_mangle)]
pub extern "C" fn PQsslStruct(_conn: *mut PGconn, _struct_name: *const c_char) -> *mut c_void {
    ptr::null_mut()
}

/// `PQsslAttribute`, `fe-secure.c:464`: `NULL` for every attribute, including
/// `PQsslAttribute(NULL, "library")`, which is how `libpq_testclient --ssl`
/// tells that SSL is not enabled.
#[unsafe(no_mangle)]
pub extern "C" fn PQsslAttribute(
    _conn: *mut PGconn,
    _attribute_name: *const c_char,
) -> *const c_char {
    ptr::null()
}

/// `PQsslAttributeNames`, `fe-secure.c:470`.
#[unsafe(no_mangle)]
pub extern "C" fn PQsslAttributeNames(_conn: *mut PGconn) -> *const *const c_char {
    NO_SSL_ATTRIBUTES.as_ptr().cast()
}

/// `PQgetSSLKeyPassHook_OpenSSL`, `fe-secure.c:485`.
#[unsafe(no_mangle)]
pub extern "C" fn PQgetSSLKeyPassHook_OpenSSL() -> PQsslKeyPassHook_OpenSSL_type {
    None
}

/// `PQsetSSLKeyPassHook_OpenSSL`, `fe-secure.c:491`: ignored.
#[unsafe(no_mangle)]
pub extern "C" fn PQsetSSLKeyPassHook_OpenSSL(_hook: PQsslKeyPassHook_OpenSSL_type) {}

/// `PQdefaultSSLKeyPassHook_OpenSSL`, `fe-secure.c:497`.
#[unsafe(no_mangle)]
pub extern "C" fn PQdefaultSSLKeyPassHook_OpenSSL(
    _buf: *mut c_char,
    _size: c_int,
    _conn: *mut PGconn,
) -> c_int {
    0
}

/// `PQgetgssctx`, `fe-secure.c:507`.
#[unsafe(no_mangle)]
pub extern "C" fn PQgetgssctx(_conn: *mut PGconn) -> *mut c_void {
    ptr::null_mut()
}

/// `PQgssEncInUse`, `fe-secure.c:513`.
#[unsafe(no_mangle)]
pub extern "C" fn PQgssEncInUse(_conn: *mut PGconn) -> c_int {
    0
}
