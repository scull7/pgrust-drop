//! The share directory pgrust's server reads at run time, embedded (NAT-408).
//!
//! C `postgres` finds `timezonesets` and `tsearch_data` under `share_path`,
//! `<bindir>/../share` of its own executable (`get_share_path(my_exec_path)`:
//! `src/backend/utils/misc/tzparser.c:320`,
//! `src/backend/tsearch/ts_utils.c:55`), and the compiled timezone database
//! under `share_path/timezone` (`pg_TZDIR`, `src/timezone/pgtz.c:43`-`:54`).
//! pgrust does the same, except that a `PGRUST_PGSHAREDIR` in the environment
//! comes first for the former and a `PGRUST_TZDIR` for the latter. A lone
//! `pgdrop` has no `share/` beside it, so it carries one: `build.rs` compiles
//! every file under `crates/pgdrop/share/` into [`FILES`] — PostgreSQL 18.6's
//! files byte for byte, and its `zic`'s output for `timezone/`
//! (`crates/pgdrop/share/README.md`). Before the server starts, [`prepare`]
//! writes them once to `$XDG_CACHE_HOME/pgdrop/<KEY>/share` and points both
//! variables there.
//!
//! Each variable the user set wins on its own: it is left alone, and the
//! extracted copy serves only the other. When the user set both, nothing is
//! extracted.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/share_files.rs"));
}

pub use embedded::{FILES, KEY};

/// The variable pgrust reads the share directory from before `share_path`.
pub const SHAREDIR_VAR: &str = "PGRUST_PGSHAREDIR";

/// The variable pgrust reads the timezone database from before
/// `share_path/timezone`.
pub const TZDIR_VAR: &str = "PGRUST_TZDIR";

/// A variable [`prepare`] may point into the extracted share directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareVar {
    /// `PGRUST_PGSHAREDIR`: `timezonesets/`, `tsearch_data/`.
    Sharedir,
    /// `PGRUST_TZDIR`: the compiled timezone database.
    Tzdir,
}

impl ShareVar {
    /// The environment variable's name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sharedir => SHAREDIR_VAR,
            Self::Tzdir => TZDIR_VAR,
        }
    }

    /// Pure: its value for the share directory `share`.
    #[must_use]
    pub fn value(self, share: &Path) -> PathBuf {
        match self {
            Self::Sharedir => share.to_path_buf(),
            Self::Tzdir => share.join("timezone"),
        }
    }
}

/// A variable's value, if it is set to an absolute path. The XDG base
/// directory specification has relative values ignored; an empty one is
/// unset.
fn absolute(value: Option<&OsStr>) -> Option<&Path> {
    value.map(Path::new).filter(|path| path.is_absolute())
}

/// Pure: the XDG cache directory — `$XDG_CACHE_HOME`, else `$HOME/.cache`,
/// else `tmp`, for a process with neither.
#[must_use]
pub fn cache_home(xdg_cache_home: Option<&OsStr>, home: Option<&OsStr>, tmp: &Path) -> PathBuf {
    absolute(xdg_cache_home).map_or_else(
        || absolute(home).map_or_else(|| tmp.to_path_buf(), |home| home.join(".cache")),
        Path::to_path_buf,
    )
}

/// Pure: where this build's files are extracted, `<cache_home>/pgdrop/<key>`.
/// Its `share/` is the share directory.
#[must_use]
pub fn extraction_dir(cache_home: &Path, key: &str) -> PathBuf {
    cache_home.join("pgdrop").join(key)
}

