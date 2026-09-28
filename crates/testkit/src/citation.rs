//! The upstream citation lint (NAT-519).
//!
//! AGENTS.md makes every port cite the upstream `file:line` it follows, and
//! "upstream" is PostgreSQL 18.6 at tag `REL_18_6`
//! (`724edf9bde9d356724ad384a2e196edc3c9f80f7`) and nothing else. A citation
//! is only worth its bytes if it still lands where it says, so this module
//! re-anchors every one of them against a pristine checkout of that tag.
//!
//! What counts as a citation, in a line of any text file in the repo:
//!
//! - `name.c:NNN`, with or without a directory prefix
//!   (`src/bin/initdb/initdb.c:2634`, `t/001_basic.pl:153`), or a `Makefile`,
//!   `GNUmakefile` or `configure` (`src/port/Makefile:142`), and ranges
//!   `name.c:NNN-MMM`, `name.c:NNN-:MMM`, `` `name.c:NNN`-`:MMM` ``;
//! - a backticked continuation `` `:NNN` `` (or a range of them), which cites
//!   the file of the previous citation in the same repo file, or of a
//!   backticked upstream file name after it. A backticked name of our own
//!   code (`docs/divergences.md`) is a cross-reference in passing and does
//!   not change it; after a citation or name that failed, the continuation
//!   fails the same way.
//!
//! What fails:
//!
//! 1. a path that does not exist in the tag, a basename the tag has more than
//!    once with nothing in the repo file to say which, or a line past the end
//!    of the file;
//! 2. a backticked C function, macro or typedef right before the citation
//!    (`` `InitControlFile`, `xlog.c:4217` ``) that the cited lines neither
//!    mention nor lie inside the definition of. The message names the lines
//!    the name is defined on.
//!
//! A path that the tag does not have but the repo does (`crates/…/lib.rs:12`,
//! `divergences.md:21`) is a citation of our own code and is not checked.
//! Exceptions are explicit and grep-able: a line carrying [`ALLOW_MARKER`] is
//! skipped, and whole files are skipped only through [`EXCLUDED`], each with
//! its reason.
//!
//! Data / Calculations / Actions: [`scan`] and [`Checker`] are pure (the
//! upstream tree reaches them through [`Upstream`]); the walk over the repo and
//! the reads of the tag live in `tests/citations.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A line carrying this text is not checked. Say why on the same line.
pub const ALLOW_MARKER: &str = "citation-lint: allow";

/// Repo paths (prefixes, `/`-separated, relative to the repo root) the lint
/// never reads, with the reason. Every entry is an exception to the rule, so
/// keep this short and keep the reason honest.
pub const EXCLUDED: &[(&str, &str)] = &[
    (".git/", "not our text"),
    ("target/", "build output"),
    (
        ".upstream/",
        "the upstream tree itself (scripts/fetch-upstream-src.sh)",
    ),
    (
        ".ref/",
        "reference binaries (scripts/fetch-ref-binaries.sh)",
    ),
    ("tmp/", "scratch data directories"),
    (
        "progress.md",
        "retired historical archive (AGENTS.md: do not extend it); its line numbers describe the code of its day",
    ),
    (
        "crates/pgdrop/share/",
        "vendored upstream data, not our citations",
    ),
    (
        "crates/rinitdb/share/",
        "vendored upstream data, not our citations",
    ),
    ("crates/rinitdb/image/", "binary template image"),
    (
        "crates/testkit/src/citation.rs",
        "this lint: its unit tests are made-up citations",
    ),
];

/// Whether `rel` (a repo-relative, `/`-separated path) is in [`EXCLUDED`].
#[must_use]
pub fn is_excluded(rel: &str) -> bool {
    EXCLUDED
        .iter()
        .any(|(prefix, _)| rel == prefix.trim_end_matches('/') || rel.starts_with(prefix))
}

