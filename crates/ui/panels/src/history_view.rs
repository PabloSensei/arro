//! The Local History window of a file or a folder: versions on the left, the chosen one
//! against the file now in two editors side by side, Revert for the whole file or one
//! hunk. The right editor is the file now and takes edits; an open editor takes a revert
//! as an edit, so Ctrl+Z brings the text back. A folder also lists its deleted files with
//! Restore.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use editor_view::{EditorEvent, EditorView};
use local_history::{Cause, LocalHistory, Revision};
use text_core::{decode, difference, encode};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Hunk, TextEdit, line_hunks, revert_hunk};
use workspace::Workspace;

use crate::diff_view::{decorations_with_visible, hunk_row_spans, rows_of};
use crate::editor_tab::{options_of, style_of};
use crate::history::{self, snapshot_bytes};
use crate::prompt::TextField;

const WIDTH: f32 = 1100.;
const STRIP_WIDTH: f32 = 48.;

/// The window that is open, for scripts.
struct ActiveHistory(WeakEntity<HistoryView>);

impl Global for ActiveHistory {}

/// Report lines of the open Local History window.
pub fn report(cx: &App) -> Vec<String> {
    let view = cx
        .try_global::<ActiveHistory>()
        .and_then(|active| active.0.upgrade());
    view.map(|view| view.read(cx).report()).unwrap_or_default()
}

/// Reverts the file to the version picked in the open window.
pub fn revert_shown(cx: &mut App) {
    let view = cx
        .try_global::<ActiveHistory>()
        .and_then(|active| active.0.upgrade());
    if let Some(view) = view {
        view.update(cx, |view, cx| view.revert_all(cx));
    }
}

/// The revision the keyboard of the open window moved to, for tests.
#[cfg(test)]
pub(crate) fn selected_index(cx: &App) -> Option<usize> {
    let view = cx
        .try_global::<ActiveHistory>()
        .and_then(|active| active.0.upgrade())?;
    view.read(cx).selected_index()
}

/// Whether the open window shows the version and the file side by side, for tests.
#[cfg(test)]
pub(crate) fn side_by_side(cx: &App) -> bool {
    cx.try_global::<ActiveHistory>()
        .and_then(|active| active.0.upgrade())
        .is_some_and(|view| view.read(cx).side_by_side())
}
const LIST_WIDTH: f32 = 340.;

/// A version next to the file as it is now.
struct Shown {
    revision: Revision,
    old: String,
    current: String,
    hunks: Vec<Hunk>,
}

impl Shown {
    /// The edit that takes the hunk back, if the text is still the one the diff was taken
    /// from. `None` means the text moved, so the hunk may sit elsewhere now.
    fn revert(&self, index: usize, text_now: &str) -> Option<TextEdit> {
        if text_now != self.current {
            return None;
        }
        Some(revert_hunk(
            &self.old,
            &self.current,
            self.hunks.get(index)?,
        ))
    }
}

pub struct HistoryView {
    workspace: WeakEntity<Workspace>,
    history: Arc<LocalHistory>,
    scope: PathBuf,
    root: PathBuf,
    folder: bool,
    focus: FocusHandle,
    revisions: Vec<Revision>,
    deleted: Vec<Revision>,
    selected: Option<usize>,
    shown: Option<Shown>,
    /// The version on the left, read only.
    left: Option<Entity<EditorView>>,
    /// The file now on the right, editable.
    right: Option<Entity<EditorView>>,
    /// The path the right editor shows.
    right_path: Option<PathBuf>,
    /// A scroll of one pane is being copied to the other.
    syncing: bool,
    message: Option<SharedString>,
    scroll_list: ScrollHandle,
    /// The search over the captions of the versions and the names of the labels.
    filter: TextField,
    /// Whether the caret of the search field is shown.
    filter_editing: bool,
    /// The list shows the labels alone.
    only_labels: bool,
}

fn editor_of(
    workspace: &WeakEntity<Workspace>,
    path: &Path,
    cx: &App,
) -> Option<Entity<EditorView>> {
    let view = workspace
        .read_with(cx, |workspace, _| workspace.view_of(path))
        .ok()??;
    view.downcast::<EditorView>().ok()
}

