//! Maps buffer text to what is on screen. The chain is BufferRow -> Fold -> Inlay ->
//! Tab -> Wrap -> Block -> DisplayRow. Wrap is off until the view gives it columns.
//! A block is a row of its own above a line, with a label that is no part of the text.

mod doc;
mod fold;
mod inlay;
mod layout;
mod rows;
pub mod wrap;

pub use doc::{DocComment, DocLine, DocRows, DocRun, DocRunKind};
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
    docs: DocRows,
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
            docs: DocRows::default(),
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
        self.docs.apply_edits(edits);
    }

    /// Replaces the rows that stand above lines of the buffer.
    pub fn set_blocks(&mut self, blocks: Vec<Block>) {
        self.block.set(blocks);
    }

    pub fn blocks(&self) -> &[Block] {
        self.block.as_slice()
    }

    /// Replaces the documentation comments shown as rendered blocks. The rows of each
    /// comment stand in place of the rows of its text.
    pub fn set_doc_comments(&mut self, comments: Vec<DocComment>) {
        self.docs.set(comments);
    }

    pub fn doc_comments(&self) -> &[DocComment] {
        self.docs.as_slice()
    }

    pub fn snapshot<'a, S: TextStore>(&'a self, store: &'a S) -> DisplaySnapshot<'a, S> {
        self.snapshot_with_blocks(store, &[])
    }

    /// The map at one text version with rows of one view added over the rows of the map.
    /// A view that keeps two texts on the same lines gives its empty rows here, so the
    /// rows of the map itself stay the rows of the text.
    pub fn snapshot_with_blocks<'a, S: TextStore>(
        &'a self,
        store: &'a S,
        extra: &'a [Block],
    ) -> DisplaySnapshot<'a, S> {
        let mut visible = self.folds.visible_lines(store);
        if !self.docs.is_empty() {
            visible.retain(|line| self.docs.covering_line(store, *line as usize).is_none());
        }
        let mut snapshot = DisplaySnapshot {
            map: self,
            store,
            extra,
            visible,
            wrapped: None,
            blocks_before: Vec::new(),
            group_docs: Vec::new(),
            group_start: Vec::new(),
        };
        if let Some(columns) = self.wrap_columns {
            snapshot.wrapped = Some(snapshot.wrap_rows(columns));
        }
        snapshot.place_groups();
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
    /// Blocks of this view alone, standing over the blocks of the map.
    extra: &'a [Block],
    /// Buffer line of each row before the wrap and block layers.
    visible: Vec<u32>,
    wrapped: Option<Wrapped>,
    /// The blocks shown before each wrap row, in the order they paint.
    blocks_before: Vec<Vec<&'a Block>>,
    /// The rendered documentation rows that stand before each group. A group is a wrap
    /// row and the rows of its block and text; the last group stands after all of them.
    group_docs: Vec<Vec<(usize, usize)>>,
    /// The first display row of each group; one more entry than the groups.
    group_start: Vec<usize>,
}