/// One `file:line` citation as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    /// 1-based line in the repo file.
    pub at: usize,
    /// The text as it reads in the repo file, for messages.
    pub text: String,
    /// The path as written; `None` for a continuation with nothing to attach
    /// to by itself (it takes the previous citation's file).
    pub path: Option<String>,
    /// First cited line.
    pub start: usize,
    /// Last cited line (`start` unless a range).
    pub end: usize,
    /// The backticked identifiers right before the citation, nearest first.
    pub idents: Vec<String>,
    /// The line carries [`ALLOW_MARKER`].
    pub allowed: bool,
    /// A backticked file name with no line: it cites nothing, but a
    /// continuation after it cites that file.
    pub mention: bool,
}

fn is_path_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-' | b'+')
}

fn is_ident(s: &str) -> bool {
    let mut bytes = s.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// The extension of a path's last component, when it has one that starts
/// with a letter (`initdb.c` → `c`; `127.0.0.1` → none).
fn extension(path: &str) -> Option<&str> {
    let base = path.rsplit('/').next()?;
    let (stem, ext) = base.rsplit_once('.')?;
    (!stem.is_empty()
        && ext.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
    .then_some(ext)
}

/// Upstream files with no extension that are cited by line.
const BARE_NAMES: &[&str] = &["Makefile", "GNUmakefile", "configure"];

/// Whether a path as written can be cited by line: it has an extension, or
/// its last component is one of [`BARE_NAMES`] (`src/port/Makefile:142`,
/// `configure:591`).
fn citable(path: &str) -> bool {
    extension(path).is_some()
        || path
            .rsplit('/')
            .next()
            .is_some_and(|base| BARE_NAMES.contains(&base))
}

/// A path as written, minus leading punctuation that cannot start one.
fn trim_path(raw: &str) -> &str {
    raw.trim_start_matches(['-', '+', '.', '/'].as_slice())
}

fn digits(b: &[u8], from: usize) -> Option<(usize, usize)> {
    let len = b[from.min(b.len())..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if len == 0 || len > 7 {
        return None;
    }
    let n = std::str::from_utf8(&b[from..from + len])
        .ok()?
        .parse()
        .ok()?;
    Some((n, from + len))
}

/// After `:NNN` ends at `p`, the end of a range if one follows:
/// `-MMM`, `-:MMM` or `` `-`:MMM ``. Returns the end line and the byte after it.
fn range_end(b: &[u8], p: usize) -> Option<(usize, usize)> {
    let rest = &b[p..];
    if rest.starts_with(b"-:") {
        digits(b, p + 2)
    } else if rest.starts_with(b"`-`:") {
        digits(b, p + 4)
    } else if rest.starts_with(b"-") {
        digits(b, p + 1)
    } else {
        None
    }
}

/// The backticked identifiers that end right before `before`, nearest
/// first: `` `a` `` (`` `x.c:1` ``), and lists of them joined by commas,
/// slashes, `and` or `or` (`` `a` / `b`, `x.c:1` ``). A `()` suffix is
/// dropped; anything else ends the list.
fn idents_before(b: &[u8], before: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut k = before;
    loop {
        let mut j = k;
        loop {
            while j > 0 && matches!(b[j - 1], b' ' | b',' | b'(' | b'/') {
                j -= 1;
            }
            if !out.is_empty() && b[..j].ends_with(b" and") {
                j -= 3;
            } else if !out.is_empty() && b[..j].ends_with(b" or") {
                j -= 2;
            } else {
                break;
            }
        }
        if j == 0 || b[j - 1] != b'`' {
            return out;
        }
        let close = j - 1;
        let Some(open) = b[..close].iter().rposition(|&c| c == b'`') else {
            return out;
        };
        let tok = std::str::from_utf8(&b[open + 1..close]).unwrap_or("");
        let tok = tok.strip_suffix("()").unwrap_or(tok);
        if !is_ident(tok) {
            return out;
        }
        out.push(tok.to_owned());
        k = open;
    }
}

/// Backticked file names without a line (`` `src/interfaces/libpq/fe-auth.c` ``),
/// with their byte offsets: a continuation after one cites that file.
fn mentions(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for (k, segment) in line.split('`').enumerate() {
        let inside = k % 2 == 1;
        if inside
            && extension(segment).is_some()
            && segment.bytes().all(is_path_byte)
            && !trim_path(segment).is_empty()
        {
            out.push((offset, trim_path(segment)));
        }
        offset += segment.len() + 1;
    }
    out
}

/// Every citation in `text`, in order, with the backticked file names that
/// set the file for the continuations after them ([`Citation::mention`]).
/// Continuations are left with `path: None`; the checker attaches them to the
/// previous citation or mention that resolved.
#[must_use]
pub fn scan(text: &str) -> Vec<Citation> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let b = line.as_bytes();
        let allowed = line.contains(ALLOW_MARKER);
        let mut events: Vec<(usize, Citation)> = mentions(line)
            .into_iter()
            .map(|(at, path)| {
                let citation = Citation {
                    at: index + 1,
                    text: path.to_owned(),
                    path: Some(path.to_owned()),
                    start: 0,
                    end: 0,
                    idents: Vec::new(),
                    allowed,
                    mention: true,
                };
                (at, citation)
            })
            .collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b':' || !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                i += 1;
                continue;
            }
            let Some((start, mut p)) = digits(b, i + 1) else {
                i += 1;
                continue;
            };
            // `:NNN` must not run on into more of a word (`12:30:00`, `a:1b`).
            if b.get(p)
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b':')
            {
                i = p;
                continue;
            }
            let continuation = i > 0 && b[i - 1] == b'`';
            let (path, lead) = if continuation {
                (None, i - 1)
            } else {
                let from = b[..i]
                    .iter()
                    .rposition(|&c| !is_path_byte(c))
                    .map_or(0, |k| k + 1);
                let raw = trim_path(std::str::from_utf8(&b[from..i]).unwrap_or(""));
                if !citable(raw) {
                    i = p;
                    continue;
                }
                let lead = if from > 0 && b[from - 1] == b'`' {
                    from - 1
                } else {
                    from
                };
                (Some(raw.to_owned()), lead)
            };
            let mut end = start;
            if let Some((e, q)) = range_end(b, p) {
                end = e;
                p = q;
            }
            let idents = idents_before(b, lead);
            let text_end = if b.get(p) == Some(&b'`') { p + 1 } else { p };
            let citation = Citation {
                at: index + 1,
                text: String::from_utf8_lossy(&b[lead..text_end]).into_owned(),
                path,
                start,
                end,
                idents,
                allowed,
                mention: false,
            };
            events.push((lead, citation));
            i = p;
        }
        events.sort_by_key(|(at, _)| *at);
        out.extend(events.into_iter().map(|(_, c)| c));
    }
    out
}

