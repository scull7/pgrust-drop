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
//!
//! What the host contributes is measured, not assumed: after the first mint
//! the tool runs [`HOST_QUERY`] through that `postgres` in single-user mode
//! and reads the answer with [`single_user_value`], so the manifest records
//! the ICU collator version and the `pg_collation` rows each provider added
//! ([`super::manifest::Manifest::icu`], [`super::manifest::Manifest::collations`]).
//! When two mints differ, [`first_difference`] names where.
//!
//! The minted cluster's `global/pg_control` is not in the image (it is
//! per-cluster, [`super::STRIPPED_FILES`]), but what it says about the
//! catalogs is needed to start a cluster from them: the next XID, OID and
//! multixact, where the last checkpoint was. [`template_control`] keeps it,
//! in template form, as `template.control`.

use super::manifest::{MINT_INITDB_VERSION, MINT_LIBC};
use super::parse;
use crate::control::{ControlFile, DbState, PG_CONTROL_FILE_SIZE};

/// The query the mint tool feeds `postgres --single -D <first mint>
/// postgres` on stdin: the host inputs that move the image's digest, as two
/// text columns.
///
/// - `icu`: the `collversion` of the `unicode` collation, the minting host's
///   ICU collator version, or `none` when it is NULL (no libicu). The row
///   itself always exists: `unicode` is a bootstrap `i` row
///   (`src/include/catalog/pg_collation.dat:30` at REL_18_6).
/// - `collations`: `pg_collation` rows per `collprovider`, as
///   `provider=count` sorted by provider (`b=3 c=2 d=1 i=805`): the `c` count
///   is what `locale -a` added, the `i` count what libicu did.
///
/// One line: single-user mode reads a statement per line.
pub const HOST_QUERY: &str = "SELECT coalesce((SELECT collversion FROM pg_collation \
     WHERE collname = 'unicode'), 'none') AS icu, \
     (SELECT string_agg(collprovider::text || '=' || n, ' ' ORDER BY collprovider) \
     FROM (SELECT collprovider, count(*) AS n FROM pg_collation GROUP BY 1) AS s) \
     AS collations\n";

/// Pure: the value single-user mode printed for `column` in `output`, the
/// backend's stdout.
///
/// Each non-NULL attribute of a result row is one line,
/// `\t%2d: <name> = "<value>"\t(typeid = …)` (`printatt`, called by
/// `debugtup`, `src/backend/access/common/printtup.c:423` and `:462` at
/// `REL_18_6`). The value is not escaped, so this takes it up to the `"\t`
/// that closes it. `None` when no such line is there: the column was NULL,
/// or the statement failed (single-user mode still exits 0; the error went
/// to stderr).
#[must_use]
pub fn single_user_value<'a>(output: &'a str, column: &str) -> Option<&'a str> {
    let prefix = format!("{column} = \"");
    output.lines().find_map(|line| {
        let rest = line.strip_prefix('\t')?;
        let (number, rest) = rest.trim_start().split_once(": ")?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (value, _) = rest.strip_prefix(prefix.as_str())?.split_once("\"\t")?;
        Some(value)
    })
}

/// Pure: where two packed images first differ, for the mint tool's
/// "not reproducible" refusal — the first entry path whose node differs, or
/// that only one image has — or `None` when they are the same bytes.
#[must_use]
pub fn first_difference(first: &[u8], second: &[u8]) -> Option<String> {
    if first == second {
        return None;
    }
    let (Ok(a), Ok(b)) = (parse(first), parse(second)) else {
        return Some("an image that does not parse".to_owned());
    };
    let mut a = a.iter();
    let mut b = b.iter();
    loop {
        match (a.next(), b.next()) {
            (Some(x), Some(y)) if x == y => {}
            (Some(x), Some(y)) if x.path == y.path => return Some(x.path.to_string()),
            (Some(x), Some(y)) => {
                // Entries are sorted by path; the smaller one is the one missing
                // from the other image.
                let only = if x.path < y.path { x } else { y };
                return Some(format!("{} (in one image only)", only.path));
            }
            (Some(only), None) | (None, Some(only)) => {
                return Some(format!("{} (in one image only)", only.path));
            }
            (None, None) => return Some("the image header".to_owned()),
        }
    }
}

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
    #[error("the minted cluster's pg_control cannot be a template: {reason}")]
    BadControlFile { reason: &'static str },
}

