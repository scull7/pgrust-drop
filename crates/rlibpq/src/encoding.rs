//! Client encodings: what libpq needs from `src/common/encnames.c` and
//! `src/common/wchar.c` (PostgreSQL 18.6) to step over and verify a
//! multibyte character.
//!
//! libpq tracks the server-reported `client_encoding` as a `pg_enc`
//! (`fe-exec.c:1145`) so that the escape functions can tell a lead byte from
//! a quote hidden inside a multibyte character. That needs the name lookup
//! (`pg_char_to_encoding`), each encoding's length function (`mblen`) and
//! its validators (`mbverifychar`, `mbverifystr`); and the error cursor
//! (`reportErrorPosition`, `fe-protocol3.c:1202`) needs each encoding's
//! display width (`dsplen`, over `ucs_wcwidth` for UTF-8). Nothing else from
//! the `pg_wchar_table` (`wchar.c:2086`) is here: no conversion to or from
//! `pg_wchar`.
//!
//! Every function here is a pure calculation over a byte slice. Where C
//! reads a NUL-terminated string past the slice's end (GB18030's `mblen`
//! looks at the second byte), a missing byte reads as the NUL C would find.

use std::fmt;

/// `enum pg_enc` (`src/include/mb/pg_wchar.h:240`), in declaration order.
///
/// The order is load-bearing: everything after `PG_ENCODING_BE_LAST`
/// (`Koi8U`, `pg_wchar.h:291`) is a client-only encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[repr(u8)]
pub enum Encoding {
    /// `PG_SQL_ASCII`, the default before the server reports anything
    /// (`pqMakeEmptyPGconn`, `fe-connect.c:4985`) and the fallback for an unknown name
    /// (`fe-exec.c:1150`).
    #[default]
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

/// `SS2`, `pg_wchar.h:38`: single shift 2.
const SS2: u8 = 0x8e;
/// `SS3`, `pg_wchar.h:39`: single shift 3.
const SS3: u8 = 0x8f;

/// `NONUTF8_INVALID_BYTE0` / `NONUTF8_INVALID_BYTE1`, `wchar.c:36`-`:37`:
/// the two-byte sequence every multibyte non-UTF-8 encoding rejects.
const NONUTF8_INVALID_BYTE0: u8 = 0x8d;
const NONUTF8_INVALID_BYTE1: u8 = b' ';

/// `NAMEDATALEN`, `src/include/pg_config_manual.h:29`.
const NAMEDATALEN: usize = 64;

/// `IS_HIGHBIT_SET`, `src/include/c.h`.
fn is_highbit_set(c: u8) -> bool {
    c & 0x80 != 0
}

/// `IS_EUC_RANGE_VALID`, `wchar.c:1101`.
fn is_euc_range_valid(c: u8) -> bool {
    (0xa1..=0xfe).contains(&c)
}

/// `ISSJISHEAD`, `pg_wchar.h:44`.
fn is_sjis_head(c: u8) -> bool {
    (0x81..=0x9f).contains(&c) || (0xe0..=0xfc).contains(&c)
}

/// `ISSJISTAIL`, `pg_wchar.h:45`.
fn is_sjis_tail(c: u8) -> bool {
    (0x40..=0x7e).contains(&c) || (0x80..=0xfc).contains(&c)
}

/// The byte at `i`, or the NUL a C string would end in.
fn byte_at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// Which of `wchar.c`'s function families an encoding uses: the columns of
/// `pg_wchar_table` (`wchar.c:2086`-`:2129`) that matter here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    /// `pg_ascii_*`.
    Ascii,
    /// `pg_eucjp_*` (also `PG_EUC_JIS_2004`).
    EucJp,
    /// `pg_euccn_mblen` with the EUC-KR validators (`wchar.c:1245`).
    EucCn,
    /// `pg_euckr_*`.
    EucKr,
    /// `pg_euctw_*`.
    EucTw,
    /// `pg_utf_mblen`, `pg_utf8_verify*`.
    Utf8,
    /// `pg_mule_*`.
    Mule,
    /// `pg_latin1_*`: every single-byte encoding but SQL_ASCII.
    Latin1,
    /// `pg_sjis_*` (also `PG_SHIFT_JIS_2004`).
    Sjis,
    /// `pg_big5_*`.
    Big5,
    /// `pg_gbk_*`.
    Gbk,
    /// `pg_uhc_*`.
    Uhc,
    /// `pg_gb18030_*`.
    Gb18030,
    /// `pg_johab_*`.
    Johab,
}

