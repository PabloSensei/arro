//! The storage behind a document. `ropey` is an implementation detail of [`RopeStore`].

use std::ops::Range;

use ropey::{LineType, Rope};

/// Text addressed by byte offsets. Lines end with `\n`, so a text of `n` line ends has
/// `n + 1` lines. A clone is a cheap snapshot that later edits do not change.
pub trait TextStore: Clone + Send + Sync + 'static {
    fn from_text(text: &str) -> Self;
    fn len(&self) -> usize;
    fn line_count(&self) -> usize;
    /// Offset of the first byte of `line`; `line_count()` gives `len()`.
    fn line_to_offset(&self, line: usize) -> usize;
    fn offset_to_line(&self, offset: usize) -> usize;
    fn is_char_boundary(&self, offset: usize) -> bool;
    fn slice(&self, range: Range<usize>) -> String;
    fn replace(&mut self, range: Range<usize>, text: &str);
    /// The chunk holding `offset` and the offset of its first byte. Empty at the end.
    fn chunk_at(&self, offset: usize) -> (&str, usize);
    fn offset_to_utf16(&self, offset: usize) -> usize;
    fn utf16_to_offset(&self, utf16: usize) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The end of `line`, before its `\n`.
    fn line_end(&self, line: usize) -> usize {
        if line + 1 < self.line_count() {
            self.line_to_offset(line + 1) - 1
        } else {
            self.len()
        }
    }

    fn line_text(&self, line: usize) -> String {
        self.slice(self.line_to_offset(line)..self.line_end(line))
    }

    /// The nearest character boundary before `offset`, or 0.
    fn prev_boundary(&self, offset: usize) -> usize {
        let mut offset = offset.saturating_sub(1);
        while offset > 0 && !self.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// The nearest character boundary after `offset`, or the end of the text.
    fn next_boundary(&self, offset: usize) -> usize {
        let mut offset = (offset + 1).min(self.len());
        while offset < self.len() && !self.is_char_boundary(offset) {
            offset += 1;
        }
        offset
    }

    fn char_at(&self, offset: usize) -> Option<char> {
        (offset < self.len())
            .then(|| {
                self.slice(offset..self.next_boundary(offset))
                    .chars()
                    .next()
            })
            .flatten()
    }

    fn text(&self) -> String {
        self.slice(0..self.len())
    }
}

#[derive(Clone, Default)]
pub struct RopeStore(Rope);

impl std::fmt::Debug for RopeStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RopeStore({} bytes)", self.len())
    }
}

impl TextStore for RopeStore {
    fn from_text(text: &str) -> Self {
        Self(Rope::from_str(text))
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn line_count(&self) -> usize {
        self.0.len_lines(LineType::LF)
    }

    fn line_to_offset(&self, line: usize) -> usize {
        self.0.line_to_byte_idx(line, LineType::LF)
    }

    fn offset_to_line(&self, offset: usize) -> usize {
        self.0.byte_to_line_idx(offset, LineType::LF)
    }

    fn is_char_boundary(&self, offset: usize) -> bool {
        self.0.is_char_boundary(offset)
    }

    fn slice(&self, range: Range<usize>) -> String {
        String::from(self.0.slice(range))
    }

    fn replace(&mut self, range: Range<usize>, text: &str) {
        if !range.is_empty() {
            self.0.remove(range.clone());
        }
        if !text.is_empty() {
            self.0.insert(range.start, text);
        }
    }

    fn chunk_at(&self, offset: usize) -> (&str, usize) {
        if offset >= self.0.len() {
            return ("", self.0.len());
        }
        self.0.chunk(offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.0.byte_to_utf16_idx(offset)
    }

    fn utf16_to_offset(&self, utf16: usize) -> usize {
        self.0.utf16_to_byte_idx(utf16)
    }
}
