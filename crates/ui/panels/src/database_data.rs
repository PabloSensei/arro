//! The data tab of the Database window: one page of a table, edited in place.
//!
//! The statements of a page and of the edits live in `database`; this view owns the
//! keyboard, the grid and the question of a close with unsaved edits. Every request goes
//! through [`DatabasePanel`], which owns the connection and polls the helper, so the
//! view never waits for a database on the thread that draws it.

use std::path::PathBuf;

use database::{
    Column, DataModel, Dialect, PageQuery, Plan, RowKind, SourceId, Table, TableKind, TableRef,
    Value, csv, identifying_columns, insert_literal, tsv,
};
use ui_kit::gpui::{
    DispatchPhase, ListHorizontalSizingBehavior, Modifiers, MouseMoveEvent, MouseUpEvent,
    ScrollStrategy, canvas, point,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::database_panel::{
    DataCommand, DataEvent, DataRequest, DataTicket, DatabasePanel, small_button,
};

/// The width of a cell of the data grid.
const CELL_WIDTH: f32 = 160.;
/// The height of a row of the data grid.
const DATA_ROW_HEIGHT: f32 = 24.;
/// The height of the field of the filter.
const FIELD_HEIGHT: f32 = 26.;
/// The group of a column header: its sort icon shows while the header is under the pointer.
const HEADER_GROUP: &str = "database-data-head";
/// What a table without a primary or unique key cannot do, in the hint above the grid.
const NO_KEY_HINT: &str = "The table has no key, so a row cannot be told apart";

/// A question that stands over the tab before it closes or before Submit takes the work of
/// the console with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Question {
    /// Submit, discard or keep editing before the tab or the window closes.
    Unsaved,
    /// The console of the source has work that waits for a commit, and Submit would take it
    /// with the changes of the table.
    Console {
        /// The source whose console waits, as the question names it.
        source: String,
        /// The tab closes itself once the transaction is committed.
        closing: bool,
        /// The console was in Manual before the submit and stays in it after.
        console_manual: bool,
    },
}

impl Question {
    fn text(&self) -> String {
        match self {
            Self::Unsaved => "Submit the changes of this table?".to_owned(),
            Self::Console { source, .. } => format!(
                "The console of {source} has an open transaction. \
                 Commit it with the table changes?"
            ),
        }
    }
}

/// The cell whose text is in an editor.
struct Editing {
    row: usize,
    column: usize,
    field: TextField,
}

/// The statements of Submit that are still to run.
struct Submit {
    steps: Vec<database::Statement>,
    next: usize,
    /// The tab closes itself once the transaction is committed.
    closing: bool,
    /// A statement failed and the transaction is rolling back.
    failed: bool,
    /// The console of the source was in Manual before the submit; it is left in it after
    /// the transaction ends, whatever its answer.
    console_manual: bool,
}

/// The part of the table that a press picks: one cell, a row from its number, or a column
/// from its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pick {
    Cells,
    Rows,
    Columns,
}

/// A rectangle of the grid with both of its ends included, and the gesture that drew it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
    pick: Pick,
}

impl Span {
    /// The rectangle between two cells, whichever corner each one is.
    fn new(pick: Pick, from: (usize, usize), to: (usize, usize)) -> Self {
        Self {
            top: from.0.min(to.0),
            bottom: from.0.max(to.0),
            left: from.1.min(to.1),
            right: from.1.max(to.1),
            pick,
        }
    }

    fn contains(&self, row: usize, column: usize) -> bool {
        (self.top..=self.bottom).contains(&row) && (self.left..=self.right).contains(&column)
    }

    /// The rectangles that stay when one cell is taken out of this one. What stays is cells
    /// now: a column or a row with a hole in it is no longer a whole one.
    fn without(self, row: usize, column: usize) -> Vec<Span> {
        if !self.contains(row, column) {
            return vec![self];
        }
        let pick = Pick::Cells;
        let mut rest = Vec::new();
        if self.top < row {
            rest.push(Span {
                bottom: row - 1,
                pick,
                ..self
            });
        }
        if row < self.bottom {
            rest.push(Span {
                top: row + 1,
                pick,
                ..self
            });
        }
        if self.left < column {
            rest.push(Span {
                top: row,
                bottom: row,
                right: column - 1,
                pick,
                ..self
            });
        }
        if column < self.right {
            rest.push(Span {
                top: row,
                bottom: row,
                left: column + 1,
                pick,
                ..self
            });
        }
        rest
    }

    /// The rectangles that stay when a whole column is taken out of this one. Each part keeps
    /// the gesture of this rectangle, so the columns that stay are whole.
    fn without_column(self, column: usize) -> Vec<Span> {
        if !(self.left..=self.right).contains(&column) {
            return vec![self];
        }
        let mut rest = Vec::new();
        if self.left < column {
            rest.push(Span {
                right: column - 1,
                ..self
            });
        }
        if column < self.right {
            rest.push(Span {
                left: column + 1,
                ..self
            });
        }
        rest
    }

    /// The rectangles that stay when a whole row is taken out of this one, as `without_column`.
    fn without_row(self, row: usize) -> Vec<Span> {
        if !(self.top..=self.bottom).contains(&row) {
            return vec![self];
        }
        let mut rest = Vec::new();
        if self.top < row {
            rest.push(Span {
                bottom: row - 1,
                ..self
            });
        }
        if row < self.bottom {
            rest.push(Span {
                top: row + 1,
                ..self
            });
        }
        rest
    }
}

/// A drag that draws the rectangle `index` of the selection from `anchor` to the pointer.
#[derive(Clone, Copy, Debug)]
struct Drag {
    pick: Pick,
    anchor: (usize, usize),
    index: usize,
}

/// What a copy takes from the grid: the cells of the bounding box of the selection, in the
/// order of the table.
struct Copied {
    columns: Vec<Column>,
    /// One row per row of the box; `None` is a cell that the selection does not take.
    rows: Vec<Vec<Option<Value>>>,
    /// The names of the columns go first: the selection is made of whole columns only.
    names: bool,
}

impl Copied {
    /// The grid as tab-separated text. A cell that the selection does not take is empty.
    fn tsv(&self) -> String {
        let mut lines: Vec<Vec<Value>> = Vec::new();
        if self.names {
            lines.push(
                self.columns
                    .iter()
                    .map(|column| Value::Text(column.name.clone()))
                    .collect(),
            );
        }
        lines.extend(
            self.rows
                .iter()
                .map(|row| filled(row, Value::Text(String::new()))),
        );
        tsv(&lines)
    }
}

/// The values of a row of a copy, with `empty` in the cells that the selection does not take.
fn filled(row: &[Option<Value>], empty: Value) -> Vec<Value> {
    row.iter()
        .map(|cell| cell.clone().unwrap_or_else(|| empty.clone()))
        .collect()
}

pub struct DatabaseDataView {
    workspace: WeakEntity<Workspace>,
    panel: WeakEntity<DatabasePanel>,
    focus: FocusHandle,
    /// The window of the tab, so that the view can close itself once its edits are gone.
    window: Option<WindowHandle<Workspace>>,
    /// The path of the tab in the workspace.
    path: PathBuf,
    source: SourceId,
    table: Table,
    dialect: Dialect,
    page_rows: usize,
    page: usize,
    /// The columns of the last page, as the driver named them.
    columns: Vec<Column>,
    /// The columns that tell a row apart, from the schema of the table.
    key: Vec<usize>,
    model: DataModel,
    total: Option<u64>,
    filter: TextField,
    filter_active: bool,
    /// The column the rows are sorted by and whether the order is ascending.
    sort: Option<(String, bool)>,
    /// The caret: the cell that the keys and the edits act on.
    selected: Option<(usize, usize)>,
    /// The cell that Shift extends the selection from.
    anchor: Option<(usize, usize)>,
    /// The rectangles of the selection. The last one is the one that Shift extends.
    rects: Vec<Span>,
    /// The drag that draws a rectangle of the selection, while the button is held.
    drag: Option<Drag>,
    /// Where the pointer was in the window during the drag, for the scroll at the edges.
    pointer: Option<Point<Pixels>>,
    editing: Option<Editing>,
    /// The serial of the next request and the one that is in flight.
    serial: u64,
    in_flight: Option<DataTicket>,
    /// The rows of the page that is being read.
    rows_buffer: Vec<Vec<Value>>,
    status: Option<String>,
    error: Option<String>,
    question: Option<Question>,
    submit: Option<Submit>,
    /// The commit is done and the page is read again once auto-commit is back on.
    reload_after_commit: bool,
    scroll: UniformListScrollHandle,
}

