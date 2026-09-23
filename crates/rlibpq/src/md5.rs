//! MD5, ported from `src/common/md5.c` and `src/common/md5_common.c`.
//!
//! Only `AUTH_REQ_MD5` needs it (`fe-auth.c:818`), and only through
//! [`md5_encrypt`], which is `pg_md5_encrypt` (`md5_common.c:145`): the
//! `md5`-prefixed hex string libpq puts in a PasswordMessage.
//!
//! MD5 is a broken hash and this port adds no use of it that upstream does not
//! already have; it exists so a server configured for `md5` authentication —
//! still the default for many clusters — can be talked to at all.

/// `md5.c:118` — the integer part of 4294967296 × abs(sin(i)), i in radians,
/// transcribed with upstream's leading zero so the index is upstream's `i`.
const T: [u32; 65] = [
    0,
    0xd76a_a478,
    0xe8c7_b756,
    0x2420_70db,
    0xc1bd_ceee,
    0xf57c_0faf,
    0x4787_c62a,
    0xa830_4613,
    0xfd46_9501,
    0x6980_98d8,
    0x8b44_f7af,
    0xffff_5bb1,
    0x895c_d7be,
    0x6b90_1122,
    0xfd98_7193,
    0xa679_438e,
    0x49b4_0821,
    0xf61e_2562,
    0xc040_b340,
    0x265e_5a51,
    0xe9b6_c7aa,
    0xd62f_105d,
    0x0244_1453,
    0xd8a1_e681,
    0xe7d3_fbc8,
    0x21e1_cde6,
    0xc337_07d6,
    0xf4d5_0d87,
    0x455a_14ed,
    0xa9e3_e905,
    0xfcef_a3f8,
    0x676f_02d9,
    0x8d2a_4c8a,
    0xfffa_3942,
    0x8771_f681,
    0x6d9d_6122,
    0xfde5_380c,
    0xa4be_ea44,
    0x4bde_cfa9,
    0xf6bb_4b60,
    0xbebf_bc70,
    0x289b_7ec6,
    0xeaa1_27fa,
    0xd4ef_3085,
    0x0488_1d05,
    0xd9d4_d039,
    0xe6db_99e5,
    0x1fa2_7cf8,
    0xc4ac_5665,
    0xf429_2244,
    0x432a_ff97,
    0xab94_23a7,
    0xfc93_a039,
    0x655b_59c3,
    0x8f0c_cc92,
    0xffef_f47d,
    0x8584_5dd1,
    0x6fa8_7e4f,
    0xfe2c_e6e0,
    0xa301_4314,
    0x4e08_11a1,
    0xf753_7e82,
    0xbd3a_f235,
    0x2ad7_d2bb,
    0xeb86_d391,
];

/// `md5.c:93`-`:111` — `Sa` … `Sp`, the four rotations of each round.
const S: [[u32; 4]; 4] = [
    [7, 12, 17, 22],
    [5, 9, 14, 20],
    [4, 11, 16, 23],
    [6, 10, 15, 21],
];

/// `md5.h:22` — `MD5_BLOCK_SIZE`, and `MD5_BUFLEN` in `md5.c`.
pub const BLOCK_SIZE: usize = 64;
/// `md5.h:20` — `MD5_DIGEST_LENGTH`.
pub const DIGEST_LENGTH: usize = 16;

/// `pg_md5_ctx` (`src/common/md5_int.h:54`), with `pg_md5_init`
/// (`md5.c:382`), `pg_md5_update` (`:400`) and `pg_md5_final` (`:432`).
#[derive(Debug, Clone)]
pub struct Md5 {
    state: [u32; 4],
    buf: [u8; BLOCK_SIZE],
    /// `md5_i` — bytes currently in `buf`.
    used: usize,
    /// `md5_n` — the message length in *bits*, as upstream counts it.
    bits: u64,
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Md5 {
    /// `pg_md5_init`, `md5.c:382`: `MD5_A0` … `MD5_D0`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476],
            buf: [0; BLOCK_SIZE],
            used: 0,
            bits: 0,
        }
    }

    /// `pg_md5_update`, `md5.c:400`.
    pub fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);
        while !data.is_empty() {
            let take = (BLOCK_SIZE - self.used).min(data.len());
            self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used == BLOCK_SIZE {
                let block = self.buf;
                self.calc(&block);
                self.used = 0;
            }
        }
    }

    /// `pg_md5_final`, `md5.c:432`, which is `md5_pad` (`:310`) then
    /// `md5_result` (`:348`): the 0x80 pad of `md5_paddat` (`:143`), zeroes,
    /// then the bit count as a little-endian 64-bit word.
    #[must_use]
    pub fn finish(mut self) -> [u8; DIGEST_LENGTH] {
        let bits = self.bits;
        self.update(&[0x80]);
        while self.used != BLOCK_SIZE - 8 {
            self.update(&[0x00]);
        }
        // `update` counted the padding; `md5_pad` does not ("Don't count up
        // padding. Keep md5_n.", md5.c:313), so restore the real length.
        self.buf[BLOCK_SIZE - 8..].copy_from_slice(&bits.to_le_bytes());
        let block = self.buf;
        self.calc(&block);

        let mut digest = [0u8; DIGEST_LENGTH];
        for (i, word) in self.state.iter().enumerate() {
            digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        digest
    }

    /// `md5_calc`, `md5.c:154`. Upstream unrolls the sixty-four rounds into
    /// `ROUND1` … `ROUND4` macro calls (`md5.c:65`-`:91`); the schedule below
    /// is the same one those calls spell out, and the RFC 1321 vectors in this
    /// module's tests pin every step of it.
    #[allow(clippy::many_single_char_names, clippy::similar_names)]
    fn calc(&mut self, block: &[u8; BLOCK_SIZE]) {
        let mut x = [0u32; 16];
        for (i, word) in x.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }

        let [mut a, mut b, mut c, mut d] = self.state;
        for i in 0..64usize {
            let (f, k) = match i / 16 {
                // md5.c:60 — F, G, H, I
                0 => ((b & c) | (!b & d), i),
                1 => ((b & d) | (c & !d), (1 + 5 * i) % 16),
                2 => (b ^ c ^ d, (5 + 3 * i) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let s = S[i / 16][i % 4];
            // md5.c:65 — a = SHIFT(a + f + X[k] + T[i], s); a = b + a.
            let tmp = a
                .wrapping_add(f)
                .wrapping_add(x[k])
                .wrapping_add(T[i + 1])
                .rotate_left(s);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(tmp);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }
}

/// `pg_md5_binary`, `md5_common.c:107`.
#[must_use]
pub fn md5(data: &[u8]) -> [u8; DIGEST_LENGTH] {
    let mut ctx = Md5::new();
    ctx.update(data);
    ctx.finish()
}

/// `bytesToHex`, `md5_common.c:27`.
#[must_use]
fn bytes_to_hex(bytes: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[usize::from(b >> 4) & 0x0f]);
        out.push(HEX[usize::from(b & 0x0f)]);
    }
    out
}

