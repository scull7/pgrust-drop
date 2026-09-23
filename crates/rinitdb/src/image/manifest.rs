//! The template image's provenance manifest: which `initdb` minted the image,
//! on which C library, what the host's ICU and locales put into
//! `pg_collation`, with which options, and the SHA-256 of the result and of
//! the template `pg_control` minted with it.
//!
//! The image is a committed blob (NAT-381, owner decision 2026-09-23), so a
//! build never runs PostgreSQL; the manifest is what lets a reader check,
//! without trusting the blob, where it came from and that it is the one
//! recorded. `scripts/mint-template-image.sh` writes both; a test in
//! [`super`] pins the embedded bytes to the manifest's digest and the
//! manifest's recipe to [`super::MINT_ARGS`].
//!
//! Pure: [`Manifest::render`] and [`Manifest::parse`] are inverses over the
//! text format, and nothing here reads a file.
//!
//! ## The format
//!
//! UTF-8 text, one `key: value` per line, in this order, each exactly once.
//! Lines starting with `#` and empty lines are comments:
//!
//! ```text
//! format: 1                                  the image format version (MAGIC[7])
//! initdb: initdb (PostgreSQL) 18.6           `initdb --version`, verbatim
//! libc: musl                                 the C library initdb was linked against
//! icu: 153.136                               the host's ICU collator version, or `none`
//! collations: b=3 c=2 d=1 i=805              pg_collation rows per provider, measured
//! options: --no-locale --encoding=UTF8 …     MINT_ARGS, space-separated, after `-D <dir>`
//! bytes: 23633969                            the image's length
//! sha256: 0123…                              the image's SHA-256, lowercase hex
//! control: 4567…                             template.control's SHA-256, lowercase hex
//! ```
//!
//! `icu` and `collations` are measured by the mint tool, not asserted: it
//! runs [`super::mint::HOST_QUERY`] through the minting `postgres` in
//! single-user mode on the first mint. They are the host inputs that move
//! the digest (ADR-0002): libicu decides the `i` rows and their
//! `collversion`, and `locale -a` the `c` rows beyond `C` and `POSIX`.

use std::fmt::Write as _;

/// The `initdb --version` line the image must be minted by: genuine
/// PostgreSQL 18.6 (ADR-0008), whose catalogs this port targets.
pub const MINT_INITDB_VERSION: &str = "initdb (PostgreSQL) 18.6";

/// The C library the minting `initdb` must be linked against (owner decision,
/// NAT-381): on musl every imported libc collation has a NULL `collversion`
/// (ADR-0002), so no libc release is baked into `pg_collation`.
pub const MINT_LIBC: &str = "musl";

/// The keys, in the order [`Manifest::render`] writes them and
/// [`Manifest::parse`] requires them.
const KEYS: [&str; 9] = [
    "format",
    "initdb",
    "libc",
    "icu",
    "collations",
    "options",
    "bytes",
    "sha256",
    "control",
];

/// What a template image records about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The image format version, the last byte of [`super::MAGIC`].
    pub format: u8,
    /// `initdb --version` of the minting binary, without its newline.
    pub initdb: String,
    /// The C library the minting binary was linked against.
    pub libc: String,
    /// The minting host's ICU collator version, the `collversion` of the
    /// `unicode` collation, or `none` without libicu.
    pub icu: String,
    /// `pg_collation` rows per `collprovider` in the minted cluster, as
    /// `provider=count` sorted by provider.
    pub collations: String,
    /// The options after `-D <dir>`, space-separated.
    pub options: String,
    /// The image's length in bytes.
    pub bytes: u64,
    /// The image's SHA-256, 64 lowercase hex digits.
    pub sha256: String,
    /// The SHA-256 of `template.control`, the minted cluster's `pg_control`
    /// in template form (`crate::control::ControlFile::as_template`), 64
    /// lowercase hex digits.
    pub control: String,
}

