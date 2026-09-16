//! A byte string, printed the way C prints one.

use std::fmt;

/// An owned byte string that renders as libpq's `%s` renders a `char *`.
///
/// libpq's conninfo values are bytes, not text: percent-decoding `%C3%A9` in a
/// URI yields two bytes libpq never inspects, and `printf("%s")` writes them
/// back out unchanged whatever the client encoding is. Holding them as `String`
/// would mean either rejecting a URI that C accepts or replacing bytes with
/// U+FFFD, so values and the tokens quoted in error messages are `RawText`.
///
/// [`fmt::Display`] is the lossy Rust-side rendering, for a panic message or a
/// `{err}`. Anything that has to reproduce C's bytes exactly writes
/// [`RawText::as_bytes`] instead — see [`crate::ConnError::message`].
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawText(Vec<u8>);

impl RawText {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&[u8]> for RawText {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<Vec<u8>> for RawText {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&str> for RawText {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl fmt::Display for RawText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

impl fmt::Debug for RawText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&String::from_utf8_lossy(&self.0), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_survive_a_round_trip_that_is_not_utf8() {
        let raw = RawText::new(vec![b'a', 0xC3, b'b']);
        assert_eq!(raw.as_bytes(), [b'a', 0xC3, b'b']);
    }

    #[test]
    fn display_is_lossy_but_as_bytes_is_not() {
        let raw = RawText::new(vec![0xFF]);
        assert_eq!(raw.to_string(), "\u{fffd}");
        assert_eq!(raw.as_bytes(), [0xFF]);
    }
}