/// The pristine tree, as the checker needs it.
pub trait Upstream {
    /// Every file path in the tag, `/`-separated, relative to its root.
    fn paths(&self) -> &BTreeSet<String>;
    /// The lines of one of [`Upstream::paths`].
    fn lines(&mut self, path: &str) -> Option<&[String]>;
}

/// Why a citation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// No file in the tag has that path, and neither does the repo.
    NoSuchFile,
    /// The basename is in the tag more than once and nothing says which.
    Ambiguous(Vec<String>),
    /// A continuation with no earlier citation to take its file from.
    Orphan,
    /// The file has `len` lines.
    PastEnd { path: String, len: usize },
    /// The file is in the tag's listing but could not be read.
    Unreadable { path: String },
    /// The identifier is defined in the file, but neither on the cited lines
    /// nor around them; `defined` is where it is.
    Misplaced {
        path: String,
        ident: String,
        defined: Vec<(usize, usize)>,
    },
}

/// A failed citation, located in the repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Repo-relative path of the file with the citation.
    pub file: String,
    /// The citation.
    pub citation: Citation,
    /// What is wrong with it.
    pub problem: Problem,
}

fn span_text(path: &str, (a, b): (usize, usize)) -> String {
    if a == b {
        format!("{path}:{a}")
    } else {
        format!("{path}:{a}-{b}")
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Finding {
            file,
            citation,
            problem,
        } = self;
        write!(f, "{file}:{}: {}: ", citation.at, citation.text)?;
        match problem {
            Problem::NoSuchFile => write!(f, "no such file in REL_18_6"),
            Problem::Ambiguous(candidates) => write!(
                f,
                "REL_18_6 has {} files by that name ({}); cite one with its directory",
                candidates.len(),
                candidates.join(", ")
            ),
            Problem::Orphan => write!(f, "a continuation with no file cited before it"),
            Problem::PastEnd { path, len } => {
                write!(f, "{path} has only {len} lines")
            }
            Problem::Unreadable { path } => write!(f, "cannot read {path} in the upstream tree"),
            Problem::Misplaced {
                path,
                ident,
                defined,
            } => {
                write!(
                    f,
                    "`{ident}` is not at {}",
                    span_text(path, (citation.start, citation.end))
                )?;
                let at: Vec<String> = defined.iter().map(|&s| span_text(path, s)).collect();
                write!(f, "; it is defined at {}", at.join(", "))
            }
        }
    }
}