/// Pure: the directory to extract into and the variables to point there —
/// each of [`ShareVar`] the user did not set to a non-empty value — or
/// `None` when the user set both.
#[must_use]
pub fn plan(
    sharedir: Option<&OsStr>,
    tzdir: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
    tmp: &Path,
) -> Option<(PathBuf, Vec<ShareVar>)> {
    let unset = |value: Option<&OsStr>| value.is_none_or(OsStr::is_empty);
    let vars: Vec<ShareVar> = [(ShareVar::Sharedir, sharedir), (ShareVar::Tzdir, tzdir)]
        .into_iter()
        .filter(|(_, value)| unset(*value))
        .map(|(var, _)| var)
        .collect();
    (!vars.is_empty()).then(|| {
        (
            extraction_dir(&cache_home(xdg_cache_home, home, tmp), KEY),
            vars,
        )
    })
}

/// Action: make `<dir>/share` hold `files`, and return it.
///
/// An existing `<dir>` is taken as complete: it only ever appears by the
/// `rename` below, whole. Otherwise the files are written to a staging
/// directory beside it, named for this process, and renamed into place; if
/// another process's rename won the race, its directory is used and the
/// staging copy removed.
///
/// # Errors
///
/// Any failure to create, write or rename, when no complete `<dir>` exists.
pub fn extract(files: &[(&str, &[u8])], dir: &Path) -> std::io::Result<PathBuf> {
    let share = dir.join("share");
    if dir.is_dir() {
        return Ok(share);
    }
    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staging_name = OsStr::new(".").to_os_string();
    staging_name.push(dir.file_name().unwrap_or(OsStr::new("share")));
    staging_name.push(format!(".{}", std::process::id()));
    let staging = parent.join(staging_name);
    let _ = std::fs::remove_dir_all(&staging);
    let written =
        write_all(files, &staging.join("share")).and_then(|()| std::fs::rename(&staging, dir));
    match written {
        Ok(()) => Ok(share),
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            if dir.is_dir() { Ok(share) } else { Err(error) }
        }
    }
}

