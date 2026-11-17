//! The table of a localized set of `.properties` files, shown in the tab of one of
//! them: the keys on the left, the value of the selected key in every file on the
//! right. Edits go through the buffer of the file they belong to, so the usual undo,
//! the star of the tab and Save work as they do for text.
//!
//! The set and its files come from the language when it knows them; otherwise the
//! files are the `.properties` files of the folder that share the base name of the
//! open one.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use editor_view::{BundleHost, EditorView, Semantic};
use futures_util::StreamExt;
use lang_api::{DocumentSpot, LanguageSupport, RenameQuery, TextPoint, ask};
use text_core::properties::{self, Entry};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::outline::OpenEditors;

/// One file of the set with the text the table was built from.
#[derive(Clone)]
struct FileState {
    path: PathBuf,
    locale: Option<String>,
    /// Whether characters outside ASCII are written as `\uXXXX`.
    non_ascii: bool,
    text: String,
    entries: Vec<Entry>,
}

/// One key and its value in every file of the set.
#[derive(Clone)]
struct KeyState {
    key: String,
    values: Vec<Option<String>>,
    /// The locales whose non-empty file has no such key.
    missing: Vec<String>,
}

/// What has the keys of the table.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FocusOn {
    None,
    Filter,
    Field(usize),
}

/// The set each open file belongs to.
#[derive(Default)]
struct BundleSets(HashMap<PathBuf, Vec<(PathBuf, Option<String>)>>);

impl Global for BundleSets {}

/// The tables of the session, one per file they were opened from.
#[derive(Default)]
struct BundleViews(HashMap<PathBuf, Entity<BundleEditorView>>);

impl Global for BundleViews {}

/// Grows when a file of a set may have changed, so every open table reads it again.
pub(crate) struct BundleRevision(pub u64);

impl Global for BundleRevision {}

/// Puts the table of a set in place of the text of a file of it.
pub(crate) fn install(workspace: WeakEntity<Workspace>, cx: &mut App) {
    cx.set_global(BundleSets::default());
    cx.set_global(BundleViews::default());
    cx.set_global(BundleRevision(0));
    let build = workspace.clone();
    let changed = workspace;
    cx.set_global(BundleHost {
        available: Rc::new(move |path, cx| {
            cx.try_global::<BundleSets>()
                .is_some_and(|sets| sets.0.contains_key(path))
        }),
        build: Rc::new(move |path, _window, cx| bundle_view(path, build.clone(), cx)),
        changed: Rc::new(move |cx| {
            let workspace = changed.clone();
            cx.defer(move |cx| {
                workspace
                    .update(cx, |workspace, cx| workspace.tab_layout_changed(cx))
                    .ok();
            });
        }),
    });
}

/// Registers the set the file at `path` belongs to and returns whether it has one with
/// two or more files. The language answers first; the file names answer otherwise.
pub(crate) fn register(path: &Path, cx: &mut App) -> bool {
    let files = files_of(path, cx);
    if files.len() < 2 {
        return false;
    }
    cx.default_global::<BundleSets>()
        .0
        .insert(path.to_path_buf(), files);
    true
}

/// The table of `path`, opened once per file.
fn bundle_view(path: &Path, workspace: WeakEntity<Workspace>, cx: &mut App) -> Option<AnyElement> {
    let existing = cx
        .try_global::<BundleViews>()
        .and_then(|views| views.0.get(path).cloned());
    let view = match existing {
        Some(view) => view,
        None => {
            let view = cx.new(|cx| BundleEditorView::new(path.to_path_buf(), workspace, cx));
            cx.default_global::<BundleViews>()
                .0
                .insert(path.to_path_buf(), view.clone());
            view
        }
    };
    view.update(cx, |view, cx| view.refresh_if_needed(cx));
    Some(view.into_any_element())
}

/// Tells every table that a file of a set may have changed.
pub(crate) fn files_changed(cx: &mut App) {
    let next = cx
        .try_global::<BundleRevision>()
        .map_or(1, |revision| revision.0.wrapping_add(1));
    cx.set_global(BundleRevision(next));
}

