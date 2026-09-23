//! Server encoding names, ported from `src/common/encnames.c` and
//! `src/include/mb/pg_wchar.h` (PostgreSQL 18.6).
//!
//! `initdb` needs three things from that file and nothing else: turn the
//! `-E`/`--encoding` string into an encoding (`pg_char_to_encoding`,
//! `encnames.c:552`), reject the ones that cannot be a *server* encoding
//! (`PG_VALID_BE_ENCODING`, `pg_wchar.h:297`), and recognize UTF-8 for the
//! builtin-provider rule at `initdb.c:2781`.
//!
//! All of it is a pure calculation over a static table, so it is unit-tested
//! without a filesystem or a server.

/// `enum pg_enc` (`src/include/mb/pg_wchar.h:240`), in declaration order.
///
/// The order is load-bearing: `PG_VALID_BE_ENCODING` is a range check against
/// `PG_ENCODING_BE_LAST` (`PG_KOI8U`), so everything declared after it is
/// client-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Encoding {
    SqlAscii = 0,
    EucJp,
    EucCn,
    EucKr,
    EucTw,
    EucJis2004,
    Utf8,
    MuleInternal,
    Latin1,
    Latin2,
    Latin3,
    Latin4,
    Latin5,
    Latin6,
    Latin7,
    Latin8,
    Latin9,
    Latin10,
    Win1256,
    Win1258,
    Win866,
    Win874,
    Koi8R,
    Win1251,
    Win1252,
    Iso8859_5,
    Iso8859_6,
    Iso8859_7,
    Iso8859_8,
    Win1250,
    Win1253,
    Win1254,
    Win1255,
    Win1257,
    /// `PG_ENCODING_BE_LAST`: the last encoding a server may use.
    Koi8U,
    Sjis,
    Big5,
    Gbk,
    Uhc,
    Gb18030,
    Johab,
    ShiftJis2004,
}

impl Encoding {
    /// `PG_VALID_BE_ENCODING` (`pg_wchar.h:297`): may this be a server encoding?
    #[must_use]
    pub fn is_valid_server_encoding(self) -> bool {
        (self as u8) <= (Encoding::Koi8U as u8)
    }
}

/// `pg_encname_tbl[]` (`encnames.c:39`): every accepted spelling, already
/// cleaned, sorted as upstream sorts it for its binary search.
///
/// Copied entry for entry; the aliases matter, because `--encoding UTF-8`,
/// `--encoding utf8` and `--encoding Unicode` must all mean the same thing.
const ENCNAME_TBL: &[(&str, Encoding)] = &[
    ("abc", Encoding::Win1258),
    ("alt", Encoding::Win866),
    ("big5", Encoding::Big5),
    ("euccn", Encoding::EucCn),
    ("eucjis2004", Encoding::EucJis2004),
    ("eucjp", Encoding::EucJp),
    ("euckr", Encoding::EucKr),
    ("euctw", Encoding::EucTw),
    ("gb18030", Encoding::Gb18030),
    ("gbk", Encoding::Gbk),
    ("iso88591", Encoding::Latin1),
    ("iso885910", Encoding::Latin6),
    ("iso885913", Encoding::Latin7),
    ("iso885914", Encoding::Latin8),
    ("iso885915", Encoding::Latin9),
    ("iso885916", Encoding::Latin10),
    ("iso88592", Encoding::Latin2),
    ("iso88593", Encoding::Latin3),
    ("iso88594", Encoding::Latin4),
    ("iso88595", Encoding::Iso8859_5),
    ("iso88596", Encoding::Iso8859_6),
    ("iso88597", Encoding::Iso8859_7),
    ("iso88598", Encoding::Iso8859_8),
    ("iso88599", Encoding::Latin5),
    ("johab", Encoding::Johab),
    ("koi8", Encoding::Koi8R),
    ("koi8r", Encoding::Koi8R),
    ("koi8u", Encoding::Koi8U),
    ("latin1", Encoding::Latin1),
    ("latin10", Encoding::Latin10),
    ("latin2", Encoding::Latin2),
    ("latin3", Encoding::Latin3),
    ("latin4", Encoding::Latin4),
    ("latin5", Encoding::Latin5),
    ("latin6", Encoding::Latin6),
    ("latin7", Encoding::Latin7),
    ("latin8", Encoding::Latin8),
    ("latin9", Encoding::Latin9),
    ("mskanji", Encoding::Sjis),
    ("muleinternal", Encoding::MuleInternal),
    ("shiftjis", Encoding::Sjis),
    ("shiftjis2004", Encoding::ShiftJis2004),
    ("sjis", Encoding::Sjis),
    ("sqlascii", Encoding::SqlAscii),
    ("tcvn", Encoding::Win1258),
    ("tcvn5712", Encoding::Win1258),
    ("uhc", Encoding::Uhc),
    ("unicode", Encoding::Utf8),
    ("utf8", Encoding::Utf8),
    ("vscii", Encoding::Win1258),
    ("win", Encoding::Win1251),
    ("win1250", Encoding::Win1250),
    ("win1251", Encoding::Win1251),
    ("win1252", Encoding::Win1252),
    ("win1253", Encoding::Win1253),
    ("win1254", Encoding::Win1254),
    ("win1255", Encoding::Win1255),
    ("win1256", Encoding::Win1256),
    ("win1257", Encoding::Win1257),
    ("win1258", Encoding::Win1258),
    ("win866", Encoding::Win866),
    ("win874", Encoding::Win874),
    ("win932", Encoding::Sjis),
    ("win936", Encoding::Gbk),
    ("win949", Encoding::Uhc),
    ("win950", Encoding::Big5),
    ("windows1250", Encoding::Win1250),
    ("windows1251", Encoding::Win1251),
    ("windows1252", Encoding::Win1252),
    ("windows1253", Encoding::Win1253),
    ("windows1254", Encoding::Win1254),
    ("windows1255", Encoding::Win1255),
    ("windows1256", Encoding::Win1256),
    ("windows1257", Encoding::Win1257),
    ("windows1258", Encoding::Win1258),
    ("windows866", Encoding::Win866),
    ("windows874", Encoding::Win874),
    ("windows932", Encoding::Sjis),
    ("windows936", Encoding::Gbk),
    ("windows949", Encoding::Uhc),
    ("windows950", Encoding::Big5),
];

