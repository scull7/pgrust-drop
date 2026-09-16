//! Reading a byte slice the way C reads a NUL-terminated `char *`.
//!
//! Every parser in `fe-connect.c` walks a `char *` and reads the terminator as
//! an ordinary loop condition — `while (*p && *p != ':')`, then `prevchar = *p`
//! where `*p` may be the NUL. Transcribing those loops with `index < len`
//! guards would restate each bound in a second, drift-prone way, so this module
//! gives the two primitives that let the loops keep upstream's shape.

/// The byte at `index`, or NUL past the end.
///
/// A slice that itself contains a NUL stops here exactly where `strlen` would
/// stop in C, so a conninfo string with an embedded NUL parses identically.
#[must_use]
pub fn at(bytes: &[u8], index: usize) -> u8 {
    bytes.get(index).copied().unwrap_or(0)
}

/// `isspace()` in the `C` locale, which is the locale a TAP test pins
/// (`Utils.pm:112` sets `LC_MESSAGES=C`) and the only one whose answer here is
/// portable: space, tab, newline, vertical tab, form feed, carriage return.
///
/// `conninfo_parse` (`fe-connect.c:6314`) calls it on `(unsigned char) *cp`, so
/// it is a byte question and never a character one.
#[must_use]
pub fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn past_the_end_reads_as_the_terminator() {
        assert_eq!(at(b"ab", 1), b'b');
        assert_eq!(at(b"ab", 2), 0);
        assert_eq!(at(b"ab", 99), 0);
        assert_eq!(at(b"", 0), 0);
    }

    #[test]
    fn an_embedded_nul_ends_the_string_as_strlen_would() {
        assert_eq!(at(b"a\0b", 1), 0);
    }

    #[test]
    fn the_six_c_locale_space_bytes_and_no_others() {
        for byte in 0u8..=255 {
            let expected = matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
            assert_eq!(is_space(byte), expected, "byte {byte:#04x}");
        }
    }
}
