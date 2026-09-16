//! HMAC, ported from `src/common/hmac.c` (the build without OpenSSL).
//!
//! SCRAM uses it four ways — PBKDF2's pseudorandom function
//! (`scram-common.c:37`), ClientKey (`:141`), ServerKey (`:171`) and the two
//! signatures in `fe-auth-scram.c` — always with SHA-256.

use crate::sha256::{self, Sha256};

/// `hmac.c:67` — `HMAC_IPAD`.
const IPAD: u8 = 0x36;
/// `hmac.c:68` — `HMAC_OPAD`.
const OPAD: u8 = 0x5C;

/// `pg_hmac_ctx` for `PG_SHA256` (`hmac.c:77`), with `pg_hmac_init`
/// (`hmac.c:138`), `pg_hmac_update` (`:223`) and `pg_hmac_final` (`:244`).
#[derive(Debug, Clone)]
pub struct HmacSha256 {
    /// `ctx->k_opad`, kept for the outer hash.
    k_opad: [u8; sha256::BLOCK_LENGTH],
    /// The inner hash, already fed `k_ipad`.
    inner: Sha256,
}

impl HmacSha256 {
    /// `pg_hmac_init`, `hmac.c:138`: a key longer than the block is replaced
    /// by its own digest, a shorter one is zero-padded, and the two pads are
    /// then XORed over the whole block.
    #[must_use]
    #[allow(clippy::similar_names)] // k_ipad / k_opad are upstream's names.
    pub fn new(key: &[u8]) -> Self {
        let mut keybuf = [0u8; sha256::BLOCK_LENGTH];
        if key.len() > sha256::BLOCK_LENGTH {
            keybuf[..sha256::DIGEST_LENGTH].copy_from_slice(&sha256::sha256(key));
        } else {
            keybuf[..key.len()].copy_from_slice(key);
        }

        let mut k_ipad = [0u8; sha256::BLOCK_LENGTH];
        let mut k_opad = [0u8; sha256::BLOCK_LENGTH];
        for i in 0..sha256::BLOCK_LENGTH {
            k_ipad[i] = keybuf[i] ^ IPAD;
            k_opad[i] = keybuf[i] ^ OPAD;
        }

        let mut inner = Sha256::new();
        inner.update(&k_ipad);
        Self { k_opad, inner }
    }

    /// `pg_hmac_update`, `hmac.c:223`.
    pub fn update(&mut self, data: &[u8]) {
        self.inner.update(data);
    }

    /// `pg_hmac_final`, `hmac.c:244`.
    #[must_use]
    pub fn finish(self) -> [u8; sha256::DIGEST_LENGTH] {
        let inner = self.inner.finish();
        let mut outer = Sha256::new();
        outer.update(&self.k_opad);
        outer.update(&inner);
        outer.finish()
    }
}

/// One-shot HMAC-SHA-256.
#[must_use]
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; sha256::DIGEST_LENGTH] {
    let mut ctx = HmacSha256::new(key);
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

    /// RFC 4231's HMAC-SHA-256 test cases 1, 2, 3, 6 and 7. Case 6 and 7 are
    /// the ones that take `pg_hmac_init`'s "key longer than the block" arm
    /// (`hmac.c:160`).
    #[test]
    fn the_rfc_4231_vectors() {
        // Case 1
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Case 2
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 3
        assert_eq!(
            hex(&hmac_sha256(&[0xaa; 20], &[0xdd; 50])),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        // Case 6: 131-byte key
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        // Case 7: 131-byte key, long message
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm."
            )),
            "9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2"
        );
    }

    /// Feeding the message in pieces is the same HMAC: `pg_hmac_update` is
    /// called once per SCRAM message part (`fe-auth-scram.c:812`).
    #[test]
    fn a_streamed_message_is_the_same_hmac() {
        let mut ctx = HmacSha256::new(b"key");
        ctx.update(b"n=,r=abc");
        ctx.update(b",");
        ctx.update(b"r=abcdef,s=c2FsdA==,i=4096");
        assert_eq!(
            ctx.finish(),
            hmac_sha256(b"key", b"n=,r=abc,r=abcdef,s=c2FsdA==,i=4096")
        );
    }
}
