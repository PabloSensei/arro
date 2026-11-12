//! Find in Files and Replace in Files. The search runs on the background executor and
//! reads open tabs from their buffers. Replace writes through the open tabs where a
//! file is open and through one `WorkspaceTransaction` for the rest.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ::base::Cancel;
use editor_view::{EditorView, Languages, Spot};
use search::{FileHits, SearchError, TextQuery, find_in_files, find_in_selection, replace_edits};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;
use workspace_core::{DocumentStore, EditSource, LoadedFile, SaveRequest, WorkspaceTransaction};

use crate::navigation;

const CONTEXT: &str = "FindInFiles";
const WIDTH: f32 = 900.;
const HEIGHT: f32 = 640.;
/// More lines than this are no use to look through; the search says so.
const MAX_LINES: usize = 1000;

actions!(
    find_in_files,
    [Previous, Next, Open, SwitchField, ReplaceAll]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some(CONTEXT)),
        KeyBinding::new("down", Next, Some(CONTEXT)),
        KeyBinding::new("enter", Open, Some(CONTEXT)),
        KeyBinding::new("tab", SwitchField, Some(CONTEXT)),
        KeyBinding::new("ctrl-enter", ReplaceAll, Some(CONTEXT)),
    ]);
}

/// The Find in Files window that is on screen, if any.
pub struct ActiveFinder(pub Option<Entity<Finder>>);

impl Global for ActiveFinder {}

/// The queries of Find in Files, newest first, kept for the whole window.
#[derive(Default)]
pub struct SearchHistory(pub Vec<String>);

impl Global for SearchHistory {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Find,
    Replace,
}

#[derive(Clone, Copy)]
enum Row {
    File(usize),
    Hit(usize, usize),
}

pub struct Finder {
    files: Entity<Arc<Vec<PathBuf>>>,
    /// The hint follows the list of files while the user keeps the mask empty.
    _files_changed: Subscription,
    root: Arc<PathBuf>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    history_scroll: ScrollHandle,
    replace_mode: bool,
    field: Field,
    find: TextField,
    replacement: TextField,
    match_case: bool,
    whole_words: bool,
    regex: bool,
    results: Vec<FileHits>,
    truncated: bool,
    message: Option<String>,
    selected: usize,
    serial: usize,
    cancel: Cancel,
    task: Option<Task<()>>,
    /// The file mask popup of Ctrl+Alt+F: `*.txt !*Test.txt`.
    mask: TextField,
    mask_popup: bool,
    /// What the mask field offers: the mask of the language of the project, or its most
    /// common extensions.
    mask_hint: String,
    /// The queries of this window, newest first, for Alt+Down.
    history: Vec<String>,
    history_popup: bool,
    history_selected: usize,
    /// Ctrl+Alt+E: search only the selection of the active editor.
    in_selection: bool,
}

type Found = (Result<(), SearchError>, Vec<FileHits>);

impl Finder {
    pub fn show(
        replace_mode: bool,
        files: Entity<Arc<Vec<PathBuf>>>,
        root: Arc<PathBuf>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let mask_hint = project_mask(files.read(cx).as_slice(), cx);
        let view = cx.new(|cx| Self {
            // The list of files may arrive after the dialog: the hint waits for it.
            _files_changed: cx.observe(&files, |finder: &mut Self, _, cx| {
                finder.follow_mask_hint(cx);
            }),
            files,
            root,
            workspace: weak,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            history_scroll: ScrollHandle::new(),
            replace_mode,
            field: Field::Find,
            find: TextField::default(),
            replacement: TextField::default(),
            match_case: false,
            whole_words: false,
            regex: false,
            results: Vec::new(),
            truncated: false,
            message: None,
            selected: 0,
            serial: 0,
            cancel: Cancel::new(),
            task: None,
            mask: TextField::default(),
            mask_popup: false,
            mask_hint,
            history: cx
                .try_global::<SearchHistory>()
                .map_or_else(Vec::new, |history| history.0.clone()),
            history_popup: false,
            history_selected: 0,
            in_selection: false,
        });
        cx.set_global(ActiveFinder(Some(view.clone())));
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn query(&self) -> TextQuery {
        TextQuery {
            pattern: self.find.text.clone(),
            regex: self.regex,
            match_case: self.match_case,
            whole_words: self.whole_words,
        }
    }

    /// Open tabs by path, the views of them.
    fn open_views(&self, cx: &App) -> Vec<(PathBuf, Entity<EditorView>)> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let workspace = workspace.read(cx);
        let paths = workspace.open_tabs().into_iter().flatten();
        let views = paths.filter_map(|path| {
            let view = workspace.view_of(&path)?.downcast::<EditorView>().ok()?;
            Some((path, view))
        });
        views.collect()
    }

