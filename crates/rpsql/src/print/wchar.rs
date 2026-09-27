//! Display width: `ucs_wcwidth()` (`src/common/wchar.c:646`), which
//! `PQdsplen` reaches for a UTF-8 client through `pg_utf_dsplen` (`:680`).
//!
//! Its two tables are generated headers, vendored verbatim from REL_18_6
//! (ADR-0008) under `unicode/` and read at compile time:
//!
//! - `src/include/common/unicode_nonspacing_table.h`, git blob
//!   `d67f5b3f281d3fe6c6501c729a989b43d94d1e05`;
//! - `src/include/common/unicode_east_asian_fw_table.h`, git blob
//!   `db8bd0ad89779e21b5ceed16a73f081bed98f77b`.
//!
//! `git hash-object crates/rpsql/src/print/unicode/*.h` reproduces both, and
//! a unit test pins their sha256.
//! Parsing them rather than transcribing them keeps the bytes upstream's; a
//! header that does not parse, or whose intervals are not sorted and
//! disjoint as `mbbisearch` needs, fails the build.

/// `nonspacing[]`: general category Mn, Me or Cf.
static NONSPACING: [(u32, u32); count_intervals(NONSPACING_H)] = parse_intervals(NONSPACING_H);
const NONSPACING_H: &[u8] = include_bytes!("unicode/unicode_nonspacing_table.h");

/// `east_asian_fw[]`: East Asian Wide (W) or Fullwidth (F), UAX #11.
static EAST_ASIAN_FW: [(u32, u32); count_intervals(EAST_ASIAN_FW_H)] =
    parse_intervals(EAST_ASIAN_FW_H);
const EAST_ASIAN_FW_H: &[u8] = include_bytes!("unicode/unicode_east_asian_fw_table.h");

/// `ucs_wcwidth()` (`wchar.c:646`): how many columns `ucs` takes, or `None`
/// where C returns -1, a control character (C0, DEL, C1) or a value past
/// U+10FFFF.
///
/// U+0000 takes 0 columns. A non-spacing character takes 0 even when it is
/// also wide: upstream searches that table first (`wchar.c:667`). A wide
/// or fullwidth character takes 2, and everything else 1.
pub(super) fn ucs_wcwidth(ucs: u32) -> Option<usize> {
    if ucs == 0 {
        return Some(0);
    }
    if ucs < 0x20 || (0x7f..0xa0).contains(&ucs) || ucs > 0x0010_ffff {
        return None;
    }
    if mbbisearch(ucs, &NONSPACING) {
        return Some(0);
    }
    if mbbisearch(ucs, &EAST_ASIAN_FW) {
        return Some(2);
    }
    Some(1)
}

/// `mbbisearch()` (`wchar.c:599`): whether an interval of `table` holds
/// `ucs`. The table is sorted and disjoint ([`parse_intervals`] checks), so
/// the first interval not wholly below `ucs` is the only one that can.
fn mbbisearch(ucs: u32, table: &[(u32, u32)]) -> bool {
    let i = table.partition_point(|&(_, last)| last < ucs);
    table.get(i).is_some_and(|&(first, _)| first <= ucs)
}

/// How many `{0x…, 0x…}` intervals a generated header holds.
const fn count_intervals(src: &[u8]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + 2 < src.len() {
        if src[i] == b'{' && src[i + 1] == b'0' && src[i + 2] == b'x' {
            n += 1;
        }
        i += 1;
    }
    n
}

/// The intervals of a generated header, each line `\t{0xFIRST, 0xLAST},`.
/// Panics — at compile time, since it only runs in a `static` initializer —
/// unless every interval is well-formed and they are sorted and disjoint.
const fn parse_intervals<const N: usize>(src: &[u8]) -> [(u32, u32); N] {
    let mut table = [(0, 0); N];
    let mut n = 0;
    let mut i = 0;
    while i + 2 < src.len() {
        if src[i] == b'{' && src[i + 1] == b'0' && src[i + 2] == b'x' {
            let (first, comma) = parse_hex(src, i + 3);
            assert!(
                src[comma] == b','
                    && src[comma + 1] == b' '
                    && src[comma + 2] == b'0'
                    && src[comma + 3] == b'x'
            );
            let (last, brace) = parse_hex(src, comma + 4);
            assert!(src[brace] == b'}');
            assert!(first <= last);
            assert!(n == 0 || table[n - 1].1 < first);
            table[n] = (first, last);
            n += 1;
            i = brace;
        }
        i += 1;
    }
    assert!(n == N);
    table
}

