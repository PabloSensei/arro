//! Find in Files and Replace in Files. The search runs on the background executor and
//! reads open tabs from their buffers. Replace writes through the open tabs where a
//! file is open and through one `WorkspaceTransaction` for the rest.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ::base::Cancel;
use editor_view::{EditorView, Languages, Spot};
use futures_util::StreamExt;
use search::{FileHits, SearchError, TextQuery, find_in_files, find_in_selection, replace_edits};
use serde::{Deserialize, Serialize};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{FileIcons, USAGES_TOOL, Workspace};
use workspace_core::{DocumentStore, EditSource, LoadedFile, SaveRequest, WorkspaceTransaction};

use crate::git::ActiveGit;
use crate::items::{Item, PanelRow};
use crate::navigation;
use crate::preview::{self, FilePreview, PreviewTarget};
use crate::project_structure::ProjectEnvironmentGlobal;
use crate::usages::{ActiveUsages, UsagesHub};

const CONTEXT: &str = "FindInFiles";
const WIDTH: f32 = 900.;
const HEIGHT: f32 = 640.;
/// The smallest size the window resizes to.
pub(crate) const MIN_WIDTH: f32 = 480.;
pub(crate) const MIN_HEIGHT: f32 = 360.;
/// More lines than this are no use to look through; the search says so.
const MAX_LINES: usize = 1000;

actions!(
    find_in_files,
    [Previous, Next, Open, SwitchField, CtrlEnter]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some(CONTEXT)),
        KeyBinding::new("down", Next, Some(CONTEXT)),
        KeyBinding::new("enter", Open, Some(CONTEXT)),
        KeyBinding::new("tab", SwitchField, Some(CONTEXT)),
    ]);
    // Replace All in Replace, Open in Find Window otherwise; the window branches on its mode.
    let row = workspace::KeyRow {
        keys: "ctrl-enter",
        mac: Some("cmd-enter"),
        action: "find_in_files::CtrlEnter",
        context: Some(CONTEXT),
        idea: None,
    };
    for key in row.keys_for(keymap::platform()) {
        cx.bind_keys([KeyBinding::new(key, CtrlEnter, Some(CONTEXT))]);
    }
}

/// The Find in Files window that is on screen, if any.
pub struct ActiveFinder(pub Option<Entity<Finder>>);

impl Global for ActiveFinder {}

/// The queries of Find in Files, newest first, kept for the whole window and in a file of
/// the state folder.
#[derive(Default)]
pub struct SearchHistory {
    pub queries: Vec<String>,
    path: Option<PathBuf>,
}

impl Global for SearchHistory {}

/// The file of the state folder that keeps the queries.
const HISTORY_FILE: &str = "find-in-files-history.txt";

/// Where the next search looks, as the row under the query says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    /// Every file of the project.
    #[default]
    Project,
    /// The files under one folder.
    Directory,
    /// The files of the open tabs, with their unsaved text.
    OpenFiles,
    /// The files under the source and resource folders of one module of the project.
    Module,
    /// The files that Local Changes of Git lists: the changed and the new ones.
    ChangedFiles,
}

/// What Find in Files remembers between openings for one project: the scope, the folder
/// of the Directory scope, the module of the Module scope, and the file mask. It lives in
/// the state folder like the queries do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct FinderState {
    scope: Scope,
    directory: Option<PathBuf>,
    module: Option<String>,
    mask: String,
    mask_on: bool,
}

/// The dropdowns under the scope row: the modules of the project, or the scopes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowList {
    Module,
    Scope,
}

/// The key of the state of Find in Files of one project.
fn state_key(root: &std::path::Path) -> String {
    format!("finder:{}", root.display())
}

/// What the project remembered, or the defaults.
fn load_state(store: Option<&store::Store>, root: &std::path::Path) -> FinderState {
    store
        .and_then(|store| store.get(&state_key(root)).ok().flatten())
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default()
}

