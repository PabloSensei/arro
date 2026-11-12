//! Builds one display row. Column is a byte in `DisplayLine::text`, with tabs expanded:
//! the view turns it into pixels through the shaped line.

use std::ops::Range;

use text_core::TextStore;

use crate::fold::Fold;
use crate::inlay::Inlay;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct DisplayPoint {
    pub row: usize,
    pub column: usize,
}

/// Which side wins when a column falls inside something that has no buffer text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bias {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayChunk {
    /// Buffer text. Maps to the buffer byte for byte.
    Text {
        buffer_start: usize,
    },
    /// A tab, expanded with spaces.
    Tab {
        buffer_start: usize,
    },
    /// A collapsed fold. Clicking it should expand it.
    Placeholder {
        fold_start: usize,
        fold_end: usize,
    },
    Inlay {
        offset: usize,
    },
}

/// The row of the block layer: a label of its own instead of buffer text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockText {
    /// Leading columns, taken from the line under the block.
    pub indent: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayLine {
    pub text: String,
    /// The piece and the range of `text` it covers, in order.
    pub chunks: Vec<(Range<usize>, DisplayChunk)>,
    /// Buffer offset at the end of the row.
    pub end_offset: usize,
    pub start_offset: usize,
    /// The row goes on from the one above, which soft wrap broke.
    pub continuation: bool,
    /// The row stands above the text: it holds this label, not bytes of the buffer.
    pub block: Option<BlockText>,
}

impl DisplayLine {
    /// The part of the row in `columns`, as a row of its own.
    pub fn slice(&self, columns: Range<usize>) -> DisplayLine {
        let mut chunks = Vec::new();
        for (range, chunk) in &self.chunks {
            let start = range.start.max(columns.start);
            let end = range.end.min(columns.end);
            if start >= end {
                continue;
            }
            let moved = start - range.start;
            let kept = match chunk {
                DisplayChunk::Text { buffer_start } => DisplayChunk::Text {
                    buffer_start: buffer_start + moved,
                },
                // A tab cut in two stays where it begins.
                DisplayChunk::Tab { .. } if moved > 0 => continue,
                other => other.clone(),
            };
            chunks.push((start - columns.start..end - columns.start, kept));
        }
        let whole = columns.start == 0 && columns.end >= self.text.len();
        DisplayLine {
            text: self.text[columns.clone()].to_owned(),
            chunks,
            start_offset: if columns.start == 0 {
                self.start_offset
            } else {
                self.offset_of(columns.start, Bias::Left)
            },
            end_offset: if whole || columns.end >= self.text.len() {
                self.end_offset
            } else {
                self.offset_of(columns.end, Bias::Left)
            },
            continuation: columns.start > 0,
            block: None,
        }
    }

    pub fn column_of(&self, offset: usize) -> usize {
        for (range, chunk) in &self.chunks {
            match chunk {
                DisplayChunk::Text { buffer_start } => {
                    let len = range.len();
                    if offset >= *buffer_start && offset < buffer_start + len {
                        return range.start + offset - buffer_start;
                    }
                }
                DisplayChunk::Tab { buffer_start } if offset == *buffer_start => {
                    return range.start;
                }
                DisplayChunk::Placeholder {
                    fold_start,
                    fold_end,
                } if offset >= *fold_start && offset < *fold_end => {
                    return range.start;
                }
                _ => {}
            }
        }
        self.text.len()
    }

    pub fn offset_of(&self, column: usize, bias: Bias) -> usize {
        for (range, chunk) in &self.chunks {
            if column >= range.end {
                continue;
            }
            if column < range.start {
                break;
            }
            return match chunk {
                DisplayChunk::Text { buffer_start } => buffer_start + column - range.start,
                DisplayChunk::Tab { buffer_start } => match bias {
                    Bias::Right if column > range.start => buffer_start + 1,
                    _ => *buffer_start,
                },
                DisplayChunk::Placeholder {
                    fold_start,
                    fold_end,
                } => match bias {
                    Bias::Left => *fold_start,
                    Bias::Right => *fold_end,
                },
                DisplayChunk::Inlay { offset } => *offset,
            };
        }
        self.end_offset
    }
}

pub(crate) fn build_line<S: TextStore>(
    store: &S,
    lines: Range<usize>,
    folds: &[Fold],
    inlays: &[Inlay],
    tab_size: usize,
) -> DisplayLine {
    let start = store.line_to_offset(lines.start);
    let end = store.line_end(lines.end - 1);
    let mut line = DisplayLine {
        start_offset: start,
        end_offset: end,
        ..DisplayLine::default()
    };

    let first_fold = folds.partition_point(|fold| fold.range.end <= start);
    let row_folds = folds[first_fold..]
        .iter()
        .take_while(|fold| fold.range.start <= end);
    let first_inlay = inlays.partition_point(|inlay| inlay.offset < start);
    let mut row_inlays = inlays[first_inlay..]
        .iter()
        .take_while(|inlay| inlay.offset <= end)
        .peekable();

    let mut position = start;
    for fold in row_folds {
        push_text(
            &mut line,
            store,
            position..fold.range.start,
            &mut row_inlays,
            tab_size,
        );
        let column = line.text.len();
        line.text.push_str(&fold.placeholder);
        line.chunks.push((
            column..line.text.len(),
            DisplayChunk::Placeholder {
                fold_start: fold.range.start,
                fold_end: fold.range.end,
            },
        ));
        position = fold.range.end;
        while row_inlays
            .next_if(|inlay| inlay.offset < position)
            .is_some()
        {}
    }
    push_text(&mut line, store, position..end, &mut row_inlays, tab_size);
    for inlay in row_inlays {
        push_inlay(&mut line, inlay);
    }
    line
}

fn push_inlay(line: &mut DisplayLine, inlay: &Inlay) {
    let column = line.text.len();
    line.text.push_str(&inlay.text);
    line.chunks.push((
        column..line.text.len(),
        DisplayChunk::Inlay {
            offset: inlay.offset,
        },
    ));
}

fn push_text<'a, S: TextStore>(
    line: &mut DisplayLine,
    store: &S,
    range: Range<usize>,
    inlays: &mut std::iter::Peekable<impl Iterator<Item = &'a Inlay>>,
    tab_size: usize,
) {
    let mut position = range.start;
    while position < range.end {
        while let Some(inlay) = inlays.next_if(|inlay| inlay.offset <= position) {
            push_inlay(line, inlay);
        }
        let stop = inlays
            .peek()
            .map_or(range.end, |inlay| inlay.offset.min(range.end));
        let text = store.slice(position..stop);
        for piece in text.split_inclusive('\t') {
            let (body, tab) = match piece.strip_suffix('\t') {
                Some(body) => (body, true),
                None => (piece, false),
            };
            if !body.is_empty() {
                let column = line.text.len();
                line.text.push_str(body);
                line.chunks.push((
                    column..line.text.len(),
                    DisplayChunk::Text {
                        buffer_start: position,
                    },
                ));
                position += body.len();
            }
            if tab {
                let column = line.text.len();
                let shown = line.text[line.text.rfind('\n').map_or(0, |at| at + 1)..]
                    .chars()
                    .count();
                let width = tab_size - shown % tab_size;
                line.text.extend(std::iter::repeat_n(' ', width));
                line.chunks.push((
                    column..line.text.len(),
                    DisplayChunk::Tab {
                        buffer_start: position,
                    },
                ));
                position += 1;
            }
        }
    }
}
