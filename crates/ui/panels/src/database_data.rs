//! The data tab of the Database window: one page of a table, edited in place.
//!
//! The statements of a page and of the edits live in `database`; this view owns the
//! keyboard, the grid and the question of a close with unsaved edits. Every request goes
//! through [`DatabasePanel`], which owns the connection and polls the helper, so the
//! view never waits for a database on the thread that draws it.

use std::path::PathBuf;

use database::{
    Column, DataModel, Dialect, PageQuery, Plan, RowKind, SourceId, Table, TableKind, TableRef,
    Value, csv, insert_literal, tsv,
};
use ui_kit::gpui::{ListHorizontalSizingBehavior, ScrollStrategy};
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

/// A question that stands over the tab before it closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Question {
    /// Submit, discard or keep editing before the tab or the window closes.
    Unsaved,
}

impl Question {
    fn text(self) -> &'static str {
        match self {
            Self::Unsaved => "Submit the changes of this table?",
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
    model: DataModel,
    total: Option<u64>,
    filter: TextField,
    filter_active: bool,
    /// The column the rows are sorted by and whether the order is ascending.
    sort: Option<(String, bool)>,
    selected: Option<(usize, usize)>,
    anchor: Option<(usize, usize)>,
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
        Self {
            workspace,
            panel,
            focus: cx.focus_handle(),
            window,
            path,
            source,
            model: DataModel::new(table.columns.clone()),
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
        self.question
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
                        self.model = DataModel::new(columns);
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
                if let (Some(count), DataRequest::Statement(_)) = (update_count, ticket.request) {
                    self.status = Some(format!("{count} row(s) changed."));
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
                    DataRequest::Statement(_) => self.advance_submit(cx),
                    DataRequest::Commit => self.committed(cx),
                    DataRequest::Rollback => self.rolled_back(cx),
                }
            }
            DataEvent::Error(message) => {
                self.in_flight = None;
                match ticket.request {
                    DataRequest::Statement(index) => self.failed_submit(index, message, cx),
                    DataRequest::AutoCommit | DataRequest::Commit | DataRequest::Rollback => {
                        self.submit = None;
                        self.error = Some(message);
                        cx.notify();
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
        let missing_key = !self.model.has_key();
        self.error = None;
        self.status = Some(if missing_key {
            format!(
                "No primary key: each row is matched by all of its columns. {} statement(s)…",
                plan.statements.len()
            )
        } else {
            format!("Saving {} statement(s)…", plan.statements.len())
        });
        self.submit = Some(Submit {
            steps: plan.statements,
            next: 0,
            closing,
            failed: false,
        });
        self.send_command(DataRequest::AutoCommit, DataCommand::AutoCommit(false), cx);
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
        let closing = self.submit.take().is_some_and(|submit| submit.closing);
        self.status = Some("The changes are saved.".to_owned());
        // The rows are the table's now: the next page replaces them without a question.
        self.model.revert();
        self.error = None;
        if closing {
            self.close(cx);
            return;
        }
        // The page is read again from the table, and the default of the driver comes
        // back, so the next statement of the console commits.
        self.reload_after_commit = true;
        self.send_command(DataRequest::AutoCommit, DataCommand::AutoCommit(true), cx);
    }

    fn rolled_back(&mut self, cx: &mut Context<Self>) {
        self.submit = None;
        if self.error.is_none() {
            self.status = Some("The changes are rolled back.".to_owned());
        }
        // The rows keep their edits: the user can fix the row and submit again.
        cx.notify();
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
        self.selected = Some((row, 0));
        self.anchor = self.selected;
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
        self.model.delete_row(row);
        let rows = self.model.row_count();
        self.selected = (rows > 0).then(|| ((row).min(rows - 1), 0));
        self.anchor = self.selected;
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
        let Some(found) = self.model.row(row) else {
            return;
        };
        let text = match found.cell(column) {
            Some(Value::Null) | None => String::new(),
            Some(value) => value.text(),
        };
        self.selected = Some((row, column));
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

    /// The selected cells as text: the whole rectangle when the user extended it.
    fn selected_rows(&self) -> Vec<Vec<Value>> {
        let Some((row, column)) = self.selected else {
            return Vec::new();
        };
        let (top, bottom, left, right) = match self.anchor {
            Some((anchor_row, anchor_column)) => (
                row.min(anchor_row),
                row.max(anchor_row),
                column.min(anchor_column),
                column.max(anchor_column),
            ),
            None => (row, row, column, column),
        };
        let mut rows = Vec::new();
        for index in top..=bottom {
            let Some(found) = self.model.row(index) else {
                continue;
            };
            rows.push(
                (left..=right)
                    .map(|column| found.cell(column).cloned().unwrap_or(Value::Null))
                    .collect(),
            );
        }
        rows
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        if rows.is_empty() {
            return;
        }
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(tsv(&rows)));
    }

    fn copy_as_insert(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        if rows.is_empty() {
            return;
        }
        let names = self.selected_columns();
        let text = rows
            .iter()
            .map(|row| insert_literal(self.dialect, &self.table_ref(), &names, row))
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(text));
    }

    fn copy_as_csv(&mut self, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        if rows.is_empty() {
            return;
        }
        let names = self.selected_columns();
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(csv(&names, &rows)));
    }

    fn selected_columns(&self) -> Vec<Column> {
        let Some((row, column)) = self.selected else {
            return Vec::new();
        };
        let columns = self.model.columns();
        let (left, right) = match self.anchor {
            Some((_, anchor_column)) => (column.min(anchor_column), column.max(anchor_column)),
            None => (column, column),
        };
        let _ = row;
        (left..=right)
            .filter_map(|index| columns.get(index).cloned())
            .collect()
    }

    fn move_selection(&mut self, row_delta: isize, column_delta: isize, extend: bool) {
        let rows = self.model.row_count();
        let columns = self.model.columns().len();
        if rows == 0 || columns == 0 {
            return;
        }
        let Some((row, column)) = self.selected else {
            self.selected = Some((0, 0));
            self.anchor = self.selected;
            return;
        };
        let row = (row as isize + row_delta).clamp(0, rows as isize - 1) as usize;
        let column = (column as isize + column_delta).clamp(0, columns as isize - 1) as usize;
        if extend && self.anchor.is_none() {
            self.anchor = Some((row, column));
        }
        if !extend {
            self.anchor = Some((row, column));
        }
        self.selected = Some((row, column));
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
                    if editing.field.key_with_clipboard(event, false, cx) {
                        cx.notify();
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
                    if self.filter.key_with_clipboard(event, false, cx) {
                        cx.notify();
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

    fn question_band(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(question) = self.question else {
            return div().into_any_element();
        };
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(submit)
                    .child(discard)
                    .child(cancel),
            )
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
                    .child(
                        "No primary key: a row is found by all of its columns, so two equal \
                         rows are written twice.",
                    ),
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

    /// The header of the grid, moved by the horizontal offset of the rows.
    fn header(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let offset = self.scroll.0.borrow().base_handle.offset().x;
        let mut cells = div().flex().ml(px((-offset).into()));
        for (index, column) in self.columns.iter().enumerate() {
            let sorted = self
                .sort
                .as_ref()
                .is_some_and(|(name, _)| name == &column.name);
            let arrow = match (sorted, self.sort.as_ref().map(|(_, ascending)| *ascending)) {
                (true, Some(true)) => " ▲",
                (true, Some(false)) => " ▼",
                _ => "",
            };
            let title = format!("{}{arrow}", column.name);
            let name = column.name.clone();
            cells = cells.child(
                div()
                    .id(("database-data-head", index))
                    .debug_selector(move || format!("database-data-head-{index}"))
                    .w(px(CELL_WIDTH))
                    .flex_none()
                    .px(px(SPACE_S))
                    .h_full()
                    .overflow_hidden()
                    .border_r_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child(title)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ascending = !matches!(
                            &this.sort,
                            Some((sorted, true)) if sorted == &name
                        );
                        this.sort = Some((name.clone(), ascending));
                        this.page = 0;
                        this.load(cx);
                    })),
            );
        }
        div()
            .flex()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .bg(theme.tab_selected)
            .overflow_hidden()
            .child(cells)
            .into_any_element()
    }

    fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        theme: &Theme,
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
            let mut line = div()
                .id(("database-data-row", index))
                .debug_selector(move || format!("database-data-row-{index}"))
                .relative()
                .flex()
                .flex_none()
                .w(px(CELL_WIDTH * columns as f32))
                .h(px(DATA_ROW_HEIGHT))
                .when(index % 2 == 1, |this| this.bg(theme.hover.opacity(0.3)))
                .when(kind == RowKind::New, |this| {
                    this.bg(theme.accent.opacity(0.12))
                })
                .when(kind == RowKind::Deleted, |this| {
                    this.bg(theme.stripe_error.opacity(0.12))
                });
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
                            if event.modifiers.shift {
                                this.selected = Some((index, column));
                                if this.anchor.is_none() {
                                    this.anchor = Some((index, column));
                                }
                            } else {
                                this.selected = Some((index, column));
                                this.anchor = Some((index, column));
                            }
                            cx.notify();
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
                            this.selected = Some((index, column));
                            this.anchor = Some((index, column));
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
                line = line.child(cell);
            }
            rows.push(line);
        }
        rows
    }

    fn in_range(&self, row: usize, column: usize) -> bool {
        let Some((selected_row, selected_column)) = self.selected else {
            return false;
        };
        let (top, bottom, left, right) = match self.anchor {
            Some((anchor_row, anchor_column)) => (
                selected_row.min(anchor_row),
                selected_row.max(anchor_row),
                selected_column.min(anchor_column),
                selected_column.max(anchor_column),
            ),
            None => (selected_row, selected_row, selected_column, selected_column),
        };
        row >= top && row <= bottom && column >= left && column <= right
    }
}

impl Render for DatabaseDataView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let toolbar = self.toolbar(&theme, cx);
        let question = self.question_band(&theme, cx);
        let status = self.status_line(&theme);
        let header = self.header(&theme, cx);
        let count = self.model.row_count();
        let columns = self.columns.len();
        let width = CELL_WIDTH * columns as f32;
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
                                this.rows(range, &theme, cx)
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
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.editor_bg)
            .child(toolbar)
            .child(question)
            .child(status)
            .child(grid)
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