impl DatabaseDataView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn create(
        workspace: WeakEntity<Workspace>,
        panel: WeakEntity<DatabasePanel>,
        window: Option<WindowHandle<Workspace>>,
        path: PathBuf,
        source: SourceId,
        table: Table,
        dialect: Dialect,
        page_rows: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        let key = identifying_columns(&table);
        Self {
            workspace,
            panel,
            focus: cx.focus_handle(),
            window,
            path,
            source,
            model: DataModel::with_key(table.columns.clone(), key.clone()),
            key,
            table,
            dialect,
            page_rows: page_rows.max(1),
            page: 0,
            columns: Vec::new(),
            total: None,
            filter: TextField::default(),
            filter_active: false,
            sort: None,
            selected: None,
            anchor: None,
            rects: Vec::new(),
            drag: None,
            pointer: None,
            editing: None,
            serial: 0,
            in_flight: None,
            rows_buffer: Vec::new(),
            status: None,
            error: None,
            question: None,
            submit: None,
            reload_after_commit: false,
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Why the rows of this tab cannot be changed, when they cannot.
    pub fn read_only_reason(&self) -> Option<String> {
        if self.table.kind == TableKind::View {
            return Some(
                "A view has no rows of its own: its data is shown here and cannot be changed."
                    .to_owned(),
            );
        }
        if self.table.columns.is_empty() {
            return Some(
                "The driver did not describe the columns of this table, so its rows cannot be \
                 matched."
                    .to_owned(),
            );
        }
        None
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only_reason().is_some()
    }

    /// Starts reading the first page; the panel calls it after the tab is in place.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        self.load(cx);
    }

    /// The text of the filter field, for the window tests.
    #[cfg(test)]
    pub(crate) fn filter_for_test(&self) -> String {
        self.filter.text.clone()
    }

    /// The page the tab shows, for the window tests.
    #[cfg(test)]
    pub(crate) fn page_for_test(&self) -> usize {
        self.page
    }

    #[cfg(test)]
    pub(crate) fn total_for_test(&self) -> Option<u64> {
        self.total
    }

    #[cfg(test)]
    pub(crate) fn question_for_test(&self) -> Option<Question> {
        self.question.clone()
    }

    #[cfg(test)]
    pub(crate) fn editing_for_test(&self) -> bool {
        self.editing.is_some()
    }

    /// The hint above the grid for a table whose rows cannot be told apart, for the window
    /// tests.
    #[cfg(test)]
    pub(crate) fn hint_for_test(&self) -> Option<&'static str> {
        (!self.model.has_key() && !self.model.columns().is_empty()).then_some(NO_KEY_HINT)
    }

    #[cfg(test)]
    pub(crate) fn error_for_test(&self) -> Option<String> {
        self.error.clone()
    }

    #[cfg(test)]
    pub(crate) fn row_error_for_test(&self, row: usize) -> Option<String> {
        self.model
            .row(row)
            .and_then(|row| row.error().map(str::to_owned))
    }

    #[cfg(test)]
    pub(crate) fn model_for_test(&self) -> &DataModel {
        &self.model
    }

    #[cfg(test)]
    pub(crate) fn selected_for_test(&self) -> Option<(usize, usize)> {
        self.selected
    }

    /// The number that the row at `row` shows, for the window tests.
    #[cfg(test)]
    pub(crate) fn number_for_test(&self, row: usize) -> usize {
        self.row_number(row)
    }

    /// The column the rows are sorted by and whether they ascend, for the window tests.
    #[cfg(test)]
    pub(crate) fn sort_for_test(&self) -> Option<(String, bool)> {
        self.sort.clone()
    }

    /// The horizontal offset of the rows, for the window tests. It is negative once the
    /// rows are scrolled to the right.
    #[cfg(test)]
    pub(crate) fn scroll_x_for_test(&self) -> f32 {
        f32::from(self.scroll.0.borrow().base_handle.offset().x)
    }

    /// Sets the horizontal offset of the rows, for the window tests. The next frame clamps it
    /// to the width that the grid can scroll.
    #[cfg(test)]
    pub(crate) fn scroll_sideways_for_test(&self, x: f32) {
        let handle = self.scroll.0.borrow().base_handle.clone();
        handle.set_offset(point(px(x), handle.offset().y));
    }

    /// The table the tab writes to.
    fn table_ref(&self) -> TableRef {
        TableRef::new(self.table.schema.clone(), self.table.name.clone())
    }

    fn page_query(&self) -> PageQuery {
        PageQuery {
            table: self.table_ref(),
            filter: self.filter.text.clone(),
            sort: self.sort.as_ref().map(|(name, _)| name.clone()),
            ascending: self.sort.as_ref().is_none_or(|(_, ascending)| *ascending),
            key: self
                .key
                .iter()
                .filter_map(|index| self.table.columns.get(*index))
                .map(|column| column.name.clone())
                .collect(),
            offset: self.page * self.page_rows,
            limit: self.page_rows,
        }
    }

    /// Reads the page that the buttons, the filter and the sort name.
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        if self.model.is_dirty() {
            self.status =
                Some("Submit or revert the changes before reading other rows.".to_owned());
            cx.notify();
            return;
        }
        let query = self.page_query();
        self.rows_buffer.clear();
        self.error = None;
        self.editing = None;
        self.selected = None;
        self.anchor = None;
        self.rects.clear();
        self.drag = None;
        self.status = Some("Reading…".to_owned());
        let sql = query.sql(self.dialect);
        self.send(DataRequest::Page, sql, cx);
    }

    fn send(&mut self, request: DataRequest, sql: String, cx: &mut Context<Self>) {
        self.serial = self.serial.wrapping_add(1).max(1);
        let ticket = DataTicket {
            serial: self.serial,
            request,
        };
        self.in_flight = Some(ticket);
        let Some(panel) = self.panel.upgrade() else {
            self.in_flight = None;
            self.status = Some("The Database window is closed.".to_owned());
            cx.notify();
            return;
        };
        let reader = cx.entity();
        let source = self.source.clone();
        let max_rows = Some(self.page_rows);
        panel.update(cx, |panel, cx| {
            panel.data_query(source, sql, max_rows, ticket, reader, cx)
        });
    }

    fn send_command(&mut self, request: DataRequest, command: DataCommand, cx: &mut Context<Self>) {
        self.serial = self.serial.wrapping_add(1).max(1);
        let ticket = DataTicket {
            serial: self.serial,
            request,
        };
        self.in_flight = Some(ticket);
        let Some(panel) = self.panel.upgrade() else {
            self.in_flight = None;
            return;
        };
        let reader = cx.entity();
        let source = self.source.clone();
        panel.update(cx, |panel, cx| {
            panel.data_command(source, command, ticket, reader, cx)
        });
    }

    /// An answer of the connection. Only the request that is in flight counts: a page
    /// that the user has moved away from is dropped.
    pub(crate) fn event(&mut self, ticket: DataTicket, event: DataEvent, cx: &mut Context<Self>) {
        if self.in_flight != Some(ticket) {
            return;
        }
        match event {
            DataEvent::Columns(columns) => {
                if matches!(ticket.request, DataRequest::Page) {
                    self.columns = columns.clone();
                    if self.model.columns().is_empty() {
                        self.model = DataModel::with_key(columns, self.key.clone());
                    }
                }
            }
            DataEvent::Rows(rows) => {
                self.rows_buffer.extend(rows);
            }
            DataEvent::Done {
                update_count,
                cancelled,
            } => {
                self.in_flight = None;
                if cancelled {
                    self.status = Some("The statement was cancelled.".to_owned());
                    cx.notify();
                    return;
                }
                if let DataRequest::Statement(index) = ticket.request {
                    if self.statement_wrote_one_row(index) && update_count != Some(1) {
                        self.row_count_failed(index, update_count, cx);
                        return;
                    }
                    if let Some(count) = update_count {
                        self.status = Some(format!("{count} row(s) changed."));
                    }
                    self.advance_submit(cx);
                    return;
                }
                match ticket.request {
                    DataRequest::Page => self.page_read(cx),
                    DataRequest::Count => {
                        self.total = self
                            .rows_buffer
                            .first()
                            .and_then(|row| row.first())
                            .and_then(count_of);
                        self.status = None;
                        cx.notify();
                    }
                    DataRequest::AutoCommit => {
                        if self.submit.is_some() {
                            self.advance_submit(cx);
                        } else if self.reload_after_commit {
                            self.reload_after_commit = false;
                            self.load(cx);
                        }
                    }
                    DataRequest::Statement(_) => {}
                    DataRequest::Commit => self.committed(cx),
                    DataRequest::Rollback => self.rolled_back(cx),
                }
            }
            DataEvent::Error(message) => {
                self.in_flight = None;
                match ticket.request {
                    DataRequest::Statement(index) => self.failed_submit(index, message, cx),
                    DataRequest::Commit | DataRequest::Rollback => {
                        // The transaction is still open, or the driver could not end it: the
                        // console goes back to its own mode, so auto-commit does not stay off.
                        self.reload_after_commit = false;
                        self.status = None;
                        self.error = Some(message);
                        self.restore_console(cx);
                    }
                    DataRequest::AutoCommit => {
                        let restore = self.submit.is_some();
                        self.reload_after_commit = false;
                        self.status = None;
                        self.error = Some(message);
                        if restore {
                            self.restore_console(cx);
                        } else {
                            cx.notify();
                        }
                    }
                    _ => {
                        self.status = None;
                        self.error = Some(message);
                        cx.notify();
                    }
                }
            }
        }
    }

    /// The page arrived: the rows go into the model and the count of the table follows.
    fn page_read(&mut self, cx: &mut Context<Self>) {
        let rows = self.align_rows();
        self.model.set_rows(rows);
        self.selected = None;
        self.rects.clear();
        self.error = None;
        let query = self.page_query();
        self.send(DataRequest::Count, query.count_sql(self.dialect), cx);
    }

    /// Puts every value under the column of the schema with the same name. A name the
    /// result does not keep takes its place, so a renamed column still lines up.
    fn align_rows(&mut self) -> Vec<Vec<Value>> {
        let model_columns = self.model.columns();
        let rows = std::mem::take(&mut self.rows_buffer);
        if model_columns.is_empty() || self.columns.is_empty() {
            return rows;
        }
        rows.into_iter()
            .map(|row| {
                model_columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let at = self
                            .columns
                            .iter()
                            .position(|found| found.name == column.name)
                            .or_else(|| {
                                self.columns
                                    .iter()
                                    .position(|found| found.name.eq_ignore_ascii_case(&column.name))
                            })
                            .unwrap_or(index);
                        row.get(at).cloned().unwrap_or(Value::Null)
                    })
                    .collect()
            })
            .collect()
    }

    fn submit(&mut self, closing: bool, cx: &mut Context<Self>) {
        if self.is_read_only() {
            self.status = self.read_only_reason();
            cx.notify();
            return;
        }
        if let Some(submit) = &mut self.submit {
            // The transaction is already running; a close that waits for it now closes
            // the tab once it commits.
            submit.closing |= closing;
            return;
        }
        let plan: Plan = self.model.plan(self.dialect, &self.table_ref());
        if let Some(error) = plan.error {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if plan.statements.is_empty() {
            self.status = Some("There are no changes to submit.".to_owned());
            cx.notify();
            return;
        }
        let (open, console_manual, source) = self.console_state(cx);
        if open {
            self.question = Some(Question::Console {
                source,
                closing,
                console_manual,
            });
            cx.notify();
            return;
        }
        self.start_submit(closing, console_manual, cx);
    }

    /// What the console of the source holds before the table writes: whether it has a
    /// transaction to commit with the table, the mode it is in, and its name.
    fn console_state(&self, cx: &App) -> (bool, bool, String) {
        let Some(panel) = self.panel.upgrade() else {
            return (false, false, self.source.to_string());
        };
        let panel = panel.read(cx);
        (
            panel.has_open_transaction(&self.source),
            panel.console_manual(&self.source),
            panel.source_title(&self.source),
        )
    }

    /// Reads the plan and starts the transaction; the console is put back into
    /// `console_manual` once the transaction ends.
    fn start_submit(&mut self, closing: bool, console_manual: bool, cx: &mut Context<Self>) {
        let plan: Plan = self.model.plan(self.dialect, &self.table_ref());
        if let Some(error) = plan.error {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if plan.statements.is_empty() {
            self.status = Some("There are no changes to submit.".to_owned());
            cx.notify();
            return;
        }
        self.error = None;
        self.status = Some(format!("Saving {} statement(s)…", plan.statements.len()));
        self.submit = Some(Submit {
            steps: plan.statements,
            next: 0,
            closing,
            failed: false,
            console_manual,
        });
        self.send_command(DataRequest::AutoCommit, DataCommand::AutoCommit(false), cx);
    }

    /// Whether the statement at `index` names one row, so the driver must report one.
    fn statement_wrote_one_row(&self, index: usize) -> bool {
        self.submit
            .as_ref()
            .and_then(|submit| submit.steps.get(index))
            .is_some_and(|statement| statement.one_row)
    }

    /// A statement of the transaction reported another number of rows than the one it named:
    /// the row went away, or the matching found several. The whole transaction rolls back,
    /// because a part of it is not what the user saw.
    fn row_count_failed(&mut self, index: usize, count: Option<u64>, cx: &mut Context<Self>) {
        let Some(submit) = &mut self.submit else {
            return;
        };
        let message = match count {
            Some(count) if count > 1 => format!("{count} rows matched; nothing was saved."),
            _ => "The row changed or was deleted; nothing was saved.".to_owned(),
        };
        submit.failed = true;
        if let Some(row) = submit.steps.get(index).and_then(|statement| statement.row) {
            self.model.set_error(row, Some(message.clone()));
        }
        self.status = None;
        self.error = Some(message);
        self.send_command(DataRequest::Rollback, DataCommand::Rollback, cx);
    }

    /// Auto-commit is off and the transaction is open: the next statement or the commit.
    fn advance_submit(&mut self, cx: &mut Context<Self>) {
        let Some(submit) = &mut self.submit else {
            return;
        };
        if submit.failed {
            self.send_command(DataRequest::Rollback, DataCommand::Rollback, cx);
            return;
        }
        let statement = submit.steps.get(submit.next).cloned();
        if let Some(statement) = statement {
            let index = submit.next;
            submit.next += 1;
            self.send(DataRequest::Statement(index), statement.sql, cx);
            return;
        }
        self.send_command(DataRequest::Commit, DataCommand::Commit, cx);
    }

    /// A statement of the transaction failed: the error goes to its row and the whole
    /// transaction rolls back, so the database keeps what it had.
    fn failed_submit(&mut self, index: usize, message: String, cx: &mut Context<Self>) {
        let Some(submit) = &mut self.submit else {
            self.error = Some(message);
            cx.notify();
            return;
        };
        submit.failed = true;
        if let Some(statement) = submit.steps.get(index)
            && let Some(row) = statement.row
        {
            self.model.set_error(row, Some(message.clone()));
        }
        self.error = Some(message);
        self.send_command(DataRequest::Rollback, DataCommand::Rollback, cx);
    }

    fn committed(&mut self, cx: &mut Context<Self>) {
        let closing = self.submit.as_ref().is_some_and(|submit| submit.closing);
        self.status = Some("The changes are saved.".to_owned());
        // The rows are the table's now: the next page replaces them without a question.
        self.model.revert();
        self.error = None;
        self.reload_after_commit = !closing;
        // The console goes back to the mode it had, rather than to auto-commit: a user who
        // worked in Manual stays there.
        self.restore_console(cx);
        if closing {
            self.close(cx);
        }
    }

    fn rolled_back(&mut self, cx: &mut Context<Self>) {
        self.reload_after_commit = false;
        if self.error.is_none() {
            self.status = Some("The changes are rolled back.".to_owned());
        }
        // The console goes back to its own mode, and the rows keep their edits: the user
        // can fix the row and submit again.
        self.restore_console(cx);
    }

    /// Ends the table transaction and puts the console into the mode it had before it: in
    /// Manual it stays without auto-commit, and otherwise auto-commit comes back on, even
    /// when the commit or the rollback failed.
    fn restore_console(&mut self, cx: &mut Context<Self>) {
        let Some(submit) = self.submit.take() else {
            cx.notify();
            return;
        };
        self.send_command(
            DataRequest::AutoCommit,
            DataCommand::AutoCommit(!submit.console_manual),
            cx,
        );
    }

    /// Asks whether the tab may close; the question stays over the tab until answered.
    pub(crate) fn ask_to_close(&mut self, cx: &mut Context<Self>) -> bool {
        if self.submit.is_some() {
            self.question = Some(Question::Unsaved);
            cx.notify();
            return false;
        }
        if !self.model.is_dirty() {
            return true;
        }
        self.question = Some(Question::Unsaved);
        cx.notify();
        false
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let (Some(window), path) = (self.window, self.path.clone()) else {
            return;
        };
        // The guard of the tab asks the view again, so the close waits for this update.
        cx.defer(move |cx| {
            let _ = window.update(cx, |workspace, window, cx| {
                workspace.close_tab_of(&path, window, cx);
            });
        });
    }

    /// Whether a transaction is running, which keeps the rows still until it answers.
    fn busy(&mut self, cx: &mut Context<Self>) -> bool {
        if self.submit.is_none() {
            return false;
        }
        self.status = Some("A transaction is running; wait for its answer.".to_owned());
        cx.notify();
        true
    }

    fn set_null(&mut self, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        let Some((row, column)) = self.selected else {
            return;
        };
        if self.is_read_only() || !self.model.is_editable_column(column) {
            self.status = Some("This cell cannot be changed.".to_owned());
            cx.notify();
            return;
        }
        let existing = self
            .model
            .row(row)
            .is_some_and(|found| found.kind() == RowKind::Existing);
        if existing && !self.model.has_key() {
            self.status = Some(NO_KEY_HINT.to_owned());
            cx.notify();
            return;
        }
        match self.model.set_null(row, column) {
            Ok(()) => self.error = None,
            Err(text) => self.error = Some(text),
        }
        cx.notify();
    }

    fn add_row(&mut self, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        if self.is_read_only() {
            self.status = self.read_only_reason();
            cx.notify();
            return;
        }
        let row = self.model.add_row();
        self.select_cell(row, 0);
        self.scroll.scroll_to_item(row, ScrollStrategy::Bottom);
        cx.notify();
    }

    fn delete_row(&mut self, cx: &mut Context<Self>) {
        if self.busy(cx) {
            return;
        }
        if self.is_read_only() {
            self.status = self.read_only_reason();
            cx.notify();
            return;
        }
        let Some((row, _)) = self.selected else {
            return;
        };
        let existing = self
            .model
            .row(row)
            .is_some_and(|found| found.kind() == RowKind::Existing);
        if existing && !self.model.has_key() {
            self.status = Some(NO_KEY_HINT.to_owned());
            cx.notify();
            return;
        }
        self.model.delete_row(row);
        let rows = self.model.row_count();
        self.selected = (rows > 0).then(|| ((row).min(rows - 1), 0));
        self.anchor = self.selected;
        self.rects = self
            .selected
            .into_iter()
            .map(|cell| Span::new(Pick::Cells, cell, cell))
            .collect();
        cx.notify();
    }

    fn begin_edit(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        // A double click inside the cell being edited selects a word of it: the editor stays.
        if self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.row == row && editing.column == column)
        {
            return;
        }
        if self.busy(cx) {
            return;
        }
        if self.is_read_only() || !self.model.is_editable_column(column) {
            self.status = Some(if self.is_read_only() {
                self.read_only_reason().unwrap_or_default()
            } else {
                "A binary value has no text to edit.".to_owned()
            });
            cx.notify();
            return;
        }
        let existing = self
            .model
            .row(row)
            .is_some_and(|found| found.kind() == RowKind::Existing);
        if existing && !self.model.has_key() {
            self.status = Some(NO_KEY_HINT.to_owned());
            cx.notify();
            return;
        }
        let Some(found) = self.model.row(row) else {
            return;
        };
        let text = match found.cell(column) {
            Some(Value::Null) | None => String::new(),
            Some(value) => value.text(),
        };
        self.select_cell(row, column);
        self.editing = Some(Editing {
            row,
            column,
            field: TextField::from_text(text),
        });
        self.error = None;
        cx.notify();
    }

    fn accept_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        match self
            .model
            .set_text(editing.row, editing.column, &editing.field.text)
        {
            Ok(()) => {
                self.error = None;
                self.selected = Some((editing.row, editing.column));
            }
            Err(text) => {
                self.error = Some(text);
                self.editing = Some(editing);
            }
        }
        cx.notify();
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        cx.notify();
    }

    /// Selects one cell: it is the caret, the anchor and the only rectangle.
    fn select_cell(&mut self, row: usize, column: usize) {
        self.selected = Some((row, column));
        self.anchor = Some((row, column));
        self.rects = vec![Span::new(Pick::Cells, (row, column), (row, column))];
        self.drag = None;
    }

    /// Selects every loaded row, as Ctrl+A and a click on the corner of the grid do. The
    /// selection is of rows, so a copy of it has no names of the columns.
    fn select_all(&mut self, cx: &mut Context<Self>) {
        let rows = self.model.row_count();
        let columns = self.model.columns().len();
        if rows == 0 || columns == 0 {
            return;
        }
        self.rects = vec![Span {
            top: 0,
            bottom: rows - 1,
            left: 0,
            right: columns - 1,
            pick: Pick::Rows,
        }];
        self.anchor = Some((0, 0));
        self.selected = Some(self.selected.unwrap_or((0, 0)));
        self.drag = None;
        cx.notify();
    }

    /// The cell that a press of `pick` names. A row keeps the column of the anchor, and a
    /// column keeps its row, so that the anchor still gives Shift an end to draw from.
    fn picked(&self, pick: Pick, row: usize, column: usize) -> (usize, usize) {
        let (anchor_row, anchor_column) = self.anchor.unwrap_or((0, 0));
        match pick {
            Pick::Cells => (row, column),
            Pick::Rows => (row, anchor_column),
            Pick::Columns => (anchor_row, column),
        }
    }

    /// The rectangle that `pick` draws from `from` to `to`. A row or a column reaches across
    /// the whole table, so only its own row or column decides it.
    fn span(&self, pick: Pick, from: (usize, usize), to: (usize, usize)) -> Span {
        let last_row = self.model.row_count().saturating_sub(1);
        let last_column = self.model.columns().len().saturating_sub(1);
        match pick {
            Pick::Cells => Span::new(pick, from, to),
            Pick::Rows => Span {
                top: from.0.min(to.0),
                bottom: from.0.max(to.0),
                left: 0,
                right: last_column,
                pick,
            },
            Pick::Columns => Span {
                top: 0,
                bottom: last_row,
                left: from.1.min(to.1),
                right: from.1.max(to.1),
                pick,
            },
        }
    }

    /// Whether the part of the table that `pick` names at `cell` is selected in full.
    fn taken(&self, pick: Pick, cell: (usize, usize)) -> bool {
        match pick {
            Pick::Cells => self.in_range(cell.0, cell.1),
            Pick::Rows => {
                (0..self.model.columns().len()).all(|column| self.in_range(cell.0, column))
            }
            Pick::Columns => (0..self.model.row_count()).all(|row| self.in_range(row, cell.1)),
        }
    }

    /// The cells that the part of the table that `pick` names at `cell` covers.
    fn cells_of(&self, pick: Pick, cell: (usize, usize)) -> Vec<(usize, usize)> {
        match pick {
            Pick::Cells => vec![cell],
            Pick::Rows => (0..self.model.columns().len())
                .map(|column| (cell.0, column))
                .collect(),
            Pick::Columns => (0..self.model.row_count())
                .map(|row| (row, cell.1))
                .collect(),
        }
    }

    /// Takes the part of the table that `pick` names at `cell` out of the selection. A whole
    /// column or row comes out of a rectangle of its own gesture as a whole, so the parts that
    /// stay keep the gesture; any other rectangle loses the cells of the part one by one.
    fn take_out(&mut self, pick: Pick, cell: (usize, usize)) {
        let cells = self.cells_of(pick, cell);
        let rects = std::mem::take(&mut self.rects);
        self.rects = rects
            .into_iter()
            .flat_map(|span| match pick {
                Pick::Columns if span.pick == Pick::Columns => span.without_column(cell.1),
                Pick::Rows if span.pick == Pick::Rows => span.without_row(cell.0),
                _ => cells.iter().fold(vec![span], |parts, &(row, column)| {
                    parts
                        .into_iter()
                        .flat_map(|part| part.without(row, column))
                        .collect()
                }),
            })
            .collect();
    }

    /// A press of the left button on the part of the table that `pick` names. A plain press
    /// selects it and starts a drag. Shift draws the rectangle from the anchor to it. Ctrl
    /// takes the part out of the selection when it is selected in full, and adds it as a
    /// rectangle of its own otherwise, with a drag that draws that rectangle.
    fn press(
        &mut self,
        pick: Pick,
        row: usize,
        column: usize,
        modifiers: &Modifiers,
        cx: &mut Context<Self>,
    ) {
        if self.model.row_count() == 0 || self.model.columns().is_empty() {
            return;
        }
        let at = self.picked(pick, row, column);
        if modifiers.shift {
            let from = self.anchor.unwrap_or(at);
            self.rects = vec![self.span(pick, from, at)];
            self.drag = None;
        } else if modifiers.control || modifiers.platform {
            if self.taken(pick, at) {
                self.take_out(pick, at);
                self.drag = None;
            } else {
                self.rects.push(self.span(pick, at, at));
                self.drag = Some(Drag {
                    pick,
                    anchor: at,
                    index: self.rects.len() - 1,
                });
            }
            self.anchor = Some(at);
        } else {
            self.rects = vec![self.span(pick, at, at)];
            self.drag = Some(Drag {
                pick,
                anchor: at,
                index: 0,
            });
            self.anchor = Some(at);
        }
        self.selected = Some(at);
        cx.notify();
    }

    /// The pointer is over `cell` during a drag, and the rectangle of the drag follows it.
    /// `only` names the gesture of the element: a header extends a drag of columns only, and
    /// a row number a drag of rows only.
    fn drag_over(&mut self, cell: (usize, usize), only: Option<Pick>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else {
            return;
        };
        if only.is_some_and(|pick| pick != drag.pick) {
            return;
        }
        self.extend_drag(cell);
        cx.notify();
    }

    /// The rectangle of the drag reaches `cell`, and the caret follows it.
    fn extend_drag(&mut self, cell: (usize, usize)) {
        let Some(drag) = self.drag else {
            return;
        };
        let span = self.span(drag.pick, drag.anchor, cell);
        if let Some(slot) = self.rects.get_mut(drag.index) {
            *slot = span;
        }
        self.selected = Some(match drag.pick {
            Pick::Cells => cell,
            Pick::Rows => (cell.0, drag.anchor.1),
            Pick::Columns => (drag.anchor.0, cell.1),
        });
    }

    /// The pointer moved in the window during a drag. A drag ends once no button is held,
    /// and otherwise the pointer is kept for the scroll at the edges of the rows.
    fn drag_pointer(&mut self, position: Point<Pixels>, held: bool, cx: &mut Context<Self>) {
        if self.drag.is_none() {
            return;
        }
        if held {
            self.pointer = Some(position);
        } else {
            self.end_drag();
        }
        cx.notify();
    }

    /// The drag ends with the release of the left button.
    fn end_drag(&mut self) {
        self.drag = None;
        self.pointer = None;
    }

    /// While a drag holds the pointer past an edge of the rows, the rows scroll and the drag
    /// reaches the row or the column at that edge. Each frame asks for the next one, until the
    /// pointer is back among the rows or the rows reach their end. A cell drag scrolls both
    /// ways, a row drag only up and down, and a column drag only sideways.
    fn edge_scroll(&mut self, window: &mut Window, gutter: f32) {
        let Some(drag) = self.drag else {
            return;
        };
        let Some(pointer) = self.pointer else {
            return;
        };
        let (rows, columns) = (self.model.row_count(), self.model.columns().len());
        if rows == 0 || columns == 0 {
            return;
        }
        let handle = self.scroll.0.borrow().base_handle.clone();
        let bounds = handle.bounds();
        let offset = handle.offset();
        let max = handle.max_offset();
        // The numbers keep the gutter, so the cells begin to its right.
        let (top, bottom) = (f32::from(bounds.top()), f32::from(bounds.bottom()));
        let (left, right) = (f32::from(bounds.left()) + gutter, f32::from(bounds.right()));
        let (pointer_x, pointer_y) = (f32::from(pointer.x), f32::from(pointer.y));
        let vertical = (drag.pick != Pick::Columns)
            .then(|| {
                edge_step(
                    f32::from(offset.y),
                    f32::from(max.y),
                    pointer_y,
                    top,
                    bottom,
                    DATA_ROW_HEIGHT,
                )
            })
            .flatten();
        let horizontal = (drag.pick != Pick::Rows)
            .then(|| {
                edge_step(
                    f32::from(offset.x),
                    f32::from(max.x),
                    pointer_x,
                    left,
                    right,
                    CELL_WIDTH,
                )
            })
            .flatten();
        if vertical.is_none() && horizontal.is_none() {
            return;
        }
        let next_y = vertical.unwrap_or(f32::from(offset.y));
        let next_x = horizontal.unwrap_or(f32::from(offset.x));
        handle.set_offset(point(px(next_x), px(next_y)));
        window.request_animation_frame();

        // The drag reaches the first row under the top edge or the last row above the bottom,
        // and the first or the last column beside the left or the right edge.
        let (mut row, mut column) = self.selected.unwrap_or((0, 0));
        if vertical.is_some() {
            let first = (-next_y / DATA_ROW_HEIGHT).floor().max(0.) as usize;
            let last = ((bottom - top - next_y) / DATA_ROW_HEIGHT).ceil().max(1.) as usize - 1;
            row = if pointer_y < top { first } else { last };
            row = row.min(rows - 1);
        }
        if horizontal.is_some() {
            let first = (-next_x / CELL_WIDTH).floor().max(0.) as usize;
            let last = ((right - left - next_x) / CELL_WIDTH).ceil().max(1.) as usize - 1;
            column = if pointer_x < left { first } else { last };
            column = column.min(columns - 1);
        }
        self.extend_drag((row, column));
    }

    /// The selected cells as text. The rectangles are taken in one box, in the order of the
    /// table: a cell that no rectangle takes stays empty. The names of the columns come first
    /// when the rectangles are whole columns.
    fn copied(&self) -> Option<Copied> {
        let count = self.model.row_count();
        let width = self.model.columns().len();
        if self.rects.is_empty() || count == 0 || width == 0 {
            return None;
        }
        let top = self.rects.iter().map(|span| span.top).min()?;
        let bottom = self
            .rects
            .iter()
            .map(|span| span.bottom)
            .max()?
            .min(count - 1);
        let left = self.rects.iter().map(|span| span.left).min()?;
        let right = self
            .rects
            .iter()
            .map(|span| span.right)
            .max()?
            .min(width - 1);
        let columns = self.model.columns().get(left..=right)?.to_vec();
        let mut rows: Vec<Vec<Option<Value>>> = Vec::new();
        for row in top..=bottom {
            let Some(found) = self.model.row(row) else {
                continue;
            };
            rows.push(
                (left..=right)
                    .map(|column| {
                        self.in_range(row, column)
                            .then(|| found.cell(column).cloned().unwrap_or(Value::Null))
                    })
                    .collect(),
            );
        }
        // A whole column is a column rectangle that reaches the first and the last row.
        let names = self
            .rects
            .iter()
            .all(|span| span.pick == Pick::Columns && span.top == 0 && span.bottom + 1 == count);
        if rows.is_empty() {
            return None;
        }
        Some(Copied {
            columns,
            rows,
            names,
        })
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(copied) = self.copied() else {
            return;
        };
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(copied.tsv()));
    }

    fn copy_as_insert(&mut self, cx: &mut Context<Self>) {
        let Some(copied) = self.copied() else {
            return;
        };
        let table = self.table_ref();
        // Each row writes only the cells the selection takes: a row whose cells are a
        // different set of columns gets a statement of its own, and no column of it is
        // filled with a NULL that the user did not select.
        let text = copied
            .rows
            .iter()
            .filter_map(|row| {
                let mut columns = Vec::new();
                let mut values = Vec::new();
                for (column, cell) in copied.columns.iter().zip(row) {
                    if let Some(value) = cell {
                        columns.push(column.clone());
                        values.push(value.clone());
                    }
                }
                (!values.is_empty())
                    .then(|| insert_literal(self.dialect, &table, &columns, &values))
            })
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text));
    }

    fn copy_as_csv(&mut self, cx: &mut Context<Self>) {
        let Some(copied) = self.copied() else {
            return;
        };
        let rows: Vec<Vec<Value>> = copied
            .rows
            .iter()
            .map(|row| filled(row, Value::Text(String::new())))
            .collect();
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(csv(
            &copied.columns,
            &rows,
        )));
    }

    fn move_selection(&mut self, row_delta: isize, column_delta: isize, extend: bool) {
        let rows = self.model.row_count();
        let columns = self.model.columns().len();
        if rows == 0 || columns == 0 {
            return;
        }
        let Some((row, column)) = self.selected else {
            self.select_cell(0, 0);
            return;
        };
        let row = (row as isize + row_delta).clamp(0, rows as isize - 1) as usize;
        let column = (column as isize + column_delta).clamp(0, columns as isize - 1) as usize;
        if !extend || self.anchor.is_none() {
            self.anchor = Some((row, column));
        }
        let from = self.anchor.unwrap_or((row, column));
        self.selected = Some((row, column));
        // A plain move leaves one cell; a move with Shift draws the rectangle from the anchor.
        self.rects = vec![Span::new(Pick::Cells, from, (row, column))];
        self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();
        let ctrl = stroke.modifiers.control || stroke.modifiers.platform;
        let alt = stroke.modifiers.alt;
        let shift = stroke.modifiers.shift;

        if let Some(editing) = &mut self.editing {
            match key {
                "escape" => self.cancel_edit(cx),
                "enter" => self.accept_edit(cx),
                "tab" => {
                    self.accept_edit(cx);
                    self.move_selection(0, if shift { -1 } else { 1 }, false);
                    cx.notify();
                }
                _ => {
                    if editing.field.key_tool_field(event, false, cx) {
                        cx.notify();
                    } else {
                        return;
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        if self.filter_active {
            match key {
                "escape" => {
                    self.filter_active = false;
                    cx.notify();
                }
                "enter" => {
                    self.filter_active = false;
                    if !self.model.is_dirty() {
                        self.page = 0;
                        self.load(cx);
                    }
                }
                _ => {
                    if self.filter.key_tool_field(event, false, cx) {
                        cx.notify();
                    } else {
                        return;
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        match (key, ctrl, alt) {
            ("escape", _, _) => {
                if self.question.take().is_none() {
                    self.editing = None;
                }
                cx.notify();
            }
            ("f5", _, _) | ("r", true, false) => self.load(cx),
            ("enter", true, _) => self.submit(false, cx),
            ("insert", false, true) => self.add_row(cx),
            ("delete", true, _) => self.delete_row(cx),
            ("n", true, true) => self.set_null(cx),
            ("c", true, _) => self.copy(cx),
            ("a", true, false) => self.select_all(cx),
            ("enter", false, false) => {
                if let Some((row, column)) = self.selected {
                    self.begin_edit(row, column, cx);
                }
            }
            ("up", _, _) => {
                self.move_selection(-1, 0, shift);
                cx.notify();
            }
            ("down", _, _) => {
                self.move_selection(1, 0, shift);
                cx.notify();
            }
            ("left", _, _) => {
                self.move_selection(0, -1, shift);
                cx.notify();
            }
            ("right", _, _) => {
                self.move_selection(0, 1, shift);
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn toolbar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .flex_none()
            .border_b_1()
            .border_color(theme.border);
        bar = bar.child(
            icon_button(
                "database-data-refresh",
                "general/refresh",
                "Refresh",
                Some("Ctrl+R".into()),
                theme,
            )
            .debug_selector(|| "database-data-refresh".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
        );
        bar = bar.child(
            icon_button(
                "database-data-first",
                "general/arrowLeft",
                "First Page",
                None,
                theme,
            )
            .debug_selector(|| "database-data-first".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.page = 0;
                this.load(cx);
            })),
        );
        bar = bar.child(
            icon_button(
                "database-data-previous",
                "general/arrowLeft",
                "Previous Page",
                None,
                theme,
            )
            .debug_selector(|| "database-data-previous".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.page = this.page.saturating_sub(1);
                this.load(cx);
            })),
        );
        bar = bar.child(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(format!("Page {}", self.page + 1)),
        );
        bar = bar.child(
            icon_button(
                "database-data-next",
                "general/chevronRight",
                "Next Page",
                None,
                theme,
            )
            .debug_selector(|| "database-data-next".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.page += 1;
                this.load(cx);
            })),
        );
        let last = self
            .total
            .map(|total| total.div_ceil(self.page_rows as u64));
        let last = last.unwrap_or(0).saturating_sub(1) as usize;
        bar = bar.child(
            icon_button(
                "database-data-last",
                "general/arrowRight",
                "Last Page",
                None,
                theme,
            )
            .debug_selector(|| "database-data-last".to_owned())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.page = last;
                this.load(cx);
            })),
        );
        if let Some(total) = self.total {
            bar = bar.child(
                div()
                    .debug_selector(|| "database-data-total".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(format!("{total} rows")),
            );
        }
        bar = bar.child(div().w(px(SPACE_M)));
        // The placeholder is drawn beside the text of the field, so the caret and the
        // selection of the filter stay on screen, as in the other fields of the toolbox.
        let filter_empty = self.filter.text.is_empty();
        bar = bar.child(
            div()
                .flex()
                .items_center()
                .w(px(280.))
                .h(px(FIELD_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(if self.filter_active {
                    theme.accent
                } else {
                    theme.control_border
                })
                .debug_selector(|| "database-data-filter".to_owned())
                .cursor_text()
                .children(text_field_children_named(
                    FieldMouse::new(cx, |this| Some(&mut this.filter)),
                    "database-data-filter-field",
                    &self.filter,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    TEXT_SMALL,
                    self.filter_active,
                ))
                .when(filter_empty, |this| {
                    this.child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child("WHERE …"),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.filter_active = true;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }),
                ),
        );
        bar = bar.child(div().flex_1());
        if let Some(ticket) = self.in_flight.filter(|ticket| {
            matches!(
                ticket.request,
                DataRequest::Page | DataRequest::Count | DataRequest::Statement(_)
            )
        }) {
            let reading = matches!(ticket.request, DataRequest::Page | DataRequest::Count);
            bar = bar.child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(if reading { "Reading…" } else { "Saving…" }),
            );
            bar = bar.child(
                small_button("database-data-cancel", "Cancel", theme).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.cancel(ticket, cx);
                    },
                )),
            );
        }
        bar = bar.child(
            icon_button(
                "database-data-add",
                "general/add",
                "Add Row",
                Some(keymap::shortcut_label("alt-insert", keymap::platform()).into()),
                theme,
            )
            .debug_selector(|| "database-data-add".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.add_row(cx))),
        );
        bar = bar.child(
            icon_button(
                "database-data-delete",
                "general/minus",
                "Delete Row",
                Some(
                    keymap::shortcut_label(
                        if cfg!(target_os = "macos") {
                            "cmd-delete"
                        } else {
                            "ctrl-delete"
                        },
                        keymap::platform(),
                    )
                    .into(),
                ),
                theme,
            )
            .debug_selector(|| "database-data-delete".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.delete_row(cx))),
        );
        bar = bar.child(
            small_button("database-data-submit", "Submit", theme)
                .debug_selector(|| "database-data-submit".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx))),
        );
        bar = bar.child(
            icon_button(
                "database-data-revert",
                "general/rollback",
                "Revert",
                None,
                theme,
            )
            .debug_selector(|| "database-data-revert".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                if this.busy(cx) {
                    return;
                }
                this.model.revert();
                this.submit = None;
                this.error = None;
                this.status = Some("The changes are reverted.".to_owned());
                cx.notify();
            })),
        );
        bar.child(
            icon_button(
                "database-data-menu",
                "general/moreVertical",
                "More Actions",
                None,
                theme,
            )
            .on_click(
                cx.listener(|this, event: &ClickEvent, _, cx| this.menu(event.position(), cx)),
            ),
        )
        .into_any_element()
    }

    fn cancel(&mut self, ticket: DataTicket, cx: &mut Context<Self>) {
        let Some(panel) = self.panel.upgrade() else {
            return;
        };
        let source = self.source.clone();
        panel.update(cx, |panel, cx| panel.data_cancel(source, ticket, cx));
    }

    /// The menu of the grid and of a cell: the copy of the selected rows.
    fn menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let view = cx.entity();
        let copy = view.clone();
        let insert = view.clone();
        let out = view.clone();
        let null = view.clone();
        let mut entries = vec![
            MenuEntry::new("Copy", move |_, cx| {
                copy.update(cx, |view, cx| view.copy(cx));
            })
            .shortcut("Ctrl+C"),
            MenuEntry::new("Copy as INSERT", move |_, cx| {
                insert.update(cx, |view, cx| view.copy_as_insert(cx));
            }),
            MenuEntry::new("Copy as CSV", move |_, cx| {
                out.update(cx, |view, cx| view.copy_as_csv(cx));
            }),
        ];
        if !self.is_read_only() {
            entries.push(MenuEntry::separator());
            entries.push(
                MenuEntry::new("Set NULL", move |_, cx| {
                    null.update(cx, |view, cx| view.set_null(cx));
                })
                .shortcut("Ctrl+Alt+N"),
            );
        }
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    /// The menu of a column header: the sort in either order.
    fn header_menu(&mut self, column: usize, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(name) = self.columns.get(column).map(|column| column.name.clone()) else {
            return;
        };
        let view = cx.entity();
        let (ascending, descending) = (view.clone(), view.clone());
        let (up_name, down_name) = (name.clone(), name);
        let entries = vec![
            MenuEntry::new("Sort Ascending", move |_, cx| {
                ascending.update(cx, |view, cx| view.sort_by(up_name.clone(), true, cx));
            }),
            MenuEntry::new("Sort Descending", move |_, cx| {
                descending.update(cx, |view, cx| view.sort_by(down_name.clone(), false, cx));
            }),
        ];
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        workspace.update(cx, |workspace, cx| {
            workspace.show_menu(position, entries, cx)
        });
    }

    /// The sort icon of a column: ascending, then descending, then no sort, and the rows
    /// are read again in that order.
    fn cycle_sort(&mut self, column: &str, cx: &mut Context<Self>) {
        self.sort = match &self.sort {
            Some((sorted, true)) if sorted.as_str() == column => Some((column.to_owned(), false)),
            Some((sorted, false)) if sorted.as_str() == column => None,
            _ => Some((column.to_owned(), true)),
        };
        self.page = 0;
        self.load(cx);
    }

    /// Sorts the rows by `column` in the order asked for, from the menu of a header.
    fn sort_by(&mut self, column: String, ascending: bool, cx: &mut Context<Self>) {
        self.sort = Some((column, ascending));
        self.page = 0;
        self.load(cx);
    }

    fn question_band(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(question) = self.question.clone() else {
            return div().into_any_element();
        };
        let answers = match question {
            Question::Unsaved => {
                let submit = small_button("database-data-question-submit", "Submit", theme)
                    .debug_selector(|| "database-data-question-submit".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.question = None;
                        this.submit(true, cx);
                    }));
                let discard = small_button("database-data-question-discard", "Discard", theme)
                    .debug_selector(|| "database-data-question-discard".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.question = None;
                        this.model.revert();
                        this.submit = None;
                        this.close(cx);
                    }));
                let cancel = small_button("database-data-question-cancel", "Cancel", theme)
                    .debug_selector(|| "database-data-question-cancel".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.question = None;
                        cx.notify();
                    }));
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(submit)
                    .child(discard)
                    .child(cancel)
            }
            Question::Console {
                closing,
                console_manual,
                ..
            } => {
                let commit = small_button("database-data-question-commit", "Commit", theme)
                    .debug_selector(|| "database-data-question-commit".to_owned())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.question = None;
                        this.start_submit(closing, console_manual, cx);
                    }));
                let cancel = small_button("database-data-question-cancel", "Cancel", theme)
                    .debug_selector(|| "database-data-question-cancel".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.question = None;
                        cx.notify();
                    }));
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(commit)
                    .child(cancel)
            }
        };
        div()
            .id("database-data-question")
            .debug_selector(|| "database-data-question".to_owned())
            .flex()
            .items_center()
            .justify_between()
            .gap(px(SPACE_M))
            .px(px(DIALOG_PADDING))
            .py(px(SPACE_S))
            .flex_none()
            .bg(theme.hover)
            .child(div().child(question.text()))
            .child(answers)
            .into_any_element()
    }

    fn status_line(&self, theme: &Theme) -> AnyElement {
        let mut rows = div()
            .flex()
            .flex_col()
            .flex_none()
            .px(px(SPACE_S))
            .py(px(SPACE_XS))
            .text_size(px(TEXT_SMALL));
        if let Some(reason) = self.read_only_reason() {
            rows = rows.child(
                div()
                    .debug_selector(|| "database-data-readonly".to_owned())
                    .text_color(theme.text_muted)
                    .child(reason),
            );
        } else if !self.model.has_key() && !self.model.columns().is_empty() {
            rows = rows.child(
                div()
                    .debug_selector(|| "database-data-nokey".to_owned())
                    .text_color(theme.text_muted)
                    .child(NO_KEY_HINT),
            );
        }
        if let Some(error) = &self.error {
            rows = rows.child(
                div()
                    .debug_selector(|| "database-data-error".to_owned())
                    .text_color(theme.stripe_error)
                    .child(error.clone()),
            );
        }
        if let Some(status) = &self.status {
            rows = rows.child(
                div()
                    .debug_selector(|| "database-data-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone()),
            );
        }
        rows.into_any_element()
    }

    /// The header of the grid: a corner that selects every row, then the names of the columns
    /// with their sort icons. The names move with the horizontal offset of the rows, and the
    /// corner stays where the numbers of the rows are.
    fn header(
        &mut self,
        theme: &Theme,
        gutter: f32,
        shift: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hover = theme.hover;
        let mut cells = div().relative().left(px(shift)).flex().h_full();
        for (index, column) in self.columns.iter().enumerate() {
            let name = column.name.clone();
            let sorted = self
                .sort
                .as_ref()
                .filter(|(sorted, _)| sorted == &name)
                .map(|(_, ascending)| *ascending);
            // The icon shows the current order; the tooltip names the order of the next click.
            let (icon_name, label) = match sorted {
                Some(true) => ("general/arrowUp", "Sort Descending"),
                Some(false) => ("general/arrowDown", "No Sorting"),
                None => ("general/chevronDown", "Sort Ascending"),
            };
            let picked = self.taken(Pick::Columns, (0, index));
            let sort_name = name.clone();
            // The icon is not occluding, so the header keeps the hover that shows it.
            let sort = div()
                .id(("database-data-sort", index))
                .debug_selector(move || format!("database-data-sort-{index}"))
                .flex_none()
                .size(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .when(sorted.is_none(), |this| {
                    this.invisible()
                        .group_hover(HEADER_GROUP, |style| style.visible())
                })
                .tooltip(move |_, cx| tooltip(label, None, cx))
                .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
                .on_click(cx.listener(move |this, _, _, cx| this.cycle_sort(&sort_name, cx)))
                .child(icon_sized(icon_name, ICON, theme));
            // The name takes the presses of the header, so a press on the icon does not select
            // the column; the cell takes the moves of a drag across the header.
            let title = div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(name)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.press(Pick::Columns, 0, index, &event.modifiers, cx);
                    }),
                );
            cells = cells.child(
                div()
                    .id(("database-data-head", index))
                    .debug_selector(move || format!("database-data-head-{index}"))
                    .group(HEADER_GROUP)
                    .w(px(CELL_WIDTH))
                    .flex_none()
                    .h_full()
                    .px(px(SPACE_S))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .when(picked, |this| this.bg(theme.selection))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        this.drag_over((0, index), Some(Pick::Columns), cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if !this.taken(Pick::Columns, (0, index)) {
                                this.press(Pick::Columns, 0, index, &Modifiers::default(), cx);
                            }
                            this.drag = None;
                            this.header_menu(index, event.position, cx);
                        }),
                    )
                    .child(title)
                    .child(sort),
            );
        }
        let corner = div()
            .id("database-data-corner")
            .debug_selector(|| "database-data-corner".to_owned())
            .flex_none()
            .w(px(gutter))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    this.select_all(cx);
                }),
            );
        div()
            .flex()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .bg(theme.tab_selected)
            .border_b_1()
            .border_color(theme.border)
            .overflow_hidden()
            .child(corner)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .child(cells),
            )
            .into_any_element()
    }

    /// The number that the row at `row` of the page shows: the rows of the earlier pages come
    /// first, so the first row of the second page is the one after the page size.
    fn row_number(&self, row: usize) -> usize {
        self.page * self.page_rows + row + 1
    }

    /// The width of the column of row numbers. The longest number that the page can show
    /// decides it, in the font of the data.
    fn gutter_width(&self, window: &Window) -> f32 {
        let longest = (self.page * self.page_rows + self.model.row_count()).max(1);
        let text = longest.to_string();
        let style = window.text_style();
        let size = style.font_size.to_pixels(window.rem_size());
        let run = style.to_run(text.len());
        let line =
            window
                .text_system()
                .shape_line(text.into(), size, std::slice::from_ref(&run), None);
        f32::from(line.width()) + 2. * SPACE_S
    }

    /// The rows of the grid in `range`. Each row starts with its number, which stays in place
    /// when the grid scrolls sideways: the cells scroll in a clip beside the number.
    fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        gutter: f32,
        shift: f32,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let columns = self.model.columns().len();
        let mut rows = Vec::new();
        for index in range {
            let Some(row) = self.model.row(index) else {
                continue;
            };
            let kind = row.kind();
            let error = row.error().map(str::to_owned);
            let marked = self
                .rects
                .iter()
                .any(|span| (span.top..=span.bottom).contains(&index));
            let mut line = div()
                .id(("database-data-row", index))
                .debug_selector(move || format!("database-data-row-{index}"))
                .relative()
                .flex()
                .flex_none()
                .w(px(gutter + CELL_WIDTH * columns as f32))
                .h(px(DATA_ROW_HEIGHT))
                .when(index % 2 == 1, |this| this.bg(theme.hover.opacity(0.3)))
                .when(kind == RowKind::New, |this| {
                    this.bg(theme.accent.opacity(0.12))
                })
                .when(kind == RowKind::Deleted, |this| {
                    this.bg(theme.stripe_error.opacity(0.12))
                });
            // The number is the first child: it is laid out in the gutter and moved back by the
            // offset of the rows, so that it stays where the grid starts.
            let number = self.row_number(index);
            line = line.child(
                div()
                    .id(("database-data-number", index))
                    .debug_selector(move || format!("database-data-number-{index}"))
                    .flex_none()
                    .relative()
                    .left(px(-shift))
                    .h_full()
                    .w(px(gutter))
                    .px(px(SPACE_S))
                    .flex()
                    .items_center()
                    .justify_end()
                    .border_r_1()
                    .border_color(theme.border)
                    .bg(if marked {
                        theme.selection
                    } else {
                        theme.editor_bg
                    })
                    .text_color(theme.text_muted)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            this.press(Pick::Rows, index, 0, &event.modifiers, cx);
                        }),
                    )
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        this.drag_over((index, 0), Some(Pick::Rows), cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if !this.taken(Pick::Rows, (index, 0)) {
                                this.press(Pick::Rows, index, 0, &Modifiers::default(), cx);
                            }
                            this.drag = None;
                            this.menu(event.position, cx);
                            cx.notify();
                        }),
                    )
                    .child(number.to_string()),
            );
            // The cells scroll in a clip beside the number, so they never pass under it.
            let mut cells_row = div()
                .relative()
                .left(px(shift))
                .flex()
                .flex_none()
                .h_full()
                .w(px(CELL_WIDTH * columns as f32));
            for column in 0..columns {
                let value = row.cell(column).cloned();
                let edited = row.is_edited(column);
                let selected = self
                    .selected
                    .is_some_and(|(row, found)| row == index && found == column);
                let in_range = self.in_range(index, column);
                let editing = self
                    .editing
                    .as_ref()
                    .is_some_and(|editing| editing.row == index && editing.column == column);
                let text = match (&value, editing) {
                    (Some(value), false) => value.text(),
                    _ => String::new(),
                };
                let mut cell = div()
                    .id(("database-data-cell", index * columns + column))
                    .debug_selector(move || format!("database-data-cell-{index}-{column}"))
                    .relative()
                    .w(px(CELL_WIDTH))
                    .flex_none()
                    .h_full()
                    .px(px(SPACE_S))
                    .overflow_hidden()
                    .border_r_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(in_range, |this| this.bg(theme.selection))
                    .when(selected, |this| this.bg(theme.selection_hover))
                    .when(edited, |this| this.bg(theme.accent.opacity(0.2)))
                    .when(matches!(value, Some(Value::Null)), |this| {
                        this.text_color(theme.text_muted).italic()
                    })
                    .when(kind == RowKind::Deleted, |this| {
                        this.text_color(theme.text_muted)
                    })
                    .cursor_text()
                    .child(if editing {
                        let field = self
                            .editing
                            .as_ref()
                            .map(|editing| editing.field.clone())
                            .unwrap_or_default();
                        div()
                            .flex()
                            .items_center()
                            .size_full()
                            .children(text_field_children_named(
                                FieldMouse::new(cx, |this| {
                                    this.editing.as_mut().map(|editing| &mut editing.field)
                                }),
                                "database-data-cell-field",
                                &field,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                TEXT_SMALL,
                                true,
                            ))
                            .into_any_element()
                    } else {
                        div().child(text).into_any_element()
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            // The cell being edited takes its own press and drag in its field.
                            if this.editing.as_ref().is_some_and(|editing| {
                                editing.row == index && editing.column == column
                            }) {
                                return;
                            }
                            this.press(Pick::Cells, index, column, &event.modifiers, cx);
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, _, _, cx| {
                            this.drag_over((index, column), None, cx)
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() >= 2 {
                            this.begin_edit(index, column, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus, cx);
                            if !this.in_range(index, column) {
                                this.press(Pick::Cells, index, column, &Modifiers::default(), cx);
                            }
                            // The menu acts on the cell under the pointer, and the rectangles stay.
                            this.selected = Some((index, column));
                            this.drag = None;
                            this.menu(event.position, cx);
                            cx.notify();
                        }),
                    );
                if error.as_ref().is_some_and(|_| column == 0) {
                    cell = cell.child(
                        div()
                            .id(("database-data-row-error", index))
                            .debug_selector(move || format!("database-data-row-error-{index}"))
                            .absolute()
                            .left(px(SPACE_S))
                            .top_0()
                            .h_full()
                            .px(px(SPACE_S))
                            .bg(theme.stripe_error)
                            .text_color(theme.over_accent)
                            .child(error.clone().unwrap_or_default()),
                    );
                }
                cells_row = cells_row.child(cell);
            }
            line = line.child(
                div()
                    .flex_none()
                    .relative()
                    .left(px(-shift))
                    .w(px(CELL_WIDTH * columns as f32))
                    .h_full()
                    .overflow_hidden()
                    .child(cells_row),
            );
            rows.push(line);
        }
        rows
    }

    /// Whether the cell at `row` and `column` is inside one of the rectangles.
    fn in_range(&self, row: usize, column: usize) -> bool {
        self.rects.iter().any(|span| span.contains(row, column))
    }
}

