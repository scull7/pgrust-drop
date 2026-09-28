//! [`CText`]: bytes C reads through a `char *` that libpq keeps owning.

use std::ffi::c_char;

/// Bytes followed by one NUL, so C can read them as a string while this
/// crate keeps them: `conn->errorMessage`, a result's `errMsg`, `cmdStatus`
/// and each field value. C never frees these, so they live on Rust's heap.
///
/// The bytes are kept whole, a NUL inside them included, as `PQgetvalue`
/// keeps a binary value whole behind `PQgetlength` (`fe-exec.c:3918`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CText(Box<[u8]>);

impl CText {
    /// Calculation: `bytes` and a terminating NUL.
    pub(crate) fn new(bytes: &[u8]) -> Self {
        let mut text = Vec::with_capacity(bytes.len() + 1);
        text.extend_from_slice(bytes);
        text.push(0);
        CText(text.into_boxed_slice())
    }

    /// The bytes, without the terminating NUL.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.0[..self.0.len() - 1]
    }

    /// Where C reads it. Valid as long as `self` is.
    pub(crate) fn as_ptr(&self) -> *mut c_char {
        self.0.as_ptr().cast::<c_char>().cast_mut()
    }
}

impl Default for CText {
    fn default() -> Self {
        CText::new(b"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ctext_is_its_bytes_and_one_nul() {
        let text = CText::new(b"a\0b");
        assert_eq!(text.bytes(), b"a\0b");
        // SAFETY: `as_ptr` points at the four bytes `new` wrote.
        let raw = unsafe { std::slice::from_raw_parts(text.as_ptr().cast::<u8>(), 4) };
        assert_eq!(raw, b"a\0b\0");
        assert_eq!(CText::default().bytes(), b"");
    }
}