impl Encoding {
    /// `pg_char_to_encoding`, `encnames.c:552`: the encoding a name spells,
    /// or `None` (C's `-1`).
    #[must_use]
    pub fn from_name(name: &[u8]) -> Option<Encoding> {
        if name.is_empty() || name.len() >= NAMEDATALEN {
            return None;
        }
        // clean_encoding_name, encnames.c:527: keep ASCII alphanumerics
        // (isalnum in the C locale), lowercase them.
        let key: Vec<u8> = name
            .iter()
            .filter(|b| b.is_ascii_alphanumeric())
            .map(u8::to_ascii_lowercase)
            .collect();
        ENCNAME_TBL
            .binary_search_by(|(candidate, _)| candidate.as_bytes().cmp(&key))
            .ok()
            .map(|index| ENCNAME_TBL[index].1)
    }

    /// `PG_ENCODING_IS_CLIENT_ONLY`, `pg_wchar.h:300`.
    #[must_use]
    pub fn is_client_only(self) -> bool {
        self > Encoding::Koi8U
    }

    fn family(self) -> Family {
        match self {
            Encoding::SqlAscii => Family::Ascii,
            Encoding::EucJp | Encoding::EucJis2004 => Family::EucJp,
            Encoding::EucCn => Family::EucCn,
            Encoding::EucKr => Family::EucKr,
            Encoding::EucTw => Family::EucTw,
            Encoding::Utf8 => Family::Utf8,
            Encoding::MuleInternal => Family::Mule,
            Encoding::Sjis | Encoding::ShiftJis2004 => Family::Sjis,
            Encoding::Big5 => Family::Big5,
            Encoding::Gbk => Family::Gbk,
            Encoding::Uhc => Family::Uhc,
            Encoding::Gb18030 => Family::Gb18030,
            Encoding::Johab => Family::Johab,
            _ => Family::Latin1,
        }
    }

    /// `pg_encoding_max_length`, `wchar.c:2235`: the table's `maxmblen`.
    #[must_use]
    pub fn max_length(self) -> usize {
        match self.family() {
            Family::Ascii | Family::Latin1 => 1,
            Family::Sjis | Family::Big5 | Family::Gbk | Family::Uhc => 2,
            Family::EucJp | Family::EucCn | Family::EucKr | Family::Johab => 3,
            Family::EucTw | Family::Utf8 | Family::Mule | Family::Gb18030 => 4,
        }
    }

    /// `pg_encoding_mblen`, `wchar.c:2157`: the length the character at
    /// `s[0]` claims, read as C reads a NUL-terminated string. `s` must not
    /// be empty.
    #[must_use]
    pub fn mblen(self, s: &[u8]) -> usize {
        let c = s[0];
        match self.family() {
            Family::Ascii | Family::Latin1 => 1,
            Family::EucJp | Family::EucKr | Family::Johab => euc_mblen(c),
            Family::EucCn => euccn_mblen(c),
            Family::EucTw => euctw_mblen(c),
            Family::Utf8 => utf_mblen(c),
            Family::Mule => mule_mblen(c),
            Family::Sjis => {
                // pg_sjis_mblen, wchar.c:913
                if (0xa1..=0xdf).contains(&c) || !is_highbit_set(c) {
                    1
                } else {
                    2
                }
            }
            // pg_big5_mblen, pg_gbk_mblen, pg_uhc_mblen: wchar.c:944, :971, :998
            Family::Big5 | Family::Gbk | Family::Uhc => {
                if is_highbit_set(c) {
                    2
                } else {
                    1
                }
            }
            Family::Gb18030 => {
                // pg_gb18030_mblen, wchar.c:1037
                if !is_highbit_set(c) {
                    1
                } else if (0x30..=0x39).contains(&byte_at(s, 1)) {
                    4
                } else {
                    2
                }
            }
        }
    }

