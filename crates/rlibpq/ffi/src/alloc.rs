//! The C library's allocator, which everything this crate hands C comes from.
//!
//! C libpq callers free what libpq gave them with `PQfreemem`, `PQconninfoFree`
//! or plain `free()`, so each buffer must be one `malloc` returned; Rust's
//! allocator is never used for memory that crosses the ABI.

use std::ffi::c_char;

// The C library's `malloc`, `calloc` and `free` through the `libc` crate
// (approved for rlibpq-ffi, owner 2026-09-27; musl is a priority target), not
// hand-declared imports.
pub(crate) use libc::{calloc, free, malloc};

/// Calculation: the bytes a C string holding `bytes` would keep — everything
/// before the first NUL, which is where `strdup` would have stopped.
pub(crate) fn until_nul(bytes: &[u8]) -> &[u8] {
    bytes
        .iter()
        .position(|&byte| byte == 0)
        .map_or(bytes, |nul| &bytes[..nul])
}

/// Action: a `malloc`'d, NUL-terminated copy of `bytes` (up to its first NUL,
/// as `strdup` copies), or null when `malloc` fails.
pub(crate) fn malloc_cstring(bytes: &[u8]) -> *mut c_char {
    let bytes = until_nul(bytes);
    // SAFETY: `malloc` has no preconditions.
    let out = unsafe { malloc(bytes.len() + 1) }.cast::<u8>();
    if out.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `out` holds `bytes.len() + 1` bytes and does not overlap `bytes`.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, bytes.len());
        out.add(bytes.len()).write(0);
    }
    out.cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn until_nul_stops_where_strdup_stops() {
        assert_eq!(until_nul(b"abc"), b"abc");
        assert_eq!(until_nul(b"ab\0c"), b"ab");
        assert_eq!(until_nul(b""), b"");
    }

    #[test]
    fn malloc_cstring_copies_and_terminates() {
        let copy = malloc_cstring(b"host\0ignored");
        assert!(!copy.is_null());
        // SAFETY: `copy` is the NUL-terminated string just allocated.
        let text = unsafe { std::ffi::CStr::from_ptr(copy) };
        assert_eq!(text.to_bytes(), b"host");
        // SAFETY: `copy` came from `malloc`.
        unsafe { free(copy.cast()) };
    }
}
