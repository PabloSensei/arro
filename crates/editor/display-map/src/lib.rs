//! Maps buffer text to what is on screen. The chain is BufferRow -> Fold -> Inlay ->
//! Tab -> Wrap -> Block -> DisplayRow. Wrap is off until the view gives it columns.
//! A block is a row of its own above a line, with a label that is no part of the text.

mod fold;
mod inlay;
mod layout;
mod rows;
pub mod wrap;

pub use fold::{Fold, FoldMap};
pub use inlay::{Inlay, InlayMap};
pub use layout::{Bias, BlockText, DisplayChunk, DisplayLine, DisplayPoint};
pub use rows::{Block, BlockRows};

use std::ops::Range;

use text_core::{Edit, TextStore};

#[derive(Clone, Debug)]
pub struct DisplayMap {
    pub folds: FoldMap,
    pub inlays: InlayMap,
    pub tab_size: usize,
    /// Soft wrap: the columns a row breaks at, `None` for no wrap.
    wrap_columns: Option<usize>,
    block: BlockRows,
}

impl Default for DisplayMap {
    fn default() -> Self {
        Self::new(4)
    }
}

impl DisplayMap {
    pub fn new(tab_size: usize) -> Self {
        Self {
            folds: FoldMap::default(),
            inlays: InlayMap::default(),
            tab_size: tab_size.max(1),
            wrap_columns: None,
            block: BlockRows::default(),
        }
    }

    pub fn set_wrap(&mut self, columns: Option<usize>) {
        self.wrap_columns = columns.map(|columns| columns.max(2));
    }

    pub fn wrap(&self) -> Option<usize> {
        self.wrap_columns
    }

    /// Moves folds, inlays and blocks along one step of edits.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        self.folds.apply_edits(edits);
        self.inlays.apply_edits(edits);
        self.block.apply_edits(edits);
    }

    /// Replaces the rows that stand above lines of the buffer.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.block.set(blocks);
    }

    pub fn blocks(&self) -> &[Block] {
        self.block.as_slice()
    }

    pub fn snapshot<'a, S: TextStore>(&'a self, store: &'a S) -> DisplaySnapshot<'a, S> {
        let visible = self.folds.visible_lines(store);
        let mut snapshot = DisplaySnapshot {
            map: self,
            store,
            visible,
            wrapped: None,
            blocks_before: Vec::new(),
            block_prefix: vec![0],
        };
        if let Some(columns) = self.wrap_columns {
            snapshot.wrapped = Some(snapshot.wrap_rows(columns));
        }
        snapshot.place_blocks();
        snapshot
    }
}

/// One display row of a wrapped snapshot.
struct WrappedRow {
    fold_row: u32,
    /// The columns of the whole fold row that this row shows.
    columns: Range<usize>,
}

struct Wrapped {
    rows: Vec<WrappedRow>,
    /// The first display row of each fold row.
    first: Vec<u32>,
}

/// A view of the map at one text version.
pub struct DisplaySnapshot<'a, S: TextStore> {
    map: &'a DisplayMap,
    store: &'a S,
    /// Buffer line of each row before the wrap and block layers.
    visible: Vec<u32>,
    wrapped: Option<Wrapped>,
    /// The block shown before each wrap row, as an index into the block list.
    blocks_before: Vec<Option<usize>>,
    /// How many block rows stand before each wrap row; one more entry than the rows.
    block_prefix: Vec<usize>,
}

