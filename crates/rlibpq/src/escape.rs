//! The string and bytea escaping of `fe-exec.c` (PostgreSQL 18.6):
//! `PQescapeStringConn` / `PQescapeString` (`:4102`-`:4235`),
//! `PQescapeLiteral` / `PQescapeIdentifier` (`:4245`-`:4423`),
//! `PQescapeByteaConn` / `PQescapeBytea` (`:4466`-`:4612`) and
//! `PQunescapeBytea` (`:4631`).
//!
//! What C reads off the `PGconn` — the client encoding and
//! `standard_conforming_strings` — is an argument here, so every function is
//! a pure calculation; [`crate::Connection`] supplies the values its server
//! reported. An input is read as C reads it: up to `len` bytes or the first
//! NUL, whichever comes first (`strnlen`, `:4107`, `:4253`), and an output
//! carries no terminating NUL.
//!
//! The one difference in shape: C's `size_t` overflow checks
//! (`escaped string size exceeds the maximum allowed`, `:4407`;
//! `escaped bytea size exceeds …`, `:4584`) have no counterpart, because a
//! `Vec` that large cannot be allocated in the first place.

use std::fmt;

use crate::encoding::Encoding;

/// Why an escape function refused, or complained about, its input: the
/// messages `libpq_append_conn_error` leaves in `PQerrorMessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeError {
    /// `fe-exec.c:4172`, `:4286` — a multibyte character runs past the end
    /// of the input.
    IncompleteMultibyte,
    /// `fe-exec.c:4174`, `:4305` — bytes that are not a character in the
    /// client encoding.
    InvalidMultibyte,
}

impl EscapeError {
    /// The bytes libpq appends to `conn->errorMessage`, newline included
    /// (`libpq_append_conn_error`, `fe-misc.c:1568`).
    #[must_use]
    pub fn message(self) -> Vec<u8> {
        match self {
            EscapeError::IncompleteMultibyte => b"incomplete multibyte character\n".to_vec(),
            EscapeError::InvalidMultibyte => b"invalid multibyte character\n".to_vec(),
        }
    }
}

impl fmt::Display for EscapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for EscapeError {}

/// What `PQescapeStringConn` produces: the escaped bytes are written even
/// when the input was not valid, with each bad byte replaced by an invalid
/// sequence the server will refuse (`fe-exec.c:4179`), so a caller
/// that ignores `error` still cannot smuggle a quote through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscapedString {
    /// The escaped text, without the surrounding quotes C leaves to the
    /// caller.
    pub bytes: Vec<u8>,
    /// `*error`, with the message C appends once per string
    /// (`already_complained`, `:4176`).
    pub error: Option<EscapeError>,
}

/// The input as C reads it: `strnlen(from, length)`.
fn up_to_nul(from: &[u8]) -> &[u8] {
    let end = from.iter().position(|&b| b == 0).unwrap_or(from.len());
    &from[..end]
}

/// `SQL_STR_DOUBLE`, `src/include/c.h`: does `c` have to be doubled inside
/// a quoted literal?
fn sql_str_double(c: u8, escape_backslash: bool) -> bool {
    c == b'\'' || (c == b'\\' && escape_backslash)
}

/// `PQescapeStringInternal`, `fe-exec.c:4102`: the body of
/// `PQescapeStringConn` and `PQescapeString`. Doubles `'`, and `\` too
/// unless `std_strings`; copies each validly encoded multibyte character
/// whole and replaces the first byte of an invalid one with
/// [`Encoding::invalid_sequence`].
#[must_use]
pub fn escape_string(from: &[u8], encoding: Encoding, std_strings: bool) -> EscapedString {
    let source = up_to_nul(from);
    let mut target = Vec::with_capacity(source.len() * 2);
    let mut error = None;
    let mut i = 0;
    while i < source.len() {
        let c = source[i];
        // Fast path for plain ASCII.
        if c & 0x80 == 0 {
            if sql_str_double(c, !std_strings) {
                target.push(c);
            }
            target.push(c);
            i += 1;
            continue;
        }
        // Slow path for possible multibyte characters.
        let rest = &source[i..];
        let charlen = encoding.mblen_or_incomplete(rest);
        let valid = match charlen {
            Some(n) if n <= rest.len() => encoding.verify_char(&rest[..n]).is_some(),
            _ => false,
        };
        if valid {
            let n = charlen.unwrap_or(1);
            target.extend_from_slice(&rest[..n]);
            i += n;
        } else {
            // fe-exec.c:4169 — complain once, with the first reason found.
            if error.is_none() {
                error = Some(if charlen.is_none_or(|n| n > rest.len()) {
                    EscapeError::IncompleteMultibyte
                } else {
                    EscapeError::InvalidMultibyte
                });
            }
            target.extend_from_slice(&encoding.invalid_sequence());
            // Handle the following bytes as if this byte didn't exist.
            i += 1;
        }
    }
    EscapedString {
        bytes: target,
        error,
    }
}

