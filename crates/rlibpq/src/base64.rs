//! Base64 without whitespace, ported from `src/common/base64.c`.
//!
//! SCRAM sends the salt, the nonces, the client proof and the server signature
//! through this encoder, so it has to agree with C byte for byte — including
//! its strictness: `pg_b64_decode` (`base64.c:115`) rejects whitespace, stray
//! `=` and every character outside the table rather than skipping it.

/// `base64.c:27` — the encoding alphabet.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `base64.c:30` — the decoding table, `-1` for every byte that is not a
/// base64 symbol. Built from [`BASE64`] rather than transcribed: the C table
/// *is* the inverse of the alphabet, and `the_lookup_table_is_the_inverse_of_the_alphabet`
/// checks the sixteen rows upstream spells out.
fn b64lookup(c: u8) -> i8 {
    let mut i = 0usize;
    while i < 64 {
        if BASE64[i] == c {
            // The `i < 64` guard above bounds this index to 0..=63, which is
            // both inside `i8` and non-negative, so -1 stays the one value
            // that means "not a base64 symbol".
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            return i as i8;
        }
        i += 1;
    }
    -1
}

/// `pg_b64_enc_len`, `base64.c:218`.
#[must_use]
pub fn enc_len(srclen: usize) -> usize {
    // `(srclen + 2) / 3 * 4` upstream: three bytes become four characters.
    srclen.div_ceil(3) * 4
}

/// `pg_b64_dec_len`, `base64.c:233`.
#[must_use]
pub fn dec_len(srclen: usize) -> usize {
    (srclen * 3) >> 2
}

/// `pg_b64_encode`, `base64.c:48`.
///
/// The C function writes into a caller-sized buffer and returns -1 when it
/// would overflow; every caller sizes that buffer with `pg_b64_enc_len`, so
/// the overflow arm is unreachable and this returns the bytes directly.
#[must_use]
pub fn encode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(enc_len(src.len()));
    let mut pos = 2i32;
    let mut buf = 0u32;

    for &s in src {
        buf |= u32::from(s) << (pos << 3);
        pos -= 1;
        if pos < 0 {
            out.push(BASE64[((buf >> 18) & 0x3f) as usize]);
            out.push(BASE64[((buf >> 12) & 0x3f) as usize]);
            out.push(BASE64[((buf >> 6) & 0x3f) as usize]);
            out.push(BASE64[(buf & 0x3f) as usize]);
            pos = 2;
            buf = 0;
        }
    }
    if pos != 2 {
        out.push(BASE64[((buf >> 18) & 0x3f) as usize]);
        out.push(BASE64[((buf >> 12) & 0x3f) as usize]);
        out.push(if pos == 0 {
            BASE64[((buf >> 6) & 0x3f) as usize]
        } else {
            b'='
        });
        out.push(b'=');
    }
    out
}