/// Why text is not a manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest line {line}: expected \"key: value\"")]
    NotKeyValue { line: usize },
    #[error("manifest line {line}: expected key \"{expected}\", found \"{found}\"")]
    UnexpectedKey {
        line: usize,
        expected: &'static str,
        found: String,
    },
    #[error("manifest line {line}: extra key \"{found}\" after the last one")]
    ExtraKey { line: usize, found: String },
    #[error("manifest ends before key \"{expected}\"")]
    Missing { expected: &'static str },
    #[error("manifest key \"{key}\": invalid value \"{value}\"")]
    BadValue { key: &'static str, value: String },
}

impl Manifest {
    /// Pure: whether the two measured host facts agree with each other.
    ///
    /// `unicode` is a bootstrap `i` row on every host
    /// (`src/include/catalog/pg_collation.dat:30` at REL_18_6), so there is
    /// always at least one `i` row. Without libicu its `collversion` is NULL
    /// (`get_collation_actual_version`, the `#ifdef USE_ICU` branch in
    /// `src/backend/utils/adt/pg_locale.c:1266`) and
    /// `pg_import_system_collations` adds no ICU rows
    /// (`src/backend/commands/collationcmds.c:978`, also under `USE_ICU`).
    /// So `icu` is `none` exactly when `unicode` is the only `i` row.
    #[must_use]
    pub fn host_facts_agree(&self) -> bool {
        let icu_rows = self
            .collations
            .split(' ')
            .find_map(|token| token.strip_prefix("i="))
            .and_then(|count| count.parse::<u64>().ok());
        match icu_rows {
            Some(1) => self.icu == "none",
            Some(2..) => self.icu != "none",
            None | Some(0) => false,
        }
    }

    /// Pure: the manifest as the text [`Manifest::parse`] reads.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::from(
            "# rinitdb template image provenance (NAT-381, ADR-0002).\n\
             # Written by scripts/mint-template-image.sh together with template.img;\n\
             # do not edit by hand.\n",
        );
        let values = [
            self.format.to_string(),
            self.initdb.clone(),
            self.libc.clone(),
            self.icu.clone(),
            self.collations.clone(),
            self.options.clone(),
            self.bytes.to_string(),
            self.sha256.clone(),
            self.control.clone(),
        ];
        for (key, value) in KEYS.iter().zip(values) {
            // Writing to a String cannot fail.
            let _ = writeln!(out, "{key}: {value}");
        }
        out
    }

    /// Pure: the manifest `text` holds.
    ///
    /// # Errors
    /// The first [`ManifestError`] found: a line that is not `key: value`, a
    /// key out of order, missing or extra, or a value that does not parse.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut values: Vec<&str> = Vec::with_capacity(KEYS.len());
        for (index, line) in text.lines().enumerate() {
            let line_no = index + 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(": ")
                .ok_or(ManifestError::NotKeyValue { line: line_no })?;
            let Some(&expected) = KEYS.get(values.len()) else {
                return Err(ManifestError::ExtraKey {
                    line: line_no,
                    found: key.to_owned(),
                });
            };
            if key != expected {
                return Err(ManifestError::UnexpectedKey {
                    line: line_no,
                    expected,
                    found: key.to_owned(),
                });
            }
            values.push(value);
        }
        if let Some(&expected) = KEYS.get(values.len()) {
            return Err(ManifestError::Missing { expected });
        }
        let bad = |key: &'static str, value: &str| ManifestError::BadValue {
            key,
            value: value.to_owned(),
        };
        let format = values[0].parse().map_err(|_| bad("format", values[0]))?;
        let collations = values[4];
        let is_count = |token: &str| {
            token.split_once('=').is_some_and(|(provider, count)| {
                provider.len() == 1
                    && provider.bytes().all(|b| b.is_ascii_lowercase())
                    && !count.is_empty()
                    && count.bytes().all(|b| b.is_ascii_digit())
            })
        };
        if collations.is_empty() || !collations.split(' ').all(is_count) {
            return Err(bad("collations", collations));
        }
        if values[3].is_empty() {
            return Err(bad("icu", values[3]));
        }
        let bytes = values[6].parse().map_err(|_| bad("bytes", values[6]))?;
        let is_digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        };
        let sha256 = values[7];
        if !is_digest(sha256) {
            return Err(bad("sha256", sha256));
        }
        let control = values[8];
        if !is_digest(control) {
            return Err(bad("control", control));
        }
        Ok(Self {
            format,
            initdb: values[1].to_owned(),
            libc: values[2].to_owned(),
            icu: values[3].to_owned(),
            collations: collations.to_owned(),
            options: values[5].to_owned(),
            bytes,
            sha256: sha256.to_owned(),
            control: control.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            format: 1,
            initdb: MINT_INITDB_VERSION.to_owned(),
            libc: MINT_LIBC.to_owned(),
            icu: "153.136".to_owned(),
            collations: "b=3 c=2 d=1 i=805".to_owned(),
            options: "--no-locale --encoding=UTF8".to_owned(),
            bytes: 42,
            sha256: "ab".repeat(32),
            control: "cd".repeat(32),
        }
    }

    #[test]
    fn the_host_facts_agree_with_and_without_libicu() {
        let with = |icu: &str, collations: &str| Manifest {
            icu: icu.to_owned(),
            collations: collations.to_owned(),
            ..sample()
        };
        // Built with ICU: libicu imported rows and reports a version.
        assert!(sample().host_facts_agree());
        // Built without ICU: `unicode` is the only `i` row, and NULL.
        assert!(with("none", "b=3 c=2 d=1 i=1").host_facts_agree());
        // Disagreements.
        assert!(!with("none", "b=3 c=2 d=1 i=805").host_facts_agree());
        assert!(!with("153.136", "b=3 c=2 d=1 i=1").host_facts_agree());
        // No `i` row at all cannot come from 18.6, whose bootstrap has one.
        assert!(!with("none", "b=3 c=2 d=1").host_facts_agree());
        assert!(!with("153.136", "b=3 c=2 d=1").host_facts_agree());
    }

    #[test]
    fn render_then_parse_is_the_identity() {
        assert_eq!(Manifest::parse(&sample().render()), Ok(sample()));
    }

    #[test]
    fn the_rendering_is_the_documented_one() {
        let text = sample().render();
        let body: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(
            body,
            [
                "format: 1",
                "initdb: initdb (PostgreSQL) 18.6",
                "libc: musl",
                "icu: 153.136",
                "collations: b=3 c=2 d=1 i=805",
                "options: --no-locale --encoding=UTF8",
                "bytes: 42",
                &format!("sha256: {}", "ab".repeat(32)),
                &format!("control: {}", "cd".repeat(32)),
            ]
        );
    }

    #[test]
    fn parse_refuses_a_malformed_manifest() {
        let good = sample().render();
        assert_eq!(
            Manifest::parse(&good.replace("libc: musl", "libc musl")),
            Err(ManifestError::NotKeyValue { line: 6 })
        );
        assert_eq!(
            Manifest::parse(&good.replace("libc: ", "libcx: ")),
            Err(ManifestError::UnexpectedKey {
                line: 6,
                expected: "libc",
                found: "libcx".to_owned()
            })
        );
        assert_eq!(
            Manifest::parse(&format!("{good}extra: 1\n")),
            Err(ManifestError::ExtraKey {
                line: 13,
                found: "extra".to_owned()
            })
        );
        let truncated = good.replace(&format!("control: {}\n", "cd".repeat(32)), "");
        assert_eq!(
            Manifest::parse(&truncated),
            Err(ManifestError::Missing {
                expected: "control"
            })
        );
        assert_eq!(
            Manifest::parse(&good.replace("bytes: 42", "bytes: many")),
            Err(ManifestError::BadValue {
                key: "bytes",
                value: "many".to_owned()
            })
        );
        assert_eq!(
            Manifest::parse(&good.replace("c=2 d=1", "c=2  d=1")),
            Err(ManifestError::BadValue {
                key: "collations",
                value: "b=3 c=2  d=1 i=805".to_owned()
            })
        );
        let upper = "AB".repeat(32);
        assert_eq!(
            Manifest::parse(&good.replace(&"ab".repeat(32), &upper)),
            Err(ManifestError::BadValue {
                key: "sha256",
                value: upper
            })
        );
        assert_eq!(
            Manifest::parse(&good.replace(&"cd".repeat(32), "cd")),
            Err(ManifestError::BadValue {
                key: "control",
                value: "cd".to_owned()
            })
        );
    }
}
