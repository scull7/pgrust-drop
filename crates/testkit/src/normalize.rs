//! Justified normalizations applied to both sides before a gate diffs them.
//!
//! A gate is byte-for-byte, so every normalizer narrows what it proves and has
//! to earn its place. Each one here is a pure `fn(&str) -> String` that carries
//! the single line of justification the method asks for
//! (`docs/test-stealing.md`, rule 3) and names the upstream `printf` whose
//! output it rewrites. Nothing else is normalized: a gate that needs another
//! normalizer adds it here, with its justification, rather than loosening a
//! comparison at the call site.

/// A justified rewrite of one output stream.
///
/// Data, not behaviour: [`crate::gate`] applies these in the order given.
#[derive(Debug, Clone, Copy)]
pub struct Normalizer {
    /// Short identifier, printed when a gate reports what it normalized.
    pub name: &'static str,
    /// Why the underlying text is legitimately nondeterministic.
    pub justification: &'static str,
    /// The pure rewrite.
    pub apply: fn(&str) -> String,
}

impl Normalizer {
    /// Apply this normalizer.
    #[must_use]
    pub fn normalize(&self, text: &str) -> String {
        (self.apply)(text)
    }
}

/// Apply `normalizers` left to right.
#[must_use]
pub fn apply_all(text: &str, normalizers: &[Normalizer]) -> String {
    normalizers
        .iter()
        .fold(text.to_owned(), |text, normalizer| {
            normalizer.normalize(&text)
        })
}

/// psql's `\timing` line is a wall-clock measurement of the last query, so it
/// differs on every run of either tool.
///
/// Upstream: `src/bin/psql/common.c:608` prints `Time: %.3f ms`, and
/// `common.c:623`, `:632` and `:639` print the same value with an
/// hours/minutes/days breakdown appended; replacing the whole line covers all
/// four forms.
pub const TIMING: Normalizer = Normalizer {
    name: "timing",
    justification: "psql prints the measured query duration (src/bin/psql/common.c:608)",
    apply: timing,
};

/// A backend PID is assigned by the operating system and differs per run.
///
/// Upstream: `src/bin/psql/common.c:755` and `:758` print "received from
/// server process with PID %d." for an asynchronous notification.
pub const PID: Normalizer = Normalizer {
    name: "pid",
    justification: "the OS assigns backend PIDs (src/bin/psql/common.c:755)",
    apply: pid,
};

/// The cluster's system identifier is built from the creation timestamp and
/// the creating process's PID, so a freshly initialized cluster never repeats
/// one.
///
/// Upstream: `src/include/catalog/pg_control.h:107`, "Unique system identifier
/// --- to ensure we match up xlog files with the installation that produced
/// them".
pub const SYSTEM_IDENTIFIER: Normalizer = Normalizer {
    name: "system-identifier",
    justification: "each cluster gets a fresh system identifier (src/include/catalog/pg_control.h:107)",
    apply: system_identifier,
};

/// A distribution appends its own vendor and package revision to the version
/// string the C tool prints, so PGDG's Ubuntu build of `initdb` answers
/// `initdb (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)` where a stock 18.6
/// build — and this port — answers `initdb (PostgreSQL) 18.6`. Neither side
/// is wrong: both print the `PG_VERSION` they were compiled with.
///
/// Upstream: `src/bin/initdb/initdb.c` prints
/// `puts("initdb (PostgreSQL) " PG_VERSION)`, and `configure.ac`'s
/// `--with-extra-version` is the only thing that appends a parenthesized
/// suffix to that `PG_VERSION`.
///
/// Unlike the three above this one *removes* rather than substitutes: the
/// candidate has no suffix at all, so writing a placeholder would move the
/// difference instead of settling it. It is deliberately narrow — it fires
/// only on a whole line of the shape `<progname> (PostgreSQL) <version>
/// (<extra>)` and never touches the version itself, so `18.6` and `19.1`
/// still differ after it. A normalizer that made those two compare equal
/// would destroy exactly what the gate exists to prove.
pub const EXTRA_VERSION: Normalizer = Normalizer {
    name: "extra-version",
    justification: "a distribution's --with-extra-version is appended to the PG_VERSION initdb prints (src/bin/initdb/initdb.c)",
    apply: extra_version,
};

/// The three nondeterminism normalizers, in the order a psql gate wants them.
///
/// [`EXTRA_VERSION`] is deliberately not one of them: only a gate that runs
/// `--version` has a line for it to rewrite, and a gate should carry no
/// normalizer it does not need.
pub const DEFAULT: [Normalizer; 3] = [TIMING, PID, SYSTEM_IDENTIFIER];

/// Placeholder written in place of an elapsed time.
pub const ELAPSED_PLACEHOLDER: &str = "Time: <elapsed>";
/// Placeholder written in place of a PID.
pub const PID_PLACEHOLDER: &str = "<pid>";
/// Placeholder written in place of a system identifier.
pub const SYSTEM_IDENTIFIER_PLACEHOLDER: &str = "<system identifier>";

const SYSTEM_IDENTIFIER_LABEL: &str = "Database system identifier:";