    /// The selection of the active editor, the file and the text it is in.
    fn selection_text(&self, cx: &App) -> Option<(PathBuf, String, std::ops::Range<usize>)> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let path = workspace.active_path()?;
        let view = workspace.view_of(&path)?.downcast::<EditorView>().ok()?;
        let view = view.read(cx);
        let text = view.text()?;
        let (range, _) = view.selection()?;
        (!range.is_empty()).then_some((path, text, range))
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.serial += 1;
        self.cancel.cancel();
        self.cancel = Cancel::new();
        self.task = None;
        self.message = None;
        // The subscription keeps the hint up to date; a keystroke does not read the
        // whole list of files again.
        if self.mask_hint.is_empty() {
            self.mask_hint = project_mask(self.files.read(cx).as_slice(), cx);
        }
        if self.find.text.is_empty() {
            self.results.clear();
            self.truncated = false;
            self.selected = 0;
            cx.notify();
            return;
        }
        let open: HashMap<PathBuf, String> = self
            .open_views(cx)
            .into_iter()
            .filter_map(|(path, view)| Some((path, view.read(cx).text()?)))
            .collect();
        let files: Vec<PathBuf> = self
            .files
            .read(cx)
            .iter()
            .filter(|path| accepts_mask(&self.mask.text, path))
            .cloned()
            .collect();
        let query = self.query();
        let cancel = self.cancel.clone();
        let serial = self.serial;
        let selection_requested = self.in_selection;
        let selection = selection_requested
            .then(|| self.selection_text(cx))
            .flatten();
        let search = cx.background_executor().spawn(async move {
            if selection_requested && selection.is_none() {
                return (Ok(()), Vec::new());
            }
            if let Some((path, text, range)) = selection {
                let found = match find_in_selection(&query, &path, &text, range) {
                    Ok(hits) => vec![hits],
                    Err(_) => Vec::new(),
                };
                return (Ok(()), found);
            }
            let found = Mutex::new(Vec::new());
            let on_file = |hits: FileHits| {
                found
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(hits)
            };
            let result = find_in_files(&query, &files, &open, &cancel, &on_file);
            let found = found
                .into_inner()
                .unwrap_or_else(|poison| poison.into_inner());
            (result, found)
        });
        self.task = Some(cx.spawn(async move |this, cx| {
            let found = search.await;
            this.update(cx, |this, cx| this.arrived(serial, found, cx))
                .ok();
        }));
        cx.notify();
    }

    /// The hint of the mask follows the list of files of the project until the user
    /// types a mask: the list may arrive after the dialog is open, and a stale hint of a
    /// half-read project misleads.
    fn follow_mask_hint(&mut self, cx: &mut Context<Self>) {
        if !self.mask.text.is_empty() {
            return;
        }
        let hint = project_mask(self.files.read(cx).as_slice(), cx);
        if hint != self.mask_hint {
            self.mask_hint = hint;
            cx.notify();
        }
    }

    fn arrived(&mut self, serial: usize, (result, mut found): Found, cx: &mut Context<Self>) {
        if serial != self.serial {
            return;
        }
        if let Err(error) = result {
            self.message = Some(error.to_string());
            self.results.clear();
            cx.notify();
            return;
        }
        found.sort_by(|a, b| a.path.cmp(&b.path));
        let mut lines = 0;
        self.truncated = false;
        for file in &mut found {
            let room = MAX_LINES.saturating_sub(lines);
            if file.lines.len() > room {
                file.lines.truncate(room);
                self.truncated = true;
            }
            lines += file.lines.len();
        }
        found.retain(|file| !file.lines.is_empty());
        self.results = found;
        self.selected = self.first_hit().unwrap_or(0);
        cx.notify();
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (file, hits) in self.results.iter().enumerate() {
            rows.push(Row::File(file));
            rows.extend((0..hits.lines.len()).map(|line| Row::Hit(file, line)));
        }
        rows
    }

    fn first_hit(&self) -> Option<usize> {
        self.rows()
            .iter()
            .position(|row| matches!(row, Row::Hit(..)))
    }

    /// The field of the find row, for a test of selection and editing.
    #[cfg(test)]
    pub(crate) fn find_field(&self) -> &TextField {
        &self.find
    }

    /// The field of the replace row, for a test of selection and editing.
    #[cfg(test)]
    pub(crate) fn replace_field(&self) -> &TextField {
        &self.replacement
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.mask_popup {
            match key.key.as_str() {
                "escape" => self.mask_popup = false,
                "enter" => {
                    self.mask_popup = false;
                    self.refresh(cx);
                }
                _ => {
                    if self.mask.key_with_clipboard(event, false, cx) {
                        self.refresh(cx);
                    } else {
                        return;
                    }
                }
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.history_popup {
            match key.key.as_str() {
                "escape" => self.history_popup = false,
                "up" => {
                    self.history_selected = self.history_selected.saturating_sub(1);
                }
                "down" => {
                    self.history_selected =
                        (self.history_selected + 1).min(self.history.len().saturating_sub(1));
                }
                "enter" => {
                    if let Some(text) = self.history.get(self.history_selected).cloned() {
                        self.find = TextField::from_text(text);
                        self.history_popup = false;
                        self.refresh(cx);
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key.modifiers.control && key.modifiers.alt && key.key == "f" {
            self.mask_popup = true;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key.modifiers.alt && !key.modifiers.control && key.key == "down" {
            self.history_popup = true;
            self.history_selected = 0;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key.modifiers.control && key.modifiers.alt && key.key == "e" {
            self.in_selection = !self.in_selection;
            cx.stop_propagation();
            self.refresh(cx);
            return;
        }
        if key.modifiers.alt && !key.modifiers.control {
            match key.key.as_str() {
                "c" => self.match_case = !self.match_case,
                "w" => self.whole_words = !self.whole_words,
                "x" => self.regex = !self.regex,
                _ => return,
            }
            cx.stop_propagation();
            self.refresh(cx);
            return;
        }
        let edited = match self.field {
            Field::Find => self.find.key_with_clipboard(event, false, cx),
            Field::Replace => self.replacement.key_with_clipboard(event, false, cx),
        };
        if !edited {
            return;
        }
        if self.field == Field::Find {
            self.refresh(cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.rows();
        let mut at = self.selected as isize;
        loop {
            at += delta;
            match rows.get(at as usize).filter(|_| at >= 0) {
                Some(Row::Hit(..)) => {
                    self.selected = at as usize;
                    break;
                }
                Some(Row::File(_)) => continue,
                None => break,
            }
        }
        self.scroll.scroll_to_item(self.selected);
        cx.notify();
    }

    /// The current query joins the history of Alt+Down.
    fn remember_query(&mut self) {
        let text = self.find.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        self.history.retain(|kept| *kept != text);
        self.history.insert(0, text);
        self.history.truncate(20);
    }

    fn remember_global(&self, cx: &mut App) {
        cx.default_global::<SearchHistory>().0 = self.history.clone();
    }

    fn open(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Row::Hit(file, line)) = self.rows().get(index).copied() else {
            return;
        };
        self.remember_query();
        self.remember_global(cx);
        let hits = &self.results[file];
        let hit = &hits.lines[line];
        let column = hit.text[..hit.matches.first().map_or(0, |found| found.start)]
            .chars()
            .count();
        let spot = Spot::Line {
            line: (hit.line as usize).saturating_sub(1),
            column,
        };
        let path = hits.path.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                navigation::go_to(workspace, path, Some(spot), true, window, cx);
            })
            .ok();
    }

    fn switch_field(&mut self, cx: &mut Context<Self>) {
        if self.replace_mode {
            self.field = match self.field {
                Field::Find => Field::Replace,
                Field::Replace => Field::Find,
            };
            cx.notify();
        }
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        if !self.replace_mode || self.results.is_empty() {
            return;
        }
        let query = self.query();
        let replacement = self.replacement.text.clone();
        let open = self.open_views(cx);
        let mut done = 0;
        let mut closed: Vec<PathBuf> = Vec::new();
        let mut error = None;
        for hits in &self.results {
            match open.iter().find(|(path, _)| *path == hits.path) {
                Some((_, view)) => {
                    let text = view.read(cx).text().unwrap_or_default();
                    match replace_edits(&query, &replacement, &text) {
                        Ok(edits) => {
                            done += edits.len();
                            let bytes = view.read(cx).file_bytes();
                            let cause = local_history::Cause::BeforeReplace;
                            crate::history::snapshot_bytes(&hits.path, bytes, cause, cx);
                            view.update(cx, |view, cx| {
                                view.apply_edits(edits, cx);
                                view.save(cx);
                            });
                        }
                        Err(failed) => error = Some(failed.to_string()),
                    }
                }
                None => closed.push(hits.path.clone()),
            }
        }
        if let Some(error) = error {
            self.message = Some(error);
            cx.notify();
            return;
        }
        let files = self.results.len();
        let history = crate::history::get(cx);
        let work = cx.background_executor().spawn(async move {
            if let Some(history) = history {
                for path in &closed {
                    let _ = history.record_file(path, local_history::Cause::BeforeReplace);
                }
            }
            replace_on_disk(&query, &replacement, &closed)
        });
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            let text = match outcome {
                Ok(count) => format!("{} replacements in {files} files", done + count),
                Err(message) => message,
            };
            workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_user("Replace in Files", text, cx)
                })
                .ok();
            this.update(cx, |this, cx| this.refresh(cx)).ok();
        })
        .detach();
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.set_global(ActiveFinder(None));
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// Ctrl+Alt+F from the command of the window.
    pub fn show_mask_popup(&mut self, cx: &mut Context<Self>) {
        self.mask_popup = true;
        cx.notify();
    }

    /// Alt+Down from the command of the window.
    pub fn show_history(&mut self, cx: &mut Context<Self>) {
        self.history_popup = true;
        self.history_selected = 0;
        cx.notify();
    }

    /// Ctrl+Alt+E from the command of the window.
    pub fn toggle_selection(&mut self, cx: &mut Context<Self>) {
        self.in_selection = !self.in_selection;
        // The command may run while the window is being updated, and the refresh reads
        // the workspace for its open tabs: the search waits until that update is over.
        let finder = cx.weak_entity();
        cx.defer(move |cx| {
            finder.update(cx, |finder, cx| finder.refresh(cx)).ok();
        });
    }

    /// The results as plain text, for Export to Text File.
    pub fn export_text(&self) -> String {
        let mut lines = Vec::new();
        for file in &self.results {
            lines.push(file.path.display().to_string());
            for hit in &file.lines {
                lines.push(format!("  {}: {}", hit.line, hit.text.trim()));
            }
        }
        lines.join("\n")
    }

    /// The mask popup and the history list, for tests.
    #[cfg(test)]
    pub(crate) fn mask_open(&self) -> bool {
        self.mask_popup
    }

    #[cfg(test)]
    pub(crate) fn history_open(&self) -> bool {
        self.history_popup
    }

    #[cfg(test)]
    pub(crate) fn history_len(&self) -> usize {
        self.history.len()
    }

    #[cfg(test)]
    pub(crate) fn in_selection(&self) -> bool {
        self.in_selection
    }

    /// The mask the field offers, for tests.
    #[cfg(test)]
    pub(crate) fn mask_hint(&self) -> &str {
        &self.mask_hint
    }

    /// The mask the user typed, for tests.
    #[cfg(test)]
    pub(crate) fn mask_text(&self) -> &str {
        &self.mask.text
    }

    /// The list of files it searches, so a test waits until the index has reached it
    /// before it reads the mask of the project.
    #[cfg(test)]
    pub(crate) fn files(&self) -> &Entity<Arc<Vec<PathBuf>>> {
        &self.files
    }

    fn input(&self, field: Field, value: &TextField, label: &str, theme: &Theme) -> Div {
        let active = field == self.field;
        let selector = match field {
            Field::Find => "finder-input-find",
            Field::Replace => "finder-input-replace",
        };
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .flex_1()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(if active {
                theme.accent
            } else {
                theme.control_border
            })
            .overflow_hidden()
            .when(value.text.is_empty() && !active, |this| {
                this.text_color(theme.text_secondary)
                    .child(label.to_owned())
            })
            .when(!value.text.is_empty() || active, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .children(text_field_children(
                            value,
                            theme.text,
                            theme.text,
                            theme.selection,
                            theme.caret,
                            18.,
                            active,
                        )),
                )
            })
    }

    fn toggle(
        &self,
        spec: (&'static str, &'static str, &'static str, &'static str),
        on: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
        flip: fn(&mut Self),
    ) -> impl IntoElement + use<> {
        let (id, icon, label, shortcut) = spec;
        icon_button(id, icon, label, Some(shortcut.into()), theme)
            .when(on, |this| {
                this.bg(theme.tab_selected)
                    .border_1()
                    .border_color(theme.tab_selected_border)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                flip(this);
                this.refresh(cx);
            }))
    }

    fn row(
        &self,
        at: usize,
        row: Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        match row {
            Row::File(file) => {
                let hits = &self.results[file];
                let relative = hits
                    .path
                    .strip_prefix(self.root.as_path())
                    .unwrap_or(&hits.path);
                let title = format!("{}  ({})", relative.display(), hits.lines.len());
                div()
                    .id(("find-row", at))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .pt(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child(title)
                    .into_any_element()
            }
            Row::Hit(file, line) => {
                let hit = &self.results[file].lines[line];
                let selected = at == self.selected;
                let hover = theme.selection_hover;
                let (match_bg, match_text) = (theme.match_bg, theme.match_text);
                let spans = segments(&hit.text, &hit.matches);
                div()
                    .id(("find-row", at))
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .overflow_hidden()
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                    .on_click(cx.listener(move |this, _, window, cx| this.open(at, window, cx)))
                    .child(
                        div()
                            .w(px(40.))
                            .flex_none()
                            .text_color(theme.text_secondary)
                            .child(hit.line.to_string()),
                    )
                    .child(
                        div()
                            .flex()
                            .whitespace_nowrap()
                            .children(spans.into_iter().map(move |(text, matched)| {
                                div()
                                    .when(matched, |this| this.bg(match_bg).text_color(match_text))
                                    .child(text)
                            })),
                    )
                    .into_any_element()
            }
        }
    }
}

/// `text` without its indent, cut at the matches: each part and whether it matched.
/// The offsets of the matches are those of the whole line.
fn segments(text: &str, matches: &[std::ops::Range<usize>]) -> Vec<(String, bool)> {
    let skipped = text.len() - text.trim_start().len();
    let text = &text[skipped..];
    let mut parts = Vec::new();
    let mut at = 0;
    for found in matches {
        if found.start < skipped + at || found.end > skipped + text.len() {
            continue;
        }
        let (start, end) = (found.start - skipped, found.end - skipped);
        if start > at {
            parts.push((text[at..start].to_owned(), false));
        }
        parts.push((text[start..end].to_owned(), true));
        at = end;
    }
    if at < text.len() {
        parts.push((text[at..].to_owned(), false));
    }
    parts
}

/// Replaces in files that are not open: all or none of them change, the writes follow.
fn replace_on_disk(
    query: &TextQuery,
    replacement: &str,
    paths: &[PathBuf],
) -> Result<usize, String> {
    let mut store = DocumentStore::new();
    let mut transaction = WorkspaceTransaction::new(EditSource::User);
    let mut documents = Vec::new();
    let mut count = 0;
    for path in paths {
        let file =
            LoadedFile::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let document = store.open(file);
        let Some(state) = store.state(document) else {
            continue;
        };
        let text = text_core::TextStore::text(&state.text);
        let edits = replace_edits(query, replacement, &text).map_err(|error| error.to_string())?;
        count += edits.len();
        transaction = transaction.edit_at(document, state.version, edits);
        documents.push(document);
    }
    store
        .apply(transaction)
        .map_err(|error| error.to_string())?;
    let requests: Vec<SaveRequest> = documents
        .into_iter()
        .filter_map(|document| store.save_request(document))
        .collect();
    SaveRequest::write_all(requests).map_err(|error| error.to_string())?;
    Ok(count)
}

impl Render for Finder {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.12;
        let rows = self.rows();
        let items: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(at, row)| self.row(at, *row, &theme, cx))
            .collect();
        let status = match (
            &self.message,
            self.results.is_empty(),
            self.find.text.is_empty(),
        ) {
            (Some(message), _, _) => message.clone(),
            (None, _, true) => String::new(),
            (None, true, false) => "Nothing found".to_owned(),
            (None, false, false) => {
                let lines: usize = self.results.iter().map(|file| file.lines.len()).sum();
                let more = if self.truncated { "+" } else { "" };
                format!("{lines}{more} matches in {} files", self.results.len())
            }
        };
        let heading = if self.replace_mode {
            "Replace in Files"
        } else {
            "Find in Files"
        };
        let find_label = "Search for";
        let mask_row = div()
            .id("find-mask")
            .debug_selector(|| "find-mask".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .mb(px(POPUP_PADDING))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(theme.accent)
            .child(div().text_color(theme.text_secondary).child("File mask:"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(text_field_children(
                        &self.mask,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        true,
                    )),
            )
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child(self.mask_hint.clone()),
            );
        let history_rows: Vec<_> = self
            .history
            .iter()
            .enumerate()
            .map(|(index, query)| {
                let chosen = index == self.history_selected;
                let query = query.clone();
                let click_query = query.clone();
                div()
                    .id(("find-history", index))
                    .debug_selector(move || format!("find-history-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.find = TextField::from_text(click_query.clone());
                        this.history_popup = false;
                        this.refresh(cx);
                    }))
                    .child(query)
            })
            .collect();
        let history = self.history_popup.then(|| {
            scrollable(
                "find-history-list",
                &self.history_scroll,
                div()
                    .id("find-history-list")
                    .debug_selector(|| "find-history-list".to_owned())
                    .flex()
                    .flex_col()
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .track_scroll(&self.history_scroll)
                    .children(history_rows),
                &theme,
                window,
                cx,
            )
            .flex_none()
            .mb(px(POPUP_PADDING))
        });

        let body = surface(&theme)
            .id("finder")
            .debug_selector(|| "finder".to_owned())
            .key_context("FindInFiles ToolWindow")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(px(HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_action(cx.listener(|this, _: &Previous, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Next, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SwitchField, _, cx| this.switch_field(cx)))
            .on_action(cx.listener(|this, _: &ReplaceAll, _, cx| this.replace_all(cx)))
            .on_action(
                cx.listener(|this, _: &Open, window, cx| this.open(this.selected, window, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .child(heading),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_S))
                    .mb(px(POPUP_PADDING))
                    .child(self.input(Field::Find, &self.find, find_label, &theme))
                    .child(self.toggle(
                        ("case", "general/caseSensitive", "Match Case", "Alt+C"),
                        self.match_case,
                        &theme,
                        cx,
                        |this| this.match_case = !this.match_case,
                    ))
                    .child(self.toggle(
                        ("word", "general/wholeWord", "Whole Words", "Alt+W"),
                        self.whole_words,
                        &theme,
                        cx,
                        |this| this.whole_words = !this.whole_words,
                    ))
                    .child(self.toggle(
                        ("regex", "general/regex", "Regex", "Alt+X"),
                        self.regex,
                        &theme,
                        cx,
                        |this| this.regex = !this.regex,
                    )),
            )
            .when(self.mask_popup, |this| this.child(mask_row))
            .children(history)
            .when(self.replace_mode, |this| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .flex_none()
                        .gap(px(SPACE_S))
                        .mb(px(POPUP_PADDING))
                        .child(self.input(
                            Field::Replace,
                            &self.replacement,
                            "Replace with",
                            &theme,
                        ))
                        .child(
                            div()
                                .id("replace-all")
                                .flex()
                                .items_center()
                                .h(px(26.))
                                .px(px(SPACE_M))
                                .rounded(px(CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.control_border)
                                .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx)))
                                .child("Replace All"),
                        ),
                )
            })
            .child(
                div()
                    .flex_none()
                    .h(px(22.))
                    .px(px(ROW_PADDING_X))
                    .text_color(theme.text_secondary)
                    .child(status),
            )
            .child(
                scrollable(
                    "find-list",
                    &self.scroll,
                    div()
                        .id("find-list")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(items),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );

        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("finder-enter", body))
    }
}