/// `PQescapeInternal`, `fe-exec.c:4245`: the body of `PQescapeLiteral`
/// (`as_ident` false) and `PQescapeIdentifier` (true). The result is quoted;
/// a literal with a backslash in it is written ` E'…'` so that it means the
/// same under either `standard_conforming_strings`.
///
/// # Errors
/// A multibyte character that runs past the end of the input, or input
/// that is not valid in `encoding`: nothing is produced.
pub fn escape_internal(
    from: &[u8],
    encoding: Encoding,
    as_ident: bool,
) -> Result<Vec<u8>, EscapeError> {
    let input = up_to_nul(from);
    let quote_char = if as_ident { b'"' } else { b'\'' };
    let mut num_quotes = 0usize;
    let mut num_backslashes = 0usize;
    let mut validated_mb = false;

    // Scan the string for characters that must be escaped and for invalidly
    // encoded data (:4265).
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        if c == quote_char {
            num_quotes += 1;
        } else if c == b'\\' {
            num_backslashes += 1;
        } else if c & 0x80 != 0 {
            let rest = &input[i..];
            let charlen = match encoding.mblen_or_incomplete(rest) {
                Some(n) if n <= rest.len() => n,
                _ => return Err(EscapeError::IncompleteMultibyte),
            };
            // Validate the whole remainder once, at the first multibyte
            // character (:4291).
            if !validated_mb {
                if encoding.verify_str(rest) != rest.len() {
                    return Err(EscapeError::InvalidMultibyte);
                }
                validated_mb = true;
            }
            i += charlen;
            continue;
        }
        i += 1;
    }

    let backslash_prefix = !as_ident && num_backslashes > 0;
    let mut result = Vec::with_capacity(
        input.len()
            + num_quotes
            + 2
            + if backslash_prefix {
                num_backslashes + 2
            } else {
                0
            },
    );
    if backslash_prefix {
        result.extend_from_slice(b" E");
    }
    result.push(quote_char);
    if num_quotes == 0 && (num_backslashes == 0 || as_ident) {
        // Fast path (:4356): nothing to double.
        result.extend_from_slice(input);
    } else {
        let mut i = 0;
        while i < input.len() {
            let c = input[i];
            if c == quote_char || (!as_ident && c == b'\\') {
                result.push(c);
                result.push(c);
                i += 1;
            } else if c & 0x80 == 0 {
                result.push(c);
                i += 1;
            } else {
                // Already validated, so the character is whole.
                let n = encoding.mblen(&input[i..]).min(input.len() - i);
                result.extend_from_slice(&input[i..i + n]);
                i += n;
            }
        }
    }
    result.push(quote_char);
    Ok(result)
}

/// `hextbl`, `fe-exec.c:4425`.
const HEXTBL: &[u8; 16] = b"0123456789abcdef";

