//! SHA-256 (FIPS 180-4), for the vendoring gate in [`super`] and nothing else.
//!
//! The gate has to state each template's digest in the form upstream publishes
//! it — `postgresql-18.6.tar.bz2.sha256` and `sha256sum` both speak SHA-256 —
//! so that a human two years from now can recompute it against PostgreSQL's
//! own release artifacts without trusting anything in this repository. An
//! in-house 64-bit digest cannot be checked against upstream at all: it can
//! only say "the same bytes as last time", which is precisely the weakness
//! that let a pgrust-modified `postgresql.conf.sample` be vendored here and
//! pass.
//!
//! Why a second SHA-256 in this workspace: `rlibpq::sha256` already has one,
//! ported from `src/common/sha2.c`, but `rinitdb` does not depend on `rlibpq`
//! and must not grow a dependency — on a crate inside or outside this
//! workspace — to run a test (AGENTS.md: "No new dependencies without explicit
//! approval"). The honest cost is ~80 duplicated lines of a stable, fully
//! specified algorithm; the alternative cost was a permanent inter-crate edge
//! from the config renderer to the wire-protocol client, which is the more
//! expensive of the two to live with. This copy is `#[cfg(test)]`, so it is
//! not in the shipped binary, and it is written from FIPS 180-4 rather than
//! copied from anywhere.
//!
//! Everything here is a calculation over immutable input: same bytes in, same
//! digest out, no state that outlives a call. `digest_hex` is checked against
//! the FIPS 180-4 example vectors below, so the gate's measuring stick is
//! itself measured.

use std::fmt::Write as _;

/// FIPS 180-4 §4.2.2 — the first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes.
const ROUND_CONSTANTS: [u32; 64] = [
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

/// FIPS 180-4 §5.3.3 — `H(0)`, the fractional parts of the square roots of the
/// first eight primes.
const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// FIPS 180-4 §5.1.1 — 512-bit blocks.
const BLOCK_BYTES: usize = 64;

/// The padding a block always ends with: the `0x80` byte plus the 64-bit
/// length, which is why a message of 56 bytes already needs a second block.
const MANDATORY_PADDING_BYTES: usize = 9;

/// The SHA-256 of `message`, lowercase hex — byte for byte what `sha256sum`
/// prints.
pub(super) fn digest_hex(message: &[u8]) -> String {
    final_state(message)
        .iter()
        .fold(String::with_capacity(64), |mut hex, word| {
            write!(hex, "{word:08x}").expect("writing to a String cannot fail");
            hex
        })
}

/// FIPS 180-4 §6.2.2 — `H(N)`, the state after every padded block.
fn final_state(message: &[u8]) -> [u32; 8] {
    padded(message)
        .chunks_exact(BLOCK_BYTES)
        .fold(INITIAL_STATE, compress)
}

/// FIPS 180-4 §5.1.1 — the message, a `1` bit, zeros, and the length in bits.
fn padded(message: &[u8]) -> Vec<u8> {
    let length_in_bits = u64::try_from(message.len())
        .expect("a message this crate hashes fits in a u64 of bytes")
        * 8;
    let zeros =
        (BLOCK_BYTES - (message.len() + MANDATORY_PADDING_BYTES) % BLOCK_BYTES) % BLOCK_BYTES;

    message
        .iter()
        .copied()
        .chain(std::iter::once(0x80))
        .chain(std::iter::repeat_n(0, zeros))
        .chain(length_in_bits.to_be_bytes())
        .collect()
}

/// FIPS 180-4 §6.2.2 — one block folded into the state.
fn compress(state: [u32; 8], block: &[u8]) -> [u32; 8] {
    let mixed = ROUND_CONSTANTS
        .iter()
        .zip(message_schedule(block))
        .fold(state, |working, (constant, word)| {
            round(working, constant.wrapping_add(word))
        });

    std::array::from_fn(|index| state[index].wrapping_add(mixed[index]))
}

/// FIPS 180-4 §6.2.2 step 3 — one of the 64 rounds. `round_input` is the
/// round's `K(t) + W(t)`, the only place the message enters.
///
/// The spec calls the eight working words `a`…`h`; they are spelled out by
/// position here, which also makes the round's shape plain: every word moves
/// down one place, and only the two new ones are computed.
fn round(working: [u32; 8], round_input: u32) -> [u32; 8] {
    let [first, second, third, fourth, fifth, sixth, seventh, eighth] = working;

    let choice = (fifth & sixth) ^ (!fifth & seventh);
    let sigma1 = fifth.rotate_right(6) ^ fifth.rotate_right(11) ^ fifth.rotate_right(25);
    let temp1 = eighth
        .wrapping_add(sigma1)
        .wrapping_add(choice)
        .wrapping_add(round_input);

    let majority = (first & second) ^ (first & third) ^ (second & third);
    let sigma0 = first.rotate_right(2) ^ first.rotate_right(13) ^ first.rotate_right(22);
    let temp2 = sigma0.wrapping_add(majority);

    [
        temp1.wrapping_add(temp2),
        first,
        second,
        third,
        fourth.wrapping_add(temp1),
        fifth,
        sixth,
        seventh,
    ]
}

/// FIPS 180-4 §6.2.2 step 1 — `W(0..64)`: the block's sixteen big-endian words
/// and the 48 derived from them.
fn message_schedule(block: &[u8]) -> [u32; 64] {
    let mut schedule = [0_u32; 64];

    for (word, chunk) in schedule.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes(chunk.try_into().expect("chunks_exact(4) yields four bytes"));
    }
    for index in 16..schedule.len() {
        let small_sigma0 = {
            let word = schedule[index - 15];
            word.rotate_right(7) ^ word.rotate_right(18) ^ (word >> 3)
        };
        let small_sigma1 = {
            let word = schedule[index - 2];
            word.rotate_right(17) ^ word.rotate_right(19) ^ (word >> 10)
        };
        schedule[index] = schedule[index - 16]
            .wrapping_add(small_sigma0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(small_sigma1);
    }

    schedule
}

#[cfg(test)]
mod tests {
    use super::digest_hex;

    /// The FIPS 180-4 examples, plus the two lengths that sit either side of
    /// the padding's block boundary: 55 bytes is the longest message that
    /// still fits one block, 56 the shortest that forces a second.
    #[test]
    fn the_fips_180_4_example_vectors_hash_to_their_published_digests() {
        for (message, expected) in [
            (
                String::new(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                String::from("abc"),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                String::from("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                "a".repeat(55),
                "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318",
            ),
            (
                "a".repeat(56),
                "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a",
            ),
            (
                "a".repeat(1_000_000),
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            ),
        ] {
            assert_eq!(
                digest_hex(message.as_bytes()),
                expected,
                "SHA-256 of a {}-byte message",
                message.len()
            );
        }
    }

    /// A one-bit change has to change the digest, or the gate above would pass
    /// on a template someone edited.
    #[test]
    fn a_single_changed_byte_changes_the_digest() {
        assert_ne!(digest_hex(b"abc"), digest_hex(b"abd"));
    }
}
