//! Rows that stand above a line of the buffer. A block holds a label of its own and no
//! byte of the text: a code vision hint, for one. Its row takes the indent of the line
//! under it.
//!
//! A block sticks to a byte offset, so an edit moves it the way it moves the text. A
//! block on the first line of a collapsed region stays above the region; one inside the
//! region is not shown.

use text_core::Edit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The byte offset of the line the block stands above.
    pub offset: usize,
    /// The label of the row.
    pub text: String,
    /// The leading columns of the line under the block.
    pub indent: usize,
    /// The block stands after the last line of the text, not above a line.
    pub after: bool,
}

#[derive(Clone, Debug, Default)]
pub struct BlockRows {
    /// Sorted by offset, one block per line.
    blocks: Vec<Block>,
}

impl BlockRows {
    pub fn as_slice(&self) -> &[Block] {
        &self.blocks
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Replaces the blocks. Two labelled blocks on one line become one row, their labels
    /// joined by two spaces; an empty block is a row of its own, so a view can keep two
    /// texts on the same lines with empty rows.
    pub fn set(&mut self, mut blocks: Vec<Block>) {
        blocks.sort_by_key(|block| (block.offset, block.after));
        let mut joined: Vec<Block> = Vec::with_capacity(blocks.len());
        for block in blocks {
            match joined.last_mut() {
                Some(last)
                    if last.offset == block.offset
                        && last.after == block.after
                        && !last.text.is_empty()
                        && !block.text.is_empty() =>
                {
                    last.text.push_str("  ");
                    last.text.push_str(&block.text);
                }
                _ => joined.push(block),
            }
        }
        self.blocks = joined;
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
    }

    /// A block inside a replaced piece goes away, later ones move with the text.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        for edit in edits.iter().rev() {
            let delta = edit.text.len() as isize - edit.range.len() as isize;
            self.blocks
                .retain(|block| block.offset <= edit.range.start || block.offset >= edit.range.end);
            for block in &mut self.blocks {
                if block.offset >= edit.range.end && block.offset > edit.range.start {
                    block.offset = block.offset.checked_add_signed(delta).unwrap_or(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Block;
    use super::BlockRows;
    use text_core::Edit;

    fn block(offset: usize, text: &str) -> Block {
        Block {
            offset,
            text: text.to_owned(),
            indent: 0,
            after: false,
        }
    }

    #[test]
    fn an_edit_moves_a_block_and_an_edit_over_it_takes_it_away() {
        let mut blocks = BlockRows::default();
        blocks.set(vec![block(8, "2 usages")]);
        blocks.apply_edits(&[Edit::insert(2, "xx")]);
        assert_eq!(blocks.as_slice()[0].offset, 10);
        blocks.apply_edits(&[Edit::new(4..12, String::new())]);
        assert!(blocks.is_empty(), "the line of the block went away");
    }

    #[test]
    fn two_blocks_of_one_line_form_one_row() {
        let mut blocks = BlockRows::default();
        blocks.set(vec![block(0, "2 usages"), block(0, "1 implementation")]);
        assert_eq!(blocks.as_slice().len(), 1);
        assert_eq!(blocks.as_slice()[0].text, "2 usages  1 implementation");
    }

    #[test]
    fn empty_blocks_of_one_line_stay_separate_rows() {
        let mut blocks = BlockRows::default();
        blocks.set(vec![block(4, ""), block(4, ""), block(4, "2 usages")]);
        assert_eq!(blocks.as_slice().len(), 3, "each empty block is a row");
        assert_eq!(blocks.as_slice()[2].text, "2 usages");
    }
}
