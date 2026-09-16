//! CRC-32C (Castagnoli), the checksum `pg_control` and every WAL record carry.
//!
//! Port of `src/port/pg_crc32c_sb8.c` and the four public macros
//! `src/include/port/pg_crc32c.h:41` onwards declares:
//!
//! ```text
//! INIT_CRC32C(crc)          (crc) = 0xFFFFFFFF          pg_crc32c.h:41
//! EQ_CRC32C(c1, c2)         (c1) == (c2)                pg_crc32c.h:42
//! COMP_CRC32C(crc, d, len)  accumulate bytes            pg_crc32c.h:106
//! FIN_CRC32C(crc)           (crc) ^= 0xFFFFFFFF         pg_crc32c.h:108
//! ```
//!
//! Upstream picks between an SSE 4.2 / AVX-512 implementation, an ARMv8 one, a
//! LoongArch one and the portable slicing-by-8 tables at build time; all of
//! them compute the same function, so this port is the portable one. The
//! per-byte step is `pg_crc32c_sb8.c:31`'s little-endian `CRC8` macro
//!
//! ```c
//! #define CRC8(x) pg_crc32c_table[0][(crc ^ (x)) & 0xFF] ^ (crc >> 8)
//! ```
//!
//! and the eight-at-a-time unrolling above it is a speed optimisation over
//! exactly that recurrence, not a different checksum. `TABLE` below is
//! upstream's `pg_crc32c_table[0]` (`pg_crc32c_sb8.c:109`) generated from the
//! reflected Castagnoli polynomial rather than transcribed, and
//! [`tests::the_table_is_upstreams_first_row`] pins the generated rows against
//! the literals upstream prints.

/// The reflected CRC-32C polynomial, `0x1EDC6F41` bit-reversed.
///
/// `pg_crc32c_sb8.c` never spells it out — it ships the tables it generates —
/// so this is the constant the tables were generated from, pinned by
/// [`tests::the_table_is_upstreams_first_row`].
const POLYNOMIAL: u32 = 0x82F6_3B78;

/// `INIT_CRC32C` (`src/include/port/pg_crc32c.h:41`).
pub const INIT: u32 = 0xFFFF_FFFF;

/// `FIN_CRC32C` (`src/include/port/pg_crc32c.h:108`) xors with this.
const FINAL_XOR: u32 = 0xFFFF_FFFF;

/// `pg_crc32c_table[0]` (`src/port/pg_crc32c_sb8.c:109`).
static TABLE: [u32; 256] = build_table();

/// Generate `pg_crc32c_table[0]`: for each byte value, the remainder of that
/// byte shifted through the reflected polynomial eight times.
// `index as u32` is exact — the loop stops at 256 — and `u32::try_from` is not
// available in a `const fn` on the pinned toolchain.
#[allow(clippy::cast_possible_truncation)]
const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut remainder = index as u32;
        let mut bit = 0;
        while bit < 8 {
            remainder = if remainder & 1 == 1 {
                (remainder >> 1) ^ POLYNOMIAL
            } else {
                remainder >> 1
            };
            bit += 1;
        }
        table[index] = remainder;
        index += 1;
    }
    table
}

/// `COMP_CRC32C(crc, data, len)`: accumulate `data` into `crc`.
///
/// `src/port/pg_crc32c_sb8.c:31` (`CRC8`), applied one byte at a time.
#[must_use]
pub fn comp(crc: u32, data: &[u8]) -> u32 {
    let mut crc = crc;
    for &byte in data {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// `FIN_CRC32C(crc)` (`src/include/port/pg_crc32c.h:108`).
#[must_use]
pub const fn fin(crc: u32) -> u32 {
    crc ^ FINAL_XOR
}

/// The whole `INIT` / `COMP` / `FIN` sequence over one buffer.
///
/// This is what `WriteControlFile` (`src/backend/access/transam/xlog.c:4290`)
/// and `get_controlfile_by_exact_path` (`src/common/controldata_utils.c:136`)
/// each do over `offsetof(ControlFileData, crc)` bytes.
#[must_use]
pub fn crc32c(data: &[u8]) -> u32 {
    fin(comp(INIT, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first sixteen entries of `pg_crc32c_table[0]`, transcribed from
    /// `src/port/pg_crc32c_sb8.c:112`-`:115`.
    ///
    /// Generating the table from [`POLYNOMIAL`] is only safe if it reproduces
    /// upstream's; these rows are the pin that says it does.
    const UPSTREAM_FIRST_ROWS: [u32; 16] = [
        0x0000_0000,
        0xF26B_8303,
        0xE13B_70F7,
        0x1350_F3F4,
        0xC79A_971F,
        0x35F1_141C,
        0x26A1_E7E8,
        0xD4CA_64EB,
        0x8AD9_58CF,
        0x78B2_DBCC,
        0x6BE2_2838,
        0x9989_AB3B,
        0x4D43_CFD0,
        0xBF28_4CD3,
        0xAC78_BF27,
        0x5E13_3C24,
    ];

    #[test]
    fn the_table_is_upstreams_first_row() {
        assert_eq!(TABLE[..16], UPSTREAM_FIRST_ROWS);
    }

    /// The canonical CRC-32C check value: `"123456789"` is `0xE3069283`
    /// (RFC 3720 appendix B.4, the iSCSI CRC the Castagnoli polynomial was
    /// standardized for).
    #[test]
    fn the_canonical_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// `INIT` then `FIN` with nothing in between: upstream's macros make this
    /// `0xFFFFFFFF ^ 0xFFFFFFFF`.
    #[test]
    fn an_empty_buffer_is_zero() {
        assert_eq!(crc32c(b""), 0);
    }

    /// `COMP_CRC32C` is called more than once per checksum in upstream
    /// (`BootStrapXLOG`, `xlog.c:5170`-`:5171`, accumulates two ranges), so
    /// splitting a buffer must give the same answer as one call over all of it.
    #[test]
    fn accumulating_in_pieces_matches_one_pass() {
        let data: Vec<u8> = (0u16..=511)
            .map(|n| u8::try_from(n % 251).expect("a remainder below 251 is a byte"))
            .collect();
        for split in [0, 1, 3, 4, 7, 8, 100, 255, 511, 512] {
            let (head, tail) = data.split_at(split);
            assert_eq!(
                fin(comp(comp(INIT, head), tail)),
                crc32c(&data),
                "split at {split}"
            );
        }
    }

    /// A one-bit change anywhere must change the checksum; this is the whole
    /// reason `pg_control` carries one.
    #[test]
    fn every_byte_is_covered() {
        let base = [0u8; 292];
        let clean = crc32c(&base);
        for index in 0..base.len() {
            let mut flipped = base;
            flipped[index] = 1;
            assert_ne!(crc32c(&flipped), clean, "byte {index} did not move the CRC");
        }
    }
}
