//! `PQescapeLiteral` and `PQescapeIdentifier`: `PQescapeInternal`,
//! `src/interfaces/libpq/fe-exec.c:4245`.
//!
//! Escaping is a pure calculation over the input and the connection's client
//! encoding, which is all `PQescapeInternal` reads from `conn`; the
//! [`crate::Connection`] methods only supply the encoding.
//!
//! The encoding matters for bytes with the high bit set: a multibyte
//! character is copied whole, and the input must be valid in the encoding,
//! because an invalid sequence could swallow the quote that follows it on
//! the server's side. This port knows the character boundaries of UTF-8 and
//! of the single-byte encodings. The other multibyte encodings (EUC_*,
//! MULE_INTERNAL, and the client-only SJIS, BIG5, GBK, UHC, GB18030, JOHAB,
//! SHIFT_JIS_2004) are refused as soon as a high-bit byte appears, rather
//! than escaped with a guess (`docs/divergences.md`). ASCII input escapes
//! identically in every encoding.

/// What `PQescapeInternal` needs to know about `conn->client_encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientEncoding {
    /// `PG_UTF8`.
    Utf8,
    /// `PG_SQL_ASCII` and every encoding from `PG_LATIN1` to `PG_KOI8U`
    /// (`pg_wchar.h:250`-`:276`): one byte is one character.
    SingleByte,
    /// A multibyte encoding other than UTF-8, whose character boundaries
    /// this port does not know yet. Carries the name, for the refusal.
    Unported(&'static str),
}

/// The multibyte encodings other than UTF-8 (`pg_wchar.h:243`-`:249`,
/// `:280`-`:286`), by `clean_encoding_name`'s spelling of the name the
/// server reports and by that name.
const UNPORTED_MULTIBYTE: [(&str, &str); 13] = [
    ("eucjp", "EUC_JP"),
    ("euccn", "EUC_CN"),
    ("euckr", "EUC_KR"),
    ("euctw", "EUC_TW"),
    ("eucjis2004", "EUC_JIS_2004"),
    ("muleinternal", "MULE_INTERNAL"),
    ("sjis", "SJIS"),
    ("big5", "BIG5"),
    ("gbk", "GBK"),
    ("uhc", "UHC"),
    ("gb18030", "GB18030"),
    ("johab", "JOHAB"),
    ("shiftjis2004", "SHIFT_JIS_2004"),
];

impl ClientEncoding {
    /// `pg_char_to_encoding` (`encnames.c:552`) of a `client_encoding`
    /// ParameterStatus, as `pqSaveParameterStatus` stores it
    /// (`fe-exec.c:1147`-`:1150`): case and punctuation are ignored
    /// (`clean_encoding_name`, `encnames.c:527`), and a name libpq does not
    /// know is `PG_SQL_ASCII`, which is single-byte.
    ///
    /// Only the canonical names a server reports are recognized, plus
    /// `UNICODE`; `encnames.c`'s other aliases are for `SET client_encoding`,
    /// which the server answers with the canonical name.
    #[must_use]
    pub fn from_name(name: &[u8]) -> Self {
        let clean: String = name
            .iter()
            .filter(|b| b.is_ascii_alphanumeric())
            .map(|b| char::from(b.to_ascii_lowercase()))
            .collect();
        if clean == "utf8" || clean == "unicode" {
            return ClientEncoding::Utf8;
        }
        UNPORTED_MULTIBYTE
            .iter()
            .find(|(key, _)| *key == clean)
            .map_or(ClientEncoding::SingleByte, |(_, name)| {
                ClientEncoding::Unported(name)
            })
    }

    /// `pg_encoding_mblen_or_incomplete` (`wchar.c:2169`) with at least one
    /// byte remaining: the length the lead byte announces.
    fn mblen(self, lead: u8) -> usize {
        match self {
            // `pg_utf_mblen` (`wchar.c:556`).
            ClientEncoding::Utf8 => match lead {
                _ if lead & 0x80 == 0 => 1,
                _ if lead & 0xe0 == 0xc0 => 2,
                _ if lead & 0xf0 == 0xe0 => 3,
                _ if lead & 0xf8 == 0xf0 => 4,
                _ => 1,
            },
            ClientEncoding::SingleByte | ClientEncoding::Unported(_) => 1,
        }
    }

