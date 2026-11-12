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

use crate::diff_view::{decorations_of, hunk_row_spans, rows_of};
use crate::editor_tab::{options_of, style_of};
use crate::history::{self, snapshot_bytes};

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
                let first = this
                    .revisions
                    .iter()
                    .position(|revision| revision.content.is_some());
                match this.selected.or(first) {
                    Some(index) => this.select(index, cx),
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
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

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
            if let Some(revision) = self.selected.and_then(|index| self.revisions.get(index)) {
                let text = self.revision_title(revision);
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            cx.stop_propagation();
            return;
        }
        let count = self.revisions.len();
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        let next = match key.key.as_str() {
            "up" => Some(match self.selected {
                Some(index) => (index as isize - 1).clamp(0, last),
                None => last,
            }),
            "down" => Some(match self.selected {
                Some(index) => (index as isize + 1).clamp(0, last),
                None => 0,
            }),
            _ => None,
        };
        if let Some(index) = next {
            self.select(index as usize, cx);
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
                let row = div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(ROW_HEIGHT))
                    .px(px(SPACE_M))
                    .child(
                        div()
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
        for (index, revision) in self.revisions.iter().enumerate() {
            let selected = self.selected == Some(index);
            let hover = theme.selection_hover;
            let row = div()
                .id(("history-row", index))
                .flex()
                .flex_col()
                .justify_center()
                .h(px(ROW_HEIGHT + SPACE_L))
                .px(px(SPACE_M))
                .when(selected, |row| row.bg(theme.selection_inactive))
                .when(!selected, |row| row.hover(move |style| style.bg(hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
                .child(div().truncate().child(self.revision_title(revision)))
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(age(revision.at)),
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
        let (left_decor, right_decor) =
            decorations_of(&rows, &shown.old, &shown.current, selected, theme);
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
        for revision in &self.revisions {
            lines.push(format!("history-version {}", self.revision_title(revision)));
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
        let title = format!("Local History: {}", relative(&self.root, &self.scope));
        let can_revert = self
            .shown
            .as_ref()
            .is_some_and(|shown| !shown.hunks.is_empty());
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(40.))
            .px(px(DIALOG_PADDING))
            .border_b_1()
            .border_color(theme.border)
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .when(can_revert, |header| {
                header.child(
                    Self::button("history-revert", "Revert", &theme)
                        .on_click(cx.listener(|this, _, _, cx| this.revert_all(cx))),
                )
            });
        let message = self.message.clone().map(|message| {
            div()
                .px(px(DIALOG_PADDING))
                .py(px(4.))
                .text_color(theme.close_hover)
                .child(message)
        });
        let body = surface(&theme)
            .id("history")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(height)
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(header)
            .children(message)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.list(&theme, window, cx))
                    .child(self.diff(&theme, window, cx)),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("history-enter", body))
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