impl Render for DatabaseDataView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The rows scroll first, so that the header and the numbers below read this frame's offset.
        let gutter = self.gutter_width(window);
        self.edge_scroll(window, gutter);
        let shift = f32::from(self.scroll.0.borrow().base_handle.offset().x);
        let toolbar = self.toolbar(&theme, cx);
        let question = self.question_band(&theme, cx);
        let status = self.status_line(&theme);
        let header = self.header(&theme, gutter, shift, cx);
        let count = self.model.row_count();
        let columns = self.columns.len();
        let width = gutter + CELL_WIDTH * columns as f32;
        let view = cx.entity();
        let grid = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .when(count == 0, |this| {
                this.child(div().p(px(SPACE_M)).text_color(theme.text_muted).child(
                    if self.in_flight.is_some() {
                        "Reading the rows…"
                    } else {
                        "The table has no rows on this page."
                    },
                ))
            })
            .child(
                scrollable(
                    "database-data",
                    &self.scroll,
                    div().size_full().min_w(px(width)).child(
                        uniform_list(
                            "database-data-rows",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.rows(range, &theme, gutter, shift, cx)
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .with_horizontal_sizing_behavior(
                            ListHorizontalSizingBehavior::Unconstrained,
                        )
                        .on_scroll(cx.listener(|_, _, _, cx| cx.notify()))
                        .debug_selector(|| "database-data-rows".to_owned())
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        div()
            .id("database-data-tab")
            .debug_selector(|| "database-data-tab".to_owned())
            .key_context("Database Data")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.editor_bg)
            .child(toolbar)
            .child(question)
            .child(status)
            .child(grid)
            // The window hears the pointer everywhere, so a drag ends on a release outside the
            // grid. The canvas draws nothing and takes no pointer.
            .child(
                canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let moved = view.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                let held = event.pressed_button == Some(MouseButton::Left);
                                moved.update(cx, |this, cx| {
                                    this.drag_pointer(event.position, held, cx)
                                });
                            }
                        });
                        let released = view;
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture && event.button == MouseButton::Left
                            {
                                released.update(cx, |this, _| this.end_drag());
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
    }
}