    /// `pg_encoding_mblen_or_incomplete`, `wchar.c:2169`: [`Encoding::mblen`]
    /// of a string with `s.len()` bytes remaining, or `None` (C's `INT_MAX`)
    /// when too few remain to tell.
    #[must_use]
    pub fn mblen_or_incomplete(self, s: &[u8]) -> Option<usize> {
        if s.is_empty() || (self == Encoding::Gb18030 && is_highbit_set(s[0]) && s.len() < 2) {
            return None;
        }
        Some(self.mblen(s))
    }

    /// `PQmblenBounded`, `fe-misc.c:1410`: [`Encoding::mblen`], cut short at
    /// the first NUL or the end of `s` (C's `strnlen`). Zero only for an
    /// empty `s` or one starting with a NUL.
    #[must_use]
    pub fn mblen_bounded(self, s: &[u8]) -> usize {
        if s.is_empty() {
            return 0;
        }
        let len = self.mblen(s).min(s.len());
        s[..len].iter().position(|&c| c == 0).unwrap_or(len)
    }

    /// `pg_encoding_dsplen`, `wchar.c:2198`: the screen width of the
    /// character at `s[0]` — 0 for a NUL, -1 for a control character, and
    /// otherwise each encoding's `dsplen` from `pg_wchar_table`
    /// (`wchar.c:2086`). An empty `s` reads as the NUL C would find.
    #[must_use]
    pub fn dsplen(self, s: &[u8]) -> i32 {
        let c = byte_at(s, 0);
        let wide_or_ascii = |wide: bool| if wide { 2 } else { ascii_dsplen(c) };
        match self.family() {
            // pg_ascii_dsplen, wchar.c:94, and pg_latin1_dsplen, :904.
            Family::Ascii | Family::Latin1 => ascii_dsplen(c),
            // pg_euc_dsplen, wchar.c:165, for EUC_KR (:227), EUC_TW (:376,
            // the same body) and JOHAB (:450).
            Family::EucKr | Family::EucTw | Family::Johab => {
                wide_or_ascii(c == SS2 || c == SS3 || is_highbit_set(c))
            }
            // pg_eucjp_dsplen, wchar.c:196: SS2 is half-width kana.
            Family::EucJp if c == SS2 => 1,
            Family::EucJp => wide_or_ascii(c == SS3 || is_highbit_set(c)),
            // pg_euccn_dsplen, wchar.c:301; pg_big5_dsplen, :956;
            // pg_gbk_dsplen, :983; pg_uhc_dsplen, :1010; pg_gb18030_dsplen,
            // :1051.
            Family::EucCn | Family::Big5 | Family::Gbk | Family::Uhc | Family::Gb18030 => {
                wide_or_ascii(is_highbit_set(c))
            }
            // pg_utf_dsplen, wchar.c:680.
            Family::Utf8 => crate::wcwidth::ucs_wcwidth(crate::wcwidth::utf8_to_unicode(s)),
            // pg_mule_dsplen, wchar.c:833: IS_LC2 and IS_LCPRV2 are double
            // width, everything else — controls included — is 1.
            Family::Mule => {
                if (0x90..=0x99).contains(&c) || c == 0x9c || c == 0x9d {
                    2
                } else {
                    1
                }
            }
            // pg_sjis_dsplen, wchar.c:927: 0xa1-0xdf is half-width kana.
            Family::Sjis if (0xa1..=0xdf).contains(&c) => 1,
            Family::Sjis => wide_or_ascii(is_highbit_set(c)),
        }
    }