/// `pg_md5_hash`, `md5_common.c:73`: the digest as 32 lowercase hex digits.
#[must_use]
pub fn md5_hash(data: &[u8]) -> Vec<u8> {
    bytes_to_hex(&md5(data))
}

/// `pg_md5_encrypt`, `md5_common.c:145`: `"md5"` followed by the hex MD5 of
/// `passwd` concatenated with `salt`. The salt goes last "because it may be
/// known by users trying to crack the MD5 output" (`md5_common.c:160`).
///
/// `MD5_PASSWD_LEN` (`md5.h:26`) is 35: three plus thirty-two.
#[must_use]
pub fn md5_encrypt(passwd: &[u8], salt: &[u8]) -> Vec<u8> {
    let mut crypt_buf = Vec::with_capacity(passwd.len() + salt.len());
    crypt_buf.extend_from_slice(passwd);
    crypt_buf.extend_from_slice(salt);

    let mut out = Vec::with_capacity(35);
    out.extend_from_slice(b"md5");
    out.extend_from_slice(&md5_hash(&crypt_buf));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1321 appendix A.5, the "MD5 test suite" — the vectors upstream's
    /// own header points at ("STANDARDS  MD5 is described in RFC 1321",
    /// `md5_common.c:67`).
    #[test]
    fn the_rfc_1321_test_suite() {
        let vectors: [(&[u8], &str); 7] = [
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (input, expected) in vectors {
            assert_eq!(
                String::from_utf8(md5_hash(input)).unwrap(),
                expected,
                "MD5 of {input:?}"
            );
        }
    }

    /// The block boundary: a message of exactly one block, and one that spills
    /// the length word into a second block (`md5_pad`'s `gap <= 8` arm,
    /// `md5.c:320`).
    #[test]
    fn the_padding_spills_into_a_second_block() {
        for len in [55usize, 56, 57, 63, 64, 65, 119, 120, 128] {
            let input = vec![b'x'; len];
            // Streamed one byte at a time must equal the one-shot digest.
            let mut ctx = Md5::new();
            for byte in &input {
                ctx.update(&[*byte]);
            }
            assert_eq!(ctx.finish(), md5(&input), "length {len}");
        }
        // Length 56 is the case that needs the extra block; check it against a
        // known digest rather than only against itself.
        assert_eq!(
            String::from_utf8(md5_hash(&[b'x'; 56])).unwrap(),
            "668a72d5ba17f08e62dabcafad6db14b"
        );
    }

    /// `pg_md5_encrypt` shape: the `md5` prefix and `MD5_PASSWD_LEN` == 35
    /// (`md5.h:26`), over the concatenation `password || salt`.
    #[test]
    fn md5_encrypt_is_the_hash_of_password_then_salt() {
        let encrypted = md5_encrypt(b"secret", b"user");
        assert_eq!(encrypted.len(), 35);
        assert!(encrypted.starts_with(b"md5"));
        assert_eq!(&encrypted[3..], &md5_hash(b"secretuser")[..]);
        // Fixed vectors, so the two-step hash `AUTH_REQ_MD5` sends is pinned
        // by values this code did not produce: md5("secretalice") and then
        // md5 of that hex string followed by the four salt bytes, both from
        // the system `md5sum`. Deriving the second from the first through
        // these same functions would have asserted nothing.
        assert_eq!(
            String::from_utf8(md5_encrypt(b"secret", b"alice")).unwrap(),
            "md54a0a68b43b6cd5cf266fa02f196e2371"
        );
        let first = md5_encrypt(b"secret", b"alice");
        assert_eq!(
            String::from_utf8(md5_encrypt(&first[3..], &[0xde, 0xad, 0xbe, 0xef])).unwrap(),
            "md53e1d73ba00a55e8805aa0277d29996c5"
        );
        assert_eq!(
            String::from_utf8(md5_encrypt(&first[3..], &[1, 2, 3, 4])).unwrap(),
            "md598a0412b9c31436fc53776e863350083"
        );
    }
}