/// The fixed middle of upstream's version line, the only anchor this file has
/// for one: `progname`, the version and any suffix are all build-dependent.
const VERSION_MARKER: &str = " (PostgreSQL) ";

fn timing(text: &str) -> String {
    map_lines(text, |line| {
        if line.starts_with("Time: ") {
            ELAPSED_PLACEHOLDER.to_owned()
        } else {
            line.to_owned()
        }
    })
}

fn pid(text: &str) -> String {
    const MARKER: &str = "PID ";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        // "RAPID 42" is not a PID: the marker only counts at a word boundary,
        // or a normalizer would erase real numbers and mask a difference.
        let preceding = if at > 0 {
            rest[..at].chars().last()
        } else {
            out.chars().last()
        };
        let boundary = preceding.is_none_or(|before| !before.is_alphanumeric() && before != '_');
        let (head, tail) = rest.split_at(at + MARKER.len());
        out.push_str(head);
        let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if boundary && digits > 0 {
            out.push_str(PID_PLACEHOLDER);
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

fn system_identifier(text: &str) -> String {
    map_lines(text, |line| match line.find(SYSTEM_IDENTIFIER_LABEL) {
        Some(at) => {
            let end = at + SYSTEM_IDENTIFIER_LABEL.len();
            format!("{} {SYSTEM_IDENTIFIER_PLACEHOLDER}", &line[..end])
        }
        None => line.to_owned(),
    })
}

fn extra_version(text: &str) -> String {
    map_lines(text, |line| {
        strip_extra_version(line).unwrap_or_else(|| line.to_owned())
    })
}

/// The line without its trailing parenthetical, and `None` for every line that
/// is not exactly `<progname> (PostgreSQL) <version> (<extra>)`.
///
/// Each of the three guards is the reason this cannot erase a real difference:
/// without them the marker would fire in the middle of prose, over a token
/// that is not a version, or over a trailing word that is not a suffix at all.
fn strip_extra_version(line: &str) -> Option<String> {
    let (progname, rest) = line.split_once(VERSION_MARKER)?;
    let (version, suffix) = rest.split_once(' ')?;
    if !is_progname(progname) || !is_version(version) || !is_parenthesized(suffix) {
        return None;
    }
    Some(format!("{progname}{VERSION_MARKER}{version}"))
}

/// One bare word: `get_progname` never yields whitespace or a parenthesis, and
/// demanding that keeps the marker from matching in the middle of a sentence.
fn is_progname(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| !c.is_whitespace() && c != '(' && c != ')')
}

/// A version number, not an arbitrary token: `18.6`, `18beta1`, `19devel`.
fn is_version(word: &str) -> bool {
    word.starts_with(|c: char| c.is_ascii_digit())
        && word.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// The whole remainder of the line is one parenthetical, which is the shape
/// `--with-extra-version` leaves; anything else is a difference to keep.
fn is_parenthesized(suffix: &str) -> bool {
    suffix.len() >= 2
        && suffix.starts_with('(')
        && suffix.ends_with(')')
        && !suffix[1..suffix.len() - 1].contains(['(', ')'])
}

/// Rewrite each line, keeping the exact line terminators: a normalizer must
/// never be the reason a trailing newline appears or disappears.
fn map_lines(text: &str, rewrite: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for chunk in text.split_inclusive('\n') {
        match chunk.strip_suffix('\n') {
            Some(body) => {
                out.push_str(&rewrite(body));
                out.push('\n');
            }
            None => out.push_str(&rewrite(chunk)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_collapses_every_upstream_form() {
        let before = "Time: 1.234 ms\n\
                      Time: 61234.000 ms (01:01.234)\n\
                      Time: 3661234.000 ms (01:01:01.234)\n\
                      Time: 90061234.000 ms (1 d 01:01:01.234)\n";
        assert_eq!(
            timing(before),
            "Time: <elapsed>\nTime: <elapsed>\nTime: <elapsed>\nTime: <elapsed>\n"
        );
    }

    #[test]
    fn timing_leaves_other_lines_alone() {
        let before = " Time: 1.234 ms\nTiming is on.\ntotal Time: 5 ms\n";
        assert_eq!(timing(before), before);
    }

    #[test]
    fn timing_keeps_a_missing_final_newline() {
        assert_eq!(timing("Time: 1.234 ms"), "Time: <elapsed>");
    }

    #[test]
    fn pid_replaces_only_the_digits_after_the_marker() {
        let before =
            "Asynchronous notification \"x\" received from server process with PID 4210.\n";
        assert_eq!(
            pid(before),
            "Asynchronous notification \"x\" received from server process with PID <pid>.\n"
        );
    }

    #[test]
    fn pid_handles_several_occurrences_and_no_digits() {
        assert_eq!(
            pid("PID 1 and PID 22 and PID x"),
            "PID <pid> and PID <pid> and PID x"
        );
    }

    /// Reviewer's case: the marker must not fire inside a longer word.
    #[test]
    fn pid_does_not_fire_inside_a_word() {
        assert_eq!(pid("RAPID 42 rows"), "RAPID 42 rows");
        assert_eq!(pid("rapid 42"), "rapid 42");
        assert_eq!(pid("_PID 42"), "_PID 42");
        assert_eq!(pid("(PID 42)"), "(PID <pid>)");
    }

    #[test]
    fn pid_leaves_unrelated_numbers_alone() {
        assert_eq!(pid("2 rows, 18 columns"), "2 rows, 18 columns");
    }

    #[test]
    fn system_identifier_keeps_the_label() {
        let before = "Database system identifier:            7412345678901234567\n";
        assert_eq!(
            system_identifier(before),
            "Database system identifier: <system identifier>\n"
        );
    }

    #[test]
    fn system_identifier_leaves_other_control_fields_alone() {
        let before =
            "pg_control version number:            1800\nCatalog version number: 202504071\n";
        assert_eq!(system_identifier(before), before);
    }

    /// The exact two lines the CI gate diffed: PGDG's Ubuntu 18.6 build of C
    /// `initdb` against this port's stock-build `PG_VERSION`.
    #[test]
    fn a_distribution_extra_version_suffix_is_stripped_so_the_two_sides_match() {
        let reference = "initdb (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)\n";
        let candidate = "initdb (PostgreSQL) 18.6\n";

        let reference = apply_all(reference, &[EXTRA_VERSION]);
        let candidate = apply_all(candidate, &[EXTRA_VERSION]);

        assert_eq!(reference, "initdb (PostgreSQL) 18.6\n");
        assert_eq!(reference, candidate);
    }

    /// The failure mode this normalizer is designed against: it must not make
    /// a real version difference disappear.
    #[test]
    fn two_genuinely_different_versions_still_differ_after_normalization() {
        let theirs = "initdb (PostgreSQL) 19.1 (Ubuntu 19.1-1.pgdg24.04+2)\n";
        let ours = "initdb (PostgreSQL) 18.6\n";

        let theirs = apply_all(theirs, &[EXTRA_VERSION]);
        let ours = apply_all(ours, &[EXTRA_VERSION]);

        assert_eq!(theirs, "initdb (PostgreSQL) 19.1\n");
        assert_ne!(theirs, ours);
    }

    #[test]
    fn a_version_line_with_no_suffix_is_left_exactly_as_it_is() {
        let before = "initdb (PostgreSQL) 18.6\npsql (PostgreSQL) 18beta1\n";
        assert_eq!(extra_version(before), before);
    }

    #[test]
    fn only_a_trailing_parenthetical_counts_as_a_suffix() {
        assert_eq!(
            extra_version("initdb (PostgreSQL) 18.6 (Ubuntu) trailing words"),
            "initdb (PostgreSQL) 18.6 (Ubuntu) trailing words"
        );
        assert_eq!(
            extra_version("initdb (PostgreSQL) 18.6 and then some"),
            "initdb (PostgreSQL) 18.6 and then some"
        );
        assert_eq!(
            extra_version("initdb (PostgreSQL) 18.6 (a (nested) one)"),
            "initdb (PostgreSQL) 18.6 (a (nested) one)"
        );
    }

    #[test]
    fn the_marker_does_not_fire_outside_a_version_line() {
        // A progname is one word, so prose around the marker is not a match.
        assert_eq!(
            extra_version("built against (PostgreSQL) 18.6 (Ubuntu 18.6-1)"),
            "built against (PostgreSQL) 18.6 (Ubuntu 18.6-1)"
        );
        // ... and what follows the marker has to look like a version.
        assert_eq!(
            extra_version("initdb (PostgreSQL) server (Ubuntu 18.6-1)"),
            "initdb (PostgreSQL) server (Ubuntu 18.6-1)"
        );
        assert_eq!(
            extra_version("initdb initializes a PostgreSQL database cluster.\n"),
            "initdb initializes a PostgreSQL database cluster.\n"
        );
    }

    #[test]
    fn extra_version_keeps_a_missing_final_newline() {
        assert_eq!(
            extra_version("initdb (PostgreSQL) 18.6 (Ubuntu 18.6-1.pgdg24.04+2)"),
            "initdb (PostgreSQL) 18.6"
        );
    }

    /// It rewrites a `--version` line and nothing else, so no gate that lacks
    /// one — every other gate here — needs to carry it.
    #[test]
    fn extra_version_is_not_one_of_the_default_normalizers() {
        assert!(!DEFAULT.iter().any(|it| it.name == EXTRA_VERSION.name));
        assert!(EXTRA_VERSION.justification.contains("src/"));
    }

    #[test]
    fn apply_all_runs_in_order_and_is_identity_when_empty() {
        let before = "Time: 1.5 ms\nPID 99\n";
        assert_eq!(apply_all(before, &[]), before);
        assert_eq!(apply_all(before, &DEFAULT), "Time: <elapsed>\nPID <pid>\n");
    }

    #[test]
    fn every_default_normalizer_carries_a_justification() {
        for normalizer in &DEFAULT {
            assert!(!normalizer.name.is_empty());
            assert!(
                normalizer.justification.contains("src/"),
                "{} must cite the upstream source",
                normalizer.name
            );
        }
    }
}