/// Whether `word` occurs in `line` as a whole C identifier.
fn has_word(line: &str, word: &str) -> bool {
    let b = line.as_bytes();
    let word_byte = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    line.match_indices(word).any(|(k, _)| {
        let after = k + word.len();
        (k == 0 || !word_byte(b[k - 1])) && (after >= b.len() || !word_byte(b[after]))
    })
}

/// The 1-based line spans on which `ident` is defined in a C file, in the
/// PostgreSQL layout: a function whose name starts a line and whose body
/// closes with a `}` in column 0; a `#define` with its `\` continuations; a
/// `typedef`/`struct`/`enum` closed by `} ident;`.
#[must_use]
pub fn definitions(lines: &[String], ident: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let rest = line.strip_prefix(ident);
        let function = rest.is_some_and(|r| r.trim_start().starts_with('('))
            && !line.trim_end().ends_with(';');
        if function {
            // A definition reaches a `{` in column 0 before any line ends
            // in `;`; a declaration split over lines does not.
            let body = lines[k..]
                .iter()
                .find(|l| l.starts_with('{') || l.trim_end().ends_with(';'))
                .is_some_and(|l| l.starts_with('{'));
            if body {
                let close = lines[k..]
                    .iter()
                    .position(|l| l.starts_with('}'))
                    .map_or(k, |d| k + d);
                spans.push((k.saturating_sub(1) + 1, close + 1));
            }
            continue;
        }
        let define = line
            .trim_start()
            .strip_prefix('#')
            .map(str::trim_start)
            .and_then(|l| l.strip_prefix("define"))
            .map(str::trim_start)
            .and_then(|l| l.strip_prefix(ident))
            .is_some_and(|r| r.is_empty() || r.starts_with(['(', ' ', '\t']));
        if define {
            let mut end = k;
            while end + 1 < lines.len() && lines[end].trim_end().ends_with('\\') {
                end += 1;
            }
            spans.push((k + 1, end + 1));
            continue;
        }
        let closes = line
            .strip_prefix('}')
            .map(str::trim_start)
            .and_then(|l| l.strip_prefix(ident))
            .is_some_and(|r| r.trim_start().starts_with(';'));
        if closes {
            let open = lines[..k]
                .iter()
                .rposition(|l| {
                    l.starts_with("typedef") || l.starts_with("struct") || l.starts_with("enum")
                })
                .unwrap_or(k);
            spans.push((open + 1, k + 1));
        }
    }
    spans
}

/// Where a citation of our own code ends, the lint's business ends too.
fn is_c_source(path: &str) -> bool {
    matches!(extension(path), Some("c" | "h" | "y" | "l"))
}

