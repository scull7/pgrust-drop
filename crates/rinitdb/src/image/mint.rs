//! What the mint tool refuses to mint with, as pure checks.
//!
//! The committed image is only as good as the `initdb` that minted it. Two
//! properties are decided by the owner (NAT-381) and checked before anything
//! runs, not trusted to whoever runs the script:
//!
//! - it is PostgreSQL 18.6 ([`MINT_INITDB_VERSION`]), the catalog version
//!   this port targets;
//! - it is linked against musl ([`MINT_LIBC`]). `initdb` always runs
//!   `pg_import_system_collations` (`setup_collation`, `initdb.c:1781`), even
//!   under `--no-locale`, and on musl every libc collation it imports has a
//!   NULL `collversion` (ADR-0002), so no libc release is baked into the
//!   image. The C library is read from the binary's ELF `PT_INTERP`, the
//!   dynamic loader it names (`/lib/ld-musl-x86_64.so.1` on Alpine): that is a
//!   property of the file that will actually run, not of the host running the
//!   script.
//!
//! The same check applies to the `postgres` next to `initdb`: it is the
//! backend `initdb` runs (`setup_bin_paths`, `initdb.c:2652`), and the one
//! that does the import.

use super::manifest::{MINT_INITDB_VERSION, MINT_LIBC};

/// Why a binary may not mint the committed image.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MintRefusal {
    #[error("the image must be minted by \"{MINT_INITDB_VERSION}\", not \"{found}\"")]
    WrongVersion { found: String },
    #[error(
        "\"{binary}\" is not linked against {MINT_LIBC} (dynamic loader: {}); \
         the image is minted on the musl lane (NAT-381)",
        interpreter.as_deref().unwrap_or("none found")
    )]
    NotMusl {
        binary: String,
        interpreter: Option<String>,
    },
}

/// Pure: `output` of `initdb --version` names the required release.
///
/// # Errors
/// [`MintRefusal::WrongVersion`] for anything but [`MINT_INITDB_VERSION`]
/// and one optional trailing newline.
pub fn check_version(output: &str) -> Result<(), MintRefusal> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    if line == MINT_INITDB_VERSION {
        Ok(())
    } else {
        Err(MintRefusal::WrongVersion {
            found: line.to_owned(),
        })
    }
}

/// Pure: the ELF file `binary` (its contents `elf`) names a musl dynamic
/// loader.
///
/// # Errors
/// [`MintRefusal::NotMusl`] when `elf` is not a 64-bit little-endian ELF
/// file with a `PT_INTERP`, or its loader is not `ld-musl-*`.
pub fn check_musl(binary: &str, elf: &[u8]) -> Result<(), MintRefusal> {
    let interpreter = elf_interpreter(elf);
    let is_musl = interpreter
        .and_then(|path| path.rsplit('/').next())
        .is_some_and(|name| name.starts_with("ld-musl-"));
    if is_musl {
        Ok(())
    } else {
        Err(MintRefusal::NotMusl {
            binary: binary.to_owned(),
            interpreter: interpreter.map(str::to_owned),
        })
    }
}

/// `PT_INTERP`, the program header naming the dynamic loader (ELF gABI).
const PT_INTERP: u32 = 3;