/// What the user sees of the file now: the open editor, or the disk.
fn current_text(workspace: &WeakEntity<Workspace>, path: &Path, cx: &App) -> String {
    match editor_of(workspace, path, cx).and_then(|editor| editor.read(cx).text()) {
        Some(text) => text,
        None => std::fs::read(path)
            .map(|bytes| decode(&bytes).text)
            .unwrap_or_default(),
    }
}

/// "5 min ago" and the like; no clock of the user's zone is needed for that.
pub(crate) fn age(at: SystemTime) -> String {
    let seconds = SystemTime::now()
        .duration_since(at)
        .map_or(0, |age| age.as_secs());
    match seconds {
        0..60 => "just now".to_string(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86400),
    }
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

/// The icon of an event of the history, or `None` when the icon sets have none for it.
/// The rows without an icon keep its place, so the captions stay in one column.
fn event_icon(revision: &Revision) -> Option<&'static str> {
    if revision.is_label() {
        return Some("general/star");
    }
    match revision.cause {
        Cause::Saved => Some("general/check"),
        Cause::Auto => Some("general/history"),
        Cause::BeforeGitCheckout | Cause::BeforeGitPull | Cause::BeforeGitRevert => {
            Some("general/vcs")
        }
        Cause::BeforeRefactoring => Some("codeInsight/intentionBulb"),
        Cause::BeforeOptimizeImports => Some("general/settings"),
        _ => None,
    }
}

/// The icon of an event, or the empty place of one when the event has none.
fn event_icon_cell(revision: &Revision, theme: &Theme) -> AnyElement {
    match event_icon(revision) {
        Some(name) => icon(name, theme).into_any_element(),
        None => div().flex_none().size(px(ICON)).into_any_element(),
    }
}