/// What the mask field of Find in Files offers for a project: the mask of the language
/// most of its files speak, or the most common extension when no language knows them.
pub(crate) fn project_mask(files: &[std::path::PathBuf], cx: &App) -> String {
    let languages = cx.try_global::<Languages>();
    let mut known: Vec<(&'static str, usize, &'static str)> = Vec::new();
    let mut extensions: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for path in files {
        if let Some(config) = languages.and_then(|languages| (languages.resolve)(path))
            && !config.file_mask.is_empty()
        {
            match known.iter_mut().find(|(name, _, _)| *name == config.name) {
                Some((_, count, _)) => *count += 1,
                None => known.push((config.name, 1, config.file_mask)),
            }
        }
        if let Some(extension) = path.extension().and_then(|extension| extension.to_str()) {
            let extension = extension.to_ascii_lowercase();
            if !extension.is_empty() {
                *extensions.entry(extension).or_default() += 1;
            }
        }
    }
    let mut best: Option<(&str, usize, &str)> = None;
    for (name, count, mask) in known {
        if best.is_none_or(|(_, best_count, _)| count > best_count) {
            best = Some((name, count, mask));
        }
    }
    if let Some((_, _, mask)) = best {
        return mask.to_owned();
    }
    let mut best_extension: Option<(String, usize)> = None;
    for (extension, count) in extensions {
        if best_extension
            .as_ref()
            .is_none_or(|(_, best_count)| count > *best_count)
        {
            best_extension = Some((extension, count));
        }
    }
    best_extension.map_or_else(String::new, |(extension, _)| format!("*.{extension}"))
}