/// `NAMEDATALEN` (`src/include/pg_config_manual.h:29`): `pg_char_to_encoding`
/// rejects anything this long or longer before it even cleans the name.
const NAMEDATALEN: usize = 64;

/// `clean_encoding_name` (`encnames.c:527`): drop every non-alphanumeric byte
/// and lowercase the ASCII letters.
///
/// Upstream works on bytes with `isalnum()` in the C locale, so only ASCII
/// alphanumerics survive; a multibyte character is dropped byte by byte, which
/// this reproduces by filtering on `u8::is_ascii_alphanumeric`.
#[must_use]
pub fn clean_encoding_name(key: &str) -> String {
    key.bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|b| char::from(b.to_ascii_lowercase()))
        .collect()
}

/// `pg_char_to_encoding` (`encnames.c:552`): the encoding a spelling names.
#[must_use]
pub fn char_to_encoding(name: &str) -> Option<Encoding> {
    if name.is_empty() || name.len() >= NAMEDATALEN {
        return None;
    }
    let key = clean_encoding_name(name);
    ENCNAME_TBL
        .binary_search_by(|(candidate, _)| (*candidate).cmp(key.as_str()))
        .ok()
        .map(|index| ENCNAME_TBL[index].1)
}

/// `pg_valid_server_encoding` (`encnames.c:502`): the encoding a spelling
/// names, if it may be used as a server encoding.
#[must_use]
pub fn valid_server_encoding(name: &str) -> Option<Encoding> {
    char_to_encoding(name).filter(|encoding| encoding.is_valid_server_encoding())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_and_has_no_duplicates() {
        // Upstream binary-searches it (encnames.c:570), so the order is part
        // of the port, not an accident of transcription.
        for pair in ENCNAME_TBL.windows(2) {
            assert!(pair[0].0 < pair[1].0, "{} then {}", pair[0].0, pair[1].0);
        }
    }

    #[test]
    fn every_table_name_is_already_clean() {
        for (name, _) in ENCNAME_TBL {
            assert_eq!(&clean_encoding_name(name), name);
        }
    }

    #[test]
    fn punctuation_and_case_are_stripped_from_the_key() {
        assert_eq!(clean_encoding_name("UTF-8"), "utf8");
        assert_eq!(clean_encoding_name("SQL_ASCII"), "sqlascii");
        assert_eq!(clean_encoding_name("ISO-8859-15"), "iso885915");
        assert_eq!(clean_encoding_name("  "), "");
    }

    #[test]
    fn the_utf8_spellings_all_land_on_one_encoding() {
        for spelling in ["UTF8", "utf-8", "UTF_8", "Unicode", "unicode"] {
            assert_eq!(
                char_to_encoding(spelling),
                Some(Encoding::Utf8),
                "{spelling}"
            );
        }
    }

    #[test]
    fn sql_ascii_is_a_server_encoding_and_is_not_utf8() {
        // The pairing the builtin-provider check at initdb.c:2781 turns on.
        let encoding = valid_server_encoding("SQL_ASCII").expect("SQL_ASCII is a server encoding");
        assert_eq!(encoding, Encoding::SqlAscii);
        assert_ne!(encoding, Encoding::Utf8);
    }

    #[test]
    fn client_only_encodings_are_not_valid_server_encodings() {
        // Everything after PG_ENCODING_BE_LAST (= PG_KOI8U) in pg_wchar.h.
        for name in [
            "SJIS",
            "BIG5",
            "GBK",
            "UHC",
            "GB18030",
            "JOHAB",
            "SHIFT_JIS_2004",
        ] {
            assert!(char_to_encoding(name).is_some(), "{name} is an encoding");
            assert_eq!(valid_server_encoding(name), None, "{name}");
        }
        assert!(Encoding::Koi8U.is_valid_server_encoding());
        assert!(!Encoding::Sjis.is_valid_server_encoding());
    }

    #[test]
    fn an_unknown_or_overlong_name_is_not_an_encoding() {
        assert_eq!(char_to_encoding("nonsense"), None);
        assert_eq!(char_to_encoding(""), None);
        assert_eq!(char_to_encoding(&"u".repeat(NAMEDATALEN)), None);
    }

    #[test]
    fn the_enum_ordinals_match_pg_enc() {
        // pg_wchar.h:240. Only the boundaries need pinning; the rest follow.
        assert_eq!(Encoding::SqlAscii as u8, 0);
        assert_eq!(Encoding::Utf8 as u8, 6);
        assert_eq!(Encoding::Koi8U as u8, 34);
        assert_eq!(Encoding::ShiftJis2004 as u8, 41);
    }
}
