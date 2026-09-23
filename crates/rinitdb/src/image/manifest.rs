//! The template image's provenance manifest: which `initdb` minted the image,
//! on which C library, with which options, and the SHA-256 of the result.
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
//! options: --no-locale --encoding=UTF8 …     MINT_ARGS, space-separated, after `-D <dir>`
//! bytes: 23633969                            the image's length
//! sha256: 0123…                              the image's SHA-256, lowercase hex
//! ```

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
const KEYS: [&str; 6] = ["format", "initdb", "libc", "options", "bytes", "sha256"];

/// What a template image records about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The image format version, the last byte of [`super::MAGIC`].
    pub format: u8,
    /// `initdb --version` of the minting binary, without its newline.
    pub initdb: String,
    /// The C library the minting binary was linked against.
    pub libc: String,
    /// The options after `-D <dir>`, space-separated.
    pub options: String,
    /// The image's length in bytes.
    pub bytes: u64,
    /// The image's SHA-256, 64 lowercase hex digits.
    pub sha256: String,
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
            self.options.clone(),
            self.bytes.to_string(),
            self.sha256.clone(),
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
        let bytes = values[4].parse().map_err(|_| bad("bytes", values[4]))?;
        let sha256 = values[5];
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return Err(bad("sha256", sha256));
        }
        Ok(Self {
            format,
            initdb: values[1].to_owned(),
            libc: values[2].to_owned(),
            options: values[3].to_owned(),
            bytes,
            sha256: sha256.to_owned(),
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
            options: "--no-locale --encoding=UTF8".to_owned(),
            bytes: 42,
            sha256: "ab".repeat(32),
        }
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
                "options: --no-locale --encoding=UTF8",
                "bytes: 42",
                &format!("sha256: {}", "ab".repeat(32)),
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
                line: 10,
                found: "extra".to_owned()
            })
        );
        let truncated = good.replace(&format!("sha256: {}\n", "ab".repeat(32)), "");
        assert_eq!(
            Manifest::parse(&truncated),
            Err(ManifestError::Missing { expected: "sha256" })
        );
        assert_eq!(
            Manifest::parse(&good.replace("bytes: 42", "bytes: many")),
            Err(ManifestError::BadValue {
                key: "bytes",
                value: "many".to_owned()
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
    }
}