    /// `pg_encoding_verifymbstr(encoding, s, len) == len`: whether all of
    /// `s` is valid. `s` holds no NUL, which every verifier rejects.
    fn verifies(self, s: &[u8]) -> bool {
        match self {
            // `pg_utf8_verifystr` (`wchar.c:1913`) accepts what
            // `pg_utf8_islegal` (`:2011`) does: no overlong form, no
            // surrogate, nothing above U+10FFFF — exactly Rust's UTF-8.
            ClientEncoding::Utf8 => std::str::from_utf8(s).is_ok(),
            // The single-byte verifiers reject only a NUL.
            ClientEncoding::SingleByte | ClientEncoding::Unported(_) => true,
        }
    }
}

/// Why `PQescapeLiteral` / `PQescapeIdentifier` returned NULL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscapeError {
    /// A multibyte character runs past the end of the input
    /// (`fe-exec.c:4286`).
    IncompleteMultibyte,
    /// The input is not valid in the client encoding (`fe-exec.c:4305`).
    InvalidMultibyte,
    /// A high-bit byte in a multibyte encoding this port cannot step
    /// through yet. C escapes it; this port's own message.
    UnportedEncoding(&'static str),
}

impl EscapeError {
    /// The bytes libpq's error buffer would hold, without the newline
    /// `libpq_append_conn_error` adds.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        match self {
            EscapeError::IncompleteMultibyte => b"incomplete multibyte character".to_vec(),
            EscapeError::InvalidMultibyte => b"invalid multibyte character".to_vec(),
            EscapeError::UnportedEncoding(name) => format!(
                "escaping a non-ASCII byte in client encoding {name} is not implemented yet"
            )
            .into_bytes(),
        }
    }
}

impl std::fmt::Display for EscapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", String::from_utf8_lossy(&self.message()))
    }
}

impl std::error::Error for EscapeError {}

/// `PQescapeLiteral` (`fe-exec.c:4413`): `input` as a string literal,
/// quotes doubled, and in `E''` syntax with a leading space if it holds a
/// backslash, so that it reads the same under either
/// `standard_conforming_strings`.
///
/// # Errors
/// The input is not valid in `encoding`, or `encoding` is one this port
/// cannot step through ([`EscapeError`]).
pub fn escape_literal(input: &[u8], encoding: ClientEncoding) -> Result<Vec<u8>, EscapeError> {
    escape_internal(input, encoding, false)
}

/// `PQescapeIdentifier` (`fe-exec.c:4419`): `input` as a quoted identifier,
/// double quotes doubled.
///
/// # Errors
/// As [`escape_literal`].
pub fn escape_identifier(input: &[u8], encoding: ClientEncoding) -> Result<Vec<u8>, EscapeError> {
    escape_internal(input, encoding, true)
}