/// Pure: the minted cluster's `global/pg_control` as `template.control` —
/// [`ControlFile::as_template`], written back with a fresh CRC.
///
/// # Errors
/// [`MintRefusal::BadControlFile`] when the file is short, fails its CRC,
/// is not from a cleanly shut down cluster, or has data checksums off. The
/// last one matters because the image's pages carry whatever checksums the
/// mint wrote: with them on, a cluster can run with checksums on or off;
/// with them off, `-k` would claim checksums the pages do not have.
pub fn template_control(pg_control: &[u8]) -> Result<[u8; PG_CONTROL_FILE_SIZE], MintRefusal> {
    let bad = |reason| MintRefusal::BadControlFile { reason };
    let control = ControlFile::parse(pg_control).map_err(|_| bad("it is too short"))?;
    if !control.crc_is_valid() {
        return Err(bad("its CRC does not match"));
    }
    if control.state != DbState::Shutdowned {
        return Err(bad("the cluster was not shut down cleanly"));
    }
    if control.data_checksum_version == 0 {
        return Err(bad("data checksums are off"));
    }
    Ok(control.as_template().to_bytes())
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
    fn the_template_control_file_is_the_template_form() {
        use crate::control::{MOCK_AUTH_NONCE_LEN, SystemIdentifier};
        let mut control = ControlFile::parse(&[0u8; PG_CONTROL_FILE_SIZE]).unwrap();
        control.system_identifier = SystemIdentifier::from_raw(42);
        control.time = 1;
        control.check_point_copy.time = 2;
        control.check_point_copy.next_oid = 13589;
        control.mock_authentication_nonce = [3; MOCK_AUTH_NONCE_LEN];
        control.state = DbState::Shutdowned;
        control.data_checksum_version = 1;
        let minted = control.to_bytes();

        let template = ControlFile::parse(&template_control(&minted).unwrap()).unwrap();
        assert!(template.crc_is_valid());
        assert_eq!(template, template.as_template());
        assert_eq!(template.check_point_copy.next_oid, 13589);

        let refused = |mutate: fn(&mut ControlFile)| {
            let mut other = control;
            mutate(&mut other);
            template_control(&other.to_bytes())
                .err()
                .map(|err| err.to_string())
                .unwrap_or_default()
        };
        assert_eq!(
            refused(|c| c.state = DbState::InProduction),
            "the minted cluster's pg_control cannot be a template: the cluster was not shut \
             down cleanly"
        );
        assert!(refused(|c| c.data_checksum_version = 0).ends_with("data checksums are off"));
        let mut damaged = minted;
        // `checkPointCopy.nextOid`: a field, so the CRC covers it.
        damaged[72] ^= 1;
        assert!(
            template_control(&damaged)
                .is_err_and(|err| err.to_string().ends_with("its CRC does not match"))
        );
        assert!(
            template_control(&minted[..10])
                .is_err_and(|err| err.to_string().ends_with("it is too short"))
        );
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

    /// What `postgres --single` printed for [`HOST_QUERY`] on the minting
    /// host (Alpine 3.24, PostgreSQL 18.6), captured verbatim.
    const SINGLE_USER: &str = "\n\
        PostgreSQL stand-alone backend 18.6\n\
        backend> \t 1: icu\t(typeid = 25, len = -1, typmod = -1, byval = f)\n\
        \t 2: collations\t(typeid = 25, len = -1, typmod = -1, byval = f)\n\
        \t----\n\
        \t 1: icu = \"153.136\"\t(typeid = 25, len = -1, typmod = -1, byval = f)\n\
        \t 2: collations = \"b=3 c=2 d=1 i=805\"\t(typeid = 25, len = -1, typmod = -1, byval = f)\n\
        \t----\n\
        backend> ";

    #[test]
    fn single_user_values_are_read_from_debugtup_lines() {
        assert_eq!(single_user_value(SINGLE_USER, "icu"), Some("153.136"));
        assert_eq!(
            single_user_value(SINGLE_USER, "collations"),
            Some("b=3 c=2 d=1 i=805")
        );
        // The header line names the column but carries no value.
        assert_eq!(single_user_value(SINGLE_USER, "nosuch"), None);
        let failed = "PostgreSQL stand-alone backend 18.6\nbackend> \nbackend> ";
        assert_eq!(single_user_value(failed, "icu"), None);
    }

    #[test]
    fn a_non_reproducible_mint_names_the_first_difference() {
        use super::super::{Entry, ImagePath, pack};
        let path = |p: &str| ImagePath::new(p).unwrap();
        let image = |entries: &[Entry<&[u8]>]| pack(entries).unwrap();
        let base = [
            Entry::dir(path("base")),
            Entry::file(path("base/1"), b"one".as_slice()),
            Entry::dir(path("pg_xact")),
            Entry::file(path("pg_xact/0000"), b"xact".as_slice()),
        ];
        let first = image(&base);
        assert_eq!(first_difference(&first, &first), None);

        let mut changed = base.clone();
        changed[3] = Entry::file(path("pg_xact/0000"), b"XACT".as_slice());
        assert_eq!(
            first_difference(&first, &image(&changed)).as_deref(),
            Some("pg_xact/0000")
        );

        let fewer = &base[..3];
        assert_eq!(
            first_difference(&first, &image(fewer)).as_deref(),
            Some("pg_xact/0000 (in one image only)")
        );
        let mut extra = base.to_vec();
        extra.push(Entry::file(path("base/2"), b"two".as_slice()));
        assert_eq!(
            first_difference(&first, &image(&extra)).as_deref(),
            Some("base/2 (in one image only)")
        );
        assert_eq!(
            first_difference(&first, b"garbage").as_deref(),
            Some("an image that does not parse")
        );
    }
}
