//! `PQconninfoOption` arrays: `PQconndefaults`, `PQconninfoParse` and
//! `PQconninfoFree` (`fe-connect.c`).
//!
//! The parsing and the defaults are `rlibpq`'s ([`rlibpq::parse_conninfo`],
//! [`rlibpq::conndefaults`]); this module only lays a [`ConnInfo`] out the way
//! C expects it. As in `conninfo_init` (`fe-connect.c:6197`), which `memcpy`s
//! each row of the static `PQconninfoOptions[]`, every field but `val` points
//! at storage that lives as long as the library, and `PQconninfoFree` frees
//! only the `val` strings and the array (`fe-connect.c:7459`).

use std::ffi::{CStr, CString, c_char, c_int};
use std::ptr::null_mut;
use std::sync::OnceLock;

use rlibpq::{
    CONNINFO_OPTIONS, ConnError, ConnInfo, ConnOptionDef, Env, Filesystem, conndefaults,
    parse_conninfo,
};

use crate::alloc::{calloc, free, malloc_cstring};

/// `PQconninfoOption`, `libpq-fe.h:271`: one row of a connection option
/// array. The array ends at the first row whose `keyword` is NULL.
#[repr(C)]
#[derive(Debug)]
pub struct PQconninfoOption {
    /// The keyword of the option.
    pub keyword: *mut c_char,
    /// Fallback environment variable name.
    pub envvar: *mut c_char,
    /// Fallback compiled in default value.
    pub compiled: *mut c_char,
    /// Option's current value, or NULL; `malloc`'d.
    pub val: *mut c_char,
    /// Label for field in connect dialog.
    pub label: *mut c_char,
    /// `""`, `"*"` or `"D"`: how to display this field in a connect dialog.
    pub dispchar: *mut c_char,
    /// Field size in characters for dialog.
    pub dispsize: c_int,
}

/// The fields of one `PQconninfoOptions[]` row other than `val`, as the C
/// strings every array this crate returns points into.
struct StaticRow {
    keyword: CString,
    envvar: Option<CString>,
    compiled: Option<CString>,
    label: CString,
    dispchar: CString,
    dispsize: c_int,
}

/// Calculation: `def` as C strings. The table is `rlibpq`'s constant, so a
/// NUL inside it or a `dispsize` past `int` is a bug caught by a unit test,
/// never a runtime condition.
fn static_row(def: &ConnOptionDef) -> StaticRow {
    let c = |text: &str| CString::new(text).expect("no NUL in PQconninfoOptions[]");
    StaticRow {
        keyword: c(def.keyword),
        envvar: def.envvar.map(c),
        compiled: def.compiled.map(c),
        label: c(def.label),
        dispchar: c(def.dispchar.as_str()),
        dispsize: c_int::try_from(def.dispsize).expect("dispsize fits in an int"),
    }
}

/// `PQconninfoOptions[]` as C strings, built once and never freed, like the
/// static array in `fe-connect.c`.
fn static_rows() -> &'static [StaticRow] {
    static ROWS: OnceLock<Vec<StaticRow>> = OnceLock::new();
    ROWS.get_or_init(|| CONNINFO_OPTIONS.iter().map(static_row).collect())
}

fn static_ptr(text: &CStr) -> *mut c_char {
    text.as_ptr().cast_mut()
}

fn static_ptr_or_null(text: Option<&CString>) -> *mut c_char {
    text.map_or(null_mut(), |text| static_ptr(text))
}

/// Action: `info` as a `malloc`'d array terminated by an all-zero row, or null
/// when an allocation fails (C's "out of memory" returns, `fe-connect.c:6208`).
fn to_c(info: &ConnInfo) -> *mut PQconninfoOption {
    let rows = static_rows();
    // SAFETY: `calloc` has no preconditions. The extra row is the terminator,
    // and zeroed memory is a valid all-NULL `PQconninfoOption`.
    let array =
        unsafe { calloc(rows.len() + 1, size_of::<PQconninfoOption>()) }.cast::<PQconninfoOption>();
    if array.is_null() {
        return null_mut();
    }
    for (index, (option, row)) in info.iter().zip(rows).enumerate() {
        let val = match option.value {
            None => null_mut(),
            Some(bytes) => {
                let val = malloc_cstring(bytes);
                if val.is_null() {
                    // Rows from `index` on are still zeroed, so the walk stops
                    // here and frees exactly the values already copied.
                    // SAFETY: `array` is this function's, well formed so far.
                    unsafe { PQconninfoFree(array) };
                    return null_mut();
                }
                val
            }
        };
        // SAFETY: `index < rows.len()`, inside the allocation.
        unsafe {
            array.add(index).write(PQconninfoOption {
                keyword: static_ptr(&row.keyword),
                envvar: static_ptr_or_null(row.envvar.as_ref()),
                compiled: static_ptr_or_null(row.compiled.as_ref()),
                val,
                label: static_ptr(&row.label),
                dispchar: static_ptr(&row.dispchar),
                dispsize: row.dispsize,
            });
        }
    }
    array
}