/// Reads the queries of Find in Files that the state folder keeps.
pub(crate) fn install_history(state_dir: &std::path::Path, cx: &mut App) {
    if cx.try_global::<SearchHistory>().is_some() {
        return;
    }
    let path = state_dir.join(HISTORY_FILE);
    let queries = std::fs::read_to_string(&path)
        .map(|body| {
            body.lines()
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .take(20)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    cx.set_global(SearchHistory {
        queries,
        path: Some(path),
    });
}

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
    /// A replacement takes the case of the text it replaces.
    preserve_case: bool,
    /// Where the search looks.
    scope: Scope,
    /// The folder of the Directory scope, when one is chosen.
    directory: Option<PathBuf>,
    /// The module of the Module scope, by its id in the model of the project.
    module: Option<String>,
    /// The dropdown under the scope row that is open, if any.
    list: Option<RowList>,
    /// Whether the file mask filters the search.
    mask_on: bool,
    results: Vec<FileHits>,
    truncated: bool,
    message: Option<String>,
    /// Whether the search in flight has not answered yet.
    searching: bool,
    /// What the search found before it answered: files with matches and their lines.
    found_files: usize,
    found_lines: usize,
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
    /// The preview of the selected row under the list.
    preview: Entity<FilePreview>,
    /// The height of that preview, changed by the divider above it.
    preview_height: f32,
    /// Where the pointer was when a drag of the divider began.
    preview_drag: Option<f32>,
    /// The state folder that remembers the height.
    store: Option<Arc<store::Store>>,
    /// The pin of the window: a click outside does not close it.
    pinned: bool,
}

/// The answer of one search: its error, and every file with hits.
pub(crate) type Found = (Result<(), SearchError>, Vec<FileHits>);

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
        let store = workspace.store();
        let remembered = load_state(store.as_deref(), root.as_path());
        let saved = preview::load(store.as_deref(), root.as_path());
        let preview_height = saved
            .finder_height
            .unwrap_or(preview::DEFAULT_FINDER_PREVIEW)
            .clamp(preview::MIN_FINDER_PREVIEW, preview::MAX_FINDER_PREVIEW);
        let preview = cx.new(|_| FilePreview::default());
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
            preserve_case: false,
            scope: remembered.scope,
            directory: remembered.directory.clone(),
            module: remembered.module.clone(),
            list: None,
            mask_on: remembered.mask_on,
            results: Vec::new(),
            truncated: false,
            message: None,
            searching: false,
            found_files: 0,
            found_lines: 0,
            selected: 0,
            serial: 0,
            cancel: Cancel::new(),
            task: None,
            mask: TextField::from_text(remembered.mask),
            mask_popup: false,
            mask_hint,
            history: cx
                .try_global::<SearchHistory>()
                .map_or_else(Vec::new, |history| history.queries.clone()),
            history_popup: false,
            history_selected: 0,
            in_selection: false,
            preview,
            preview_height,
            preview_drag: None,
            store,
            pinned: false,
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
            preserve_case: self.preserve_case,
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
        self.searching = false;
        self.found_files = 0;
        self.found_lines = 0;
        // The subscription keeps the hint up to date; a keystroke does not read the
        // whole list of files again.
        if self.mask_hint.is_empty() {
            self.mask_hint = project_mask(self.files.read(cx).as_slice(), cx);
        }
        if self.find.text.is_empty() {
            self.results.clear();
            self.truncated = false;
            self.selected = 0;
            self.preview
                .update(cx, |preview, cx| preview.show(None, cx));
            cx.notify();
            return;
        }
        let plan = self.request(cx).plan(cx);
        let cancel = self.cancel.clone();
        let serial = self.serial;
        let (sender, mut receiver) = futures_channel::mpsc::unbounded::<FileHits>();
        let search = cx.background_executor().spawn(async move {
            let on_file = |hits: FileHits| {
                let _ = sender.unbounded_send(hits);
            };
            plan.run(&cancel, &on_file)
        });
        self.searching = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            // Every file the search finds reaches the window while it runs, so the line
            // under the fields counts what is already there.
            while let Some(hits) = receiver.next().await {
                this.update(cx, |this, cx| this.progress(serial, &hits, cx))
                    .ok();
            }
            let found = search.await;
            this.update(cx, |this, cx| this.arrived(serial, found, cx))
                .ok();
        }));
        cx.notify();
    }

    /// A file the running search found: the line of the window counts its matches.
    fn progress(&mut self, serial: usize, hits: &FileHits, cx: &mut Context<Self>) {
        if serial != self.serial {
            return;
        }
        self.found_files += 1;
        self.found_lines += hits.lines.len();
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

    /// The scope, the folder and the mask go to the state folder, so the next opening of
    /// the window starts where this one did.
    fn remember_settings(&self) {
        let state = FinderState {
            scope: self.scope,
            directory: self.directory.clone(),
            module: self.module.clone(),
            mask: self.mask.text.clone(),
            mask_on: self.mask_on,
        };
        let Ok(json) = serde_json::to_string(&state) else {
            return;
        };
        let Some(store) = &self.store else {
            return;
        };
        if let Err(error) = store.put(&state_key(self.root.as_path()), &json) {
            eprintln!("the settings of Find in Files are not saved: {error}");
        }
    }

    /// A scope of the row under the query, or of the Scope dropdown: the search starts
    /// again there. Changed Files needs a Git repository, and stays unchosen without one.
    fn choose_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if scope == Scope::ChangedFiles && !changed_available(cx) {
            return;
        }
        self.list = None;
        if scope == Scope::Directory && self.directory.is_none() {
            self.scope = scope;
            self.mask_popup = false;
            self.remember_settings();
            cx.notify();
            return;
        }
        self.scope = scope;
        self.mask_popup = false;
        self.remember_settings();
        self.refresh(cx);
    }

    /// A module of the Module dropdown: the search keeps to its source and resource folders.
    fn choose_module(&mut self, module: String, cx: &mut Context<Self>) {
        self.module = Some(module);
        self.scope = Scope::Module;
        self.list = None;
        self.mask_popup = false;
        self.remember_settings();
        self.refresh(cx);
    }

    /// A dropdown of the scope row opens, or closes when it is open already.
    fn toggle_list(&mut self, list: RowList, cx: &mut Context<Self>) {
        self.list = if self.list == Some(list) {
            None
        } else {
            Some(list)
        };
        self.mask_popup = false;
        cx.notify();
    }

    /// The request of the search the window asks for now, as the Find window keeps it.
    fn request(&self, cx: &App) -> FindRequest {
        FindRequest {
            query: self.query(),
            scope: self.scope,
            directory: self.directory.clone(),
            module: self.module.clone(),
            mask: if self.mask_on {
                self.mask.text.clone()
            } else {
                String::new()
            },
            in_selection: self.in_selection,
            selection: self.in_selection.then(|| self.selection_text(cx)).flatten(),
            files: self.files.clone(),
            workspace: self.workspace.clone(),
        }
    }

    /// The module the Module scope names in the window: the one the user picked, or the
    /// first module of the model when the picked one is not in the model.
    fn module_in_force(&self, cx: &App) -> Option<String> {
        let model = environment_model(cx)?;
        chosen_module(self.module.as_deref(), &model)
    }

    /// The Escape key closes an open dropdown first.
    fn key_closes_list(&mut self, key: &str) -> bool {
        if self.list.is_some() && key == "escape" {
            self.list = None;
            return true;
        }
        false
    }

    /// Ctrl+Enter outside Replace, and the button under the list: the places of the search
    /// move to the Find window as a tab with the query, and this window closes.
    fn open_in_find_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.replace_mode || self.find.text.trim().is_empty() {
            return;
        }
        self.remember_query();
        self.remember_global(cx);
        let request = self.request(cx);
        // A search that has not answered yet is run again by the Find window itself.
        let found = (!self.searching).then(|| (Ok(()), self.results.clone()));
        let hub = cx.try_global::<ActiveUsages>().map(|hub| hub.0.clone());
        self.dismiss(window, cx);
        if let Some(hub) = hub {
            hub.update(cx, |hub, cx| hub.add_find_tab(request, found, cx));
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_tool(USAGES_TOOL, window, cx);
            })
            .ok();
    }

    /// The folder button of the Directory scope: the search keeps to the folder the user
    /// picks.
    fn choose_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            if let Some(folder) = chosen.and_then(|paths| paths.into_iter().next()) {
                this.update(cx, |this, cx| {
                    this.directory = Some(folder);
                    this.scope = Scope::Directory;
                    this.remember_settings();
                    this.refresh(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// The File mask checkbox: the mask filters the search only while it is on.
    fn toggle_mask(&mut self, cx: &mut Context<Self>) {
        self.mask_on = !self.mask_on;
        self.remember_settings();
        self.refresh(cx);
    }

    fn arrived(&mut self, serial: usize, (result, mut found): Found, cx: &mut Context<Self>) {
        if serial != self.serial {
            return;
        }
        self.searching = false;
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
        self.show_selected_preview(cx);
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

    /// Starts a search from a test without a key press, so the state before the answer
    /// of the search can be read.
    #[cfg(test)]
    pub(crate) fn search(&mut self, text: &str, cx: &mut Context<Self>) {
        self.find = TextField::from_text(text.to_owned());
        self.refresh(cx);
    }

    /// Whether the search in flight has not answered yet, for a test of the status line.
    #[cfg(test)]
    pub(crate) fn is_searching(&self) -> bool {
        self.searching
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.key_closes_list(&key.key) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if self.mask_popup {
            match key.key.as_str() {
                "escape" => {
                    self.mask_popup = false;
                    self.remember_settings();
                }
                "enter" => {
                    self.mask_popup = false;
                    self.remember_settings();
                    self.refresh(cx);
                }
                _ => {
                    if self.mask.key_with_clipboard(event, false, cx) {
                        self.mask_on = !self.mask.text.is_empty();
                        self.remember_settings();
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
        self.show_selected_preview(cx);
        cx.notify();
    }

    /// A click selects the row and shows its file in the preview; a second click opens
    /// the file, as the list of IDEA does.
    fn pick(&mut self, index: usize, clicks: usize, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.rows().get(index), Some(Row::Hit(..))) {
            self.selected = index;
            self.scroll.scroll_to_item(index);
            self.show_selected_preview(cx);
            cx.notify();
        }
        if clicks >= 2 {
            self.open(index, window, cx);
        }
    }

    /// The row selected now, as the preview shows it: the file and its first match.
    fn selected_target(&self) -> Option<PreviewTarget> {
        let Row::Hit(file, line) = self.rows().get(self.selected).copied()? else {
            return None;
        };
        let hits = self.results.get(file)?;
        let hit = hits.lines.get(line)?;
        let found = hit.matches.first()?.clone();
        Some(PreviewTarget::found(
            hits.path.clone(),
            hit.line as usize,
            found,
        ))
    }

    /// Shows the file of the selected row in the preview under the list.
    fn show_selected_preview(&mut self, cx: &mut Context<Self>) {
        let target = self.selected_target();
        self.preview
            .update(cx, |preview, cx| preview.show(target, cx));
    }

    /// The divider above the preview follows the pointer; a drag up makes the preview
    /// taller.
    fn drag_preview(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(from) = self.preview_drag else {
            return;
        };
        let now = f32::from(event.position.y);
        self.preview_height = (self.preview_height + from - now)
            .clamp(preview::MIN_FINDER_PREVIEW, preview::MAX_FINDER_PREVIEW);
        self.preview_drag = Some(now);
        cx.notify();
    }

    /// The drag is over: the height goes to the state folder.
    fn finish_preview_drag(&mut self) {
        if self.preview_drag.take().is_none() {
            return;
        }
        preview::save_finder_height(
            self.store.as_deref(),
            self.root.as_path(),
            self.preview_height,
        );
    }

    /// The preview under the list, for a test.
    #[cfg(test)]
    pub(crate) fn preview(&self) -> &Entity<FilePreview> {
        &self.preview
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
        let (path, body) = {
            let history = cx.default_global::<SearchHistory>();
            history.queries = self.history.clone();
            (history.path.clone(), history.queries.join("\n"))
        };
        let Some(path) = path else {
            return;
        };
        cx.background_executor()
            .spawn(async move {
                if let Some(folder) = path.parent() {
                    let _ = std::fs::create_dir_all(folder);
                }
                let _ = std::fs::write(path, body);
            })
            .detach();
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
        self.remember_settings();
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

    /// Whether the window stays open on a click outside, for tests.
    #[cfg(test)]
    pub(crate) fn is_pinned(&self) -> bool {
        self.pinned
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

    /// The project the window searches, for a test that opens the window again.
    #[cfg(test)]
    pub(crate) fn root(&self) -> &Arc<PathBuf> {
        &self.root
    }

    /// The scope in force, for tests.
    #[cfg(test)]
    pub(crate) fn scope(&self) -> Scope {
        self.scope
    }

    /// Whether the mask filters the search, for tests.
    #[cfg(test)]
    pub(crate) fn mask_on(&self) -> bool {
        self.mask_on
    }

    /// Sets the scope and the folder of the Directory scope from a test.
    #[cfg(test)]
    pub(crate) fn set_scope(
        &mut self,
        scope: Scope,
        directory: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.scope = scope;
        self.directory = directory;
        self.remember_settings();
        self.refresh(cx);
    }

    /// Chooses a module of the Module dropdown from a test, and searches.
    #[cfg(test)]
    pub(crate) fn set_module(&mut self, module: &str, cx: &mut Context<Self>) {
        self.choose_module(module.to_owned(), cx);
    }

    /// Whether a scope can be chosen now: Changed Files needs a Git repository.
    #[cfg(test)]
    pub(crate) fn scope_available(&self, scope: Scope, cx: &App) -> bool {
        scope != Scope::ChangedFiles || changed_available(cx)
    }

    /// The names of the modules the Module dropdown lists, for tests.
    #[cfg(test)]
    pub(crate) fn module_names(&self, cx: &App) -> Vec<String> {
        modules_of(cx).into_iter().map(|(_, name)| name).collect()
    }

    /// Sets the file mask and its checkbox from a test.
    #[cfg(test)]
    pub(crate) fn set_mask(&mut self, mask: &str, on: bool, cx: &mut Context<Self>) {
        self.mask = TextField::from_text(mask.to_owned());
        self.mask_on = on;
        self.remember_settings();
        self.refresh(cx);
    }

    /// One choice of the scope row under the query.
    fn scope_chip(
        &self,
        scope: Scope,
        label: &'static str,
        id: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let chosen = self.scope == scope;
        div()
            .id(id)
            .debug_selector(move || id.to_owned())
            .flex()
            .items_center()
            .h(px(26.))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if chosen {
                theme.tab_selected_border
            } else {
                theme.control_border
            })
            .when(chosen, |this| this.bg(theme.tab_selected))
            .text_color(if chosen {
                theme.text
            } else {
                theme.text_secondary
            })
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.choose_scope(scope, cx)))
    }

    /// The row under the query: the scope of the search and the file mask, as IDEA shows
    /// them.
    fn scope_row(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div()
            .id("finder-scope-row")
            .debug_selector(|| "finder-scope-row".to_owned())
            .flex()
            .items_center()
            .flex_wrap()
            .flex_none()
            .gap(px(SPACE_S))
            .mb(px(POPUP_PADDING))
            .child(div().text_color(theme.text_secondary).child("In:"))
            .child(self.scope_chip(Scope::Project, "Project", "finder-scope-project", theme, cx))
            .child(self.module_field(theme, cx))
            .child(self.scope_chip(
                Scope::Directory,
                "Directory",
                "finder-scope-directory",
                theme,
                cx,
            ))
            .child(self.scope_field(theme, cx));
        if self.scope == Scope::Directory {
            let label = match self.directory.as_deref() {
                Some(directory) => directory
                    .strip_prefix(self.root.as_path())
                    .unwrap_or(directory)
                    .display()
                    .to_string(),
                None => "No folder chosen".to_owned(),
            };
            let chosen = self.directory.is_some();
            row = row.child(
                div()
                    .id("finder-directory")
                    .debug_selector(|| "finder-directory".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.control_border)
                    .child(
                        div()
                            .max_w(px(220.))
                            .overflow_hidden()
                            .truncate()
                            .text_color(if chosen { theme.text } else { theme.text_muted })
                            .child(label),
                    )
                    .child(
                        icon_button(
                            "finder-choose-directory",
                            "general/folder",
                            "Choose Directory",
                            None,
                            theme,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.choose_directory(window, cx)),
                        ),
                    ),
            );
        }
        let mask_empty = self.mask.text.is_empty();
        let mask_field = div()
            .id("finder-mask")
            .debug_selector(|| "finder-mask".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .w(px(220.))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if self.mask_popup {
                theme.accent
            } else {
                theme.control_border
            })
            .overflow_hidden()
            .cursor_text()
            .when(mask_empty && !self.mask_popup, |this| {
                this.text_color(theme.text_muted)
                    .child(self.mask_hint.clone())
            })
            .children(text_field_children(
                FieldMouse::with_edit(
                    cx,
                    |this| Some(&mut this.mask),
                    |this, cx| {
                        this.mask_on = !this.mask.text.is_empty();
                        this.remember_settings();
                        this.refresh(cx);
                    },
                ),
                &self.mask,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                self.mask_popup,
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.mask_popup = true;
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            );
        row.child(div().flex_1())
            .child(
                div()
                    .id("finder-mask-check")
                    .debug_selector(|| "finder-mask-check".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .cursor_pointer()
                    .child(checkbox(self.mask_on, theme))
                    .child("File mask")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_mask(cx))),
            )
            .child(mask_field)
            .into_any_element()
    }

    /// The Module dropdown of the scope row: the module the search keeps to, and the modules
    /// of the project under it.
    fn module_field(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.list == Some(RowList::Module);
        let label = match (self.scope, self.module_in_force(cx)) {
            (Scope::Module, Some(id)) => modules_of(cx)
                .into_iter()
                .find(|(module, _)| *module == id)
                .map_or_else(|| "Module".to_owned(), |(_, name)| name),
            _ => "Module".to_owned(),
        };
        select_field("finder-module-field", label, open, theme)
            .debug_selector(|| "finder-module".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_list(RowList::Module, cx)))
    }

    /// The Scope dropdown of the scope row: Project Files, Open Files or Changed Files.
    fn scope_field(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let open = self.list == Some(RowList::Scope);
        let label = match self.scope {
            Scope::Project => "Project Files",
            Scope::OpenFiles => "Open Files",
            Scope::ChangedFiles => "Changed Files",
            Scope::Directory | Scope::Module => "Scope",
        };
        select_field("finder-scope-field", label, open, theme)
            .debug_selector(|| "finder-scope".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_list(RowList::Scope, cx)))
    }

    /// The list of the open dropdown, drawn under the scope row. Changed Files is grey with
    /// a hint in a project that is not in a Git repository.
    fn scope_list(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self.list?;
        let menu = select_menu(theme).debug_selector(|| "finder-list".to_owned());
        let menu = match list {
            RowList::Module => {
                let modules = modules_of(cx);
                if modules.is_empty() {
                    menu.child(
                        div()
                            .text_color(theme.text_muted)
                            .child("The project has no modules"),
                    )
                } else {
                    let chosen = self.module_in_force(cx);
                    modules
                        .into_iter()
                        .enumerate()
                        .fold(menu, |menu, (index, (id, name))| {
                            let picked = self.scope == Scope::Module
                                && chosen.as_deref() == Some(id.as_str());
                            let selector = format!("finder-module-item-{index}");
                            menu.child(
                                select_option(
                                    ("finder-module-option", index),
                                    name,
                                    picked,
                                    false,
                                    theme,
                                )
                                .debug_selector(move || selector.clone())
                                .on_click(cx.listener(
                                    move |this, _, _, cx| this.choose_module(id.clone(), cx),
                                )),
                            )
                        })
                }
            }
            RowList::Scope => {
                let available = changed_available(cx);
                let scopes = [
                    (Scope::Project, "Project Files", "finder-scope-item-project"),
                    (
                        Scope::OpenFiles,
                        "Open Files",
                        "finder-scope-item-open-files",
                    ),
                    (
                        Scope::ChangedFiles,
                        "Changed Files",
                        "finder-scope-item-changed-files",
                    ),
                ];
                scopes
                    .into_iter()
                    .fold(menu, |menu, (scope, label, selector)| {
                        let option =
                            select_option(selector, label, self.scope == scope, false, theme)
                                .debug_selector(move || selector.to_owned());
                        let option = if scope == Scope::ChangedFiles && !available {
                            option.text_color(theme.text_muted).child(
                                div()
                                    .ml_auto()
                                    .text_size(px(TEXT_SMALL))
                                    .child("Not a Git repository"),
                            )
                        } else {
                            option.on_click(
                                cx.listener(move |this, _, _, cx| this.choose_scope(scope, cx)),
                            )
                        };
                        menu.child(option)
                    })
            }
        };
        Some(menu.into_any_element())
    }

    /// The button under the window that moves the places to the Find window. Replace has
    /// no such button: its Ctrl+Enter replaces everything.
    fn find_window_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let shortcut = keymap::shortcut_label("ctrl-enter", keymap::platform());
        let hover = theme.hover;
        div()
            .flex()
            .justify_end()
            .flex_none()
            .mt(px(SPACE_S))
            .child(
                div()
                    .id("finder-open-in-find-window")
                    .debug_selector(|| "finder-open-in-find-window".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.control_border)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child("Open in Find Window")
                    .child(div().text_color(theme.text_muted).child(shortcut))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_in_find_window(window, cx)),
                    ),
            )
    }

    fn input(
        &self,
        field: Field,
        value: &TextField,
        label: &str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.field = field;
                    if matches!(field, Field::Find) {
                        this.mask_popup = false;
                    }
                    cx.notify();
                }),
            )
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
                        .children(text_field_children_named(
                            FieldMouse::with_edit(
                                cx,
                                move |this: &mut Self| {
                                    Some(match field {
                                        Field::Find => &mut this.find,
                                        Field::Replace => &mut this.replacement,
                                    })
                                },
                                move |this: &mut Self, cx| {
                                    if matches!(field, Field::Find) {
                                        this.mask_popup = false;
                                        this.refresh(cx);
                                    }
                                },
                            ),
                            selector,
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
                let file_icon = self
                    .workspace
                    .upgrade()
                    .map_or("fileTypes/text", |workspace| {
                        workspace.read(cx).icons().icon_for(&hits.path)
                    });
                div()
                    .id(("find-row", at))
                    .debug_selector(move || format!("find-file-{at}"))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .pt(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .child(icon(file_icon, theme))
                            .child(title),
                    )
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
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.pick(at, event.click_count(), window, cx)
                    }))
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
        let first = DialogPlace {
            left: (f32::from(viewport.width) - WIDTH) / 2.,
            top: f32::from(viewport.height) * 0.12,
            width: WIDTH,
            height: Some(HEIGHT),
        };
        let rows = self.rows();
        let items: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(at, row)| self.row(at, *row, &theme, cx))
            .collect();
        let status = match (self.message.as_deref(), self.find.text.is_empty()) {
            (Some(message), _) => message.to_owned(),
            (None, true) => String::new(),
            (None, false) if self.searching => {
                if self.found_lines == 0 {
                    "Searching…".to_owned()
                } else {
                    format!(
                        "Searching… {} matches in {} files",
                        self.found_lines, self.found_files
                    )
                }
            }
            (None, false) if self.results.is_empty() => "Nothing found".to_owned(),
            (None, false) => {
                let lines: usize = self.results.iter().map(|file| file.lines.len()).sum();
                let more = if self.truncated { "+" } else { "" };
                format!("{lines}{more} matches in {} files", self.results.len())
            }
        };
        let status_bar = div()
            .debug_selector(|| "finder-status".to_owned())
            .flex_none()
            .h(px(22.))
            .px(px(ROW_PADDING_X))
            .text_color(theme.text_secondary)
            .child(status.clone());
        #[cfg(test)]
        let status_bar = status_bar.child(bounds_probe("finder-status-text", Some(status.into())));
        let heading = if self.replace_mode {
            "Replace in Files"
        } else {
            "Find in Files"
        };
        let pin = icon_button("finder-pin", "general/pin", "Pin window", None, &theme)
            .debug_selector(|| "finder-pin".to_owned())
            .when(self.pinned, |this| {
                this.bg(theme.tab_selected)
                    .border_1()
                    .border_color(theme.tab_selected_border)
            })
            // A press on the pin is not the start of a drag of the header.
            .on_mouse_down(
                MouseButton::Left,
                |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
                    cx.stop_propagation();
                },
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.pinned = !this.pinned;
                cx.notify();
            }));
        let header = popup_drag_header(
            cx,
            "finder",
            (MIN_WIDTH, MIN_HEIGHT),
            div()
                .flex()
                .items_center()
                .justify_between()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .px(px(ROW_PADDING_X))
                .debug_selector(|| "finder-title".to_owned())
                .child(heading)
                .child(pin),
        );
        let find_label = "Search for";
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

        let empty_hint = self.find.text.is_empty().then(|| {
            let hint = div()
                .debug_selector(|| "finder-empty".to_owned())
                .flex()
                .flex_1()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(SPACE_XS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child("Type to search in the project");
            #[cfg(test)]
            let hint = hint.child(bounds_probe(
                "finder-empty-hint",
                Some("Type to search in the project".into()),
            ));
            hint.child("Enter opens the selected match")
        });
        let list: AnyElement = match empty_hint {
            Some(hint) => hint.into_any_element(),
            None => scrollable(
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
            .min_h_0()
            .into_any_element(),
        };

        let body = surface(&theme)
            .id("finder")
            .debug_selector(|| "finder".to_owned())
            .key_context("FindInFiles ToolWindow")
            .track_focus(&self.focus)
            .occlude()
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_action(cx.listener(|this, _: &Previous, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Next, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SwitchField, _, cx| this.switch_field(cx)))
            .on_action(cx.listener(|this, _: &CtrlEnter, window, cx| {
                if this.replace_mode {
                    this.replace_all(cx);
                } else {
                    this.open_in_find_window(window, cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &Open, window, cx| this.open(this.selected, window, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if !this.pinned {
                    this.dismiss(window, cx);
                }
            }))
            .child(header)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_S))
                    .mb(px(POPUP_PADDING))
                    .child(self.input(Field::Find, &self.find, find_label, &theme, cx))
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
            .child(self.scope_row(&theme, cx))
            .children(self.scope_list(&theme, cx))
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
                            cx,
                        ))
                        .child(
                            div()
                                .id("replace-preserve-case")
                                .debug_selector(|| "replace-preserve-case".to_owned())
                                .flex()
                                .items_center()
                                .flex_none()
                                .h(px(26.))
                                .px(px(SPACE_M))
                                .rounded(px(CONTROL_RADIUS))
                                .border_1()
                                .border_color(theme.control_border)
                                .cursor_pointer()
                                .when(self.preserve_case, |this| {
                                    this.bg(theme.tab_selected)
                                        .border_color(theme.tab_selected_border)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.preserve_case = !this.preserve_case;
                                    cx.notify();
                                }))
                                .child("Preserve Case"),
                        )
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
            .child(status_bar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(
                        div()
                            .id("finder-splitter")
                            .debug_selector(|| "finder-splitter".to_owned())
                            .flex_none()
                            .w_full()
                            .h(px(SPACE_XS))
                            .bg(theme.border)
                            .cursor_row_resize()
                            .hover({
                                let hover = theme.hover;
                                move |style| style.bg(hover)
                            })
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.preview_drag = Some(f32::from(event.position.y));
                                    cx.stop_propagation();
                                }),
                            )
                            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                                this.drag_preview(event, cx)
                            }))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.finish_preview_drag();
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(|this, _, _, cx| {
                                    this.finish_preview_drag();
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(
                        div()
                            .id("finder-preview")
                            .debug_selector(|| "finder-preview".to_owned())
                            .flex_none()
                            .w_full()
                            .h(px(self.preview_height))
                            .overflow_hidden()
                            .child(self.preview.clone()),
                    ),
            )
            .when(!self.replace_mode, |this| {
                this.child(self.find_window_button(&theme, cx))
            });

        popup_window(
            PopupSpec {
                key: "finder",
                place: first,
                min: (MIN_WIDTH, MIN_HEIGHT),
                content_height: false,
                max_height: None,
            },
            body,
            window,
            cx,
        )
    }
}

