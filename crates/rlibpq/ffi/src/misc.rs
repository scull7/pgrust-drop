//! The connection-free odds and ends of `fe-misc.c` and `fe-exec.c`.

use std::ffi::{c_int, c_void};

/// `PG_VERSION_NUM` for 18.6: `configure.ac:2475`-`:2477` prints the major
/// version and the minor version zero-padded to four digits (`%d%04d`) from
/// `AC_INIT`'s `18.6` (`configure.ac:20`; `meson.build:11` agrees).
pub const PG_VERSION_NUM: c_int = 180_006;

/// `PQlibVersion`, `fe-misc.c:65`: the version this libpq is.
#[unsafe(no_mangle)]
pub extern "C" fn PQlibVersion() -> c_int {
    PG_VERSION_NUM
}

/// `PQisthreadsafe`, `fe-exec.c:4023`: always true.
#[unsafe(no_mangle)]
pub extern "C" fn PQisthreadsafe() -> c_int {
    1
}

/// `PQfreemem`, `fe-exec.c:4063`: `free(ptr)`.
///
/// It is the C library's `free` and not Rust's allocator on purpose: C libpq
/// callers routinely hand what libpq allocated to plain `free()`, so every
/// buffer this crate gives C must come from `malloc` for that to stay sound.
///
/// # Safety
///
/// `ptr` is null or a pointer `malloc` returned that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfreemem(ptr: *mut c_void) {
    // SAFETY: the caller's contract is `free`'s, and `libc::free` is the C
    // library's `free` (`fe-exec.c:4065`), the one `malloc` pairs with.
    unsafe { libc::free(ptr) }
}

/// `PQfreeNotify`, `fe-exec.c:4080`: kept for binary compatibility only;
/// `libpq-fe.h:629` turns a source-level call into `PQfreemem`.
///
/// # Safety
///
/// As [`PQfreemem`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQfreeNotify(notify: *mut c_void) {
    // SAFETY: as `PQfreemem`.
    unsafe { PQfreemem(notify) }
}