/// The table of one file a test opened, if one was built.
#[cfg(test)]
pub(crate) fn view_of(path: &Path, cx: &App) -> Option<Entity<BundleEditorView>> {
    cx.try_global::<BundleViews>()?.0.get(path).cloned()
}

pub(crate) fn files_of(path: &Path, cx: &App) -> Vec<(PathBuf, Option<String>)> {
    if let Some(files) = support_for(path, cx).and_then(|support| support.bundle_files(path)) {
        return files
            .into_iter()
            .map(|file| (file.path, file.locale))
            .collect();
    }
    properties::bundle_files(path)
        .into_iter()
        .map(|file| (file.path, file.locale))
        .collect()
}

fn support_for(path: &Path, cx: &App) -> Option<Arc<dyn LanguageSupport>> {
    let main = cx.try_global::<Semantic>()?.0.clone();
    if main.matches(path) {
        return Some(main);
    }
    main.embedded_languages()
        .into_iter()
        .find(|language| language.matches(path))
}

/// The text of the buffer of `path`, when the file is open in a tab or in a buffer that no
/// tab shows, and whether its encoding is not UTF-8.
fn open_text(cx: &App, path: &Path) -> Option<(String, bool)> {
    let view = OpenEditors::get(cx, path)?;
    let view = view.read(cx);
    let text = view.text()?;
    Some((text, !view.encoding().starts_with("UTF-8")))
}

/// The text of a file on disk and whether its encoding is not UTF-8. Blocks, so it runs
/// off the UI thread.
fn read_disk(path: &Path) -> (String, bool) {
    match text_core::read_file(path) {
        Ok(decoded) => (
            decoded.text,
            !matches!(decoded.format.encoding, text_core::TextEncoding::Utf8),
        ),
        Err(_) => (String::new(), false),
    }
}

pub(crate) struct BundleEditorView {
    path: PathBuf,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    files: Vec<FileState>,
    keys: Vec<KeyState>,
    filter: TextField,
    selected: Option<usize>,
    fields: Vec<TextField>,
    focus_on: FocusOn,
    scroll_keys: ScrollHandle,
    scroll_fields: ScrollHandle,
    pending: bool,
    refreshing: bool,
    /// The key and file whose edits share one undo step.
    session: Option<(usize, usize)>,
    session_applied: bool,
    /// The value a file whose buffer is still loading takes once it is there.
    pending_writes: HashMap<usize, (String, String, bool)>,
    /// The files with a task that waits for their buffer.
    writing: std::collections::HashSet<usize>,
}

impl BundleEditorView {
    fn new(path: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        cx.observe_global::<BundleRevision>(|view, cx| {
            view.pending = true;
            cx.notify();
        })
        .detach();
        Self {
            path,
            workspace,
            focus: cx.focus_handle(),
            files: Vec::new(),
            keys: Vec::new(),
            filter: TextField::default(),
            selected: None,
            fields: Vec::new(),
            focus_on: FocusOn::None,
            scroll_keys: ScrollHandle::new(),
            scroll_fields: ScrollHandle::new(),
            pending: true,
            refreshing: false,
            session: None,
            session_applied: false,
            pending_writes: HashMap::new(),
            writing: std::collections::HashSet::new(),
        }
    }