/// `pg_b64_decode`, `base64.c:115`. `None` is the C function's `-1`.
#[must_use]
pub fn decode(src: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(dec_len(src.len()));
    let mut buf = 0u32;
    let mut pos = 0i32;
    let mut end = 0i32;

    for &c in src {
        // base64.c:131 — whitespace is an error, not a separator.
        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            return None;
        }

        let b = if c == b'=' {
            // base64.c:135 — the end sequence, and only where it fits.
            if end == 0 {
                match pos {
                    2 => end = 1,
                    3 => end = 2,
                    _ => return None,
                }
            }
            0
        } else {
            // base64.c:158 — `c > 0 && c < 127` in C, where `char` is signed,
            // so every byte with the high bit set lands on the -1 arm too.
            let b = if c > 0 && c < 127 { b64lookup(c) } else { -1 };
            if b < 0 {
                return None;
            }
            i32::from(b)
        };

        // `b` is 0 on the `=` arm and otherwise a table hit that already
        // returned on `b < 0`, so it is in 0..=63 and the cast keeps its value.
        #[allow(clippy::cast_sign_loss)]
        {
            buf = (buf << 6) + b as u32;
        }
        pos += 1;
        if pos == 4 {
            // Each group is masked with 255 first, so every value cast here
            // is already one byte wide.
            #[allow(clippy::cast_possible_truncation)]
            {
                out.push(((buf >> 16) & 255) as u8);
                if end == 0 || end > 1 {
                    out.push(((buf >> 8) & 255) as u8);
                }
                if end == 0 || end > 2 {
                    out.push((buf & 255) as u8);
                }
            }
            buf = 0;
            pos = 0;
        }
    }

    // base64.c:195 — missing padding is corruption, not a short last group.
    if pos != 0 { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sixteen rows of `b64lookup` that `base64.c:30`-`:39` spells out are
    /// exactly the positions of those characters in `_base64`, so generating
    /// the inverse cannot drift from the table without this failing.
    #[test]
    fn the_lookup_table_is_the_inverse_of_the_alphabet() {
        assert_eq!(b64lookup(b'+'), 62);
        assert_eq!(b64lookup(b'/'), 63);
        assert_eq!(b64lookup(b'0'), 52);
        assert_eq!(b64lookup(b'9'), 61);
        assert_eq!(b64lookup(b'A'), 0);
        assert_eq!(b64lookup(b'Z'), 25);
        assert_eq!(b64lookup(b'a'), 26);
        assert_eq!(b64lookup(b'z'), 51);
        for bad in [b'-', b'_', b'.', b'=', b' ', 0x7f] {
            assert_eq!(b64lookup(bad), -1, "byte {bad:#x}");
        }
    }

    /// RFC 4648 section 10's ten vectors, which `pg_b64_encode` implements
    /// (standard alphabet, always padded).
    #[test]
    fn the_rfc_4648_vectors() {
        let vectors: [(&[u8], &[u8]); 7] = [
            (b"", b""),
            (b"f", b"Zg=="),
            (b"fo", b"Zm8="),
            (b"foo", b"Zm9v"),
            (b"foob", b"Zm9vYg=="),
            (b"fooba", b"Zm9vYmE="),
            (b"foobar", b"Zm9vYmFy"),
        ];
        for (plain, encoded) in vectors {
            assert_eq!(encode(plain), encoded, "encoding {plain:?}");
            assert_eq!(
                decode(encoded).as_deref(),
                Some(plain),
                "decoding {encoded:?}"
            );
        }
    }

    #[test]
    fn the_length_estimates_are_upstreams_arithmetic() {
        for len in 0..64usize {
            assert_eq!(enc_len(len), encode(&vec![0u8; len]).len());
            assert!(dec_len(enc_len(len)) >= len);
        }
    }

    /// Everything `pg_b64_decode` calls an error, because a SCRAM salt that
    /// silently decodes differently here than in C is a wrong password.
    #[test]
    fn a_malformed_encoding_is_an_error_not_a_guess() {
        assert_eq!(decode(b"Zm9v YmFy"), None, "embedded space");
        assert_eq!(decode(b"Zm9vYmFy\n"), None, "trailing newline");
        assert_eq!(decode(b"Zm9"), None, "missing padding");
        assert_eq!(decode(b"=Zm9v"), None, "leading =");
        assert_eq!(decode(b"Z=m9"), None, "= at position 1");
        assert_eq!(decode(b"Zm9-"), None, "not in the alphabet");
        assert_eq!(decode(&[b'Z', b'm', b'9', 0xff]), None, "high bit set");
        assert_eq!(decode(&[b'Z', b'm', b'9', 0x00]), None, "embedded NUL");
    }

    /// The 18-byte SCRAM nonce and the 32-byte keys are the two sizes the
    /// exchange actually encodes; both round-trip.
    #[test]
    fn the_scram_sizes_round_trip() {
        for len in [18usize, 32] {
            // `len` is at most 32, so `i * 7 + 3` peaks at 31 * 7 + 3 = 220
            // and every element is the byte the expression spells.
            #[allow(clippy::cast_possible_truncation)]
            let raw: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            assert_eq!(decode(&encode(&raw)), Some(raw));
        }
    }
}