/// `PQescapeByteaInternal`, `fe-exec.c:4466`: the body of
/// `PQescapeByteaConn` (`use_hex` when the server is 9.0 or later, `:4602`)
/// and `PQescapeBytea` (never hex, `:4610`).
///
/// C's `*to_length` counts the terminating NUL, so it is this result's
/// length plus one.
#[must_use]
pub fn escape_bytea(from: &[u8], std_strings: bool, use_hex: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(if use_hex {
        from.len() * 2 + 3
    } else {
        from.len()
    });
    if use_hex {
        if !std_strings {
            result.push(b'\\');
        }
        result.extend_from_slice(b"\\x");
    }
    for &c in from {
        if use_hex {
            result.push(HEXTBL[usize::from(c >> 4)]);
            result.push(HEXTBL[usize::from(c & 0xf)]);
        } else if !(0x20..=0x7e).contains(&c) {
            if !std_strings {
                result.push(b'\\');
            }
            result.push(b'\\');
            result.push((c >> 6) + b'0');
            result.push(((c >> 3) & 0o7) + b'0');
            result.push((c & 0o7) + b'0');
        } else if c == b'\'' {
            result.extend_from_slice(b"''");
        } else if c == b'\\' {
            if !std_strings {
                result.extend_from_slice(b"\\\\");
            }
            result.extend_from_slice(b"\\\\");
        } else {
            result.push(c);
        }
    }
    result
}

