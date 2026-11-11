//! Maps buffer text to what is on screen. The chain is BufferRow -> Fold -> Inlay ->
//! Tab -> Wrap -> Block -> DisplayRow. Block is an identity layer for now, it keeps the
//! place of the real one in the row mapping. Wrap is off until the view gives it columns.

mod fold;
mod inlay;
mod layout;
mod rows;
pub mod wrap;

pub use fold::{Fold, FoldMap};
pub use inlay::{Inlay, InlayMap};
pub use layout::{Bias, DisplayChunk, DisplayLine, DisplayPoint};
pub use rows::BlockRows;

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
            block: BlockRows,
        }
    }

    pub fn set_wrap(&mut self, columns: Option<usize>) {
        self.wrap_columns = columns.map(|columns| columns.max(2));
    }

    pub fn wrap(&self) -> Option<usize> {
        self.wrap_columns
    }

    /// Moves folds and inlays along one step of edits.
    pub fn apply_edits(&mut self, edits: &[Edit]) {
        self.folds.apply_edits(edits);
        self.inlays.apply_edits(edits);
    }

    pub fn snapshot<'a, S: TextStore>(&'a self, store: &'a S) -> DisplaySnapshot<'a, S> {
        let visible = self.folds.visible_lines(store);
        let mut snapshot = DisplaySnapshot {
            map: self,
            store,
            visible,
            wrapped: None,
        };
        if let Some(columns) = self.wrap_columns {
            snapshot.wrapped = Some(snapshot.wrap_rows(columns));
        }
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
}

impl<S: TextStore> DisplaySnapshot<'_, S> {
    pub fn row_count(&self) -> usize {
        match &self.wrapped {
            Some(wrapped) => self.map.block.row_count(wrapped.rows.len()),
            None => self.map.block.row_count(self.visible.len()),
        }
    }

    /// Buffer line that starts the display row.
    pub fn row_to_line(&self, row: usize) -> usize {
        let row = self.map.block.to_wrap_row(row);
        let fold_row = match &self.wrapped {
            Some(wrapped) => wrapped
                .rows
                .get(row)
                .or(wrapped.rows.last())
                .map_or(0, |row| row.fold_row as usize),
            None => row,
        };
        self.visible
            .get(fold_row)
            .or(self.visible.last())
            .map_or(0, |line| *line as usize)
    }

    /// Display row that shows the line. A hidden line maps to the row of its fold.
    pub fn line_to_row(&self, line: usize) -> usize {
        let row = self
            .visible
            .partition_point(|visible| *visible as usize <= line)
            .saturating_sub(1);
        let wrap_row = match &self.wrapped {
            Some(wrapped) => wrapped.first[row] as usize,
            None => row,
        };
        self.map.block.from_wrap_row(wrap_row)
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
        let row = self.map.block.to_wrap_row(row);
        match &self.wrapped {
            None => self.fold_row_line(row),
            Some(wrapped) => {
                let Some(shown) = wrapped.rows.get(row).or(wrapped.rows.last()) else {
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
            row: self.map.block.from_wrap_row(row),
            column: column.saturating_sub(start),
        }
    }

    pub fn to_offset(&self, point: DisplayPoint, bias: Bias) -> usize {
        let row = point.row.min(self.row_count().saturating_sub(1));
        self.line(row).offset_of(point.column, bias)
    }
}