/// The upper-case hex digits at `src[i..]`, and where they stop.
const fn parse_hex(src: &[u8], mut i: usize) -> (u32, usize) {
    let start = i;
    let mut value: u32 = 0;
    while i < src.len() {
        let digit = match src[i] {
            b @ b'0'..=b'9' => b - b'0',
            b @ b'A'..=b'F' => b - b'A' + 10,
            _ => break,
        };
        value = value * 16 + digit as u32;
        i += 1;
    }
    assert!(i > start);
    (value, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn the_vendored_headers_are_the_ones_postgresql_18_6_ships() {
        // ADR-0008: a digest pins the bytes, so a local edit cannot pass
        // for upstream. `sha256sum` over REL_18_6's two files.
        assert_eq!(
            hex(&rlibpq::sha256::sha256(NONSPACING_H)),
            "aff815586db2ac249c81d5fe027745aa6912329a40c30c2ccce5b9b4983afdc9"
        );
        assert_eq!(
            hex(&rlibpq::sha256::sha256(EAST_ASIAN_FW_H)),
            "5b24e5c822f396d45f8075f0e788d7839ed606f85baa74b48ee9773174e8f6e7"
        );
    }

    #[test]
    fn the_tables_are_upstreams_first_to_last() {
        // `unicode_nonspacing_table.h:4` and its last entry, and
        // `unicode_east_asian_fw_table.h:4` and its last.
        assert_eq!(NONSPACING.first(), Some(&(0x00AD, 0x00AD)));
        assert_eq!(NONSPACING.last(), Some(&(0xE0001, 0xE01EF)));
        assert_eq!(EAST_ASIAN_FW.first(), Some(&(0x1100, 0x115F)));
        assert_eq!(EAST_ASIAN_FW.last(), Some(&(0x30000, 0x3FFFD)));
        // One interval per line from line 4 to the `};` that ends each.
        assert_eq!((NONSPACING.len(), EAST_ASIAN_FW.len()), (334, 122));
    }

    #[test]
    fn controls_have_no_width_and_nul_has_zero() {
        assert_eq!(ucs_wcwidth(0), Some(0));
        for ucs in [0x01, 0x0a, 0x1f, 0x7f, 0x80, 0x85, 0x9f, 0x11_0000] {
            assert_eq!(ucs_wcwidth(ucs), None, "U+{ucs:04X}");
        }
    }

    #[test]
    fn ascii_and_latin_take_one_column() {
        for c in [' ', 'a', '~', '\u{a0}', 'é', 'ß', 'Ж', '─', '═'] {
            assert_eq!(ucs_wcwidth(u32::from(c)), Some(1), "{c:?}");
        }
    }

    #[test]
    fn combining_and_format_characters_take_none() {
        // A combining acute, a soft hyphen (Cf), a zero-width joiner, a
        // variation selector, an enclosing circle (Me), the last selector.
        for ucs in [0x0301, 0x00AD, 0x200D, 0xFE0F, 0x20DD, 0xE01EF] {
            assert_eq!(ucs_wcwidth(ucs), Some(0), "U+{ucs:04X}");
        }
    }

    #[test]
    fn wide_and_fullwidth_characters_take_two() {
        // Hangul jamo, CJK, a fullwidth A, hiragana, an emoji, plane 2 and 3.
        for ucs in [0x1100, 0x4E2D, 0xFF21, 0x3042, 0x1F600, 0x20000, 0x3FFFD] {
            assert_eq!(ucs_wcwidth(ucs), Some(2), "U+{ucs:04X}");
        }
        // Just past a wide interval.
        assert_eq!(ucs_wcwidth(0x1160), Some(1));
        assert_eq!(ucs_wcwidth(0x3FFFE), Some(1));
    }

    #[test]
    fn mbbisearch_finds_both_ends_of_an_interval_and_nothing_between() {
        let table = [(3, 5), (9, 9)];
        let hits: Vec<u32> = (0..12).filter(|&u| mbbisearch(u, &table)).collect();
        assert_eq!(hits, [3, 4, 5, 9]);
        assert!(!mbbisearch(1, &[]));
    }
}