/// Calculation: what C's error buffer holds for `error` — the message and the
/// newline `libpq_append_error` ends it with (`fe-misc.c:1539`).
fn error_buffer(error: &ConnError) -> Vec<u8> {
    let mut message = error.message();
    message.push(b'\n');
    message
}

/// `PQconndefaults`, `fe-connect.c:2193`: every option with its default from
/// the service file, the environment or the compiled-in value. A failing
/// service lookup is ignored, as C ignores it with no error buffer
/// (`fe-connect.c:6636`). Null only when memory runs out.
#[unsafe(no_mangle)]
pub extern "C" fn PQconndefaults() -> *mut PQconninfoOption {
    to_c(&conndefaults(&Env::from_process(), &Filesystem))
}

/// `PQconninfoParse`, `fe-connect.c:6175`: parse `conninfo` (a URI or
/// `key=value` pairs) without filling in defaults. On failure it returns null
/// and, when `errmsg` is not null, stores a `malloc`'d message there for the
/// caller to `PQfreemem`; on success `*errmsg` is null.
///
/// # Safety
///
/// `conninfo` is a NUL-terminated string, as C dereferences it; `errmsg` is
/// null or valid for a pointer write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconninfoParse(
    conninfo: *const c_char,
    errmsg: *mut *mut c_char,
) -> *mut PQconninfoOption {
    if !errmsg.is_null() {
        // SAFETY: the caller's contract.
        unsafe { errmsg.write(null_mut()) }; // default
    }
    // SAFETY: the caller's contract.
    let conninfo = unsafe { CStr::from_ptr(conninfo) }.to_bytes();
    match parse_conninfo(conninfo) {
        Ok(info) => to_c(&info),
        Err(error) => {
            if !errmsg.is_null() {
                // SAFETY: the caller's contract. A failed `malloc` leaves
                // null, as C's broken PQExpBuffer does.
                unsafe { errmsg.write(malloc_cstring(&error_buffer(&error))) };
            }
            null_mut()
        }
    }
}

/// `PQconninfoFree`, `fe-connect.c:7459`: free every `val` up to the row whose
/// `keyword` is NULL, then the array.
///
/// # Safety
///
/// `options` is null or an array `PQconndefaults` or `PQconninfoParse`
/// returned that has not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PQconninfoFree(options: *mut PQconninfoOption) {
    if options.is_null() {
        return;
    }
    let mut option = options;
    // SAFETY: the array is terminated by a row whose keyword is NULL, so every
    // row read here is inside it; each `val` is null or `malloc`'d.
    unsafe {
        while !(*option).keyword.is_null() {
            free((*option).val.cast());
            option = option.add(1);
        }
        free(options.cast());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_of_the_table_is_representable_in_c() {
        let rows = static_rows();
        assert_eq!(rows.len(), CONNINFO_OPTIONS.len());
        for (row, def) in rows.iter().zip(&CONNINFO_OPTIONS) {
            assert_eq!(row.keyword.to_bytes(), def.keyword.as_bytes());
            assert_eq!(row.dispchar.to_bytes(), def.dispchar.as_str().as_bytes());
        }
    }

    #[test]
    fn the_error_buffer_ends_in_the_newline_libpq_append_error_adds() {
        let error = parse_conninfo(b"host").expect_err("no = after host");
        assert_eq!(
            error_buffer(&error),
            b"missing \"=\" after \"host\" in connection info string\n"
        );
    }

    #[test]
    fn to_c_lays_every_row_out_and_terminates_the_array() {
        let info = parse_conninfo(b"host=h port=5433").expect("parses");
        let array = to_c(&info);
        assert!(!array.is_null());
        let mut seen = Vec::new();
        // SAFETY: `array` is what `to_c` just returned.
        unsafe {
            let mut option = array;
            while !(*option).keyword.is_null() {
                let keyword = CStr::from_ptr((*option).keyword).to_bytes().to_vec();
                if !(*option).val.is_null() {
                    seen.push((keyword, CStr::from_ptr((*option).val).to_bytes().to_vec()));
                }
                option = option.add(1);
            }
            assert_eq!(option.offset_from_unsigned(array), CONNINFO_OPTIONS.len());
            PQconninfoFree(array);
        }
        assert_eq!(
            seen,
            [
                (b"host".to_vec(), b"h".to_vec()),
                (b"port".to_vec(), b"5433".to_vec())
            ]
        );
    }
}