/// `get_hex`, `fe-exec.c:4439`, over `hexlookup` (`:4427`).
fn get_hex(c: u8) -> Option<u8> {
    char::from(c)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

/// `PQunescapeBytea`, `fe-exec.c:4631`: the bytes a bytea's text form (hex
/// `\x…` or the traditional escape format) stands for. The input is read up
/// to its first NUL (`strlen`, `:4643`). Bad input is not an error: an
/// unpaired or non-hex digit is skipped, and in the escape format a `\`
/// that starts no recognized sequence is dropped (`:4711`).
#[must_use]
pub fn unescape_bytea(strtext: &[u8]) -> Vec<u8> {
    let text = up_to_nul(strtext);
    if let Some(hex) = text.strip_prefix(b"\\x") {
        let mut buffer = Vec::with_capacity(hex.len() / 2);
        let mut s = hex.iter().copied();
        while let Some(c1) = s.next() {
            // Bad input is silently ignored; that includes whitespace
            // between hex pairs, which byteain allows (:4664).
            let Some(v1) = get_hex(c1) else { continue };
            let Some(c2) = s.next() else { break };
            if let Some(v2) = get_hex(c2) {
                buffer.push((v1 << 4) | v2);
            }
        }
        return buffer;
    }

    let first_oct = |c: u8| (b'0'..=b'3').contains(&c);
    let oct = |c: u8| (b'0'..=b'7').contains(&c);
    let at = |i: usize| text.get(i).copied().unwrap_or(0);
    let mut buffer = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\\' {
            i += 1;
            if at(i) == b'\\' {
                buffer.push(b'\\');
                i += 1;
            } else if first_oct(at(i)) && oct(at(i + 1)) && oct(at(i + 2)) {
                buffer.push(((at(i) - b'0') << 6) | ((at(i + 1) - b'0') << 3) | (at(i + 2) - b'0'));
                i += 3;
            }
        } else {
            buffer.push(text[i]);
            i += 1;
        }
    }
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_literal_doubles_quotes_and_prefixes_e_for_a_backslash() {
        let lit = |s: &[u8]| escape_internal(s, Encoding::Utf8, false).unwrap();
        assert_eq!(lit(b"1"), b"'1'");
        assert_eq!(lit(b"it's"), b"'it''s'");
        assert_eq!(lit(b"\\"), b" E'\\\\'");
        assert_eq!(lit(b"\\'"), b" E'\\\\'''");
        assert_eq!(lit(b"\""), b"'\"'");
        assert_eq!(lit("é'".as_bytes()), "'é'''".as_bytes());
    }

    #[test]
    fn an_identifier_doubles_double_quotes_and_leaves_backslashes() {
        let id = |s: &[u8]| escape_internal(s, Encoding::Utf8, true).unwrap();
        assert_eq!(id(b"tbl"), b"\"tbl\"");
        assert_eq!(id(b"a\"b"), b"\"a\"\"b\"");
        assert_eq!(id(b"a\\b"), b"\"a\\b\"");
        assert_eq!(id(b"'"), b"\"'\"");
    }

    #[test]
    fn the_input_stops_at_the_first_nul() {
        assert_eq!(
            escape_internal(b"some\0thing", Encoding::Utf8, false).unwrap(),
            b"'some'"
        );
        assert_eq!(
            escape_string(b"some\0thing", Encoding::Utf8, true).bytes,
            b"some"
        );
    }

    #[test]
    fn an_invalid_character_is_refused_by_literal_and_identifier() {
        assert_eq!(
            escape_internal(b"1\xE0", Encoding::Utf8, false),
            Err(EscapeError::IncompleteMultibyte)
        );
        assert_eq!(
            escape_internal(b"1\xE0'; ", Encoding::Utf8, true),
            Err(EscapeError::InvalidMultibyte)
        );
        // GB18030 needs a second byte to know the length at all.
        assert_eq!(
            escape_internal(b"\x80", Encoding::Gb18030, false),
            Err(EscapeError::IncompleteMultibyte)
        );
        assert_eq!(
            EscapeError::InvalidMultibyte.message(),
            b"invalid multibyte character\n"
        );
    }

    #[test]
    fn a_quote_inside_a_multibyte_character_is_not_doubled() {
        // SJIS 0x81 0x5c is one character whose trail byte is '\'.
        assert_eq!(
            escape_internal(b"\x81\\", Encoding::Sjis, false).unwrap(),
            b"'\x81\\'"
        );
        assert_eq!(
            escape_string(b"\x81\\", Encoding::Sjis, false).bytes,
            b"\x81\\"
        );
    }

    #[test]
    fn escape_string_replaces_a_bad_byte_and_reports_it_once() {
        let out = escape_string(b"1\xF0';", Encoding::Utf8, true);
        assert_eq!(out.bytes, b"1\xC0 '';");
        assert_eq!(out.error, Some(EscapeError::IncompleteMultibyte));

        let out = escape_string(b"1\xF0';;;;", Encoding::Utf8, true);
        assert_eq!(out.bytes, b"1\xC0 '';;;;");
        assert_eq!(out.error, Some(EscapeError::InvalidMultibyte));

        let out = escape_string(b"1\xE0", Encoding::Utf8, true);
        assert_eq!(out.bytes, b"1\xC0 ");
        assert_eq!(out.error, Some(EscapeError::IncompleteMultibyte));

        let out = escape_string(b"\\'", Encoding::Utf8, false);
        assert_eq!(out.bytes, b"\\\\''");
        assert_eq!(out.error, None);
    }

    #[test]
    fn bytea_hex_and_escape_formats() {
        assert_eq!(escape_bytea(b"\x00\xffA", true, true), b"\\x00ff41");
        assert_eq!(escape_bytea(b"\x00\xffA", false, true), b"\\\\x00ff41");
        assert_eq!(
            escape_bytea(b"\x00'\\A\x7f", true, false),
            b"\\000''\\\\A\\177"
        );
        assert_eq!(
            escape_bytea(b"\x00'\\A", false, false),
            b"\\\\000''\\\\\\\\A"
        );
    }

    #[test]
    fn unescape_bytea_reads_both_output_formats() {
        assert_eq!(unescape_bytea(b"\\x00ff41"), b"\x00\xffA");
        assert_eq!(unescape_bytea(b"\\x00 FF 4"), b"\x00\xff");
        assert_eq!(unescape_bytea(b"\\x0g41"), b"\x41");
        assert_eq!(unescape_bytea(b"\\000\\\\A\\377"), b"\x00\\A\xff");
        // An unrecognized escape drops the backslash; a trailing one is lost.
        assert_eq!(unescape_bytea(b"a\\qb\\"), b"aqb");
        assert_eq!(unescape_bytea(b"\\400"), b"400");
        assert_eq!(unescape_bytea(b"ab\0cd"), b"ab");
    }

    #[test]
    fn every_byte_survives_a_hex_escape_and_unescape_round_trip() {
        // Only the hex form is its own inverse: the escape form doubles `'`
        // for the SQL parser, which unescaping (byteaout's reader) never sees.
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(unescape_bytea(&escape_bytea(&all, true, true)), all);
    }
}
