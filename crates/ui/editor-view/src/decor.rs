//! What one view of a text adds to it: empty rows that keep two views on the same lines,
//! and the backgrounds of a comparison. None of it is part of the text: a copy, a
//! selection and the caret never see it.

use std::ops::Range;

use display_map::Block;
use text_core::TextStore;
use ui_kit::Hsla;

/// The rows and backgrounds a view paints over the text of an editor.
#[derive(Clone, Debug, Default)]
pub struct PaneDecor {
    /// Empty rows above the given buffer lines, counted from zero, and how many.
    pub fillers: Vec<(usize, usize)>,
    /// Empty rows after the last line of the text.
    pub trailing_fillers: usize,
    /// Whole buffer lines with a background, counted from zero.
    pub lines: Vec<(usize, Hsla)>,
    /// Byte ranges with a background over the line one.
    pub ranges: Vec<(Range<usize>, Hsla)>,
}

impl PaneDecor {
    /// Whether the view adds nothing to the rows of the text.
    pub fn is_plain(&self) -> bool {
        self.fillers.is_empty() && self.trailing_fillers == 0
    }

    /// The empty rows as blocks of the display map.
    pub(crate) fn blocks<S: TextStore>(&self, store: &S) -> Vec<Block> {
        let mut blocks = Vec::new();
        for (line, count) in &self.fillers {
            let line = (*line).min(store.line_count().saturating_sub(1));
            let offset = store.line_to_offset(line);
            for _ in 0..*count {
                blocks.push(Block {
                    offset,
                    text: String::new(),
                    indent: 0,
                    after: false,
                });
            }
        }
        for _ in 0..self.trailing_fillers {
            blocks.push(Block {
                offset: store.len(),
                text: String::new(),
                indent: 0,
                after: true,
            });
        }
        blocks
    }
}