/// The offset of the rows along one axis after one frame of a drag that holds the pointer past
/// an edge of them. `start` and `end` are the edges of the rows on that axis, and `unit` is the
/// size of a row or a cell there. The offset moves by one unit or more, as many as the pointer
/// is past the edge (at most four), and it stops at the ends. Offsets are negative when the
/// rows are scrolled down or to the right, as GPUI keeps them, and `max` is the distance that
/// they can scroll. The result is `None` while the pointer is among the rows, or when they
/// cannot move that way.
fn edge_step(offset: f32, max: f32, pointer: f32, start: f32, end: f32, unit: f32) -> Option<f32> {
    let step = |distance: f32| (distance / unit).ceil().clamp(1., 4.) * unit;
    if pointer < start {
        let next = (offset + step(start - pointer)).min(0.);
        (next > offset).then_some(next)
    } else if pointer > end {
        let next = (offset - step(pointer - end)).max(-max);
        (next < offset).then_some(next)
    } else {
        None
    }
}

fn count_of(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(integer) if *integer >= 0 => Some(*integer as u64),
        Value::Decimal(text) => text.trim().parse::<u64>().ok(),
        Value::Real(real) if *real >= 0. => Some(*real as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: a glob of the parent would bring the gpui `test` macro in place of `#[test]`.
    use super::{CELL_WIDTH, DATA_ROW_HEIGHT, Pick, Span, edge_step};

    #[test]
    fn a_cell_taken_out_of_a_rectangle_leaves_the_rest_of_it() {
        let span = Span::new(Pick::Cells, (0, 0), (2, 2));
        let rest = span.without(1, 1);
        assert_eq!(rest.len(), 4, "{rest:?}");
        assert!(rest.iter().all(|part| !part.contains(1, 1)));
        let kept = (0..=2)
            .flat_map(|row| (0..=2).map(move |column| (row, column)))
            .filter(|&(row, column)| rest.iter().any(|part| part.contains(row, column)))
            .count();
        assert_eq!(kept, 8, "every other cell stays selected");
        assert_eq!(
            span.without(5, 5),
            vec![span],
            "a cell outside stays as it is"
        );
    }

    #[test]
    fn the_grid_scrolls_at_an_edge_a_unit_at_a_time_and_stops_at_the_ends() {
        // The pointer is among the rows.
        assert_eq!(edge_step(-100., 500., 50., 0., 200., DATA_ROW_HEIGHT), None);
        // Above the top by 5 pixels: one row up.
        assert_eq!(
            edge_step(-100., 500., -5., 0., 200., DATA_ROW_HEIGHT),
            Some(-76.)
        );
        // Already at the top.
        assert_eq!(edge_step(0., 500., -5., 0., 200., DATA_ROW_HEIGHT), None);
        // Below the bottom by 50 pixels: three rows down.
        assert_eq!(
            edge_step(-100., 500., 250., 0., 200., DATA_ROW_HEIGHT),
            Some(-172.)
        );
        // At the bottom already.
        assert_eq!(
            edge_step(-500., 500., 250., 0., 200., DATA_ROW_HEIGHT),
            None
        );
        // Past the right edge by 200 pixels: two cells to the left.
        assert_eq!(
            edge_step(-100., 900., 1000., 0., 800., CELL_WIDTH),
            Some(-420.)
        );
    }
}
