//! Justified normalizations applied to both sides before a gate diffs them.
//!
//! A gate is byte-for-byte, so every normalizer narrows what it proves and has
//! to earn its place. Each one here is a pure `fn(&str) -> String` that carries
//! the single line of justification the method asks for
//! (`docs/test-stealing.md`, rule 3) and names the upstream `printf` whose
//! output it rewrites. Nothing else is normalized: a gate that needs a fourth
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

/// The three normalizers above, in the order a psql gate wants them.
pub const DEFAULT: [Normalizer; 3] = [TIMING, PID, SYSTEM_IDENTIFIER];

/// Placeholder written in place of an elapsed time.
pub const ELAPSED_PLACEHOLDER: &str = "Time: <elapsed>";
/// Placeholder written in place of a PID.
pub const PID_PLACEHOLDER: &str = "<pid>";
/// Placeholder written in place of a system identifier.
pub const SYSTEM_IDENTIFIER_PLACEHOLDER: &str = "<system identifier>";

const SYSTEM_IDENTIFIER_LABEL: &str = "Database system identifier:";

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
        let (head, tail) = rest.split_at(at + MARKER.len());
        out.push_str(head);
        let digits = tail.len() - tail.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits > 0 {
            out.push_str(PID_PLACEHOLDER);
        }
        rest = &tail[digits..];
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
