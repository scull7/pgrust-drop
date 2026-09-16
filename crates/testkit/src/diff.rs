//! Pure unified-diff rendering, for reporting a failed gate.
//!
//! pg_regress compares a test's actual output with the expected file by
//! shelling out to diff(1) with `-U3` (`src/test/regress/pg_regress.c:65`,
//! `pretty_diff_opts = "-U3"`, used by `results_differ` at
//! `src/test/regress/pg_regress.c:1537`). We render the same shape of report
//! in-process so a gate needs no external diff(1) and works identically on
//! every platform.
//!
//! This module decides nothing: whether two outputs match is a byte comparison
//! made in [`crate::gate`]. Everything here only makes a mismatch readable.

use std::fmt::Write as _;

/// Lines of unchanged context around each change, matching `-U3` above.
pub const CONTEXT_LINES: usize = 3;

/// Cap on the Myers edit distance before the renderer stops looking for a
/// minimal script and prints one whole-file replacement hunk instead. Two
/// outputs that far apart are not usefully diffed line by line anyway, and the
/// bound keeps the O(D^2) trace from growing without limit.
pub const MAX_EDIT_DISTANCE: usize = 4096;

/// One step of an edit script over the two line vectors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    /// Line `.0` of the before side equals line `.1` of the after side.
    Equal(usize, usize),
    /// Line `.0` of the before side is absent from the after side.
    Delete(usize),
    /// Line `.0` of the after side is absent from the before side.
    Insert(usize),
}

impl Edit {
    fn is_change(self) -> bool {
        !matches!(self, Edit::Equal(..))
    }
}

/// Split into lines that keep their `\n`, so that "ends without a newline" is
/// itself a difference the diff can show.
#[must_use]
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// Render a `diff -U3` of `before` against `after`, or `None` when the two
/// strings are byte-identical.
///
/// The labels become the `---` and `+++` header lines.
#[must_use]
pub fn unified(before: &str, after: &str, before_label: &str, after_label: &str) -> Option<String> {
    if before == after {
        return None;
    }
    let a = lines(before);
    let b = lines(after);
    let edits = shortest_edit_script(&a, &b);
    let mut out = format!("--- {before_label}\n+++ {after_label}\n");
    for (start, end) in hunks(&edits) {
        render_hunk(&mut out, &a, &b, &edits[start..=end]);
    }
    Some(out)
}

/// Myers' greedy algorithm ("An O(ND) Difference Algorithm", 1986): walk
/// diagonals of the edit graph until one reaches the bottom-right corner, then
/// backtrack through the recorded traces. Falls back to a whole-file
/// replacement past [`MAX_EDIT_DISTANCE`].
fn shortest_edit_script(before: &[&str], after: &[&str]) -> Vec<Edit> {
    match trace_to_corner(before, after) {
        Some(trace) => backtrack(&trace, before, after),
        None => (0..before.len())
            .map(Edit::Delete)
            .chain((0..after.len()).map(Edit::Insert))
            .collect(),
    }
}

/// Furthest-reaching paths after each edit distance `d`, up to the one that
/// reaches `(a.len(), b.len())`. `None` if that takes more than
/// [`MAX_EDIT_DISTANCE`] edits.
///
/// Diagonals `k = x - y` run from `-d` to `d`; they are stored at `k + offset`
/// so every index stays a `usize`, and `y` is recovered as `x + offset - ki`.
fn trace_to_corner(before: &[&str], after: &[&str]) -> Option<Vec<Vec<usize>>> {
    let (rows, cols) = (before.len(), after.len());
    let max = rows + cols;
    let offset = max + 1;
    let limit = max.min(MAX_EDIT_DISTANCE);
    let mut furthest = vec![0usize; 2 * max + 3];
    let mut trace = Vec::new();
    for distance in 0..=limit {
        trace.push(furthest.clone());
        let mut ki = offset - distance;
        while ki <= offset + distance {
            // Extend the path that reached furthest: down from k+1, or right
            // from k-1 (which costs one more column).
            let mut x = if ki == offset - distance
                || (ki != offset + distance && furthest[ki - 1] < furthest[ki + 1])
            {
                furthest[ki + 1]
            } else {
                furthest[ki - 1] + 1
            };
            let mut y = x + offset - ki;
            while x < rows && y < cols && before[x] == after[y] {
                x += 1;
                y += 1;
            }
            furthest[ki] = x;
            if x >= rows && y >= cols {
                return Some(trace);
            }
            ki += 2;
        }
    }
    None
}

/// Walk the traces backwards, emitting the edit that led to each state.
fn backtrack(trace: &[Vec<usize>], before: &[&str], after: &[&str]) -> Vec<Edit> {
    let offset = before.len() + after.len() + 1;
    let (mut x, mut y) = (before.len(), after.len());
    let mut edits = Vec::new();
    for (distance, furthest) in trace.iter().enumerate().rev() {
        if distance == 0 {
            // Everything left is the opening snake from the origin.
            while x > 0 && y > 0 {
                x -= 1;
                y -= 1;
                edits.push(Edit::Equal(x, y));
            }
            break;
        }
        let ki = x + offset - y;
        let prev_ki = if ki == offset - distance
            || (ki != offset + distance && furthest[ki - 1] < furthest[ki + 1])
        {
            ki + 1
        } else {
            ki - 1
        };
        let prev_x = furthest[prev_ki];
        // Saturating so that a diagnostic renderer can never panic and hide
        // the gate failure it was called to explain; on the reachable
        // diagonals `prev_ki <= prev_x + offset` always holds.
        let prev_y = (prev_x + offset).saturating_sub(prev_ki);
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            edits.push(Edit::Equal(x, y));
        }
        if distance > 0 {
            if x == prev_x {
                y -= 1;
                edits.push(Edit::Insert(y));
            } else {
                x -= 1;
                edits.push(Edit::Delete(x));
            }
        }
    }
    edits.reverse();
    edits
}