/// `PQescapeInternal` (`fe-exec.c:4245`).
fn escape_internal(
    input: &[u8],
    encoding: ClientEncoding,
    as_ident: bool,
) -> Result<Vec<u8>, EscapeError> {
    // `strnlen(str, len)`: the input ends at its first NUL.
    let input = &input[..input.iter().position(|&b| b == 0).unwrap_or(input.len())];
    let quote_char = if as_ident { b'"' } else { b'\'' };

    // Scan for characters that must be escaped and for invalidly encoded
    // data (`:4264`-`:4314`).
    let mut num_quotes = 0;
    let mut num_backslashes = 0;
    let mut validated_mb = false;
    let mut i = 0;
    while i < input.len() {
        let c = input[i];
        if c == quote_char {
            num_quotes += 1;
        } else if c == b'\\' {
            num_backslashes += 1;
        } else if c & 0x80 != 0 {
            if let ClientEncoding::Unported(name) = encoding {
                return Err(EscapeError::UnportedEncoding(name));
            }
            let remaining = input.len() - i;
            let charlen = encoding.mblen(c);
            if charlen > remaining {
                return Err(EscapeError::IncompleteMultibyte);
            }
            // Validity is checked once, for the whole remainder, at the
            // first multibyte character.
            if !validated_mb {
                if !encoding.verifies(&input[i..]) {
                    return Err(EscapeError::InvalidMultibyte);
                }
                validated_mb = true;
            }
            i += charlen;
            continue;
        }
        i += 1;
    }

    let backslashed = !as_ident && num_backslashes > 0;
    let mut result =
        Vec::with_capacity(input.len() + num_quotes + 3 + 3 * usize::from(backslashed));
    if backslashed {
        result.extend_from_slice(b" E");
    }
    result.push(quote_char);
    // The input is well-formed, so a multibyte character never holds a quote
    // or a backslash byte in the encodings stepped through here: doubling
    // byte by byte is `:4372`-`:4394`'s character walk.
    for &c in input {
        if c == quote_char || (!as_ident && c == b'\\') {
            result.push(c);
        }
        result.push(c);
    }
    result.push(quote_char);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn literal(input: &[u8]) -> Result<Vec<u8>, EscapeError> {
        escape_literal(input, ClientEncoding::Utf8)
    }

    #[test]
    fn a_literal_doubles_its_quotes_and_goes_to_e_syntax_for_a_backslash() {
        assert_eq!(literal(b"three").unwrap(), b"'three'");
        assert_eq!(literal(b"it's").unwrap(), b"'it''s'");
        assert_eq!(literal(b"dirty\\name").unwrap(), b" E'dirty\\\\name'");
        assert_eq!(literal(b"a\\'b").unwrap(), b" E'a\\\\''b'");
        assert_eq!(literal(b"\"").unwrap(), b"'\"'");
        assert_eq!(literal(b"").unwrap(), b"''");
    }

    #[test]
    fn an_identifier_doubles_its_double_quotes_and_leaves_backslashes() {
        let ident = |s: &[u8]| escape_identifier(s, ClientEncoding::Utf8).unwrap();
        assert_eq!(ident(b"dirty\\name"), b"\"dirty\\name\"");
        assert_eq!(ident(b"a\"b"), b"\"a\"\"b\"");
        assert_eq!(ident(b"it's"), b"\"it's\"");
    }

    #[test]
    fn a_multibyte_character_is_copied_whole() {
        assert_eq!(literal("zé'".as_bytes()).unwrap(), "'zé'''".as_bytes());
        assert_eq!(
            escape_literal(b"\xe9'", ClientEncoding::SingleByte).unwrap(),
            b"'\xe9'''"
        );
    }

    #[test]
    fn the_client_encoding_is_read_as_pg_char_to_encoding_reads_it() {
        assert_eq!(ClientEncoding::from_name(b"UTF8"), ClientEncoding::Utf8);
        assert_eq!(ClientEncoding::from_name(b"utf-8"), ClientEncoding::Utf8);
        assert_eq!(ClientEncoding::from_name(b"UNICODE"), ClientEncoding::Utf8);
        assert_eq!(
            ClientEncoding::from_name(b"SQL_ASCII"),
            ClientEncoding::SingleByte
        );
        assert_eq!(
            ClientEncoding::from_name(b"LATIN1"),
            ClientEncoding::SingleByte
        );
        // An unknown name is PG_SQL_ASCII (`fe-exec.c:1150`).
        assert_eq!(
            ClientEncoding::from_name(b"no such"),
            ClientEncoding::SingleByte
        );
        assert_eq!(
            ClientEncoding::from_name(b"SHIFT_JIS_2004"),
            ClientEncoding::Unported("SHIFT_JIS_2004")
        );
    }

    #[test]
    fn an_unported_multibyte_encoding_refuses_a_high_byte_but_escapes_ascii() {
        let sjis = ClientEncoding::from_name(b"SJIS");
        assert_eq!(escape_literal(b"it's", sjis).unwrap(), b"'it''s'");
        let err = escape_literal(b"\xf0\x40;", sjis).unwrap_err();
        assert_eq!(
            err.message(),
            b"escaping a non-ASCII byte in client encoding SJIS is not implemented yet"
        );
    }

    /// `pe_test_vectors[]`, `src/test/modules/test_escape/test_escape.c:445`,
    /// the rows this port escapes (UTF-8 and `sql_ascii`), in order: `TV`
    /// then `TV_LEN`.
    const PE_TEST_VECTORS: &[(&str, &[u8])] = &[
        ("UTF-8", b"1"),
        ("UTF-8", b"'"),
        ("UTF-8", b"\""),
        ("UTF-8", b"\'"),
        ("UTF-8", b"\""),
        ("UTF-8", b"\\"),
        ("UTF-8", b"\\'"),
        ("UTF-8", b"\\\""),
        ("UTF-8", b"1\xC0"),
        ("UTF-8", b"1\xE0 "),
        ("UTF-8", b"1\xF0 "),
        ("UTF-8", b"1\xF0  "),
        ("UTF-8", b"1\xF0   "),
        ("UTF-8", b"1\xE0"),
        ("UTF-8", b"1\xF0"),
        ("UTF-8", b"\xF0"),
        ("UTF-8", b"1\xE0'"),
        ("UTF-8", b"1\xE0\""),
        ("UTF-8", b"1\xF0'"),
        ("UTF-8", b"1\xF0\""),
        ("UTF-8", b"1\xF0'; "),
        ("UTF-8", b"1\xF0\"; "),
        ("UTF-8", b"1\xF0';;;;"),
        ("UTF-8", b"1\xF0  ';;;;"),
        ("UTF-8", b"1\xF0  \";;;;"),
        ("UTF-8", b"1\xE0'; \\l ; "),
        ("UTF-8", b"1\xE0\"; \\l ; "),
        ("UTF-8", b"some\0thing"),
        ("UTF-8", b"some\0"),
        ("UTF-8", b"some\xF0'\0"),
        ("UTF-8", b"some\xF0'\0'"),
        ("UTF-8", b"some\xF0ab\0'"),
        ("sql_ascii", b"1\xC0'"),
        // TV_LEN("UTF-8", "\xC3\xb6  ", 1) and (…, 2).
        ("UTF-8", b"\xC3"),
        ("UTF-8", b"\xC3\xb6"),
    ];

    /// `test_one_vector_escape` (`test_escape.c:634`) for `PQescapeLiteral`
    /// and `PQescapeIdentifier`, both `reports_errors` and
    /// `supports_input_length` (`:396`-`:407`): nothing past the input is
    /// escaped, an input valid up to its first NUL escapes and an invalid
    /// one fails, and a valid input's escaped form is valid.
    #[test]
    fn test_one_vector_escape() {
        const NEVER_ACCESS_STR: &[u8] = b"\xff never-to-be-touched";
        for (name, vector) in PE_TEST_VECTORS {
            let encoding = ClientEncoding::from_name(name.as_bytes());
            let till0 = &vector[..vector.iter().position(|&b| b == 0).unwrap_or(vector.len())];
            let input_encoding0_valid = encoding.verifies(till0);
            let mut raw = vector.to_vec();
            raw.extend_from_slice(NEVER_ACCESS_STR);
            for (func, escape) in [
                (
                    "PQescapeLiteral",
                    escape_literal as fn(&[u8], ClientEncoding) -> _,
                ),
                ("PQescapeIdentifier", escape_identifier),
            ] {
                let testname = format!("{vector:?} - {name} - {func}");
                match escape(&raw[..vector.len()], encoding) {
                    Ok(escaped) => {
                        assert!(
                            input_encoding0_valid,
                            "{testname}: invalid input escaped successfully"
                        );
                        assert!(
                            !escaped
                                .windows(NEVER_ACCESS_STR.len())
                                .any(|w| w == NEVER_ACCESS_STR),
                            "{testname}: escaped data beyond end of input"
                        );
                        assert!(
                            encoding.verifies(&escaped),
                            "{testname}: valid input produced invalid output"
                        );
                    }
                    Err(_) => assert!(
                        !input_encoding0_valid,
                        "{testname}: valid input failed to escape"
                    ),
                }
            }
        }
    }

    #[test]
    fn a_short_multibyte_character_is_incomplete_and_a_bad_one_invalid() {
        assert_eq!(literal(b"1\xE0"), Err(EscapeError::IncompleteMultibyte));
        // Long enough for the three bytes \xE0 announces, but not UTF-8.
        assert_eq!(literal(b"1\xE0''"), Err(EscapeError::InvalidMultibyte));
        assert_eq!(
            EscapeError::IncompleteMultibyte.message(),
            b"incomplete multibyte character"
        );
        assert_eq!(
            EscapeError::InvalidMultibyte.message(),
            b"invalid multibyte character"
        );
    }
}