/// Pure: the dynamic loader a 64-bit little-endian ELF file names in its
/// `PT_INTERP` program header, or `None` (not such a file, statically
/// linked, or truncated).
#[must_use]
pub fn elf_interpreter(elf: &[u8]) -> Option<&str> {
    // e_ident: magic, EI_CLASS = ELFCLASS64 (2), EI_DATA = ELFDATA2LSB (1).
    if elf.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_le_bytes(elf.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(elf.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| {
        let value = u64::from_le_bytes(elf.get(at..at + 8)?.try_into().ok()?);
        usize::try_from(value).ok()
    };
    // Elf64_Ehdr: e_phoff at 0x20, e_phentsize at 0x36, e_phnum at 0x38.
    let phoff = u64_at(0x20)?;
    let phentsize = usize::from(u16_at(0x36)?);
    let phnum = usize::from(u16_at(0x38)?);
    for index in 0..phnum {
        let header = phoff.checked_add(index.checked_mul(phentsize)?)?;
        // Elf64_Phdr: p_type at 0, p_offset at 8, p_filesz at 0x20.
        if u32_at(header)? != PT_INTERP {
            continue;
        }
        let offset = u64_at(header + 8)?;
        let size = u64_at(header + 0x20)?;
        let bytes = elf.get(offset..offset.checked_add(size)?)?;
        let bytes = bytes.strip_suffix(b"\0").unwrap_or(bytes);
        return std::str::from_utf8(bytes).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 LE header with one program header of type `p_type`
    /// pointing at `interp` (NUL-terminated).
    fn elf(p_type: u32, interp: &str) -> Vec<u8> {
        let mut out = vec![0u8; 0x40 + 0x38];
        out[..6].copy_from_slice(b"\x7fELF\x02\x01");
        out[0x20..0x28].copy_from_slice(&0x40u64.to_le_bytes());
        out[0x36..0x38].copy_from_slice(&0x38u16.to_le_bytes());
        out[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes());
        let offset = out.len() as u64;
        let mut bytes = interp.as_bytes().to_vec();
        bytes.push(0);
        out[0x40..0x44].copy_from_slice(&p_type.to_le_bytes());
        out[0x48..0x50].copy_from_slice(&offset.to_le_bytes());
        out[0x60..0x68].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        out.extend_from_slice(&bytes);
        out
    }

    #[test]
    fn only_postgresql_18_6_may_mint() {
        assert_eq!(check_version("initdb (PostgreSQL) 18.6\n"), Ok(()));
        assert_eq!(check_version("initdb (PostgreSQL) 18.6"), Ok(()));
        for other in [
            "initdb (PostgreSQL) 18.5\n",
            "initdb (PostgreSQL) 18.6 (Debian 18.6-1)\n",
            "initdb (PostgreSQL) 17.2\n",
            "",
        ] {
            assert_eq!(
                check_version(other),
                Err(MintRefusal::WrongVersion {
                    found: other.trim_end_matches('\n').to_owned()
                }),
                "{other:?}"
            );
        }
    }

    #[test]
    fn the_loader_is_read_from_pt_interp() {
        assert_eq!(
            elf_interpreter(&elf(PT_INTERP, "/lib/ld-musl-x86_64.so.1")),
            Some("/lib/ld-musl-x86_64.so.1")
        );
        // PT_LOAD, not PT_INTERP: statically linked as far as this is concerned.
        assert_eq!(elf_interpreter(&elf(1, "/lib/ld-musl-x86_64.so.1")), None);
        assert_eq!(elf_interpreter(b"#!/bin/sh\n"), None);
        let whole = elf(PT_INTERP, "/lib/ld-musl-x86_64.so.1");
        assert_eq!(elf_interpreter(&whole[..whole.len() - 4]), None);
    }

    #[test]
    fn only_a_musl_binary_may_mint() {
        assert_eq!(
            check_musl("initdb", &elf(PT_INTERP, "/lib/ld-musl-aarch64.so.1")),
            Ok(())
        );
        assert_eq!(
            check_musl("initdb", &elf(PT_INTERP, "/lib64/ld-linux-x86-64.so.2")),
            Err(MintRefusal::NotMusl {
                binary: "initdb".to_owned(),
                interpreter: Some("/lib64/ld-linux-x86-64.so.2".to_owned())
            })
        );
        let refusal = check_musl("/opt/pg/bin/initdb", b"\xcf\xfa\xed\xfe").unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "\"/opt/pg/bin/initdb\" is not linked against musl (dynamic loader: none found); \
             the image is minted on the musl lane (NAT-381)"
        );
    }
}