    /// Reads the set again when a file of it may have changed. The files are read off the
    /// UI thread; a file with an open buffer is read from that buffer. The read waits for
    /// a moment of its own: the buffer of the tab may still be in the middle of a render.
    fn refresh_if_needed(&mut self, cx: &mut Context<Self>) {
        if !self.pending || self.refreshing {
            return;
        }
        self.pending = false;
        self.refreshing = true;
        let sets = self.sets(cx);
        let on_disk: Vec<PathBuf> = sets.iter().map(|(path, _)| path.clone()).collect();
        let job = cx.background_executor().spawn(async move {
            on_disk
                .into_iter()
                .map(|path| {
                    let (text, non_ascii) = read_disk(&path);
                    (path, text, non_ascii)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(0))
                .await;
            let read = job.await;
            this.update(cx, |view, cx| {
                view.refreshing = false;
                view.install(sets, read, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The set of the table: the files the language named, or the ones beside the open
    /// one.
    fn sets(&self, cx: &App) -> Vec<(PathBuf, Option<String>)> {
        cx.try_global::<BundleSets>()
            .and_then(|sets| sets.0.get(&self.path).cloned())
            .unwrap_or_default()
    }

    /// Makes the table of the text read from the files: the buffers of the open ones and
    /// the text read off the UI thread for the rest.
    fn install(
        &mut self,
        sets: Vec<(PathBuf, Option<String>)>,
        read: Vec<(PathBuf, String, bool)>,
        cx: &mut Context<Self>,
    ) {
        let mut on_disk: HashMap<PathBuf, (String, bool)> = read
            .into_iter()
            .map(|(path, text, non_ascii)| (path, (text, non_ascii)))
            .collect();
        let mut files = Vec::with_capacity(sets.len());
        for (path, locale) in sets {
            let (text, non_ascii) = open_text(cx, &path)
                .or_else(|| on_disk.remove(&path))
                .unwrap_or_default();
            let entries = properties::entries(&text);
            files.push(FileState {
                path,
                locale,
                non_ascii,
                text,
                entries,
            });
        }
        self.install_files(files, cx);
    }

    /// Shows `files` and releases the buffers of the files that left the set.
    fn install_files(&mut self, files: Vec<FileState>, cx: &mut Context<Self>) {
        let keep: std::collections::HashSet<PathBuf> =
            files.iter().map(|file| file.path.clone()).collect();
        let stale: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|file| !keep.contains(&file.path))
            .map(|file| file.path.clone())
            .collect();
        if !stale.is_empty()
            && let Some(workspace) = self.workspace.upgrade()
        {
            for path in stale {
                workspace.update(cx, |workspace, cx| workspace.release_buffer(&path, cx));
            }
        }
        let previous = self
            .selected
            .and_then(|index| self.keys.get(index))
            .map(|key| key.key.clone());
        let mut names = BTreeSet::new();
        for file in &files {
            for entry in &file.entries {
                names.insert(entry.key.clone());
            }
        }
        let keys = names
            .into_iter()
            .map(|key| {
                let values = files
                    .iter()
                    .map(|file| {
                        properties::entry(&file.entries, &key).map(|entry| entry.value.clone())
                    })
                    .collect();
                let missing = files
                    .iter()
                    .filter(|file| file.locale.is_some() && !file.entries.is_empty())
                    .filter(|file| properties::entry(&file.entries, &key).is_none())
                    .filter_map(|file| file.locale.clone())
                    .collect();
                KeyState {
                    key,
                    values,
                    missing,
                }
            })
            .collect::<Vec<_>>();
        self.files = files;
        self.keys = keys;
        let visible = self.visible_indices();
        self.selected = previous
            .and_then(|key| {
                visible
                    .iter()
                    .copied()
                    .find(|index| self.keys[*index].key == key)
            })
            .or_else(|| visible.first().copied());
        self.rebuild_fields();
    }

    fn visible_indices(&self) -> Vec<usize> {
        let needle = self.filter.text.trim().to_lowercase();
        self.keys
            .iter()
            .enumerate()
            .filter(|(_, key)| {
                if needle.is_empty() {
                    return true;
                }
                key.key.to_lowercase().contains(&needle)
                    || key
                        .values
                        .iter()
                        .flatten()
                        .any(|value| value.to_lowercase().contains(&needle))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn rebuild_fields(&mut self) {
        let preserve = match self.focus_on {
            FocusOn::Field(index) => self
                .fields
                .get(index)
                .map(|field| (index, field.text.clone(), field.caret())),
            _ => None,
        };
        self.fields = self
            .selected
            .and_then(|selected| self.keys.get(selected))
            .map(|key| {
                key.values
                    .iter()
                    .map(|value| TextField::from_text(value.clone().unwrap_or_default()))
                    .collect()
            })
            .unwrap_or_default();
        if let Some((index, text, caret)) = preserve
            && let Some(field) = self.fields.get_mut(index)
            && self.selected.is_some_and(|selected| {
                self.keys[selected]
                    .values
                    .get(index)
                    .is_some_and(|value| value.as_deref() == Some(text.as_str()))
            })
        {
            *field = TextField::from_text(text);
            field.set_caret(caret);
        }
    }

    fn select(&mut self, selected: Option<usize>, cx: &mut Context<Self>) {
        if self.selected == selected {
            return;
        }
        self.selected = selected;
        self.rebuild_fields();
        self.session = None;
        self.session_applied = false;
        cx.notify();
    }

    fn select_first_visible(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible_indices();
        let keep = self.selected.filter(|selected| visible.contains(selected));
        self.select(keep.or_else(|| visible.first().copied()), cx);
    }

    fn apply_field(
        &mut self,
        file: usize,
        value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(key) = self.keys.get(selected).map(|key| key.key.clone()) else {
            return;
        };
        let Some(state) = self.files.get(file) else {
            return;
        };
        let path = state.path.clone();
        let non_ascii = state.non_ascii;
        let text = state.text.clone();
        let has_key = properties::entry(&state.entries, &key).is_some();
        let edit = if has_key {
            properties::set_value(&text, &key, &value, non_ascii)
        } else {
            Some(properties::add_entry(&text, &key, &value, non_ascii))
        };
        let Some(edit) = edit else {
            return;
        };
        let merge = self.session == Some((selected, file)) && self.session_applied;
        self.session = Some((selected, file));
        self.session_applied = true;
        let mut updated = text;
        updated.replace_range(edit.range.clone(), &edit.text);
        if let Some(state) = self.files.get_mut(file) {
            state.text = updated;
            state.entries = properties::entries(&state.text);
        }
        if let Some(key) = self.keys.get_mut(selected) {
            key.values[file] = Some(value.clone());
        }
        // A buffer that is not open yet is opened after the key that started it: opening
        // a tab from inside the key would take the rest of the typed text away.
        if let Some(view) = OpenEditors::get(cx, &path)
            && view.read(cx).text().is_some()
        {
            view.update(cx, |view, cx| {
                view.apply_edits_merging(vec![edit], merge, cx)
            });
            return;
        }
        self.pending_writes.insert(file, (key, value, merge));
        if !self.writing.insert(file) {
            return;
        }
        let handle = window.window_handle();
        let workspace = self.workspace.clone();
        cx.spawn(async move |this, cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.open_buffer(path.clone(), window, cx)
                        })
                        .ok();
                })
                .ok();
            loop {
                let done = this
                    .update(cx, |this, cx| {
                        let Some((key, value, merge)) = this.pending_writes.get(&file).cloned()
                        else {
                            this.writing.remove(&file);
                            return true;
                        };
                        let Some(view) = OpenEditors::get(cx, &path) else {
                            return false;
                        };
                        let Some(text) = view.read(cx).text() else {
                            return false;
                        };
                        let non_ascii = this.files.get(file).is_some_and(|file| file.non_ascii);
                        let edit = if properties::entries(&text)
                            .iter()
                            .any(|entry| entry.key == key)
                        {
                            properties::set_value(&text, &key, &value, non_ascii)
                        } else {
                            Some(properties::add_entry(&text, &key, &value, non_ascii))
                        };
                        if let Some(edit) = edit {
                            view.update(cx, |view, cx| {
                                view.apply_edits_merging(vec![edit], merge, cx)
                            });
                        }
                        this.pending_writes.remove(&file);
                        this.writing.remove(&file);
                        true
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
        })
        .detach();
    }

    fn apply_to_file(
        &self,
        path: PathBuf,
        edit: text_core::Edit,
        merge: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.editor_for(&path, window, cx) else {
            return;
        };
        apply_when_loaded(view, vec![edit], merge, cx);
    }

    fn editor_for(
        &self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<EditorView>> {
        if let Some(view) = OpenEditors::get(cx, path) {
            return Some(view);
        }
        let workspace = self.workspace.upgrade()?;
        let view = workspace.update(cx, |workspace, cx| {
            workspace.open_buffer(path.to_path_buf(), window, cx)
        });
        view.downcast::<EditorView>().ok()
    }

    fn add_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let handle = window.window_handle();
        let view = cx.weak_entity();
        workspace.update(cx, |workspace, cx| {
            crate::prompt::Prompt::show(
                "Add Key",
                "The key is added to every file of the set",
                move |name, cx| {
                    let name = name.trim().to_owned();
                    if name.is_empty() {
                        return;
                    }
                    let view = view.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                view.update(cx, |view, cx| view.add_key_to_all(&name, window, cx))
                                    .ok();
                            })
                            .ok();
                    });
                },
                workspace,
                window,
                cx,
            );
        });
    }

    fn add_key_to_all(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut edits = Vec::with_capacity(self.files.len());
        for file in &self.files {
            let edit = properties::add_entry(&file.text, key, "", file.non_ascii);
            edits.push((file.path.clone(), edit));
        }
        for (path, edit) in edits {
            self.apply_to_file(path, edit, false, window, cx);
        }
        files_changed(cx);
        self.pending = true;
        cx.notify();
    }

    fn remove_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(key) = self.keys.get(selected).map(|key| key.key.clone()) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let handle = window.window_handle();
        let view = cx.weak_entity();
        workspace.update(cx, |workspace, cx| {
            crate::prompt::Prompt::ask(
                "Remove Key",
                &format!("Remove '{key}' from every file of the set?"),
                move |cx| {
                    let view = view.clone();
                    let key = key.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.remove_key_from_all(&key, window, cx)
                                })
                                .ok();
                            })
                            .ok();
                    });
                },
                workspace,
                window,
                cx,
            );
        });
    }

    fn remove_key_from_all(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let mut edits = Vec::new();
        for file in &self.files {
            if let Some(edit) = properties::remove_entry(&file.text, key) {
                edits.push((file.path.clone(), edit));
            }
        }
        for (path, edit) in edits {
            self.apply_to_file(path, edit, false, window, cx);
        }
        files_changed(cx);
        self.pending = true;
        cx.notify();
    }

    fn rename_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(key) = self.keys.get(selected).map(|key| key.key.clone()) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let handle = window.window_handle();
        let view = cx.weak_entity();
        let rename = key.clone();
        workspace.update(cx, |workspace, cx| {
            crate::prompt::Prompt::show_with(
                "Rename Key",
                &key,
                move |name, cx| {
                    let name = name.trim().to_owned();
                    if name.is_empty() {
                        return;
                    }
                    let view = view.clone();
                    let rename = rename.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.rename_key_everywhere(&rename, &name, window, cx)
                                })
                                .ok();
                            })
                            .ok();
                    });
                },
                workspace,
                window,
                cx,
            );
        });
    }

    fn rename_key_everywhere(
        &mut self,
        key: &str,
        new_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(support) = support_for(&self.path, cx) else {
            return;
        };
        let Some(provider) = support.rename() else {
            return;
        };
        let Some((path, line)) = self.spot_of(key) else {
            return;
        };
        let Ok(canonical) = ::platform::CanonPath::new(&path) else {
            return;
        };
        let query = RenameQuery {
            spot: DocumentSpot {
                uri: canonical.to_uri(),
                version: 1,
                point: TextPoint {
                    line: line as u32,
                    character: 0,
                },
            },
            new_name: new_name.to_owned(),
        };
        let mut replies = ask(provider, query);
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.spawn(async move |_, cx| {
            let Some(answer) = replies.next().await else {
                return;
            };
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::changes::apply(workspace, answer.value, window, cx);
                        })
                        .ok();
                    files_changed(cx);
                })
                .ok();
        })
        .detach();
    }

    /// The file and line of `key`, the base file first.
    fn spot_of(&self, key: &str) -> Option<(PathBuf, usize)> {
        let file = self
            .files
            .iter()
            .find(|file| file.locale.is_none() && properties::entry(&file.entries, key).is_some())
            .or_else(|| {
                self.files
                    .iter()
                    .find(|file| properties::entry(&file.entries, key).is_some())
            })?;
        let entry = properties::entry(&file.entries, key)?;
        Some((file.path.clone(), entry.line))
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        let primary = modifiers.control || modifiers.platform;
        if primary && !modifiers.alt && event.keystroke.key.eq_ignore_ascii_case("z") {
            self.undo(window, cx);
            return;
        }
        match self.focus_on {
            FocusOn::Filter => {
                if self.filter.key_with_clipboard(event, false, cx) {
                    self.select_first_visible(cx);
                    cx.notify();
                }
            }
            FocusOn::Field(index) => {
                let before = self.fields.get(index).map(|field| field.text.clone());
                if self
                    .fields
                    .get_mut(index)
                    .is_some_and(|field| field.key_with_clipboard(event, true, cx))
                    && before.as_deref() != self.fields.get(index).map(|field| field.text.as_str())
                {
                    let value = self.fields[index].text.clone();
                    self.apply_field(index, value, window, cx);
                    cx.notify();
                }
            }
            FocusOn::None => match event.keystroke.key.as_str() {
                "up" => self.select_step(-1, cx),
                "down" => self.select_step(1, cx),
                "enter" => self.select_first_visible(cx),
                _ => {}
            },
        }
    }

    fn undo(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let FocusOn::Field(index) = self.focus_on else {
            return;
        };
        let Some(state) = self.files.get(index) else {
            return;
        };
        let path = state.path.clone();
        let Some(view) = OpenEditors::get(cx, &path) else {
            return;
        };
        view.update(cx, |view, cx| view.undo(cx));
        self.session = None;
        self.session_applied = false;
        files_changed(cx);
        cx.notify();
    }

    fn select_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|selected| visible.iter().position(|index| *index == selected))
            .unwrap_or(0) as isize;
        let next = (at + step).rem_euclid(visible.len() as isize) as usize;
        self.select(Some(visible[next]), cx);
    }

    fn filter_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let focus = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            this.focus_on = FocusOn::Filter;
            window.focus(&this.focus, cx);
        };
        div()
            .id("bundle-filter-row")
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_1()
            .min_w_0()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .on_click(cx.listener(move |this, _, window, cx| focus(this, window, cx)))
            .child(icon("general/search", theme))
            .child(
                div()
                    .id("bundle-filter")
                    .debug_selector(|| "bundle-filter".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .on_click(cx.listener(move |this, _, window, cx| focus(this, window, cx)))
                    .children(text_field_children_named(
                        "bundle-filter",
                        &self.filter,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        self.focus_on == FocusOn::Filter,
                    )),
            )
    }

    fn key_row(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.keys[index];
        let selected = self.selected == Some(index);
        let key = row.key.clone();
        let missing = row.missing.clone();
        let hint = (!missing.is_empty())
            .then(|| format!("Missing translation for: {}", missing.join(", ")));
        let label = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .child(row.key.clone());
        let warning = hint.map(|hint| {
            let name = format!("bundle-missing-{}", row.key);
            div()
                .id(("bundle-missing", index))
                .debug_selector(move || name.clone())
                .flex_none()
                .child(icon_sized("status/warning", TEXT_SMALL, theme))
                .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
        });
        div()
            .id(("bundle-key", index))
            .debug_selector({
                let key = key.clone();
                move || format!("bundle-key-{key}")
            })
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(ROW_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.rename_key(window, cx);
                } else {
                    this.select(Some(index), cx);
                }
            }))
            .child(label)
            .children(warning)
            .into_any_element()
    }

    fn fields_area(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(selected) = self.selected else {
            return div().into_any_element();
        };
        let key = &self.keys[selected];
        let mut content = div()
            .flex()
            .flex_col()
            .w_full()
            .gap(px(SPACE_S))
            .p(px(SPACE_M));
        for (index, file) in self.files.iter().enumerate() {
            let header = match &file.locale {
                Some(locale) => locale.clone(),
                None => "Default".to_owned(),
            };
            let focused = self.focus_on == FocusOn::Field(index);
            let field = self.fields.get(index);
            let value = key.values.get(index).cloned().flatten();
            let lines = field.map(|field| {
                text_field_lines(
                    field,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    16.,
                    focused,
                )
            });
            content = content.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_secondary)
                            .child(header),
                    )
                    .child(
                        div()
                            .id(("bundle-value", index))
                            .debug_selector(move || format!("bundle-value-{index}"))
                            .flex()
                            .flex_col()
                            .min_h(px(CONTROL_HEIGHT))
                            .p(px(SPACE_XS))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(if focused { theme.accent } else { theme.border })
                            .bg(theme.editor_bg)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.focus_on = FocusOn::Field(index);
                                window.focus(&this.focus, cx);
                                cx.notify();
                            }))
                            .children(lines.unwrap_or_default())
                            .when(value.as_deref().is_some_and(str::is_empty), |this| {
                                // An empty value is a row of its own, so the field has height.
                                this.child(div().h(px(16.)))
                            }),
                    ),
            );
        }
        scrollable(
            "bundle-values",
            &self.scroll_fields,
            content,
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_w_0()
        .min_h_0()
        .into_any_element()
    }
}

