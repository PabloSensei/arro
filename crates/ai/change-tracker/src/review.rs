//! Review of a session file: its hunks against the base the session started from, and the text
//! that accepting or reverting one of them leaves. Nothing here writes; the caller decides,
//! because an editor may hold the file with unsaved edits.

use std::ops::Range;

use vcs::{Hunk, line_hunks, revert_hunk};

/// One changed piece. Lines are zero-based, `before` in the base and `after` on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewHunk {
    pub before: Range<u32>,
    pub after: Range<u32>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

impl ReviewHunk {
    fn hunk(&self) -> Hunk {
        Hunk {
            before: self.before.clone(),
            after: self.after.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReview {
    /// Relative to the project, with forward slashes.
    pub path: String,
    /// `None` when the file did not exist at the start.
    pub base: Option<String>,
    /// `None` when the file is gone now.
    pub current: Option<String>,
    pub hunks: Vec<ReviewHunk>,
    /// Not UTF-8: whole-file actions only.
    pub binary: bool,
}

fn lines(text: &str, range: &Range<u32>) -> Vec<String> {
    let all = text.split_inclusive('\n');
    let picked = all.skip(range.start as usize).take(range.len());
    let trimmed = picked.map(|line| line.trim_end_matches(['\n', '\r']).to_string());
    trimmed.collect()
}

fn apply(text: &str, edit: &vcs::TextEdit) -> String {
    let (head, tail) = (&text[..edit.range.start], &text[edit.range.end..]);
    format!("{head}{}{tail}", edit.text)
}

/// A file that existed on neither side ends up absent again, not empty.
fn absent_if_empty(text: String, existed: bool) -> Option<String> {
    (existed || !text.is_empty()).then_some(text)
}

impl FileReview {
    pub fn new(path: String, base: Option<Vec<u8>>, current: Option<Vec<u8>>) -> Self {
        let decode = |bytes: Option<Vec<u8>>| bytes.map(String::from_utf8);
        let (base, current) = (decode(base), decode(current));
        let binary = matches!(base, Some(Err(_))) || matches!(current, Some(Err(_)));
        let base = base.and_then(Result::ok);
        let current = current.and_then(Result::ok);
        let hunks = if binary {
            Vec::new()
        } else {
            let (before, after) = (
                base.as_deref().unwrap_or(""),
                current.as_deref().unwrap_or(""),
            );
            line_hunks(before, after)
                .into_iter()
                .map(|hunk| ReviewHunk {
                    removed: lines(before, &hunk.before),
                    added: lines(after, &hunk.after),
                    before: hunk.before,
                    after: hunk.after,
                })
                .collect()
        };
        Self {
            path,
            base,
            current,
            hunks,
            binary,
        }
    }

    fn texts(&self) -> (&str, &str) {
        let before = self.base.as_deref().unwrap_or("");
        let after = self.current.as_deref().unwrap_or("");
        (before, after)
    }

    /// The text on disk with hunk `index` put back as it was. `Some(None)`: the file goes.
    pub fn reverted_hunk(&self, index: usize) -> Option<Option<String>> {
        let hunk = self.hunks.get(index)?;
        let (before, after) = self.texts();
        let text = apply(after, &revert_hunk(before, after, &hunk.hunk()));
        Some(absent_if_empty(text, self.base.is_some()))
    }

    /// The base with hunk `index` taken in, so the review no longer shows it.
    pub fn accepted_hunk(&self, index: usize) -> Option<Option<String>> {
        let hunk = self.hunks.get(index)?;
        let (before, after) = self.texts();
        let swapped = Hunk {
            before: hunk.after.clone(),
            after: hunk.before.clone(),
        };
        let text = apply(before, &revert_hunk(after, before, &swapped));
        Some(absent_if_empty(text, self.current.is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(base: Option<&str>, current: Option<&str>) -> FileReview {
        let bytes = |text: Option<&str>| text.map(|text| text.as_bytes().to_vec());
        FileReview::new("a.txt".to_string(), bytes(base), bytes(current))
    }

    #[test]
    fn hunks_carry_their_lines() {
        let review = review(Some("a\nb\nc\nd\n"), Some("a\nB\nc\nd\ne\n"));

        assert_eq!(review.hunks.len(), 2);
        assert_eq!(review.hunks[0].removed, ["b"]);
        assert_eq!(review.hunks[0].added, ["B"]);
        assert_eq!(review.hunks[1].added, ["e"]);
    }

    #[test]
    fn reverting_one_hunk_keeps_the_other() {
        let review = review(Some("a\nb\nc\nd\n"), Some("a\nB\nc\nd\ne\n"));

        assert_eq!(
            review.reverted_hunk(0),
            Some(Some("a\nb\nc\nd\ne\n".to_string()))
        );
        assert_eq!(
            review.reverted_hunk(1),
            Some(Some("a\nB\nc\nd\n".to_string()))
        );
        assert_eq!(review.reverted_hunk(2), None);
    }

    #[test]
    fn accepting_a_hunk_moves_the_base() {
        let review = review(Some("a\nb\nc\nd\n"), Some("a\nB\nc\nd\ne\n"));

        let base = review.accepted_hunk(0).unwrap();
        assert_eq!(base.as_deref(), Some("a\nB\nc\nd\n"));
        let rest = FileReview::new(
            "a.txt".to_string(),
            base.map(String::into_bytes),
            review.current.clone().map(String::into_bytes),
        );
        assert_eq!(rest.hunks.len(), 1);
    }

    #[test]
    fn an_added_file_reverts_to_nothing_and_crlf_survives() {
        let added = review(None, Some("x\r\ny\r\n"));
        assert_eq!(added.reverted_hunk(0), Some(None));

        let edited = review(Some("a\r\nb\r\n"), Some("a\r\nB\r\n"));
        assert_eq!(edited.hunks[0].added, ["B"]);
        assert_eq!(
            edited.reverted_hunk(0),
            Some(Some("a\r\nb\r\n".to_string()))
        );
    }

    #[test]
    fn binary_files_have_no_hunks() {
        let review = FileReview::new("b.bin".to_string(), Some(vec![0xff, 0xfe]), Some(vec![1]));

        assert!(review.binary);
        assert!(review.hunks.is_empty());
    }
}