/// Whether a file passes the mask: positive patterns keep a file, `!` patterns drop it.
/// The name is the file name; a pattern with a slash matches the whole path.
fn accepts_mask(mask: &str, path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let full = path.to_string_lossy().replace('\\', "/");
    let mut positive = false;
    let mut kept = false;
    for pattern in mask.split_whitespace() {
        let (pattern, include) = match pattern.strip_prefix('!') {
            Some(pattern) => (pattern, false),
            None => (pattern, true),
        };
        if pattern.is_empty() {
            continue;
        }
        positive |= include;
        let subject = if pattern.contains('/') || pattern.contains('\\') {
            full.as_str()
        } else {
            name.as_str()
        };
        if !wildcard(pattern, subject) {
            continue;
        }
        if include {
            kept = true;
        } else {
            return false;
        }
    }
    kept || !positive
}

/// `*` matches any run of characters and `?` one character; case is ignored.
fn wildcard(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let (mut p, mut t) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            mark = t;
            p += 1;
        } else if let Some(at) = star {
            p = at + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::{TextQuery, accepts_mask, replace_on_disk, segments, wildcard};

    #[test]
    fn a_mask_keeps_matching_files_and_drops_the_excluded_ones() {
        let path = std::path::Path::new("C:/p/src/Owner.txt");
        assert!(accepts_mask("", path));
        assert!(accepts_mask("*.txt", path));
        assert!(!accepts_mask("*.kt", path));
        assert!(accepts_mask("*.txt !*Test.txt", path));
        assert!(!accepts_mask("!*.txt", path));
        assert!(accepts_mask(
            "*.txt !*Test.txt",
            std::path::Path::new("Owner.txt")
        ));
        assert!(!accepts_mask(
            "*.txt !*Test.txt",
            std::path::Path::new("OwnerTest.txt")
        ));
        assert!(wildcard("Own?r.*", "Owner.txt"));
        assert!(wildcard("*Test.txt", "OwnerTest.txt"));
        assert!(!wildcard("*Test.txt", "Owner.txt"));
    }

    #[test]
    fn a_line_is_cut_at_its_matches() {
        let parts = segments("  Owner owner = x;", &[2..7, 8..13]);
        let expected = [
            ("Owner", true),
            (" ", false),
            ("owner", true),
            (" = x;", false),
        ];
        let expected: Vec<(String, bool)> = expected
            .iter()
            .map(|(text, hit)| (text.to_string(), *hit))
            .collect();
        assert_eq!(parts, expected);
    }

    #[test]
    fn replace_on_disk_writes_every_file_or_none() {
        let directory = test_support::scratch("finder");
        let first = directory.join("a.txt");
        let second = directory.join("b.txt");
        std::fs::write(&first, "one two\r\none").unwrap();
        std::fs::write(&second, "two one").unwrap();

        let query = TextQuery::plain("one");
        let count = replace_on_disk(&query, "1", &[first.clone(), second.clone()]).unwrap();
        assert_eq!(count, 3);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "1 two\r\n1");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two 1");

        let missing = directory.join("none.txt");
        let failed = replace_on_disk(&TextQuery::plain("1"), "x", &[first.clone(), missing]);
        assert!(failed.is_err());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "1 two\r\n1");
    }

    #[test]
    #[allow(clippy::permissions_set_readonly_false)]
    fn replace_on_disk_leaves_every_file_when_one_is_read_only() {
        let directory = test_support::scratch("finder-read-only");
        let first = directory.join("a.txt");
        let second = directory.join("b.txt");
        std::fs::write(&first, "one two\r\none").unwrap();
        std::fs::write(&second, "two one").unwrap();
        let mut permissions = std::fs::metadata(&second).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&second, permissions).unwrap();

        let failed = replace_on_disk(
            &TextQuery::plain("one"),
            "1",
            &[first.clone(), second.clone()],
        );
        let message = failed.unwrap_err();
        assert!(message.contains("b.txt"), "{message}");
        assert_eq!(std::fs::read(&first).unwrap(), b"one two\r\none");
        assert_eq!(std::fs::read(&second).unwrap(), b"two one");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);

        let mut permissions = std::fs::metadata(&second).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&second, permissions).unwrap();
    }
}