/// What one search of Find in Files looks at. The window builds it from its scope; the Find
/// window keeps it, so that its rerun repeats the search on the files and the open tabs as
/// they are at that time.
#[derive(Clone)]
pub(crate) struct FindRequest {
    query: TextQuery,
    scope: Scope,
    directory: Option<PathBuf>,
    module: Option<String>,
    /// The file mask in force; empty when the mask is off.
    mask: String,
    in_selection: bool,
    /// The text of the selection of the active editor, and the file it is in.
    selection: Option<(PathBuf, String, Range<usize>)>,
    files: Entity<Arc<Vec<PathBuf>>>,
    workspace: WeakEntity<Workspace>,
}

impl FindRequest {
    /// The query without the spaces around it: the name of a tab of the Find window.
    pub(crate) fn pattern(&self) -> String {
        self.query.pattern.trim().to_owned()
    }

    /// The files the search reads now, the texts of the open tabs that replace the disk,
    /// and the selection when the search keeps to it.
    fn plan(&self, cx: &App) -> SearchPlan {
        let open = open_texts(&self.workspace, cx);
        let candidates: Vec<PathBuf> = if self.in_selection {
            Vec::new()
        } else {
            match self.scope {
                Scope::Project => self.files.read(cx).as_slice().to_vec(),
                Scope::Directory => {
                    let directory = self.directory.as_deref();
                    self.files
                        .read(cx)
                        .iter()
                        .filter(|path| {
                            directory.is_some_and(|directory| path.starts_with(directory))
                        })
                        .cloned()
                        .collect()
                }
                Scope::OpenFiles => open.keys().cloned().collect(),
                Scope::Module => {
                    let roots = environment_model(cx)
                        .and_then(|model| {
                            let id = chosen_module(self.module.as_deref(), &model)?;
                            model.module(&id).map(module_roots)
                        })
                        .unwrap_or_default();
                    self.files
                        .read(cx)
                        .iter()
                        .filter(|path| roots.iter().any(|root| path.starts_with(root)))
                        .cloned()
                        .collect()
                }
                Scope::ChangedFiles => {
                    let changed: HashSet<PathBuf> =
                        changed_paths(cx).unwrap_or_default().into_iter().collect();
                    self.files
                        .read(cx)
                        .iter()
                        .filter(|path| changed.contains(*path))
                        .cloned()
                        .collect()
                }
            }
        };
        let files = candidates
            .into_iter()
            .filter(|path| accepts_mask(&self.mask, path))
            .collect();
        SearchPlan {
            query: self.query.clone(),
            files,
            open,
            in_selection: self.in_selection,
            selection: self.selection.clone(),
        }
    }
}