impl<'a, S: TextStore> DisplaySnapshot<'a, S> {
    /// Places the blocks above their lines and the rendered comments before the rows
    /// that follow them. A block or a comment on a line the folds hide has no row. The
    /// rows of this view come before the rows of the map, so a label stands right above
    /// the line it belongs to.
    fn place_groups(&mut self) {
        let rows = self.base_rows();
        self.blocks_before = vec![Vec::new(); rows + 1];
        self.group_docs = vec![Vec::new(); rows + 1];
        let mut placed: Vec<(usize, &'a Block)> = Vec::new();
        for block in self.extra.iter().chain(self.map.block.as_slice()) {
            let row = if block.after {
                rows
            } else {
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
                match &self.wrapped {
                    Some(wrapped) => wrapped
                        .first
                        .get(fold_row)
                        .copied()
                        .map_or(0, |row| row as usize),
                    None => fold_row,
                }
            };
            placed.push((row, block));
        }
        for (row, block) in placed {
            self.blocks_before[row].push(block);
        }
        for (index, comment) in self.map.docs.as_slice().iter().enumerate() {
            let last = comment.range.end.saturating_sub(1).min(self.store.len());
            let after = self.store.offset_to_line(last) + 1;
            let fold_row = self
                .visible
                .partition_point(|visible| (*visible as usize) < after);
            let wrap_row = match &self.wrapped {
                Some(wrapped) => wrapped
                    .first
                    .get(fold_row)
                    .copied()
                    .map_or(rows, |row| row as usize),
                None => fold_row.min(rows),
            };
            for line in 0..comment.lines.len() {
                self.group_docs[wrap_row].push((index, line));
            }
        }
        self.group_start = vec![0; rows + 2];
        for group in 0..=rows {
            let blocks = self.blocks_before.get(group).map_or(0, Vec::len);
            let text = usize::from(group < rows);
            self.group_start[group + 1] =
                self.group_start[group] + self.group_docs[group].len() + blocks + text;
        }
    }

    fn base_rows(&self) -> usize {
        match &self.wrapped {
            Some(wrapped) => wrapped.rows.len(),
            None => self.visible.len(),
        }
    }

    /// The block a display row of a group shows, `None` for the text and rendered rows.
    fn block_at(&self, group: usize, row: usize) -> Option<&'a Block> {
        let start = self.group_start.get(group).copied().unwrap_or(0)
            + self.group_docs.get(group).map_or(0, Vec::len);
        let slot = row.checked_sub(start)?;
        self.blocks_before.get(group)?.get(slot).copied()
    }

    pub fn row_count(&self) -> usize {
        self.group_start.last().copied().unwrap_or(0)
    }

    /// The buffer lines of the rows the folds do not hide, from zero.
    pub fn visible_lines(&self) -> &[u32] {
        &self.visible
    }

    /// The group a display row belongs to: its rendered rows, its block and its text.
    fn group_of(&self, row: usize) -> usize {
        let row = row.min(self.row_count().saturating_sub(1));
        self.group_start
            .partition_point(|start| *start <= row)
            .saturating_sub(1)
    }

    /// The rendered row of a documentation comment, `None` for text and block rows.
    pub fn doc_row(&self, row: usize) -> Option<&DocLine> {
        let group = self.group_of(row);
        let slot = row.saturating_sub(self.group_start.get(group).copied().unwrap_or(0));
        let (comment, line) = *self.group_docs.get(group)?.get(slot)?;
        self.map.docs.as_slice().get(comment)?.lines.get(line)
    }

    /// The range of the comment a rendered row belongs to.
    pub fn doc_range(&self, row: usize) -> Option<Range<usize>> {
        let group = self.group_of(row);
        let slot = row.saturating_sub(self.group_start.get(group).copied().unwrap_or(0));
        let (comment, _) = *self.group_docs.get(group)?.get(slot)?;
        self.map
            .docs
            .as_slice()
            .get(comment)
            .map(|comment| comment.range.clone())
    }

    /// Whether the row holds a rendered documentation comment.
    pub fn is_doc_row(&self, row: usize) -> bool {
        self.doc_row(row).is_some()
    }

    /// Whether the row holds a label or a rendered comment and no text of the buffer.
    pub fn is_block_row(&self, row: usize) -> bool {
        if self.is_doc_row(row) {
            return true;
        }
        let group = self.group_of(row);
        let start = self.group_start.get(group).copied().unwrap_or(0)
            + self.group_docs.get(group).map_or(0, Vec::len);
        let blocks = self.blocks_before.get(group).map_or(0, Vec::len);
        row >= start && row < start + blocks
    }

    /// The display row that shows a group, with the rendered rows and the blocks counted.
    fn display_row(&self, group: usize) -> usize {
        self.group_start.get(group).copied().unwrap_or(0)
            + self.group_docs.get(group).map_or(0, Vec::len)
            + self.blocks_before.get(group).map_or(0, Vec::len)
    }