fn write_all(files: &[(&str, &[u8])], share: &Path) -> std::io::Result<()> {
    for (relative, bytes) in files {
        let path = share.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// Action: before the server starts, extract [`FILES`] and set the
/// variables [`plan`] names. A failure is reported on `stderr` as a warning
/// and the server starts anyway, falling back to `share_path` as C does;
/// whatever it then cannot find it reports itself.
pub fn prepare(stderr: &mut dyn Write) {
    let Some((dir, vars)) = plan(
        std::env::var_os(SHAREDIR_VAR).as_deref(),
        std::env::var_os(TZDIR_VAR).as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        &std::env::temp_dir(),
    ) else {
        return;
    };
    match extract(FILES, &dir) {
        Ok(share) => {
            for var in vars {
                set_var(var.name(), &var.value(&share));
            }
        }
        Err(error) => {
            let _ = writeln!(
                stderr,
                "pgdrop: warning: could not extract the embedded share directory to \"{}\": {error}",
                dir.display()
            );
        }
    }
}

#[allow(unsafe_code)]
fn set_var(name: &str, value: &Path) {
    // SAFETY: called from `main`'s thread before `pg_main` and the seams
    // start any other thread, so nothing reads the environment concurrently.
    unsafe { std::env::set_var(name, value) };
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsString;

    /// Each embedded file outside `timezone/`, the upstream path `make
    /// install` copies it from, and that file's SHA-256 at tag `REL_18_6` (commit
    /// `724edf9bde9d356724ad384a2e196edc3c9f80f7`). The install lists are
    /// `src/timezone/tznames/Makefile:15`-`:19`,
    /// `src/backend/tsearch/Makefile:17`-`:21` and
    /// `src/backend/snowball/Makefile:75`-`:90`.
    const UPSTREAM: &[(&str, &str, &str)] = &[
        (
            "timezonesets/Africa.txt",
            "src/timezone/tznames/Africa.txt",
            "29f630662fde2db5724990f715b73f09791a85627dfeb5612adfef12a6a0b8f4",
        ),
        (
            "timezonesets/America.txt",
            "src/timezone/tznames/America.txt",
            "8ca517c53d723a61b85cea6880af7ce1672e6c1be73f704fbf28477db2645124",
        ),
        (
            "timezonesets/Antarctica.txt",
            "src/timezone/tznames/Antarctica.txt",
            "6f47ca542b13b594bb3a3689a63fbf3bcc8414815ece60eeeb15158ea4becb7c",
        ),
        (
            "timezonesets/Asia.txt",
            "src/timezone/tznames/Asia.txt",
            "ae480b39d72487ffc4576a7fdf9a1e613ac3f87633daafeb7e071d6cd47dc982",
        ),
        (
            "timezonesets/Atlantic.txt",
            "src/timezone/tznames/Atlantic.txt",
            "d03378d66a563226cb25390dcdbc8d5dc9015d0ec86cbba07b81718062c28ca5",
        ),
        (
            "timezonesets/Australia",
            "src/timezone/tznames/Australia",
            "249e83499ae7cd0348571d31ef50e240071bca5b063eed97ee902edcc98105dd",
        ),
        (
            "timezonesets/Australia.txt",
            "src/timezone/tznames/Australia.txt",
            "32128798fcc64d269c9033acf05ea5e404f155d13c4d7afee0046f3a4649e2b9",
        ),
        (
            "timezonesets/Default",
            "src/timezone/tznames/Default",
            "0dadd689b1998cc2d3acec42f66913194661f8d4835623776593b7bcdd075cb1",
        ),
        (
            "timezonesets/Etc.txt",
            "src/timezone/tznames/Etc.txt",
            "abe5ce63e30162ae9200603bcf68cdab453826e3180651eee3cc10d0e6fc9c69",
        ),
        (
            "timezonesets/Europe.txt",
            "src/timezone/tznames/Europe.txt",
            "3dc21632cab73cc7cbe973a671f549bc2603b6af862f130b82c09adb5a73c6b6",
        ),
        (
            "timezonesets/India",
            "src/timezone/tznames/India",
            "77eb5794aeb96c90f5016a74f1ef8efd597a2059e74ec2258be03be9a1e8d894",
        ),
        (
            "timezonesets/Indian.txt",
            "src/timezone/tznames/Indian.txt",
            "6973cbbfdf88507ff4d32566e8c5328685b6a441a8ae22ee83b35039722c1acc",
        ),
        (
            "timezonesets/Pacific.txt",
            "src/timezone/tznames/Pacific.txt",
            "55f614feedb1857c43d0f5aee1a5cabe9073365e368865c6c79495a3ee25e0a3",
        ),
        (
            "tsearch_data/danish.stop",
            "src/backend/snowball/stopwords/danish.stop",
            "1bedc9cf5a8830dacf8c4ee0d8b301f0801861756ad0d504431d01047f961b0c",
        ),
        (
            "tsearch_data/dutch.stop",
            "src/backend/snowball/stopwords/dutch.stop",
            "e5a2a7c390fe3ad0c0a132586ed11492b635d66a1f426cd89677f80b07bc76a6",
        ),
        (
            "tsearch_data/english.stop",
            "src/backend/snowball/stopwords/english.stop",
            "b3f772a000465cb76e23adb03b47073c591c156fad8f7af09c8b8e80d6bd8eac",
        ),
        (
            "tsearch_data/finnish.stop",
            "src/backend/snowball/stopwords/finnish.stop",
            "952af766edc9b8e7ddc877fc464cbd94b91754b5621fdfdd7020568fd4813fcd",
        ),
        (
            "tsearch_data/french.stop",
            "src/backend/snowball/stopwords/french.stop",
            "6ca60ffd4257c35cc3981d0e923881c3d8b85c8c0f6ef7a6697bd4ae97200fd8",
        ),
        (
            "tsearch_data/german.stop",
            "src/backend/snowball/stopwords/german.stop",
            "46bf0dcec5b5bd83cd7fb96a5283876fdf91cfae7f988252132218545a61d1f7",
        ),
        (
            "tsearch_data/hungarian.stop",
            "src/backend/snowball/stopwords/hungarian.stop",
            "cb66b000fe0c852579ab2cb01e086f3d0eef80d650353bcd854958556dcf917d",
        ),
        (
            "tsearch_data/hunspell_sample.affix",
            "src/backend/tsearch/dicts/hunspell_sample.affix",
            "0f1fb5943562c9523d010780b61784c147b49e57596b2497d1396d31c5214aaf",
        ),
        (
            "tsearch_data/hunspell_sample_long.affix",
            "src/backend/tsearch/dicts/hunspell_sample_long.affix",
            "2132c84f2453d7c3d8bc6b5598a57afe4f9c4aca04d886f46308a070a86df217",
        ),
        (
            "tsearch_data/hunspell_sample_long.dict",
            "src/backend/tsearch/dicts/hunspell_sample_long.dict",
            "1926afe91b724eb2a3bcd6dd3a03ae3d46691d3246a597d2682a209eb3ada459",
        ),
        (
            "tsearch_data/hunspell_sample_num.affix",
            "src/backend/tsearch/dicts/hunspell_sample_num.affix",
            "5c2eeed5197453472cc4f918d556a8d092b3b8891579dc8cc0b3db939da52364",
        ),
        (
            "tsearch_data/hunspell_sample_num.dict",
            "src/backend/tsearch/dicts/hunspell_sample_num.dict",
            "dc6e8fcfaf5bc2b93cd58e8273cae625107ba02dace80061bb6c765ec97dea6f",
        ),
        (
            "tsearch_data/ispell_sample.affix",
            "src/backend/tsearch/dicts/ispell_sample.affix",
            "3a5a3e7a54acd42c27d3c231526ccf8ccf7f29f688ef6ad74dbd04af11309d8f",
        ),
        (
            "tsearch_data/ispell_sample.dict",
            "src/backend/tsearch/dicts/ispell_sample.dict",
            "e913216b14f04ebfe390f8f6048c408aebab7fe7ff313d61a0d2355146d83397",
        ),
        (
            "tsearch_data/italian.stop",
            "src/backend/snowball/stopwords/italian.stop",
            "293d7841f198e4012f49e8e5653c3bfd073a58cea1259fa2c0fcec894167b628",
        ),
        (
            "tsearch_data/nepali.stop",
            "src/backend/snowball/stopwords/nepali.stop",
            "43245d062fa39a6543d5ff3216552a391bac95fd2ba26c05b1d6959d26f441fb",
        ),
        (
            "tsearch_data/norwegian.stop",
            "src/backend/snowball/stopwords/norwegian.stop",
            "f7e5b42208ccf1b1f282f9e0f8570e464272762bda5718b6b26750f510a688dc",
        ),
        (
            "tsearch_data/portuguese.stop",
            "src/backend/snowball/stopwords/portuguese.stop",
            "5f305fac1d830620eeb9b2a68a1b96bf26f65c2e9257d56c8413d6fb36ed6cac",
        ),
        (
            "tsearch_data/russian.stop",
            "src/backend/snowball/stopwords/russian.stop",
            "1743191192b4a4f77fcc216499455dc00c1b8626fdd407076a8deefff80e3d59",
        ),
        (
            "tsearch_data/spanish.stop",
            "src/backend/snowball/stopwords/spanish.stop",
            "7450c1f7196c161d9a4e537de43f88bd55cdb64028c808e14587bd7a6ac7be3d",
        ),
        (
            "tsearch_data/swedish.stop",
            "src/backend/snowball/stopwords/swedish.stop",
            "2a9d9d756bc4257d49329994f805c712cd5ab6746162e2082752238766c1c0c8",
        ),
        (
            "tsearch_data/synonym_sample.syn",
            "src/backend/tsearch/dicts/synonym_sample.syn",
            "59a46c3c25c2b5a1fe174de62bf2f53ce46021e3831eec0eb62d9301ea327e49",
        ),
        (
            "tsearch_data/thesaurus_sample.ths",
            "src/backend/tsearch/dicts/thesaurus_sample.ths",
            "346954eaf1def3007ad848731b3c26c203ed12923911b6a0ac198604831f7dc4",
        ),
        (
            "tsearch_data/turkish.stop",
            "src/backend/snowball/stopwords/turkish.stop",
            "f2c7f0c2bd3dba42da700776266831853a3b5f1207fada5c15207334109abb57",
        ),
    ];

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    /// A scratch directory under the system temporary directory, removed
    /// when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("pgdrop-share-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("create the scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The embedded files outside `timezone/`, which [`UPSTREAM`] covers.
    fn copied_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
        FILES
            .iter()
            .filter(|(path, _)| !path.starts_with("timezone/"))
    }

    #[test]
    fn each_embedded_file_is_the_file_postgresql_18_6_installs() {
        let embedded: Vec<&str> = copied_files().map(|(path, _)| *path).collect();
        let upstream: Vec<&str> = UPSTREAM.iter().map(|(path, _, _)| *path).collect();
        assert_eq!(
            embedded, upstream,
            "the embedded set is upstream's install set"
        );
        for ((path, bytes), (_, upstream_path, digest)) in copied_files().zip(UPSTREAM) {
            assert_eq!(
                hex(&rlibpq::sha256::sha256(bytes)),
                *digest,
                "crates/pgdrop/share/{path} is not {upstream_path} at REL_18_6; \
                 re-vendor it from the tag or the release tarball, never from pgrust's \
                 crates/postgres-18.6-reference/ (see crates/pgdrop/share/README.md)"
            );
        }
    }

    /// `timezone/` is zic's output, not a copy, so its digests live in a
    /// `sha256sum` manifest that `scripts/vendor-timezone.sh` writes beside
    /// the tree from the release tarball; running the script again on a
    /// clean checkout must leave both unchanged.
    #[test]
    fn each_embedded_timezone_file_is_zic_output_of_postgresql_18_6() {
        let manifest: Vec<(&str, &str)> = include_str!("../timezone.sha256")
            .lines()
            .map(|line| {
                let (digest, path) = line.split_once("  ").expect("<sha256>  <path>");
                (path, digest)
            })
            .collect();
        let embedded: Vec<(&str, String)> = FILES
            .iter()
            .filter(|(path, _)| path.starts_with("timezone/"))
            .map(|(path, bytes)| (*path, hex(&rlibpq::sha256::sha256(bytes))))
            .collect();
        assert_eq!(
            embedded.iter().map(|(path, _)| *path).collect::<Vec<_>>(),
            manifest.iter().map(|(path, _)| *path).collect::<Vec<_>>(),
            "the embedded timezone set is crates/pgdrop/timezone.sha256's"
        );
        for ((path, digest), (_, expected)) in embedded.iter().zip(&manifest) {
            assert_eq!(
                digest, expected,
                "crates/pgdrop/share/{path} is not zic's output; \
                 regenerate it with scripts/vendor-timezone.sh"
            );
        }
        // tzdata.zi at REL_18_6 is release 2026c; a sample zone is there.
        assert!(embedded.len() > 500, "{} zones", embedded.len());
        assert!(
            embedded
                .iter()
                .any(|(path, _)| *path == "timezone/Europe/Paris")
        );
    }

    #[test]
    fn the_key_names_the_version_and_changes_with_the_files() {
        let (version, digest) = KEY.rsplit_once('-').expect("<version>-<digest>");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert_eq!(digest.len(), 16);
        assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn the_cache_is_xdg_cache_home_then_home_dot_cache_then_tmp() {
        let tmp = Path::new("/tmp");
        let (xdg, home) = (os("/x/cache"), os("/home/u"));
        assert_eq!(
            cache_home(Some(&xdg), Some(&home), tmp),
            Path::new("/x/cache")
        );
        assert_eq!(
            cache_home(None, Some(&home), tmp),
            Path::new("/home/u/.cache")
        );
        assert_eq!(cache_home(None, None, tmp), tmp);
        // Relative or empty values are ignored, as the XDG specification says.
        let (relative, empty) = (os("cache"), os(""));
        assert_eq!(
            cache_home(Some(&relative), Some(&home), tmp),
            Path::new("/home/u/.cache")
        );
        assert_eq!(cache_home(Some(&empty), Some(&empty), tmp), tmp);
    }

    #[test]
    fn the_extraction_dir_is_per_build() {
        assert_eq!(
            extraction_dir(Path::new("/c"), "0.1.0-00ff"),
            Path::new("/c/pgdrop/0.1.0-00ff")
        );
    }

    #[test]
    fn a_share_dir_the_user_named_wins() {
        use ShareVar::{Sharedir, Tzdir};
        let tmp = Path::new("/tmp");
        let (mine, tz, xdg, empty) = (
            os("/opt/pg/share"),
            os("/usr/share/zoneinfo"),
            os("/x"),
            os(""),
        );
        let dir = Path::new("/x/pgdrop").join(KEY);
        assert_eq!(plan(Some(&mine), Some(&tz), Some(&xdg), None, tmp), None);
        assert_eq!(
            plan(None, None, Some(&xdg), None, tmp),
            Some((dir.clone(), vec![Sharedir, Tzdir]))
        );
        assert_eq!(
            plan(Some(&mine), None, Some(&xdg), None, tmp),
            Some((dir.clone(), vec![Tzdir]))
        );
        assert_eq!(
            plan(None, Some(&tz), Some(&xdg), None, tmp),
            Some((dir.clone(), vec![Sharedir]))
        );
        assert_eq!(
            plan(Some(&empty), Some(&empty), Some(&xdg), None, tmp),
            Some((dir, vec![Sharedir, Tzdir]))
        );
    }

    #[test]
    fn the_variables_point_into_the_share_directory() {
        let share = Path::new("/c/pgdrop/k/share");
        assert_eq!(ShareVar::Sharedir.name(), "PGRUST_PGSHAREDIR");
        assert_eq!(ShareVar::Sharedir.value(share), share);
        assert_eq!(ShareVar::Tzdir.name(), "PGRUST_TZDIR");
        assert_eq!(
            ShareVar::Tzdir.value(share),
            Path::new("/c/pgdrop/k/share/timezone")
        );
    }

    #[test]
    fn extraction_writes_every_file_once() {
        let scratch = Scratch::new("extract");
        let dir = scratch.0.join("pgdrop").join(KEY);
        let share = extract(FILES, &dir).expect("extract");
        assert_eq!(share, dir.join("share"));
        for (path, bytes) in FILES {
            assert_eq!(
                std::fs::read(share.join(path)).expect("read back"),
                *bytes,
                "{path}"
            );
        }
        // Nothing but the extraction is left in the parent: no staging copy.
        let siblings: Vec<_> = std::fs::read_dir(scratch.0.join("pgdrop"))
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(siblings, [OsString::from(KEY)]);

        // A second run takes the existing directory as it is.
        let marker = share.join("timezonesets/Default");
        std::fs::write(&marker, b"").expect("overwrite");
        assert_eq!(extract(FILES, &dir).expect("extract again"), share);
        assert_eq!(std::fs::read(&marker).expect("read"), b"");
    }

    #[test]
    fn an_unwritable_cache_is_an_error() {
        let scratch = Scratch::new("unwritable");
        let file = scratch.0.join("not-a-directory");
        std::fs::write(&file, b"").expect("write");
        assert!(extract(FILES, &file.join("pgdrop").join(KEY)).is_err());
    }
}