/// The upstream directories a crate ports, in order: the last resort for a
/// bare basename.
fn crate_roots(file: &str) -> &'static [&'static str] {
    let krate = file
        .strip_prefix("crates/")
        .and_then(|rest| rest.split('/').next());
    match krate {
        Some("rinitdb") => &["src/bin/initdb/"],
        Some("rpsql") => &["src/bin/psql/", "src/fe_utils/", "src/include/fe_utils/"],
        Some("rlibpq") => &["src/interfaces/libpq/"],
        Some("testkit") => &["src/test/perl/"],
        _ => &[],
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Checks the citations of one repo file after another.
pub struct Checker<'a, U: Upstream> {
    upstream: &'a mut U,
    /// Repo-relative paths and basenames, to recognise a citation of our own
    /// code.
    local: &'a BTreeSet<String>,
    by_base: BTreeMap<String, Vec<String>>,
    anchored: usize,
}

#[derive(Clone)]
enum Resolved {
    Upstream(String),
    Local,
    Failed(Problem),
}

impl<'a, U: Upstream> Checker<'a, U> {
    /// A checker over `upstream`, recognising the repo's own files in `local`
    /// (every repo-relative path and every basename).
    pub fn new(upstream: &'a mut U, local: &'a BTreeSet<String>) -> Self {
        let mut by_base: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for path in upstream.paths() {
            let base = path.rsplit('/').next().unwrap_or(path);
            by_base
                .entry(base.to_owned())
                .or_default()
                .push(path.clone());
        }
        Checker {
            upstream,
            local,
            by_base,
            anchored: 0,
        }
    }

    /// How many citations have been resolved to an upstream file and checked,
    /// so a caller can tell a clean run from one that found nothing to check.
    #[must_use]
    pub fn anchored(&self) -> usize {
        self.anchored
    }

    fn resolve(&self, file: &str, written: &str, hints: &[String]) -> Resolved {
        let base = written.rsplit('/').next().unwrap_or(written);
        let candidates: Vec<&String> = self
            .by_base
            .get(base)
            .map(|all| {
                all.iter()
                    .filter(|p| *p == written || p.ends_with(&format!("/{written}")))
                    .collect()
            })
            .unwrap_or_default();
        match candidates.as_slice() {
            [one] => return Resolved::Upstream((*one).clone()),
            [] => {
                return if self.local.contains(written) || self.local.contains(base) {
                    Resolved::Local
                } else {
                    Resolved::Failed(Problem::NoSuchFile)
                };
            }
            _ => {}
        }
        for hint in hints.iter().rev() {
            let near: Vec<&&String> = candidates.iter().filter(|p| dir_of(p) == hint).collect();
            if let [one] = near.as_slice() {
                return Resolved::Upstream((**one).clone());
            }
        }
        for root in crate_roots(file) {
            let near: Vec<&&String> = candidates.iter().filter(|p| p.starts_with(root)).collect();
            if let [one] = near.as_slice() {
                return Resolved::Upstream((**one).clone());
            }
        }
        Resolved::Failed(Problem::Ambiguous(
            candidates.into_iter().cloned().collect(),
        ))
    }

    /// Every failed citation in `text`, the contents of repo file `file`.
    pub fn check(&mut self, file: &str, text: &str) -> Vec<Finding> {
        let mut findings = Vec::new();
        // Directories of the upstream files cited so far, oldest first.
        let mut hints: Vec<String> = Vec::new();
        // What a bare continuation cites: the file of the last citation or
        // backticked upstream file name, whatever became of it. A
        // continuation after a citation of our own code is ours; one after a
        // file that failed to resolve fails the same way, rather than falling
        // back to an older file.
        let mut last = Resolved::Failed(Problem::Orphan);
        for citation in scan(text) {
            if citation.mention {
                let written = citation.path.as_deref().unwrap_or_default();
                match self.resolve(file, written, &hints) {
                    Resolved::Upstream(path) => {
                        let dir = dir_of(&path).to_owned();
                        if hints.last() != Some(&dir) {
                            hints.push(dir);
                        }
                        last = Resolved::Upstream(path);
                    }
                    // A mention of our own code is a cross-reference in
                    // passing (`docs/divergences.md`), and a backticked `x.y`
                    // that names no file anywhere is as likely a field
                    // (`popt.topt.encoding`) or a generated file
                    // (`pg_config_paths.h`): neither changes the file a
                    // continuation cites. An upstream name the tag has twice
                    // does, and fails it.
                    Resolved::Local | Resolved::Failed(Problem::NoSuchFile) => {}
                    ambiguous @ Resolved::Failed(_) => last = ambiguous,
                }
                continue;
            }
            let resolved = match &citation.path {
                Some(written) => self.resolve(file, written, &hints),
                None => last.clone(),
            };
            if citation.path.is_some() {
                last = resolved.clone();
            }
            let path = match resolved {
                Resolved::Local => continue,
                Resolved::Failed(problem) => {
                    if !citation.allowed {
                        findings.push(Finding {
                            file: file.to_owned(),
                            citation,
                            problem,
                        });
                    }
                    continue;
                }
                Resolved::Upstream(path) => path,
            };
            let dir = dir_of(&path).to_owned();
            if hints.last() != Some(&dir) {
                hints.push(dir);
            }
            if citation.allowed {
                continue;
            }
            if let Some(problem) = self.anchor(&path, &citation) {
                findings.push(Finding {
                    file: file.to_owned(),
                    citation,
                    problem,
                });
            }
        }
        findings
    }

    /// Whether `citation`'s lines exist in `path` and, for a C source,
    /// whether its identifier is there.
    fn anchor(&mut self, path: &str, citation: &Citation) -> Option<Problem> {
        self.anchored += 1;
        let Some(lines) = self.upstream.lines(path) else {
            return Some(Problem::Unreadable {
                path: path.to_owned(),
            });
        };
        let len = lines.len();
        if citation.start == 0 || citation.end > len || citation.start > len {
            return Some(Problem::PastEnd {
                path: path.to_owned(),
                len,
            });
        }
        if !is_c_source(path) || citation.idents.is_empty() {
            return None;
        }
        let (a, b) = (
            citation.start.min(citation.end),
            citation.end.max(citation.start),
        );
        // Only a name the file defines is a claim about the file: a variable,
        // a type from elsewhere or a libc call is left alone.
        let mut first = None;
        for ident in &citation.idents {
            let defined = definitions(lines, ident);
            if defined.is_empty() {
                continue;
            }
            let on = lines[a - 1..b].iter().any(|l| has_word(l, ident));
            let inside = defined.iter().any(|&(s, e)| s <= a && b <= e);
            if on || inside {
                return None;
            }
            first.get_or_insert((ident, defined));
        }
        let (ident, defined) = first?;
        Some(Problem::Misplaced {
            path: path.to_owned(),
            ident: ident.clone(),
            defined,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        paths: BTreeSet<String>,
        files: BTreeMap<String, Vec<String>>,
    }

    impl Fake {
        fn new(files: &[(&str, &str)]) -> Self {
            let files: BTreeMap<String, Vec<String>> = files
                .iter()
                .map(|(p, t)| ((*p).to_owned(), t.lines().map(str::to_owned).collect()))
                .collect();
            Fake {
                paths: files.keys().cloned().collect(),
                files,
            }
        }
    }

    impl Upstream for Fake {
        fn paths(&self) -> &BTreeSet<String> {
            &self.paths
        }
        fn lines(&mut self, path: &str) -> Option<&[String]> {
            self.files.get(path).map(Vec::as_slice)
        }
    }

    const XLOG: &str = "/* xlog */\n\
static void\n\
InitControlFile(uint64 sysidentifier)\n\
{\n\
\tint x = 1;\n\
\tControlFile->system_identifier = sysidentifier;\n\
}\n\
#define XLOG_BLCKSZ \\\n\
\t8192\n";

    fn tree() -> Fake {
        Fake::new(&[
            ("src/backend/access/transam/xlog.c", XLOG),
            ("src/bin/psql/t/001_basic.pl", "a\nb\nc\n"),
            ("src/bin/initdb/t/001_basic.pl", "a\nb\n"),
            ("src/bin/psql/common.c", "x\n"),
            ("src/fe_utils/common.c", "y\n"),
        ])
    }

    fn check(file: &str, text: &str) -> Vec<String> {
        let mut up = tree();
        let local: BTreeSet<String> = ["crates/rpsql/src/lib.rs", "lib.rs", "divergences.md"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut checker = Checker::new(&mut up, &local);
        checker
            .check(file, text)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_forms_of_a_citation_are_recognised() {
        let found = scan(
            "see `src/bin/initdb/initdb.c:2634`, `:2960` and (`xlog.c:4217`-`:4218`)\n\
             x.pl:153-164. y.c:22-:43 at 12:30 on 127.0.0.1:5432 `cancel.c` (`:195`-`:224`)",
        );
        let short: Vec<(Option<&str>, usize, usize)> = found
            .iter()
            .map(|c| (c.path.as_deref(), c.start, c.end))
            .collect();
        assert_eq!(
            short,
            [
                (Some("src/bin/initdb/initdb.c"), 2634, 2634),
                (None, 2960, 2960),
                (Some("xlog.c"), 4217, 4218),
                (Some("x.pl"), 153, 164),
                (Some("y.c"), 22, 43),
                (Some("cancel.c"), 0, 0),
                (None, 195, 224),
            ]
        );
        assert_eq!(found[2].text, "`xlog.c:4217`-`:4218`");
    }

    #[test]
    fn the_backticked_identifier_right_before_is_taken() {
        let found = scan(
            "(`InitControlFile`, `xlog.c:4217`) `canonicalize_path()` (`path.c:337`) \
             `a` / `b`, `c` and `d` (`x.c:1`)",
        );
        assert_eq!(found[0].idents, ["InitControlFile"]);
        assert_eq!(found[1].idents, ["canonicalize_path"]);
        assert_eq!(found[2].idents, ["d", "c", "b", "a"]);
        let found = scan("`not an ident`, `xlog.c:1` and x.c:1");
        assert!(found[0].idents.is_empty());
        assert!(found[1].idents.is_empty());
    }

    #[test]
    fn a_right_citation_passes() {
        let text = "(`InitControlFile`, `xlog.c:6`-`:7`) and `:3`, `XLOG_BLCKSZ` (`xlog.c:9`)";
        assert_eq!(check("docs/x.md", text), Vec::<String>::new());
    }

    #[test]
    fn a_wrong_identifier_fails_and_names_the_right_line() {
        let got = check("docs/x.md", "`InitControlFile` (`xlog.c:1`)");
        assert_eq!(
            got,
            ["docs/x.md:1: `xlog.c:1`: `InitControlFile` is not at \
              src/backend/access/transam/xlog.c:1; it is defined at \
              src/backend/access/transam/xlog.c:2-7"]
        );
    }

    #[test]
    fn a_name_the_file_does_not_define_is_not_checked() {
        // `sysidentifier` is a parameter, `ControlFile` a global from
        // elsewhere: neither is a function, macro or typedef of the file.
        assert_eq!(
            check(
                "docs/x.md",
                "`sysidentifier` (`xlog.c:1`), `ControlFile` (`xlog.c:1`)"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn one_name_of_a_list_on_the_line_is_enough() {
        assert_eq!(
            check("docs/x.md", "`XLOG_BLCKSZ` / `InitControlFile`, `xlog.c:3`"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_makefile_or_configure_is_cited_too() {
        let found = scan("`src/port/Makefile:142`, `configure:591`, `:966`, key:1");
        let short: Vec<(Option<&str>, usize)> =
            found.iter().map(|c| (c.path.as_deref(), c.start)).collect();
        assert_eq!(
            short,
            [
                (Some("src/port/Makefile"), 142),
                (Some("configure"), 591),
                (None, 966)
            ]
        );
    }

    #[test]
    fn a_line_past_the_end_fails() {
        let got = check("docs/x.md", "xlog.c:9-10");
        assert_eq!(
            got,
            ["docs/x.md:1: xlog.c:9-10: src/backend/access/transam/xlog.c has only 9 lines"]
        );
    }

    #[test]
    fn a_path_not_in_the_tag_fails_unless_it_is_ours() {
        assert_eq!(
            check("docs/x.md", "nosuch.c:1 lib.rs:3 divergences.md:21"),
            ["docs/x.md:1: nosuch.c:1: no such file in REL_18_6"]
        );
    }

    #[test]
    fn an_ambiguous_basename_takes_its_directory_from_context() {
        let text =
            "src/bin/psql/t/001_basic.pl:1\nt/001_basic.pl:3\nsrc/fe_utils/common.c:1\ncommon.c:1";
        assert_eq!(check("docs/x.md", text), Vec::<String>::new());
        let got = check("docs/x.md", "common.c:1");
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("2 files by that name"), "{got:?}");
        // A crate's own upstream directory is the last resort.
        assert_eq!(
            check("crates/rpsql/src/a.rs", "common.c:1"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_allow_marker_is_the_only_way_out() {
        assert_eq!(
            check(
                "docs/x.md",
                "nosuch.c:1 <!-- citation-lint: allow: pgrust -->"
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_continuation_after_our_own_code_is_ours() {
        assert_eq!(
            check("docs/x.md", "lib.rs:3 and `:4`"),
            Vec::<String>::new()
        );
        assert_eq!(
            check("docs/x.md", "`:4`"),
            ["docs/x.md:1: `:4`: a continuation with no file cited before it"]
        );
    }

    #[test]
    fn a_continuation_takes_the_upstream_file_named_last() {
        // `docs/divergences.md` is ours and mentioned in passing: `:1` after
        // it still cites `xlog.c`.
        assert_eq!(
            check(
                "docs/x.md",
                "`xlog.c:9` (`docs/divergences.md`), `InitControlFile` `:1`"
            ),
            ["docs/x.md:1: `:1`: `InitControlFile` is not at \
              src/backend/access/transam/xlog.c:1; it is defined at \
              src/backend/access/transam/xlog.c:2-7"]
        );
        // A file the tag has twice fails its continuation, rather than
        // leaving it to `xlog.c`.
        let got = check("docs/x.md", "`xlog.c:9` `common.c` `:1`");
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(
            got[0].starts_with("docs/x.md:1: `:1`: REL_18_6 has 2 files"),
            "{got:?}"
        );
        // So does a continuation after a citation that failed.
        assert_eq!(
            check("docs/x.md", "`xlog.c:9` `nosuch.c:1` and `:2`"),
            [
                "docs/x.md:1: `nosuch.c:1`: no such file in REL_18_6",
                "docs/x.md:1: `:2`: no such file in REL_18_6",
            ]
        );
    }

    #[test]
    fn a_file_the_tree_cannot_read_fails() {
        let mut up = tree();
        up.files.remove("src/bin/psql/common.c");
        let local = BTreeSet::new();
        let mut checker = Checker::new(&mut up, &local);
        let got: Vec<String> = checker
            .check("docs/x.md", "src/bin/psql/common.c:1")
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            got,
            [
                "docs/x.md:1: src/bin/psql/common.c:1: cannot read src/bin/psql/common.c in the upstream tree"
            ]
        );
    }

    #[test]
    fn excluded_paths_are_prefixes() {
        assert!(is_excluded("progress.md"));
        assert!(is_excluded("target/debug/x"));
        assert!(!is_excluded("crates/rpsql/src/lib.rs"));
    }
}