/// Applies an edit to the buffer of a file, once the editor has read it.
fn apply_when_loaded(
    view: Entity<EditorView>,
    edits: Vec<text_core::Edit>,
    merge: bool,
    cx: &mut Context<BundleEditorView>,
) {
    if view.read(cx).text().is_some() {
        view.update(cx, |view, cx| view.apply_edits_merging(edits, merge, cx));
        return;
    }
    cx.spawn(async move |_, cx| {
        loop {
            if view.read_with(cx, |view, _| view.text().is_some()) {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
        }
        view.update(cx, |view, cx| view.apply_edits_merging(edits, merge, cx));
    })
    .detach();
}

impl Render for BundleEditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_if_needed(cx);
        let theme = *cx.global::<Theme>();
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .border_b_1()
            .border_color(theme.border)
            .child(
                icon_button("bundle-add-key", "general/add", "Add Key", None, &theme)
                    .debug_selector(|| "bundle-add-key".to_owned())
                    .on_click(cx.listener(|this, _, window, cx| this.add_key(window, cx))),
            )
            .child(
                icon_button(
                    "bundle-remove-key",
                    "general/minus",
                    "Remove Key",
                    None,
                    &theme,
                )
                .debug_selector(|| "bundle-remove-key".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.remove_key(window, cx))),
            )
            .child(self.filter_row(&theme, cx));
        let visible = self.visible_indices();
        let list = {
            let mut rows = div().flex().flex_col().w_full().p(px(SPACE_XS));
            for index in visible {
                rows = rows.child(self.key_row(index, &theme, cx));
            }
            if self.keys.is_empty() {
                rows = rows.child(
                    div()
                        .p(px(SPACE_S))
                        .text_color(theme.text_secondary)
                        .child("No keys"),
                );
            }
            scrollable("bundle-keys", &self.scroll_keys, rows, &theme, window, cx)
                .w(px(240.))
                .h_full()
                .flex_none()
        };
        let fields = self.fields_area(&theme, window, cx);
        div()
            .id("bundle-editor")
            .debug_selector(|| "bundle-editor".to_owned())
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_bg)
            .text_color(theme.text)
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(div().w(px(1.)).h_full().bg(theme.border))
                    .child(fields),
            )
    }
}

#[cfg(test)]
impl BundleEditorView {
    pub(crate) fn key_names(&self) -> Vec<String> {
        self.visible_indices()
            .into_iter()
            .map(|index| self.keys[index].key.clone())
            .collect()
    }

    pub(crate) fn selected_key(&self) -> Option<&str> {
        self.selected
            .and_then(|selected| self.keys.get(selected))
            .map(|key| key.key.as_str())
    }

    pub(crate) fn missing_of(&self, key: &str) -> Vec<String> {
        self.keys
            .iter()
            .find(|row| row.key == key)
            .map(|row| row.missing.clone())
            .unwrap_or_default()
    }

    pub(crate) fn locales(&self) -> Vec<Option<String>> {
        self.files.iter().map(|file| file.locale.clone()).collect()
    }
}
