use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput};

/// Changed lines: `before` in the base, `after` in the current text, zero-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub before: Range<u32>,
    pub after: Range<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChangeKind {
    Added,
    Modified,
    /// Lines went away above `lines.start`, `lines` is empty.
    Deleted,
}

/// What the gutter paints for one hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub lines: Range<u32>,
    pub kind: LineChangeKind,
}

/// Replace `range` of the current text, in bytes, with `text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

/// Lines that differ, compared line by line with the histogram algorithm git uses too.
pub fn line_hunks(before: &str, after: &str) -> Vec<Hunk> {
    let input = InternedInput::new(before, after);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let hunks = diff.hunks().map(|hunk| Hunk {
        before: hunk.before,
        after: hunk.after,
    });
    hunks.collect()
}

pub fn gutter(hunks: &[Hunk]) -> Vec<LineChange> {
    let change = |hunk: &Hunk| {
        let kind = match (hunk.before.is_empty(), hunk.after.is_empty()) {
            (true, _) => LineChangeKind::Added,
            (false, true) => LineChangeKind::Deleted,
            (false, false) => LineChangeKind::Modified,
        };
        LineChange {
            lines: hunk.after.clone(),
            kind,
        }
    };
    hunks.iter().map(change).collect()
}

/// The edit that puts the base lines of `hunk` back into the current text. Lines are
/// compared with their newlines, so a missing newline at the end is part of the hunk.
pub fn revert_hunk(before: &str, after: &str, hunk: &Hunk) -> TextEdit {
    let text = before[line_span(before, &hunk.before)].to_owned();
    let range = line_span(after, &hunk.after);
    TextEdit { range, text }
}

/// Bytes of `lines`, newlines included. A line past the end is the end.
fn line_span(text: &str, lines: &Range<u32>) -> Range<usize> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let at = |line: u32| starts.get(line as usize).copied().unwrap_or(text.len());
    at(lines.start)..at(lines.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, edit: &TextEdit) -> String {
        format!(
            "{}{}{}",
            &text[..edit.range.start],
            edit.text,
            &text[edit.range.end..]
        )
    }

    #[test]
    fn added_modified_and_deleted_lines_are_told_apart() {
        let before = "a\nb\nc\nd\n";
        let after = "a\nB\nc\nnew\n";

        let changes = gutter(&line_hunks(before, after));

        let kinds: Vec<_> = changes.iter().map(|change| change.kind).collect();
        assert_eq!(kinds, [LineChangeKind::Modified, LineChangeKind::Modified]);
        assert_eq!(changes[0].lines, 1..2);

        let changes = gutter(&line_hunks("a\nc\n", "a\nb\nc\n"));
        assert_eq!(
            changes,
            [LineChange {
                lines: 1..2,
                kind: LineChangeKind::Added
            }]
        );

        let changes = gutter(&line_hunks("a\nb\nc\n", "a\nc\n"));
        assert_eq!(
            changes,
            [LineChange {
                lines: 1..1,
                kind: LineChangeKind::Deleted
            }]
        );
    }

    #[test]
    fn same_texts_have_no_hunks() {
        assert!(line_hunks("a\nb\n", "a\nb\n").is_empty());
    }

    #[test]
    fn reverting_each_hunk_brings_back_the_base() {
        let cases = [
            ("a\nb\nc\n", "a\nB\nc\n"),
            ("a\nc\n", "a\nb\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nb", "a\nB"),
            ("a\nb", "a\n"),
            ("a\n", "a\nb"),
            ("", "a\nb\n"),
            ("a\nb\n", ""),
            ("x\nкириллица\ny\n", "x\nлатиница\ny\n"),
        ];
        for (before, after) in cases {
            let mut text = after.to_owned();
            // From the end, so earlier hunks keep their line numbers.
            for hunk in line_hunks(before, after).iter().rev() {
                text = apply(&text, &revert_hunk(before, &text, hunk));
            }

            assert_eq!(text, before, "{before:?} <- {after:?}");
        }
    }

    #[test]
    fn one_hunk_is_reverted_and_the_rest_stays() {
        let before = "a\nb\nc\nd\ne\n";
        let after = "a\nB\nc\nd\nE\n";
        let hunks = line_hunks(before, after);

        let text = apply(after, &revert_hunk(before, after, &hunks[0]));

        assert_eq!(text, "a\nb\nc\nd\nE\n");
    }
}