/// Inclusive edit-index ranges to print: every change plus [`CONTEXT_LINES`]
/// around it, with neighbouring changes merged when their context would touch.
fn hunks(edits: &[Edit]) -> Vec<(usize, usize)> {
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < edits.len() {
        if !edits[i].is_change() {
            i += 1;
            continue;
        }
        let start = i.saturating_sub(CONTEXT_LINES);
        let mut last = i;
        while let Some(next) = next_change(edits, last) {
            if next - last <= 2 * CONTEXT_LINES + 1 {
                last = next;
            } else {
                break;
            }
        }
        hunks.push((start, (last + CONTEXT_LINES).min(edits.len() - 1)));
        i = last + 1;
    }
    hunks
}

fn next_change(edits: &[Edit], after: usize) -> Option<usize> {
    edits
        .iter()
        .enumerate()
        .skip(after + 1)
        .find(|(_, edit)| edit.is_change())
        .map(|(index, _)| index)
}

fn render_hunk(out: &mut String, before: &[&str], after: &[&str], hunk: &[Edit]) {
    let (mut a_start, mut a_count) = (None, 0usize);
    let (mut b_start, mut b_count) = (None, 0usize);
    for edit in hunk {
        match *edit {
            Edit::Equal(ai, bi) => {
                a_start.get_or_insert(ai);
                b_start.get_or_insert(bi);
                a_count += 1;
                b_count += 1;
            }
            Edit::Delete(ai) => {
                a_start.get_or_insert(ai);
                a_count += 1;
            }
            Edit::Insert(bi) => {
                b_start.get_or_insert(bi);
                b_count += 1;
            }
        }
    }
    let a_first = a_start.map_or(0, |start| start + 1);
    let b_first = b_start.map_or(0, |start| start + 1);
    // Writing to a String is infallible.
    let _ = writeln!(out, "@@ -{a_first},{a_count} +{b_first},{b_count} @@");
    for edit in hunk {
        match *edit {
            Edit::Equal(ai, _) => push_line(out, ' ', before[ai]),
            Edit::Delete(ai) => push_line(out, '-', before[ai]),
            Edit::Insert(bi) => push_line(out, '+', after[bi]),
        }
    }
}

/// One diff line. A line that does not end in `\n` gets diff(1)'s marker, so a
/// missing final newline is visible rather than invisible.
fn push_line(out: &mut String, sign: char, line: &str) {
    out.push(sign);
    if let Some(body) = line.strip_suffix('\n') {
        out.push_str(body);
        out.push('\n');
    } else {
        out.push_str(line);
        out.push_str("\n\\ No newline at end of file\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";

    #[test]
    fn identical_text_has_no_diff() {
        assert_eq!(unified(ABC, ABC, "reference", "candidate"), None);
    }

    #[test]
    fn a_changed_line_is_shown_with_three_lines_of_context() {
        let changed = ABC.replace("e\n", "E\n");
        let diff = unified(ABC, &changed, "reference", "candidate").expect("texts differ");
        assert_eq!(
            diff,
            "\
--- reference
+++ candidate
@@ -2,7 +2,7 @@
 b
 c
 d
-e
+E
 f
 g
 h
"
        );
    }

    #[test]
    fn distant_changes_become_separate_hunks() {
        let changed = ABC.replace("a\n", "A\n").replace("i\n", "I\n");
        let diff = unified(ABC, &changed, "reference", "candidate").expect("texts differ");
        assert_eq!(diff.matches("@@ ").count(), 2, "{diff}");
    }

    #[test]
    fn near_changes_merge_into_one_hunk() {
        let changed = ABC.replace("d\n", "D\n").replace("f\n", "F\n");
        let diff = unified(ABC, &changed, "reference", "candidate").expect("texts differ");
        assert_eq!(diff.matches("@@ ").count(), 1, "{diff}");
    }

    #[test]
    fn a_missing_final_newline_is_flagged() {
        let diff = unified("a\n", "a", "reference", "candidate").expect("texts differ");
        assert!(diff.contains("\\ No newline at end of file"), "{diff}");
    }

    #[test]
    fn an_empty_side_is_a_pure_insertion() {
        let diff = unified("", "a\nb\n", "reference", "candidate").expect("texts differ");
        assert_eq!(
            diff,
            "\
--- reference
+++ candidate
@@ -0,0 +1,2 @@
+a
+b
"
        );
    }

    #[test]
    fn trailing_lines_are_deleted_not_rewritten() {
        let diff = unified("a\nb\nc\n", "a\n", "reference", "candidate").expect("texts differ");
        assert!(diff.contains("-b\n-c\n"), "{diff}");
        assert!(!diff.contains("-a"), "{diff}");
    }

    #[test]
    fn wildly_different_text_still_renders_a_diff() {
        let numbered = |prefix: char| {
            let mut text = String::new();
            for line in 0..MAX_EDIT_DISTANCE {
                let _ = writeln!(text, "{prefix}{line}");
            }
            text
        };
        let (before, after) = (numbered('l'), numbered('r'));
        let diff = unified(&before, &after, "reference", "candidate").expect("texts differ");
        assert!(
            diff.starts_with("--- reference\n+++ candidate\n@@ "),
            "{diff}"
        );
    }
}