/// The files one search reads, the texts of the open tabs that replace the disk, and the
/// selection when the search keeps to it.
struct SearchPlan {
    query: TextQuery,
    files: Vec<PathBuf>,
    open: HashMap<PathBuf, String>,
    in_selection: bool,
    selection: Option<(PathBuf, String, Range<usize>)>,
}

impl SearchPlan {
    /// The search on the calling thread. Every file with hits goes to `on_file` as it is
    /// found, and the files with hits come back with the answer.
    fn run(&self, cancel: &Cancel, on_file: &(dyn Fn(FileHits) + Sync)) -> Found {
        if self.in_selection && self.selection.is_none() {
            return (Ok(()), Vec::new());
        }
        if let Some((path, text, range)) = &self.selection {
            let found = match find_in_selection(&self.query, path, text, range.clone()) {
                Ok(hits) => vec![hits],
                Err(_) => Vec::new(),
            };
            for hits in &found {
                on_file(hits.clone());
            }
            return (Ok(()), found);
        }
        let found = Mutex::new(Vec::new());
        let collect = |hits: FileHits| {
            on_file(hits.clone());
            found
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(hits);
        };
        let result = find_in_files(&self.query, &self.files, &self.open, cancel, &collect);
        let found = found
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        (result, found)
    }
}

