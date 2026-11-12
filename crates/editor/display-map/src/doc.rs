//! Documentation comments shown as rendered blocks. A comment of a range leaves the
//! rows of the buffer for the rows its markup builds; the text itself does not change.

use std::ops::Range;

use text_core::Edit;

/// How a run of a rendered line is drawn. The view gives each kind its own look.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocRunKind {
    Text,
    Code,
    Strong,
    Emphasis,
    /// A reference that opens a declaration; `target` is the reference of the language.
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocRun {
    pub text: String,
    pub kind: DocRunKind,
    /// The reference of a link, `None` for the other kinds.
    pub target: Option<String>,
}

impl DocRun {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: DocRunKind::Text,
            target: None,
        }
    }
}

/// One row of a rendered comment, with the columns of the comment under it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocLine {
    pub indent: usize,
    pub runs: Vec<DocRun>,
}

impl DocLine {
    pub fn text(&self) -> String {
        self.runs.iter().map(|run| run.text.as_str()).collect()
    }
}

/// A comment that takes the rows of its range and stands as this markup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocComment {
    pub range: Range<usize>,
    pub lines: Vec<DocLine>,
}

#[derive(Clone, Debug, Default)]
pub struct DocRows {
    /// Sorted by start offset, one comment per range.
    comments: Vec<DocComment>,
}

impl DocRows {
    pub fn as_slice(&self) -> &[DocComment] {
        &self.comments
    }

    pub fn is_empty(&self) -> bool {
        self.comments.is_empty()
    }

    pub fn set(&mut self, mut comments: Vec<DocComment>) {
        comments.retain(|comment| comment.range.start < comment.range.end);
        comments.sort_by_key(|comment| comment.range.start);
        comments.dedup_by(|next, kept| next.range == kept.range);
        self.comments = comments;
    }

    pub fn clear(&mut self) {
        self.comments.clear();
    }

    /// A comment the edit touches goes back to text; the others move with the buffer.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            self.comments.retain(|comment| {
                edit.range.end < comment.range.start || edit.range.start > comment.range.end
            });
            let delta = edit.text.len() as isize - edit.range.len() as isize;
            let moved = |offset: usize| offset.checked_add_signed(delta).unwrap_or(0);
            for comment in &mut self.comments {
                if comment.range.start >= edit.range.end {
                    comment.range.start = moved(comment.range.start);
                }
                if comment.range.end >= edit.range.end {
                    comment.range.end = moved(comment.range.end);
                }
            }
        }
    }

    /// The comment that holds the whole line, when there is one.
    pub fn covering_line<S: text_core::TextStore>(
        &self,
        store: &S,
        line: usize,
    ) -> Option<&DocComment> {
        let start = store.line_to_offset(line);
        let end = store.line_end(line);
        self.comments
            .iter()
            .find(|comment| comment.range.start <= start && end <= comment.range.end)
    }

    /// The comment that holds `offset`.
    pub fn containing(&self, offset: usize) -> Option<&DocComment> {
        self.comments
            .iter()
            .find(|comment| comment.range.start <= offset && offset < comment.range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::{DocComment, DocLine, DocRows};
    use text_core::Edit;

    fn comment(start: usize, end: usize) -> DocComment {
        DocComment {
            range: start..end,
            lines: vec![DocLine::default()],
        }
    }

    #[test]
    fn an_edit_over_a_comment_returns_its_text_and_moves_the_others() {
        let mut rows = DocRows::default();
        rows.set(vec![comment(4, 8), comment(20, 24)]);
        rows.apply_edits(&[Edit::new(6..7, "xx".to_owned())]);
        assert_eq!(rows.as_slice().len(), 1, "the touched comment is gone");
        assert_eq!(rows.as_slice()[0].range, 21..25);
        rows.apply_edits(&[Edit::insert(0, "yy")]);
        assert_eq!(rows.as_slice()[0].range, 23..27);
    }
}
