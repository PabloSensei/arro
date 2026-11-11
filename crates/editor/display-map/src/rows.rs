//! The layer that does not change rows yet. Block decorations replace it.

#[derive(Clone, Copy, Debug, Default)]
pub struct BlockRows;

impl BlockRows {
    pub fn row_count(self, wrap_rows: usize) -> usize {
        wrap_rows
    }

    pub fn to_wrap_row(self, row: usize) -> usize {
        row
    }

    pub fn from_wrap_row(self, row: usize) -> usize {
        row
    }
}