    /// The first display row of the comment that covers `line`.
    fn doc_start_row(&self, line: usize) -> Option<usize> {
        let comment = self
            .map
            .docs
            .covering_line(self.store, line)
            .and_then(|comment| {
                self.map
                    .docs
                    .as_slice()
                    .iter()
                    .position(|known| known.range == comment.range)
            })?;
        for (group, docs) in self.group_docs.iter().enumerate() {
            if let Some(slot) = docs.iter().position(|(index, _)| *index == comment) {
                return Some(self.group_start[group] + slot);
            }
        }
        None
    }

    /// Buffer line that starts the display row. A block reports the line under it.
    pub fn row_to_line(&self, row: usize) -> usize {
        let group = self.group_of(row);
        if let Some(range) = self.doc_range(row) {
            return self.store.offset_to_line(range.start.min(self.store.len()));
        }
        if self.is_block_row(row)
            && let Some(block) = self.block_at(group, row)
        {
            return self
                .store
                .offset_to_line(block.offset.min(self.store.len()));
        }
        let fold_row = match &self.wrapped {
            Some(wrapped) => wrapped
                .rows
                .get(group)
                .or(wrapped.rows.last())
                .map_or(0, |row| row.fold_row as usize),
            None => group,
        };
        self.visible
            .get(fold_row)
            .or(self.visible.last())
            .map_or(0, |line| *line as usize)
    }

    /// Display row that shows the line. A hidden line maps to the row of its fold or of
    /// its rendered comment. A block above the line does not take the place of the line.
    pub fn line_to_row(&self, line: usize) -> usize {
        if let Some(row) = self.doc_start_row(line) {
            return row;
        }
        let row = self
            .visible
            .partition_point(|visible| *visible as usize <= line)
            .saturating_sub(1);
        let group = match &self.wrapped {
            Some(wrapped) => wrapped.first[row] as usize,
            None => row,
        };
        self.display_row(group)
    }

    /// The text row `steps` away from `row`, with the block and rendered rows passed
    /// over. From the line above a block, one step down reaches the line under it.
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
        let starts_fold = self.map.folds.as_slice().iter().any(|fold| {
            self.store
                .offset_to_line(fold.range.start.min(self.store.len()))
                == start
        });
        let end = if starts_fold {
            self.visible
                .get(row + 1)
                .map_or(self.store.line_count(), |line| *line as usize)
        } else {
            start + 1
        };
        start..end.min(self.store.line_count()).max(start + 1)
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
        let group = self.group_of(row);
        if let Some(doc) = self.doc_row(row) {
            let range = self.doc_range(row).unwrap_or_default();
            return DisplayLine {
                block: Some(BlockText { indent: doc.indent }),
                start_offset: range.start,
                end_offset: range.start,
                ..DisplayLine::default()
            };
        }
        if self.is_block_row(row)
            && let Some(block) = self.block_at(group, row)
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
            None => self.fold_row_line(group),
            Some(wrapped) => {
                let Some(shown) = wrapped.rows.get(group).or(wrapped.rows.last()) else {
                    return DisplayLine::default();
                };
                let line = self.fold_row_line(shown.fold_row as usize);
                line.slice(shown.columns.clone())
            }
        }
    }

    pub fn to_display(&self, offset: usize) -> DisplayPoint {
        let line = self.store.offset_to_line(offset);
        if let Some(row) = self.doc_start_row(line) {
            return DisplayPoint { row, column: 0 };
        }
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
        if let Some(range) = self.doc_range(row) {
            return match bias {
                Bias::Left => range.start,
                Bias::Right => range.end,
            };
        }
        if self.is_block_row(row) {
            row = self.display_row(self.group_of(row));
        }
        self.line(row).offset_of(point.column, bias)
    }
}