    /// `pg_encoding_verifymbchar`, `wchar.c:2211`: the length of the validly
    /// encoded character at the start of `s`, `None` (C's `-1`) if it is not
    /// one. `s.len()` is C's `len`, the bytes remaining; `s` must not be
    /// empty.
    #[must_use]
    pub fn verify_char(self, s: &[u8]) -> Option<usize> {
        match self.family() {
            Family::Ascii | Family::Latin1 => Some(1),
            Family::EucJp => eucjp_verifychar(s),
            Family::EucCn | Family::EucKr => euckr_verifychar(s),
            Family::EucTw => euctw_verifychar(s),
            Family::Utf8 => utf8_verifychar(s),
            Family::Mule => {
                // pg_mule_verifychar, wchar.c:1382
                let l = mule_mblen(s[0]);
                (s.len() >= l && s[1..l].iter().all(|&c| is_highbit_set(c))).then_some(l)
            }
            Family::Johab => {
                // pg_johab_verifychar, wchar.c:1329
                let l = euc_mblen(s[0]);
                if s.len() < l {
                    return None;
                }
                (!is_highbit_set(s[0]) || s[1..l].iter().all(|&c| is_euc_range_valid(c)))
                    .then_some(l)
            }
            Family::Sjis => {
                // pg_sjis_verifychar, wchar.c:1449
                let l = self.mblen(s);
                if s.len() < l {
                    return None;
                }
                if l == 1 {
                    return Some(1);
                }
                (is_sjis_head(s[0]) && is_sjis_tail(s[1])).then_some(l)
            }
            // pg_big5_verifychar, pg_gbk_verifychar, pg_uhc_verifychar:
            // wchar.c:1501, :1555, :1609 are the same body.
            Family::Big5 | Family::Gbk | Family::Uhc => {
                let l = self.mblen(s);
                if s.len() < l {
                    return None;
                }
                if l == 2 && s[0] == NONUTF8_INVALID_BYTE0 && s[1] == NONUTF8_INVALID_BYTE1 {
                    return None;
                }
                s[1..l].iter().all(|&c| c != 0).then_some(l)
            }
            Family::Gb18030 => gb18030_verifychar(s),
        }
    }

    /// `pg_encoding_verifymbstr`, `wchar.c:2224`: how many leading bytes of
    /// `s` form a valid string — `s.len()` if all do. A NUL ends it.
    ///
    /// Every `*_verifystr` upstream is this loop (`pg_eucjp_verifystr`,
    /// `wchar.c:1159`, is the pattern). The two that are not written as it
    /// compute the same thing faster: `pg_ascii_verifystr` / `pg_latin1_verifystr`
    /// (`:1091`, `:1438`) are a `memchr` for the NUL, which this loop is
    /// when every byte verifies alone, and `pg_utf8_verifystr` (`:1913`) runs
    /// a DFA over strides and then falls back to exactly this loop on the
    /// remainder, and on the whole string once the DFA reports an error.
    #[must_use]
    pub fn verify_str(self, s: &[u8]) -> usize {
        let mut i = 0;
        while i < s.len() {
            let l = if is_highbit_set(s[i]) {
                match self.verify_char(&s[i..]) {
                    Some(l) => l,
                    None => break,
                }
            } else if s[i] == 0 {
                break;
            } else {
                1
            };
            i += l;
        }
        i
    }