impl HistoryView {
    /// Reads the versions in the background and opens the window when they are there.
    pub fn show(
        scope: PathBuf,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let Some(history) = history::get(cx) else {
            workspace.notify_user("Local History", "The history is not open yet", cx);
            return;
        };
        let folder = scope.is_dir();
        let root = workspace.root().to_path_buf();
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            history,
            scope,
            root,
            folder,
            focus: cx.focus_handle(),
            revisions: Vec::new(),
            deleted: Vec::new(),
            selected: None,
            shown: None,
            left: None,
            right: None,
            right_path: None,
            syncing: false,
            message: None,
            scroll_list: ScrollHandle::new(),
            filter: TextField::default(),
            filter_editing: false,
            only_labels: false,
        });
        view.update(cx, |view, cx| view.reread(cx));
        cx.set_global(ActiveHistory(view.downgrade()));
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn reread(&mut self, cx: &mut Context<Self>) {
        let (history, scope, folder) = (self.history.clone(), self.scope.clone(), self.folder);
        let job = cx.background_executor().spawn(async move {
            if folder {
                (
                    history.folder_history(&scope),
                    history.deleted_under(&scope),
                )
            } else {
                (history.file_history(&scope), Vec::new())
            }
        });
        cx.spawn(async move |this, cx| {
            let (revisions, deleted) = job.await;
            this.update(cx, |this, cx| {
                this.revisions = revisions;
                this.deleted = deleted;
                let visible = this.visible();
                let first = visible
                    .iter()
                    .copied()
                    .find(|index| this.revisions[*index].content.is_some());
                let chosen = this
                    .selected
                    .filter(|index| visible.contains(index))
                    .or(first);
                match chosen {
                    Some(index) => this.select(index, cx),
                    None => {
                        this.selected = None;
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The versions the search and the switch keep, in the order of the list.
    fn visible(&self) -> Vec<usize> {
        let query = self.filter.text.trim().to_lowercase();
        self.revisions
            .iter()
            .enumerate()
            .filter(|(_, revision)| {
                if self.only_labels && !revision.is_label() {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                let text = format!(
                    "{} {}",
                    self.revision_title(revision),
                    revision.cause.title()
                );
                text.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// A change of the filter keeps or moves the chosen version, so that the right pane
    /// shows something the list still holds.
    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        let visible = self.visible();
        if let Some(selected) = self.selected
            && visible.contains(&selected)
        {
            cx.notify();
            return;
        }
        self.selected = None;
        match visible
            .iter()
            .copied()
            .find(|index| self.revisions[*index].content.is_some())
        {
            Some(index) => self.select(index, cx),
            None => {
                self.shown = None;
                cx.notify();
            }
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(revision) = self.revisions.get(index).cloned() else {
            return;
        };
        self.selected = Some(index);
        if revision.content.is_none() {
            self.shown = None;
            cx.notify();
            return;
        }
        let current = current_text(&self.workspace, &revision.path, cx);
        let history = self.history.clone();
        let job = cx.background_executor().spawn(async move {
            let bytes = history.content(&revision)?;
            let old = decode(&bytes).text;
            let hunks = line_hunks(&old, &current);
            std::io::Result::Ok(Shown {
                revision,
                old,
                current,
                hunks,
            })
        });
        cx.spawn(async move |this, cx| {
            let shown = job.await;
            this.update(cx, |this, cx| {
                match shown {
                    Ok(shown) => {
                        this.open_editors(&shown, cx);
                        this.shown = Some(shown);
                    }
                    Err(error) => {
                        this.message = Some(format!("The version is not readable: {error}").into())
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The editors of the two panes: the version on the left, the file now on the right.
    /// The right editor of one file stays as the versions of that file are picked.
    fn open_editors(&mut self, shown: &Shown, cx: &mut Context<Self>) {
        let path = shown.revision.path.clone();
        let style = style_of(cx);
        let options = options_of(cx);
        let left = {
            let path = path.clone();
            let old = shown.old.clone();
            let style = style.clone();
            let options = options.clone();
            cx.new(move |cx| {
                let mut view = EditorView::open_text(path, old, true, style, cx);
                view.set_options(options, cx);
                view.lock_soft_wrap();
                view
            })
        };
        let tab = editor_of(&self.workspace, &path, cx);
        let right = match (&tab, self.right_path.as_ref() == Some(&path)) {
            (Some(tab), _) => tab.clone(),
            (None, true) => self.right.clone().unwrap_or_else(|| {
                let current = shown.current.clone();
                let style = style.clone();
                let path = path.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, current, false, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }),
            (None, false) => {
                let current = shown.current.clone();
                let style = style.clone();
                let path = path.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, current, false, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }
        };
        // A new version stands on the rows the file now shows.
        let top = right.read(cx).scroll_top();
        left.update(cx, |left, cx| left.scroll_to_top(top, cx));
        self.left = Some(left);
        self.right = Some(right);
        self.right_path = Some(path);
        self.subscribe_editors(cx);
    }

    fn subscribe_editors(&mut self, cx: &mut Context<Self>) {
        if let Some(left) = &self.left {
            cx.subscribe(left, |this, _, event: &EditorEvent, cx| {
                if let EditorEvent::Scrolled(top) = event {
                    this.scrolled(false, *top, cx);
                }
            })
            .detach();
        }
        if let Some(right) = &self.right {
            cx.subscribe(right, |this, _, event: &EditorEvent, cx| {
                if let EditorEvent::Scrolled(top) = event {
                    this.scrolled(true, *top, cx);
                }
            })
            .detach();
        }
    }

    /// One pane scrolled: the other one follows, so the hunks stay face to face.
    fn scrolled(&mut self, from_right: bool, top: f32, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        let other = if from_right {
            self.left.as_ref()
        } else {
            self.right.as_ref()
        };
        let Some(other) = other else {
            return;
        };
        if (other.read(cx).scroll_top() - top).abs() <= 0.5 {
            return;
        }
        self.syncing = true;
        other.update(cx, |other, cx| other.scroll_to_top(top, cx));
        self.syncing = false;
        cx.notify();
    }

    /// Puts `text` in place of the file. An open editor takes it as one edit; a closed file
    /// is written in its own encoding and line ends.
    fn put_text(&mut self, path: &Path, current: &str, text: &str, cx: &mut Context<Self>) {
        let tab = editor_of(&self.workspace, path, cx);
        let editor = tab.clone().or_else(|| {
            self.right
                .clone()
                .filter(|right| right.read(cx).path() == path)
        });
        match editor {
            Some(editor) => {
                snapshot_bytes(path, editor.read(cx).file_bytes(), Cause::BeforeRevert, cx);
                if let Some(edit) = difference(current, text) {
                    editor.update(cx, |editor, cx| editor.apply_edits(vec![edit], cx));
                }
                if tab.is_some() {
                    self.workspace
                        .update(cx, |workspace, cx| workspace.save_editor(path, cx))
                        .ok();
                } else {
                    let error = editor.update(cx, |editor, cx| editor.save_blocking(cx));
                    if let Some(error) = error {
                        self.message = Some(format!("Not written: {error}").into());
                    }
                }
            }
            None => {
                let decoded = std::fs::read(path).map(|bytes| decode(&bytes));
                let format = match decoded {
                    Ok(decoded) if !decoded.lossless => {
                        // Writing the text back would change bytes the decoding could not keep.
                        let text = "The file has bytes that are not text: open it to revert";
                        self.message = Some(text.into());
                        cx.notify();
                        return;
                    }
                    Ok(decoded) => decoded.format,
                    Err(_) => Default::default(),
                };
                let (path, bytes) = (path.to_path_buf(), encode(text, &format));
                // The snapshot reads the old bytes before the write, in the same job.
                let history = self.history.clone();
                let job = cx.background_executor().spawn(async move {
                    history.record_file(&path, Cause::BeforeRevert)?;
                    std::fs::write(&path, bytes)
                });
                cx.spawn(async move |this, cx| {
                    let result = job.await;
                    this.update(cx, |this, cx| {
                        if let Err(error) = result {
                            this.message = Some(format!("Not written: {error}").into());
                        }
                        this.refresh_selected(cx);
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    fn revert_all(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let (path, current, old) = (
            shown.revision.path.clone(),
            shown.current.clone(),
            shown.old.clone(),
        );
        self.put_text(&path, &current, &old, cx);
        self.refresh_selected(cx);
    }

    fn revert_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let path = shown.revision.path.clone();
        let current = shown.current.clone();
        let text_now = current_text(&self.workspace, &path, cx);
        let next = shown.revert(index, &text_now).map(|edit| {
            let mut next = current.clone();
            next.replace_range(edit.range, &edit.text);
            next
        });
        let Some(next) = next else {
            // The text moved while the window was open: the old hunk may land in the wrong
            // place, so read the diff again and say why nothing was taken back.
            self.message = Some("The text changed, so the diff was read again".into());
            self.refresh_selected(cx);
            return;
        };
        self.put_text(&path, &current, &next, cx);
        self.refresh_selected(cx);
    }

    /// The diff again after a revert, once the editor or the disk has the new text.
    fn refresh_selected(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.select(index, cx);
        }
    }

    fn restore(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(revision) = self.deleted.get(index).cloned() else {
            return;
        };
        let history = self.history.clone();
        let job = cx
            .background_executor()
            .spawn(async move { history.restore(&revision) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.message = Some(format!("Not restored: {error}").into());
                }
                this.reread(cx);
            })
            .ok();
        })
        .detach();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// The revision the keyboard moved to, for tests.
    #[cfg(test)]
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Whether the version and the file now stand side by side, for tests.
    #[cfg(test)]
    pub(crate) fn side_by_side(&self) -> bool {
        self.left.is_some() && self.right.is_some()
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if self.filter_editing {
            if key.key == "escape" {
                if self.filter.text.is_empty() {
                    self.close(window, cx);
                } else {
                    self.filter = TextField::default();
                    self.refresh_visible(cx);
                }
                cx.stop_propagation();
                return;
            }
            if self.filter.key_with_clipboard(event, false, cx) {
                self.refresh_visible(cx);
            }
            cx.stop_propagation();
            return;
        }
        if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
            if let Some(revision) = self.selected.and_then(|index| self.revisions.get(index)) {
                let text = self.revision_title(revision);
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        // The keyboard walks the versions the filter keeps; a label is a separator, not a
        // row the diff can show.
        let visible: Vec<usize> = self
            .visible()
            .into_iter()
            .filter(|index| self.revisions[*index].content.is_some())
            .collect();
        if visible.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|selected| visible.iter().position(|index| *index == selected));
        let last = visible.len() as isize - 1;
        let next = match key.key.as_str() {
            "up" => Some(match at {
                Some(index) => (index as isize - 1).clamp(0, last) as usize,
                None => last as usize,
            }),
            "down" => Some(match at {
                Some(index) => (index as isize + 1).clamp(0, last) as usize,
                None => 0,
            }),
            _ => None,
        };
        if let Some(index) = next {
            self.select(visible[index], cx);
            cx.stop_propagation();
        }
    }

    fn button(id: impl Into<ElementId>, label: &'static str, theme: &Theme) -> Stateful<Div> {
        let pressed = theme.pressed;
        div()
            .id(id)
            .flex()
            .items_center()
            .h(px(22.))
            .px(px(8.))
            .rounded(px(CONTROL_RADIUS))
            .text_color(theme.text)
            .hover(move |style| style.bg(pressed))
            .child(label)
    }

    fn revision_title(&self, revision: &Revision) -> String {
        let what = match (&revision.label, revision.content.is_some()) {
            (Some(label), _) => format!("Label: {label}"),
            (None, true) => revision.cause.title().to_string(),
            (None, false) => "Deleted".to_string(),
        };
        if self.folder && !revision.is_label() {
            format!("{what} · {}", relative(&self.scope, &revision.path))
        } else {
            what
        }
    }

    /// The search over the captions and the labels, and the switch that keeps the labels
    /// alone, as the other search fields of the window work.
    fn filter_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let empty = self.filter.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let labels = self.only_labels;
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(DIALOG_PADDING))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("history-filter")
                    .debug_selector(|| "history-filter".to_owned())
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(if self.filter_editing {
                        theme.accent
                    } else {
                        theme.border
                    })
                    .text_color(color)
                    .children(text_field_children_named(
                        FieldMouse::with_edit(
                            cx,
                            |this| Some(&mut this.filter),
                            |this, cx| this.refresh_visible(cx),
                        ),
                        "history-filter-field",
                        &self.filter,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        self.filter_editing,
                    ))
                    .when(empty, |this| this.child("Search history"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter_editing = true;
                        cx.notify();
                    })),
            )
            .child(
                Self::button("history-only-labels", "Labels", theme)
                    .debug_selector(|| "history-only-labels".to_owned())
                    .when(labels, |this| {
                        this.bg(theme.selection).text_color(theme.accent)
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.only_labels = !this.only_labels;
                        this.refresh_visible(cx);
                    })),
            )
    }

    fn list(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mut rows: Vec<AnyElement> = Vec::new();
        if !self.deleted.is_empty() {
            rows.push(
                div()
                    .px(px(SPACE_M))
                    .pt(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Deleted files")
                    .into_any_element(),
            );
            for (index, revision) in self.deleted.iter().enumerate() {
                let file_type = self
                    .workspace
                    .read_with(cx, |workspace, _| {
                        workspace.icons().icon_for(&revision.path)
                    })
                    .unwrap_or("fileTypes/text");
                let row = div()
                    .id(("history-deleted-row", index))
                    .debug_selector(move || format!("history-deleted-row-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(ROW_HEIGHT))
                    .px(px(SPACE_M))
                    .child(icon(file_type, theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(relative(&self.scope, &revision.path)),
                    )
                    .child(
                        Self::button(("history-restore", index), "Restore", theme)
                            .on_click(cx.listener(move |this, _, _, cx| this.restore(index, cx))),
                    );
                rows.push(row.into_any_element());
            }
            rows.push(
                div()
                    .h(px(1.))
                    .my(px(4.))
                    .bg(theme.border)
                    .into_any_element(),
            );
        }
        for index in self.visible() {
            let Some(revision) = self.revisions.get(index) else {
                continue;
            };
            if revision.is_label() {
                // A label is a separator over the versions that follow it.
                let name = revision.label.clone().unwrap_or_default();
                let row = div()
                    .id(("history-label", index))
                    .debug_selector(move || format!("history-label-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(ROW_HEIGHT))
                    .px(px(SPACE_M))
                    .child(event_icon_cell(revision, theme))
                    .child(div().text_color(theme.text_secondary).child(name))
                    .child(div().flex_1().h(px(1.)).bg(theme.border));
                rows.push(row.into_any_element());
                continue;
            }
            let selected = self.selected == Some(index);
            let hover = theme.selection_hover;
            let row = div()
                .id(("history-row", index))
                .debug_selector(move || format!("history-row-{index}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(ROW_HEIGHT + SPACE_L))
                .px(px(SPACE_M))
                .when(selected, |row| row.bg(theme.selection_inactive))
                .when(!selected, |row| row.hover(move |style| style.bg(hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
                .child(event_icon_cell(revision, theme))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .child(div().truncate().child(self.revision_title(revision)))
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(age(revision.at)),
                        ),
                );
            rows.push(row.into_any_element());
        }
        if self.revisions.is_empty() {
            rows.push(
                div()
                    .p(px(SPACE_L))
                    .text_color(theme.text_secondary)
                    .child("No history yet: versions appear on save")
                    .into_any_element(),
            );
        } else if rows.is_empty() {
            rows.push(
                div()
                    .p(px(SPACE_L))
                    .text_color(theme.text_secondary)
                    .child("No version matches the search")
                    .into_any_element(),
            );
        }
        scrollable(
            "history-list",
            &self.scroll_list,
            div()
                .id("history-list")
                .flex()
                .flex_col()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_list)
                .children(rows),
            theme,
            window,
            cx,
        )
        .flex_none()
        .w(px(LIST_WIDTH))
        .h_full()
        .border_r_1()
        .border_color(theme.border)
    }

    fn diff(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let Some(shown) = &self.shown else {
            return div()
                .id("history-diff")
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .p(px(SPACE_L))
                .text_color(theme.text_secondary)
                .child("Pick a version to compare with the file now");
        };
        let (Some(left), Some(right)) = (self.left.clone(), self.right.clone()) else {
            return div()
                .id("history-diff")
                .flex()
                .flex_1()
                .min_w_0()
                .h_full()
                .p(px(SPACE_L))
                .text_color(theme.text_secondary)
                .child("The version is not readable");
        };
        let rows = rows_of(&shown.old, &shown.current, &shown.hunks);
        let selected = self.selected.unwrap_or(0);
        let left_visible = left.read(cx).shown_lines();
        let right_visible = right.read(cx).shown_lines();
        let (left_decor, right_decor) = decorations_with_visible(
            &rows,
            &shown.old,
            &shown.current,
            selected,
            theme,
            &left_visible,
            &right_visible,
        );
        let left_pane = left.update(cx, |editor, cx| editor.pane(left_decor, None, window, cx));
        let right_pane = right.update(cx, |editor, cx| editor.pane(right_decor, None, window, cx));
        let line_height = right.read(cx).line_height().max(1.);
        let top = right.read(cx).scroll_top();
        let mut strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h_full()
            .overflow_hidden();
        for (index, span) in hunk_row_spans(&rows, shown.hunks.len())
            .into_iter()
            .enumerate()
        {
            let Some((first, last)) = span else {
                continue;
            };
            let y = first as f32 * line_height - top;
            let height = (last - first + 1) as f32 * line_height;
            strip = strip.child(
                div()
                    .absolute()
                    .left(px((STRIP_WIDTH - 4.) / 2.))
                    .top(px(y))
                    .w(px(4.))
                    .h(px(height))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(theme.text_secondary)
                    .opacity(0.5),
            );
            let revert = div()
                .id(("history-revert-hunk", index))
                .debug_selector(move || format!("history-revert-hunk-{index}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(18.))
                .h(px(18.))
                .rounded(px(CONTROL_RADIUS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text)
                .hover({
                    let pressed = theme.pressed;
                    move |style| style.bg(pressed)
                })
                .tooltip(|_, cx| tooltip("Revert this change", None, cx))
                .child(">>")
                .on_click(cx.listener(move |this, _, _, cx| this.revert_hunk(index, cx)));
            strip = strip.child(div().absolute().left(px(SPACE_XS)).top(px(y)).child(revert));
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .text_color(theme.text_secondary)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(self.revision_title(&shown.revision)),
            )
            .child(div().flex_1().min_w_0().truncate().child("Now"));
        div()
            .id("history-diff")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("history-diff-side")
                    .debug_selector(|| "history-diff-side".to_owned())
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .debug_selector(|| "history-pane-left".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(left_pane),
                    )
                    .child(strip)
                    .child(
                        div()
                            .debug_selector(|| "history-pane-right".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(right_pane),
                    ),
            )
    }

    /// A text picture of the window, for scripts.
    pub fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for revision in &self.deleted {
            lines.push(format!(
                "history-deleted {}",
                relative(&self.root, &revision.path)
            ));
        }
        for index in self.visible() {
            if let Some(revision) = self.revisions.get(index) {
                lines.push(format!("history-version {}", self.revision_title(revision)));
            }
        }
        if let Some(shown) = &self.shown {
            lines.push(format!("history-diff hunks {}", shown.hunks.len()));
        }
        lines
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.75;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.1;
        let scope = relative(&self.root, &self.scope);
        let can_revert = self
            .shown
            .as_ref()
            .is_some_and(|shown| !shown.hunks.is_empty());
        let workspace = self.workspace.clone();
        let hide = {
            let workspace = workspace.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }
        };
        let options = {
            let workspace = self.workspace.clone();
            move |event: &ClickEvent, _window: &mut Window, cx: &mut App| {
                let hiding = workspace.clone();
                let entries = vec![MenuEntry::new("Hide", move |window, cx| {
                    hiding
                        .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                        .ok();
                })];
                let position = event.position();
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_menu(position, entries, cx)
                    })
                    .ok();
            }
        };
        let header = tool_header(
            div()
                .font_weight(FontWeight::MEDIUM)
                .debug_selector(|| "history-scope".to_owned())
                .child(scope),
            div().when(can_revert, |this| {
                this.child(
                    Self::button("history-revert", "Revert", &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.revert_all(cx))),
                )
            }),
            "history-window",
            "history",
            options,
            hide,
            &theme,
        )
        .border_b_1()
        .border_color(theme.border)
        .debug_selector(|| "history-header".to_owned());
        let message = self.message.clone().map(|message| {
            div()
                .px(px(DIALOG_PADDING))
                .py(px(4.))
                .text_color(theme.close_hover)
                .child(message)
        });
        let shell = surface(&theme)
            .id("history")
            .track_focus(&self.focus)
            .group("history-window")
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(header)
            .child(self.filter_row(&theme, cx))
            .children(message)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.list(&theme, window, cx))
                    .child(self.diff(&theme, window, cx)),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "history",
                title: "Local History".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (480., 320.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use local_history::{Cause, Revision};

    use super::Shown;

    fn shown(base: &str, current: &str) -> Shown {
        Shown {
            revision: Revision {
                id: 0,
                at: SystemTime::UNIX_EPOCH,
                path: "File.txt".into(),
                cause: Cause::Saved,
                content: None,
                label: None,
            },
            old: base.into(),
            current: current.into(),
            hunks: vcs::line_hunks(base, current),
        }
    }

    fn apply(mut text: String, edit: vcs::TextEdit) -> String {
        text.replace_range(edit.range, &edit.text);
        text
    }

    #[test]
    fn a_hunk_is_taken_back_only_over_the_text_the_diff_was_taken_from() {
        let base = "a\nb\nc\n";
        let current = "a\nB\nc\n";
        let shown = shown(base, current);

        let edit = shown.revert(0, current).unwrap();
        assert_eq!(apply(current.to_string(), edit), base);
    }

    #[test]
    fn a_changed_text_gives_no_edit_and_the_caller_rereads_the_diff() {
        let base = "a\nb\nc\n";
        let current = "a\nB\nc\n";
        let shown = shown(base, current);

        assert_eq!(shown.revert(0, "a\nB\nC\n"), None);
        assert_eq!(shown.revert(7, current), None);
    }
}
