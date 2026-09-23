//! SHA-256, ported from `src/common/sha2.c`.
//!
//! SCRAM-SHA-256 is the only caller (`fe-auth-scram.c:114` sets
//! `state->hash_type = PG_SHA256`), through [`crate::hmac`] and
//! `scram_H` (`scram-common.c:112`).
//!
//! Upstream builds this file only when there is no OpenSSL to defer to
//! (`sha2.c` vs `sha2_openssl.c`); the two compute the same function, and this
//! crate has no OpenSSL to defer to at all.

/// `sha2.h:23` — `PG_SHA256_DIGEST_LENGTH`.
pub const DIGEST_LENGTH: usize = 32;
/// `sha2.h` — `PG_SHA256_BLOCK_LENGTH`.
pub const BLOCK_LENGTH: usize = 64;

/// `sha2.c:165` — `K256[64]`.
const K256: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// `sha2.c:197` — `sha256_initial_hash_value[8]`.
const H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// `pg_sha256_ctx`, with `pg_sha256_init` (`sha2.c:279`),
/// `pg_sha256_update` (`:476`) and `pg_sha256_final` (`:577`).
#[derive(Debug, Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; BLOCK_LENGTH],
    used: usize,
    bits: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: H0,
            buf: [0; BLOCK_LENGTH],
            used: 0,
            bits: 0,
        }
    }

    /// `pg_sha256_update`, `sha2.c:476`.
    pub fn update(&mut self, mut data: &[u8]) {
        self.bits = self.bits.wrapping_add((data.len() as u64) * 8);
        while !data.is_empty() {
            let take = (BLOCK_LENGTH - self.used).min(data.len());
            self.buf[self.used..self.used + take].copy_from_slice(&data[..take]);
            self.used += take;
            data = &data[take..];
            if self.used == BLOCK_LENGTH {
                let block = self.buf;
                self.transform(&block);
                self.used = 0;
            }
        }
    }

    /// `SHA256_Last` (`sha2.c:529`) then `pg_sha256_final` (`:577`): pad with
    /// 0x80 and zeroes to `PG_SHA256_SHORT_BLOCK_LENGTH` (`sha2.c:89`), then
    /// the bit count big-endian, then the state big-endian.
    #[must_use]
    pub fn finish(mut self) -> [u8; DIGEST_LENGTH] {
        let bits = self.bits;
        self.update(&[0x80]);
        while self.used != BLOCK_LENGTH - 8 {
            self.update(&[0x00]);
        }
        self.buf[BLOCK_LENGTH - 8..].copy_from_slice(&bits.to_be_bytes());
        let block = self.buf;
        self.transform(&block);

        let mut digest = [0u8; DIGEST_LENGTH];
        for (i, word) in self.state.iter().enumerate() {
            digest[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    /// `SHA256_Transform`, `sha2.c:386` (the non-unrolled build), with the
    /// six functions of `sha2.c:139`-`:146`.
    #[allow(clippy::many_single_char_names, clippy::similar_names)]
    fn transform(&mut self, block: &[u8; BLOCK_LENGTH]) {
        let mut w = [0u32; 64];
        for (i, word) in w.iter_mut().take(16).enumerate() {
            *word = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            // sigma1_256 / sigma0_256, sha2.c:145.
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            // Sigma1_256, Ch — sha2.c:144, :139.
            let t1 = h
                .wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25))
                .wrapping_add((e & f) ^ (!e & g))
                .wrapping_add(K256[i])
                .wrapping_add(w[i]);
            // Sigma0_256, Maj — sha2.c:143, :140.
            let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22))
                .wrapping_add((a & b) ^ (a & c) ^ (b & c));
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }

        for (slot, value) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
}

/// One-shot SHA-256.
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; DIGEST_LENGTH] {
    let mut ctx = Sha256::new();
    ctx.update(data);
    ctx.finish()
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

    /// The three NIST FIPS 180-4 example vectors for SHA-256 (one block, two
    /// blocks, and the million-`a` message reduced to its documented digest).
    #[test]
    fn the_fips_180_4_vectors() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// The million-`a` vector, fed in chunks, which also exercises the
    /// multi-block update path.
    #[test]
    fn the_long_message_vector() {
        let mut ctx = Sha256::new();
        let chunk = vec![b'a'; 1000];
        for _ in 0..1000 {
            ctx.update(&chunk);
        }
        assert_eq!(
            hex(&ctx.finish()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// The padding boundary: 55 bytes fits with the length word, 56 does not.
    #[test]
    fn the_padding_spills_into_a_second_block() {
        for len in [55usize, 56, 57, 63, 64, 65] {
            let input = vec![b'x'; len];
            let mut ctx = Sha256::new();
            for byte in &input {
                ctx.update(&[*byte]);
            }
            assert_eq!(ctx.finish(), sha256(&input), "length {len}");
        }
        assert_eq!(
            hex(&sha256(&[b'x'; 56])),
            "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e"
        );
    }
}