impl<S: TextStore> DisplaySnapshot<'_, S> {
    /// Places every block above the first display row of its line. A block on a line
    /// the folds hide has no row of its own.
    fn place_blocks(&mut self) {
        let rows = match &self.wrapped {
            Some(wrapped) => wrapped.rows.len(),
            None => self.visible.len(),
        };
        self.blocks_before = vec![None; rows];
        self.block_prefix = vec![0; rows + 1];
        if self.map.block.is_empty() || rows == 0 {
            return;
        }
        let mut placed = vec![None; rows];
        for (index, block) in self.map.block.as_slice().iter().enumerate() {
            let line = self
                .store
                .offset_to_line(block.offset.min(self.store.len()));
            let fold_row = self
                .visible
                .partition_point(|visible| *visible as usize <= line)
                .saturating_sub(1);
            if self.visible.get(fold_row).copied() != Some(line as u32) {
                continue;
            }
            let wrap_row = match &self.wrapped {
                Some(wrapped) => wrapped
                    .first
                    .get(fold_row)
                    .copied()
                    .map_or(0, |row| row as usize),
                None => fold_row,
            };
            if let Some(row) = placed.get_mut(wrap_row) {
                *row = Some(index);
            }
        }
        for (row, block) in placed.into_iter().enumerate() {
            self.blocks_before[row] = block;
            self.block_prefix[row + 1] = self.block_prefix[row] + usize::from(block.is_some());
        }
    }

    fn block_at(&self, wrap_row: usize) -> Option<&Block> {
        let index = (*self.blocks_before.get(wrap_row)?)?;
        self.map.block.as_slice().get(index)
    }

    pub fn row_count(&self) -> usize {
        let rows = match &self.wrapped {
            Some(wrapped) => wrapped.rows.len(),
            None => self.visible.len(),
        };
        rows + self.block_prefix.get(rows).copied().unwrap_or(0)
    }

    /// Whether the row holds the label of a block and no text of the buffer.
    pub fn is_block_row(&self, row: usize) -> bool {
        let wrap_row = self.to_wrap_row(row);
        self.blocks_before
            .get(wrap_row)
            .is_some_and(Option::is_some)
            && row == wrap_row + self.block_prefix.get(wrap_row).copied().unwrap_or(0)
    }

    /// The wrap row a display row belongs to. A block belongs to the row under it.
    fn to_wrap_row(&self, row: usize) -> usize {
        let row = row.min(self.row_count().saturating_sub(1));
        let rows = self.block_prefix.len().saturating_sub(1);
        let (mut low, mut high) = (0, rows);
        while low < high {
            let mid = (low + high) / 2;
            if self.display_row(mid) <= row {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        // `low` is the first row whose text stands below `row`: either its block is
        // the row, or the row shows the text of the one before it.
        if low < rows
            && self.blocks_before.get(low).is_some_and(Option::is_some)
            && row + 1 == self.display_row(low)
        {
            low
        } else {
            low.saturating_sub(1).min(rows.saturating_sub(1))
        }
    }

    /// The display row that shows a wrap row, with the blocks above it counted.
    fn display_row(&self, wrap_row: usize) -> usize {
        wrap_row
            + self
                .block_prefix
                .get(wrap_row + 1)
                .copied()
                .unwrap_or_else(|| self.block_prefix.last().copied().unwrap_or(0))
    }

    /// Buffer line that starts the display row. A block reports the line under it.
    pub fn row_to_line(&self, row: usize) -> usize {
        let wrap_row = self.to_wrap_row(row);
        if self.is_block_row(row)
            && let Some(block) = self.block_at(wrap_row)
        {
            return self
                .store
                .offset_to_line(block.offset.min(self.store.len()));
        }
        let fold_row = match &self.wrapped {
            Some(wrapped) => wrapped
                .rows
                .get(wrap_row)
                .or(wrapped.rows.last())
                .map_or(0, |row| row.fold_row as usize),
            None => wrap_row,
        };
        self.visible
            .get(fold_row)
            .or(self.visible.last())
            .map_or(0, |line| *line as usize)
    }

    /// Display row that shows the line. A hidden line maps to the row of its fold. A
    /// block above the line does not take the place of the line: this is its text row.
    pub fn line_to_row(&self, line: usize) -> usize {
        let row = self
            .visible
            .partition_point(|visible| *visible as usize <= line)
            .saturating_sub(1);
        let wrap_row = match &self.wrapped {
            Some(wrapped) => wrapped.first[row] as usize,
            None => row,
        };
        self.display_row(wrap_row)
    }

    /// The text row `steps` away from `row`, with the block rows passed over. From the
    /// line above a block, one step down reaches the line under it.
    pub fn text_row(&self, row: usize, steps: isize) -> Option<usize> {
        let mut row = row;
        let mut left = steps.unsigned_abs();
        let step = steps.signum();
        let last = self.row_count();
        while left > 0 {
            row = row.checked_add_signed(step)?;
            if row >= last {
                return None;
            }
            if !self.is_block_row(row) {
                left -= 1;
            }
        }
        Some(row)
    }

    /// Lines the fold row covers, more than one when it holds a fold.
    fn fold_row_lines(&self, row: usize) -> Range<usize> {
        let start = self.visible.get(row).map_or(0, |line| *line as usize);
        let end = self
            .visible
            .get(row + 1)
            .map_or(self.store.line_count(), |line| *line as usize);
        start..end.max(start + 1)
    }

    fn fold_row_line(&self, row: usize) -> DisplayLine {
        layout::build_line(
            self.store,
            self.fold_row_lines(row),
            self.map.folds.as_slice(),
            self.map.inlays.as_slice(),
            self.map.tab_size,
        )
    }

    fn wrap_rows(&self, columns: usize) -> Wrapped {
        let mut rows = Vec::new();
        let mut first = Vec::with_capacity(self.visible.len());
        for fold_row in 0..self.visible.len() {
            first.push(rows.len() as u32);
            let line = self.fold_row_line(fold_row);
            for columns in wrap::segments(&line.text, columns) {
                rows.push(WrappedRow {
                    fold_row: fold_row as u32,
                    columns,
                });
            }
        }
        Wrapped { rows, first }
    }

    pub fn line(&self, row: usize) -> DisplayLine {
        let wrap_row = self.to_wrap_row(row);
        if self.is_block_row(row)
            && let Some(block) = self.block_at(wrap_row)
        {
            return DisplayLine {
                text: block.text.clone(),
                block: Some(BlockText {
                    indent: block.indent,
                }),
                start_offset: block.offset,
                end_offset: block.offset,
                ..DisplayLine::default()
            };
        }
        match &self.wrapped {
            None => self.fold_row_line(wrap_row),
            Some(wrapped) => {
                let Some(shown) = wrapped.rows.get(wrap_row).or(wrapped.rows.last()) else {
                    return DisplayLine::default();
                };
                let line = self.fold_row_line(shown.fold_row as usize);
                line.slice(shown.columns.clone())
            }
        }
    }

    pub fn to_display(&self, offset: usize) -> DisplayPoint {
        let line = self.store.offset_to_line(offset);
        let fold_row = self
            .visible
            .partition_point(|visible| *visible as usize <= line)
            .saturating_sub(1);
        let Some(wrapped) = &self.wrapped else {
            let row = self.line_to_row(line);
            return DisplayPoint {
                row,
                column: self.line(row).column_of(offset),
            };
        };
        let full = self.fold_row_line(fold_row);
        let column = full.column_of(offset);
        let first = wrapped.first[fold_row] as usize;
        let mut row = first;
        while let Some(next) = wrapped.rows.get(row + 1) {
            if next.fold_row as usize != fold_row || next.columns.start > column {
                break;
            }
            row += 1;
        }
        let start = wrapped.rows[row].columns.start;
        DisplayPoint {
            row: self.display_row(row),
            column: column.saturating_sub(start),
        }
    }

    pub fn to_offset(&self, point: DisplayPoint, bias: Bias) -> usize {
        let mut row = point.row.min(self.row_count().saturating_sub(1));
        if self.is_block_row(row) {
            row = self.display_row(self.to_wrap_row(row));
        }
        self.line(row).offset_of(point.column, bias)
    }
}
