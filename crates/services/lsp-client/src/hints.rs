//! Hints of a document that follow its edits, and the places a range covers.
//!
//! A server answers `textDocument/inlayHint` for the version it saw. The editor keeps
//! showing those hints while the user types, so each edit moves them. A hint inside a
//! replaced piece goes away: its place says nothing any more.

use lsp_types::{InlayHint, Position, Range, TextDocumentContentChangeEvent};

/// The hints of one version of a document.
#[derive(Clone, Debug, Default)]
pub struct InlayHints {
    version: i32,
    range: Range,
    hints: Vec<InlayHint>,
}

impl InlayHints {
    pub fn new(version: i32, range: Range, hints: Vec<InlayHint>) -> Self {
        Self {
            version,
            range,
            hints,
        }
    }

    pub fn version(&self) -> i32 {
        self.version
    }

    pub fn hints(&self) -> &[InlayHint] {
        &self.hints
    }

    /// Whether an answer for `version` and `range` may be replaced by this one.
    pub fn matches(&self, version: i32, range: &Range) -> bool {
        self.version == version && self.range == *range
    }

    /// Moves the hints along the changes that lead to `version`. A change without a
    /// range replaces the whole text, and every hint of the answer is dropped.
    pub fn apply(&mut self, version: i32, changes: &[TextDocumentContentChangeEvent]) {
        for change in changes {
            let Some(range) = change.range else {
                self.hints.clear();
                continue;
            };
            let end = end_of(range, &change.text);
            self.hints.retain_mut(|hint| {
                let Some(position) = move_position(hint.position, range, end) else {
                    return false;
                };
                hint.position = position;
                true
            });
        }
        self.version = version;
    }
}

/// The end of the new text inside a change that starts at `range.start`: a line and a
/// character, counted the way LSP does.
fn end_of(range: Range, text: &str) -> Position {
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    let mut line = range.start.line;
    let mut character = range.start.character + first.encode_utf16().count() as u32;
    for next in lines {
        line += 1;
        character = next.encode_utf16().count() as u32;
    }
    Position::new(line, character)
}

/// Where a hint at `position` lands after the text of `range` becomes the text that ends
/// at `end`, or `None` when the hint was inside the replaced piece.
pub fn move_position(position: Position, range: Range, end: Position) -> Option<Position> {
    if position <= range.start {
        return Some(position);
    }
    if position >= range.end {
        let line_delta = end.line as isize - range.end.line as isize;
        let character = if position.line == range.end.line {
            (end.character as isize + position.character as isize - range.end.character as isize)
                .max(0) as u32
        } else {
            position.character
        };
        let line = (position.line as isize + line_delta).max(0) as u32;
        return Some(Position::new(line, character));
    }
    None
}

/// Whether a point is inside a range, both ends included.
pub fn in_range(range: &Range, position: Position) -> bool {
    position >= range.start && position <= range.end
}

#[cfg(test)]
mod tests {
    use lsp_types::{Position, Range, TextDocumentContentChangeEvent};

    use super::{InlayHints, in_range};

    fn hint(line: u32, character: u32) -> lsp_types::InlayHint {
        lsp_types::InlayHint {
            position: Position::new(line, character),
            label: "flag:".to_owned().into(),
            kind: None,
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        }
    }

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn an_edit_moves_the_hints_after_it_and_drops_the_ones_inside() {
        let mut hints = InlayHints::new(
            3,
            Range::default(),
            vec![hint(0, 10), hint(0, 3), hint(1, 2)],
        );

        hints.apply(4, &[edit((0, 2), (0, 4), "yyy")]);

        let moved: Vec<_> = hints
            .hints()
            .iter()
            .map(|hint| (hint.position.line, hint.position.character))
            .collect();
        assert_eq!(moved, [(0, 11), (1, 2)]);
        assert_eq!(hints.version(), 4, "the hints describe the new text");
        assert!(hints.matches(4, &Range::default()));
        assert!(
            !hints.matches(3, &Range::default()),
            "an answer for an older version is dropped"
        );
    }

    #[test]
    fn a_change_with_a_new_line_moves_the_hints_to_the_line_below() {
        let mut hints = InlayHints::new(1, Range::default(), vec![hint(0, 10)]);
        hints.apply(2, &[edit((0, 2), (0, 4), "a\nb")]);
        let position = hints.hints()[0].position;
        assert_eq!((position.line, position.character), (1, 7));
    }

    #[test]
    fn an_answer_for_one_range_does_not_replace_one_for_another() {
        let hints = InlayHints::new(
            1,
            Range::new(Position::new(0, 0), Position::new(4, 0)),
            vec![],
        );
        assert!(hints.matches(1, &Range::new(Position::new(0, 0), Position::new(4, 0))));
        assert!(!hints.matches(1, &Range::new(Position::new(5, 0), Position::new(9, 0))));
    }

    #[test]
    fn a_range_covers_its_ends() {
        let range = Range::new(Position::new(1, 2), Position::new(1, 6));
        assert!(in_range(&range, Position::new(1, 2)));
        assert!(in_range(&range, Position::new(1, 6)));
        assert!(!in_range(&range, Position::new(1, 7)));
        assert!(!in_range(&range, Position::new(0, 6)));
    }
}