    /// `pg_encoding_set_invalid`, `wchar.c:2073`: two bytes that
    /// [`Encoding::mblen`] reads as one character and
    /// [`Encoding::verify_str`] rejects at offset 0. Only meaningful for an
    /// encoding whose [`Encoding::max_length`] is above 1.
    #[must_use]
    pub fn invalid_sequence(self) -> [u8; 2] {
        debug_assert!(self.max_length() > 1);
        let first = if self == Encoding::Utf8 {
            0xc0
        } else {
            NONUTF8_INVALID_BYTE0
        };
        [first, NONUTF8_INVALID_BYTE1]
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// `pg_ascii_dsplen`, `wchar.c:94`.
fn ascii_dsplen(c: u8) -> i32 {
    match c {
        0 => 0,
        c if c < 0x20 || c == 0x7f => -1,
        _ => 1,
    }
}

/// `pg_euc_mblen`, `wchar.c:149`.
fn euc_mblen(c: u8) -> usize {
    match c {
        SS2 => 2,
        SS3 => 3,
        c if is_highbit_set(c) => 2,
        _ => 1,
    }
}

/// `pg_euccn_mblen`, `wchar.c:285`.
fn euccn_mblen(c: u8) -> usize {
    match c {
        SS2 | SS3 => 3,
        c if is_highbit_set(c) => 2,
        _ => 1,
    }
}

/// `pg_euctw_mblen`, `wchar.c:360`.
fn euctw_mblen(c: u8) -> usize {
    match c {
        SS2 => 4,
        SS3 => 3,
        c if is_highbit_set(c) => 2,
        _ => 1,
    }
}

/// `pg_utf_mblen`, `wchar.c:556` (the `NOT_USED` 5- and 6-byte arms are
/// compiled out upstream).
fn utf_mblen(c: u8) -> usize {
    if c & 0x80 == 0 {
        1
    } else if c & 0xe0 == 0xc0 {
        2
    } else if c & 0xf0 == 0xe0 {
        3
    } else if c & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

/// `pg_mule_mblen`, `wchar.c:815`, over `IS_LC1` (`pg_wchar.h:126`),
/// `IS_LCPRV1` (`:155`), `IS_LC2` (`:147`) and `IS_LCPRV2` (`:167`).
fn mule_mblen(c: u8) -> usize {
    if (0x81..=0x8d).contains(&c) {
        2
    } else if c == 0x9a || c == 0x9b || (0x90..=0x99).contains(&c) {
        3
    } else if c == 0x9c || c == 0x9d {
        4
    } else {
        1
    }
}

/// `pg_eucjp_verifychar`, `wchar.c:1104`.
fn eucjp_verifychar(s: &[u8]) -> Option<usize> {
    let len = s.len();
    match s[0] {
        SS2 => (len >= 2 && (0xa1..=0xdf).contains(&s[1])).then_some(2),
        SS3 => (len >= 3 && is_euc_range_valid(s[1]) && is_euc_range_valid(s[2])).then_some(3),
        c1 if is_highbit_set(c1) => {
            (len >= 2 && is_euc_range_valid(c1) && is_euc_range_valid(s[1])).then_some(2)
        }
        _ => Some(1),
    }
}

/// `pg_euckr_verifychar`, `wchar.c:1188` (EUC-CN shares it, `:1245`).
fn euckr_verifychar(s: &[u8]) -> Option<usize> {
    if is_highbit_set(s[0]) {
        (s.len() >= 2 && is_euc_range_valid(s[0]) && is_euc_range_valid(s[1])).then_some(2)
    } else {
        Some(1)
    }
}

/// `pg_euctw_verifychar`, `wchar.c:1250`.
fn euctw_verifychar(s: &[u8]) -> Option<usize> {
    let len = s.len();
    match s[0] {
        SS2 => (len >= 4
            && (0xa1..=0xa7).contains(&s[1])
            && is_euc_range_valid(s[2])
            && is_euc_range_valid(s[3]))
        .then_some(4),
        SS3 => None,
        // "no further range check on c1?" — upstream's own question, kept.
        c1 if is_highbit_set(c1) => (len >= 2 && is_euc_range_valid(s[1])).then_some(2),
        _ => Some(1),
    }
}

/// `pg_gb18030_verifychar`, `wchar.c:1663`.
fn gb18030_verifychar(s: &[u8]) -> Option<usize> {
    let len = s.len();
    let lead = |c: u8| (0x81..=0xfe).contains(&c);
    let digit = |c: u8| (0x30..=0x39).contains(&c);
    if !is_highbit_set(s[0]) {
        Some(1)
    } else if len >= 4 && digit(s[1]) {
        (lead(s[0]) && lead(s[2]) && digit(s[3])).then_some(4)
    } else if len >= 2 && lead(s[0]) {
        ((0x40..=0x7e).contains(&s[1]) || (0x80..=0xfe).contains(&s[1])).then_some(2)
    } else {
        None
    }
}

/// `pg_utf8_verifychar`, `wchar.c:1723`.
fn utf8_verifychar(s: &[u8]) -> Option<usize> {
    if s[0] & 0x80 == 0 {
        return (s[0] != 0).then_some(1);
    }
    let l = utf_mblen(s[0]);
    (l <= s.len() && utf8_islegal(&s[..l])).then_some(l)
}

/// `pg_utf8_islegal`, `wchar.c:2011`: RFC 3629's rules for one character
/// whose length `pg_utf_mblen` gave.
fn utf8_islegal(source: &[u8]) -> bool {
    let continuation = |a: u8| (0x80..=0xbf).contains(&a);
    let length = source.len();
    if !(1..=4).contains(&length) {
        return false;
    }
    if length >= 4 && !continuation(source[3]) {
        return false;
    }
    if length >= 3 && !continuation(source[2]) {
        return false;
    }
    if length >= 2 {
        let a = source[1];
        let ok = match source[0] {
            0xe0 => (0xa0..=0xbf).contains(&a),
            0xed => (0x80..=0x9f).contains(&a),
            0xf0 => (0x90..=0xbf).contains(&a),
            0xf4 => (0x80..=0x8f).contains(&a),
            _ => continuation(a),
        };
        if !ok {
            return false;
        }
    }
    let a = source[0];
    !((0x80..0xc2).contains(&a) || a > 0xf4)
}

/// `pg_encname_tbl[]`, `encnames.c:39`: every accepted spelling, already
/// cleaned, in upstream's sorted order for its binary search.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_table_is_sorted_for_the_binary_search() {
        assert!(ENCNAME_TBL.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(ENCNAME_TBL.len(), 81);
    }

    #[test]
    fn a_name_is_cleaned_before_it_is_looked_up() {
        assert_eq!(Encoding::from_name(b"UTF-8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::from_name(b"UTF8"), Some(Encoding::Utf8));
        assert_eq!(Encoding::from_name(b"gb18030"), Some(Encoding::Gb18030));
        assert_eq!(Encoding::from_name(b"SQL_ASCII"), Some(Encoding::SqlAscii));
        assert_eq!(
            Encoding::from_name(b"mule_internal"),
            Some(Encoding::MuleInternal)
        );
        assert_eq!(Encoding::from_name(b"klingon"), None);
        assert_eq!(Encoding::from_name(b""), None);
        assert_eq!(Encoding::from_name(&[b'u'; NAMEDATALEN]), None);
    }

    #[test]
    fn client_only_starts_after_koi8u() {
        assert!(!Encoding::Koi8U.is_client_only());
        assert!(Encoding::Sjis.is_client_only());
        assert!(Encoding::Gb18030.is_client_only());
        assert!(!Encoding::Utf8.is_client_only());
    }

    #[test]
    fn gb18030_looks_at_the_second_byte_and_can_be_incomplete() {
        assert_eq!(Encoding::Gb18030.mblen(b"\x90\x31"), 4);
        assert_eq!(Encoding::Gb18030.mblen(b"\x81\x5c"), 2);
        // A lone lead byte reads the terminating NUL: two bytes.
        assert_eq!(Encoding::Gb18030.mblen(b"\x80"), 2);
        assert_eq!(Encoding::Gb18030.mblen_or_incomplete(b"\x80"), None);
        assert_eq!(Encoding::Gb18030.mblen_or_incomplete(b"a"), Some(1));
        assert_eq!(Encoding::Utf8.mblen_or_incomplete(b""), None);
    }

    #[test]
    fn utf8_rejects_overlong_surrogates_and_truncation() {
        assert_eq!(Encoding::Utf8.verify_char("é".as_bytes()), Some(2));
        assert_eq!(Encoding::Utf8.verify_char("€".as_bytes()), Some(3));
        assert_eq!(Encoding::Utf8.verify_char(b"\xC0\x80"), None);
        assert_eq!(Encoding::Utf8.verify_char(b"\xED\xA0\x80"), None);
        assert_eq!(Encoding::Utf8.verify_char(b"\xF4\x90\x80\x80"), None);
        assert_eq!(Encoding::Utf8.verify_char(b"\xE0 "), None);
        assert_eq!(Encoding::Utf8.verify_char(b"\x80"), None);
        assert_eq!(Encoding::Utf8.verify_str(b"ab\xC3\xB6c"), 5);
        assert_eq!(Encoding::Utf8.verify_str(b"ab\xC3"), 2);
        assert_eq!(Encoding::Utf8.verify_str(b"ab\0cd"), 2);
    }

    #[test]
    fn each_family_validates_its_trail_bytes() {
        assert_eq!(Encoding::Sjis.verify_char(b"\xF0\x40"), Some(2));
        assert_eq!(Encoding::Sjis.verify_char(b"\xFD\x40"), None); // 0xFD is no SJIS head
        assert_eq!(Encoding::Sjis.verify_char(b"\xF0'"), None);
        assert_eq!(Encoding::Sjis.verify_char(b"\x88\x9f"), Some(2));
        assert_eq!(Encoding::Sjis.verify_char(b"\xB1"), Some(1)); // half-width kana
        assert_eq!(Encoding::Gbk.verify_char(b"\x80'"), Some(2));
        assert_eq!(Encoding::Gbk.verify_char(b"\x80\0"), None);
        assert_eq!(Encoding::Gbk.verify_char(b"\x8d "), None);
        assert_eq!(Encoding::Gb18030.verify_char(b"\x81';"), None);
        assert_eq!(Encoding::Gb18030.verify_char(b"\x81\\"), Some(2));
        assert_eq!(Encoding::Gb18030.verify_char(b"\x81\x30\x81\x30"), Some(4));
        assert_eq!(Encoding::MuleInternal.verify_char(b"\x9c';"), None);
        assert_eq!(Encoding::MuleInternal.verify_char(b"\x81\xa1"), Some(2));
        assert_eq!(Encoding::EucJp.verify_char(b"\x8e\xb1"), Some(2));
        assert_eq!(Encoding::EucTw.verify_char(b"\x8f\xa1\xa1"), None);
        assert_eq!(Encoding::EucKr.verify_char(b"\xb0\xa1"), Some(2));
        assert_eq!(Encoding::Johab.verify_char(b"\x88\x61"), None);
        assert_eq!(Encoding::Latin1.verify_str(b"\xff\xfe"), 2);
        assert_eq!(Encoding::SqlAscii.verify_str(b"1\xC0'"), 3);
    }

    /// One character from each `dsplen` arm, and `PQmblenBounded` stopping
    /// at a NUL and at the end of the slice.
    #[test]
    fn each_encoding_measures_its_own_display_width() {
        assert_eq!(Encoding::SqlAscii.dsplen(b"a"), 1);
        assert_eq!(Encoding::SqlAscii.dsplen(b"\0"), 0);
        assert_eq!(Encoding::SqlAscii.dsplen(b""), 0, "the NUL C would find");
        assert_eq!(Encoding::Latin1.dsplen(b"\x7f"), -1);
        assert_eq!(Encoding::Latin1.dsplen(b"\xe9"), 1);
        assert_eq!(Encoding::Utf8.dsplen("\u{4e16}".as_bytes()), 2);
        assert_eq!(Encoding::Utf8.dsplen("\u{301}".as_bytes()), 0);
        assert_eq!(Encoding::Utf8.dsplen(b"\t"), -1);
        assert_eq!(Encoding::EucJp.dsplen(&[SS2, 0xb1]), 1, "half-width kana");
        assert_eq!(Encoding::EucJp.dsplen(&[SS3, 0xa1, 0xa1]), 2);
        assert_eq!(Encoding::EucKr.dsplen(&[SS2, 0xa1]), 2);
        assert_eq!(Encoding::EucCn.dsplen(&[0xb0, 0xa1]), 2);
        assert_eq!(Encoding::Sjis.dsplen(&[0xb1]), 1, "half-width kana");
        assert_eq!(Encoding::Sjis.dsplen(&[0x82, 0xa0]), 2);
        assert_eq!(Encoding::Gb18030.dsplen(b"\x1b"), -1);
        assert_eq!(Encoding::MuleInternal.dsplen(&[0x81, 0xa1]), 1, "IS_LC1");
        assert_eq!(
            Encoding::MuleInternal.dsplen(&[0x92, 0xa1, 0xa1]),
            2,
            "IS_LC2"
        );
        assert_eq!(Encoding::MuleInternal.dsplen(b"\x01"), 1, "no control arm");

        assert_eq!(Encoding::Utf8.mblen_bounded("\u{4e16}".as_bytes()), 3);
        assert_eq!(Encoding::Utf8.mblen_bounded(&[0xe4, 0]), 1);
        assert_eq!(Encoding::Utf8.mblen_bounded(&[0xe4, 0xb8]), 2);
        assert_eq!(Encoding::Utf8.mblen_bounded(b""), 0);
    }

    #[test]
    fn the_invalid_sequence_is_one_character_and_never_valid() {
        for encoding in [
            Encoding::Utf8,
            Encoding::Gbk,
            Encoding::Gb18030,
            Encoding::Sjis,
            Encoding::Big5,
            Encoding::Uhc,
            Encoding::EucJp,
            Encoding::EucKr,
            Encoding::EucCn,
            Encoding::EucTw,
            Encoding::MuleInternal,
            Encoding::Johab,
        ] {
            let bad = encoding.invalid_sequence();
            assert_eq!(encoding.mblen(&bad), 2, "{encoding}");
            assert_eq!(encoding.verify_str(&bad), 0, "{encoding}");
        }
    }
}