/// Repeats a search for a tab of the Find window. The search runs on the background
/// executor, and its places replace the tab's when they come.
pub(crate) fn repeat_search(
    hub: WeakEntity<UsagesHub>,
    serial: u64,
    request: &FindRequest,
    cx: &mut App,
) {
    let plan = request.plan(cx);
    let search = cx
        .background_executor()
        .spawn(async move { plan.run(&Cancel::new(), &|_: FileHits| {}) });
    cx.spawn(async move |cx| {
        let outcome = search.await;
        hub.update(cx, |hub, cx| hub.replace_find(serial, outcome, cx))
            .ok();
    })
    .detach();
}

/// The places of a search as the Find window lists them: every file with its count of
/// matching lines, and under it the lines that match, with their places.
pub(crate) fn find_tree(
    results: &[FileHits],
    root: &Path,
    icons: &FileIcons,
) -> (Vec<Item>, Vec<PanelRow>) {
    let mut items = Vec::new();
    let mut rows = Vec::new();
    for hits in results {
        let relative = hits.path.strip_prefix(root).unwrap_or(&hits.path);
        rows.push(PanelRow::Group {
            label: format!("{}  ({})", relative.display(), hits.lines.len()),
            depth: 0,
            icon: icons.icon_for(&hits.path),
        });
        for hit in &hits.lines {
            let column = hit
                .matches
                .first()
                .map_or(0, |found| hit.text[..found.start].chars().count());
            rows.push(PanelRow::Item {
                index: items.len(),
                depth: 1,
                excluded: false,
            });
            items.push(Item {
                path: hits.path.clone(),
                line: (hit.line as usize).saturating_sub(1),
                column,
                label: hit.text.trim().to_owned(),
                hint: format!("{}:{}", hit.line, column + 1),
                icon: "general/search",
            });
        }
    }
    (items, rows)
}

/// The text of every open tab that has an editor, by path: the unsaved text when the
/// editor has it.
fn open_texts(workspace: &WeakEntity<Workspace>, cx: &App) -> HashMap<PathBuf, String> {
    let Some(workspace) = workspace.upgrade() else {
        return HashMap::new();
    };
    let workspace = workspace.read(cx);
    workspace
        .open_tabs()
        .into_iter()
        .flatten()
        .filter_map(|path| {
            let view = workspace.view_of(&path)?.downcast::<EditorView>().ok()?;
            let text = view.read(cx).text()?;
            Some((path, text))
        })
        .collect()
}

/// The model of the project as Project Structure reads it, once the import has made it.
fn environment_model(cx: &App) -> Option<Arc<project_model::ProjectModel>> {
    cx.try_global::<ProjectEnvironmentGlobal>()?
        .0
        .as_ref()?
        .model()
}

/// The module the Module scope searches: the chosen one when the model has it, else the
/// first module of the model.
fn chosen_module(chosen: Option<&str>, model: &project_model::ProjectModel) -> Option<String> {
    chosen
        .filter(|id| model.module(id).is_some())
        .or_else(|| model.modules.first().map(|module| module.id.as_str()))
        .map(str::to_owned)
}

/// The modules of the project model, by id and name, in the order of the model.
fn modules_of(cx: &App) -> Vec<(String, String)> {
    environment_model(cx)
        .map(|model| {
            model
                .modules
                .iter()
                .map(|module| (module.id.clone(), module.name.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The folders a module searches: the source and the resource folders of all its units.
fn module_roots(module: &project_model::Module) -> Vec<PathBuf> {
    module
        .units
        .iter()
        .flat_map(|unit| unit.source_roots.iter().chain(&unit.resource_roots))
        .cloned()
        .collect()
}

/// The changed and the new files of the repository of the project that are on disk, as
/// Local Changes lists them. `None` when the project is not in a Git repository.
fn changed_paths(cx: &App) -> Option<Vec<PathBuf>> {
    let git = cx.try_global::<ActiveGit>()?.0.read(cx);
    let root = git.repo()?.root().to_path_buf();
    Some(
        git.status()
            .files
            .iter()
            .map(|file| root.join(&file.path))
            .filter(|path| path.is_file())
            .collect(),
    )
}

/// Whether the project is in a Git repository, so that Changed Files has a list.
fn changed_available(cx: &App) -> bool {
    cx.try_global::<ActiveGit>()
        .is_some_and(|git| git.0.read(cx).repo().is_some())
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
/// The name is the file name; a pattern with a slash matches the whole path. The patterns
/// come as IDEA writes them, separated by commas or spaces.
pub(crate) fn accepts_mask(mask: &str, path: &std::path::Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let full = path.to_string_lossy().replace('\\', "/");
    let mut positive = false;
    let mut kept = false;
    for pattern in mask.split(|character: char| character.is_whitespace() || character == ',') {
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
    fn a_mask_list_takes_commas_and_spaces() {
        let text = std::path::Path::new("Owner.txt");
        let other = std::path::Path::new("Owner.rst");
        assert!(accepts_mask("*.txt,*.md", text));
        assert!(!accepts_mask("*.txt,*.md", other));
        assert!(accepts_mask("*.txt, *.md", text));
        assert!(!accepts_mask("*.txt,!Owner.txt", text));
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
