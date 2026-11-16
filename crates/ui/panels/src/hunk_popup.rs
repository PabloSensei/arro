//! The window of one change: the text the lines had before the last commit, shown when
//! the user comes to a change mark in the gutter of an editor. The words that differ are
//! marked inside the changed lines, and the buttons take the change back, open the diff
//! window or copy the old text.

use std::path::{Path, PathBuf};

use editor_view::{EditorOptions, EditorStyle, EditorView, PaneDecor};
use keymap::workspace as keys;
use text_core::FileFormat;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::Hunk;
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::editor_tab::{options_of, style_of};
use crate::git::{GitState, Kind, hunk_index_at};

/// The name the popup is shown with, so that a late close of the hover finds only its
/// own window.
pub(crate) const TAG: &str = "change-popup";

const WIDTH: f32 = 560.;
/// A hunk longer than this keeps the rest behind the bar of its own editor.
const MAX_LINES: usize = 12;
/// The room between the popup and the changed lines.
const GAP: f32 = 6.;
/// The width of the number column of an editor, so that the window starts at the text.
const GUTTER_ROOM: f32 = 64.;

/// The keys the buttons name in their hints, taken from the window keymap.
#[derive(Clone, Default)]
struct Shortcuts {
    previous: Option<SharedString>,
    next: Option<SharedString>,
    rollback: Option<SharedString>,
}

pub struct HunkPopup {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: PathBuf,
    /// The editor of the tab, for the place of the hunk on the screen.
    editor: Entity<EditorView>,
    /// The file as the last commit has it and the text the hunks were taken from.
    base: String,
    current: String,
    hunks: Vec<Hunk>,
    /// The hunk the window shows.
    index: usize,
    /// The old text of the hunk, read only and highlighted by the language of the file.
    old: Entity<EditorView>,
    /// The same text plain, for Copy and for a test.
    old_text: String,
    lines: usize,
    decor: PaneDecor,
    shortcuts: Shortcuts,
}

impl HunkPopup {
    /// Shows the window of the hunk that holds `index`, next to the changed lines.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        git: Entity<GitState>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        path: PathBuf,
        editor: Entity<EditorView>,
        base: String,
        current: String,
        hunks: Vec<Hunk>,
        index: usize,
    ) -> Option<Entity<Self>> {
        let hunk = hunks.get(index)?;
        let style = style_of(cx);
        let options = options_of(cx);
        let (old, old_text, lines, decor) =
            hunk_editor(&path, &base, &current, hunk, style, options, cx);
        let shortcuts = Shortcuts {
            previous: workspace.shortcut_of(keys::PREVIOUS_CHANGE).map(Into::into),
            next: workspace.shortcut_of(keys::NEXT_CHANGE).map(Into::into),
            rollback: workspace
                .shortcut_of(keys::ROLLBACK_CHANGED_LINES)
                .map(Into::into),
        };
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            git,
            workspace: weak,
            focus: cx.focus_handle(),
            path,
            editor,
            base,
            current,
            hunks,
            index,
            old,
            old_text,
            lines,
            decor,
            shortcuts,
        });
        workspace.show_popup_tagged(view.clone().into(), Some(TAG), window, cx);
        Some(view)
    }

    /// Takes the window to the next or the previous hunk, and the editor with it.
    pub(crate) fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let target = if forward {
            self.index + 1
        } else {
            self.index.saturating_sub(1)
        };
        if target != self.index && target < self.hunks.len() {
            self.show_index(target, cx);
        }
    }

    /// Puts the old text of the hunk back into the buffer as one undoable edit. The
    /// editor keeps the change unsaved, as Rollback Lines of IDEA does.
    pub(crate) fn rollback(&mut self, cx: &mut Context<Self>) {
        let Some(hunk) = self.hunks.get(self.index).cloned() else {
            return;
        };
        let Some(text_now) = self.editor.read(cx).text() else {
            return;
        };
        // The text moved on while the window was open: the edit would land wrong.
        if text_now != self.current {
            return;
        }
        let edit = vcs::revert_hunk(&self.base, &self.current, &hunk);
        let bytes = self.editor.read(cx).file_bytes();
        crate::history::snapshot_bytes(
            &self.path,
            bytes,
            local_history::Cause::BeforeGitRevert,
            cx,
        );
        self.current = apply_edit(&self.current, &edit);
        self.editor.update(cx, |view, cx| {
            view.apply_edits(vec![text_core::Edit::new(edit.range, edit.text)], cx)
        });
        self.hunks = vcs::line_hunks(&self.base, &self.current);
        match hunk_index_at(&self.hunks, hunk.after.start) {
            Some(index) => self.show_index(index, cx),
            None => {
                self.git.update(cx, |git, cx| git.close_popup(cx));
            }
        };
    }

    /// Show Diff: the diff window of the file, at the hunk of this window.
    pub(crate) fn show_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let git = self.git.clone();
        let workspace = self.workspace.clone();
        let path = self.path.clone();
        let line = self.hunks[self.index].after.start;
        DiffView::open_at(git, workspace, path, line, window, cx);
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.old_text.clone()));
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let git = self.git.clone();
        workspace
            .update(cx, |workspace, cx| workspace.close_menu(window, cx))
            .ok();
        git.update(cx, |git, cx| git.popup_closed(cx));
    }

    fn hover(&mut self, hovered: bool, cx: &mut Context<Self>) {
        self.git
            .update(cx, |git, cx| git.popup_hovered(hovered, cx));
    }

    /// Replaces the old text with the one of hunk `index` and brings the editor to it.
    fn show_index(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(hunk) = self.hunks.get(index).cloned() else {
            return;
        };
        let style = style_of(cx);
        let options = options_of(cx);
        let (old, old_text, lines, decor) = hunk_editor(
            &self.path,
            &self.base,
            &self.current,
            &hunk,
            style,
            options,
            cx,
        );
        self.index = index;
        self.old = old;
        self.old_text = old_text;
        self.lines = lines;
        self.decor = decor;
        let line = hunk.after.start as usize;
        self.editor
            .update(cx, |editor, cx| editor.go_to_line(line, cx));
        cx.notify();
    }

    /// Where the window stands: below the changed lines when there is room, above them
    /// otherwise, so it never covers them.
    fn place(&self, viewport: Size<Pixels>, height: Pixels, cx: &App) -> Point<Pixels> {
        let editor = self.editor.read(cx);
        let bounds = editor.editor_bounds();
        let line_height = editor.line_height().max(1.);
        let hunk = &self.hunks[self.index];
        let first = hunk.after.start as usize;
        let last = hunk.after.end.saturating_sub(1).max(hunk.after.start) as usize;
        let visible = editor.shown_lines();
        let in_hunk = |line: &usize| *line >= first && *line <= last;
        let shown = |line: &usize| {
            editor
                .line_bounds(*line)
                .map(|place| (f32::from(place.origin.y), f32::from(place.size.height)))
        };
        let first_visible = visible.iter().find(|line| in_hunk(line)).and_then(shown);
        let last_visible = visible
            .iter()
            .rev()
            .find(|line| in_hunk(line))
            .and_then(shown);
        let (top, bottom) = match (first_visible, last_visible) {
            (Some((top, _)), Some((bottom, row))) => (top, bottom + row),
            _ => {
                let top = bounds.map_or(0., |bounds| f32::from(bounds.origin.y));
                (top, top + line_height)
            }
        };
        let viewport_height = f32::from(viewport.height);
        let viewport_width = f32::from(viewport.width);
        let margin = f32::from(px(SPACE_M));
        let height = f32::from(height);
        let below = bottom + GAP;
        let above = top - GAP - height;
        let y = if below + height <= viewport_height - margin {
            below
        } else if above >= margin {
            above
        } else {
            (viewport_height - height - margin).max(margin)
        };
        let left = bounds.map_or(margin, |bounds| {
            f32::from(bounds.origin.x) + GUTTER_ROOM + 16.
        });
        let x = left.min(viewport_width - WIDTH - margin).max(margin);
        point(px(x), px(y))
    }

    /// The height the content of the window asks for.
    fn height(&self, cx: &App) -> Pixels {
        let line_height = self.old.read(cx).line_height().max(1.);
        let lines = self.lines.clamp(1, MAX_LINES) as f32;
        px(lines * line_height + 2. * CONTROL_HEIGHT + 2. * SPACE_M + 2. * SPACE_XS + 4.)
    }

    /// The old text the window shows, for a test.
    #[cfg(test)]
    pub(crate) fn old_text(&self) -> &str {
        &self.old_text
    }

    /// Whether the window shows the changes of `path`.
    pub(crate) fn is_for(&self, path: &Path) -> bool {
        self.path == path
    }

    #[cfg(test)]
    pub(crate) fn hunk_index(&self) -> usize {
        self.index
    }

    #[cfg(test)]
    pub(crate) fn hunk_count(&self) -> usize {
        self.hunks.len()
    }
}

/// The old text of `hunk` with the words that differ marked, as the editor of the
/// window shows it.
#[allow(clippy::too_many_arguments)]
fn hunk_editor(
    path: &Path,
    base: &str,
    current: &str,
    hunk: &Hunk,
    style: EditorStyle,
    options: EditorOptions,
    cx: &mut App,
) -> (Entity<EditorView>, String, usize, PaneDecor) {
    let theme = *cx.global::<Theme>();
    let before: Vec<&str> = base.split('\n').collect();
    let after: Vec<&str> = current.split('\n').collect();
    let before_from = (hunk.before.start as usize).min(before.len());
    let before_to = (hunk.before.end as usize)
        .min(before.len())
        .max(before_from);
    let after_from = (hunk.after.start as usize).min(after.len());
    let after_to = (hunk.after.end as usize).min(after.len()).max(after_from);
    let mut decor = PaneDecor::default();
    let mut lines: Vec<String> = before[before_from..before_to]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    if hunk.before.is_empty() {
        // Nothing stood here before: the added lines take the place of the old text.
        lines = after[after_from..after_to]
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        let tint = Kind::Added.color(&theme).opacity(0.18);
        for line in 0..lines.len() {
            decor.lines.push((line, tint));
        }
    } else {
        let color = if hunk.after.is_empty() {
            Kind::Deleted.color(&theme)
        } else {
            Kind::Modified.color(&theme)
        };
        let tint = color.opacity(0.18);
        for line in 0..lines.len() {
            decor.lines.push((line, tint));
        }
        if !hunk.after.is_empty() {
            let pair_count = lines.len().min(after_to - after_from);
            let starts = line_starts(&lines);
            for offset in 0..pair_count {
                let old = &lines[offset];
                let new = after[after_from + offset];
                if old == new {
                    continue;
                }
                let changes = vcs::word_changes(old, new);
                let start = starts[offset];
                for range in changes.before {
                    decor.ranges.push((
                        start + range.start..start + range.end,
                        Kind::Conflicted.color(&theme).opacity(0.35),
                    ));
                }
            }
        }
    }
    let text = lines.join("\n");
    let lines_count = lines.len();
    let shape = text.clone();
    let path = path.to_path_buf();
    let old = cx.new(move |cx| {
        let mut view = EditorView::open_text_document(
            path,
            shape,
            FileFormat::default(),
            true,
            false,
            style,
            cx,
        );
        view.set_options(options, cx);
        view.lock_soft_wrap();
        view
    });
    (old, text, lines_count, decor)
}

/// `text` with the edit applied.
fn apply_edit(text: &str, edit: &vcs::TextEdit) -> String {
    let mut text = text.to_owned();
    text.replace_range(edit.range.clone(), &edit.text);
    text
}

/// The byte offset every line of `lines` starts at.
fn line_starts(lines: &[String]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(lines.len());
    let mut offset = 0;
    for line in lines {
        starts.push(offset);
        offset += line.len() + 1;
    }
    starts
}

impl Render for HunkPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(hunk) = self.hunks.get(self.index) else {
            return div().into_any_element();
        };
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let height = self.height(cx);
        let position = self.place(viewport, height, cx);
        let hunk = hunk.clone();
        let (label, color) = if hunk.before.is_empty() {
            ("Added lines", Kind::Added.color(&theme))
        } else if hunk.after.is_empty() {
            ("Deleted lines", Kind::Deleted.color(&theme))
        } else {
            ("Changed lines", Kind::Modified.color(&theme))
        };
        let title = div()
            .debug_selector(|| "hunk-popup-title".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .child(div().w(px(3.)).h(px(14.)).rounded(px(1.)).bg(color))
            .child(label.to_owned());
        let text_height =
            px(self.lines.clamp(1, MAX_LINES) as f32 * self.old.read(cx).line_height().max(1.));
        let pane = self.old.update(cx, |editor, cx| {
            editor.pane(self.decor.clone(), None, window, cx)
        });
        let text = div()
            .debug_selector(|| "hunk-popup-lines".to_owned())
            .w_full()
            .h(text_height)
            .flex_none()
            .overflow_hidden()
            .child(pane);
        let previous = icon_button(
            "hunk-popup-previous",
            "general/arrowLeft",
            "Previous Change",
            self.shortcuts.previous.clone(),
            &theme,
        )
        .debug_selector(|| "hunk-popup-previous".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.step(false, cx)));
        let next = icon_button(
            "hunk-popup-next",
            "general/arrowRight",
            "Next Change",
            self.shortcuts.next.clone(),
            &theme,
        )
        .debug_selector(|| "hunk-popup-next".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.step(true, cx)));
        let rollback = icon_button(
            "hunk-popup-rollback",
            "general/trash",
            "Rollback Lines",
            self.shortcuts.rollback.clone(),
            &theme,
        )
        .debug_selector(|| "hunk-popup-rollback".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.rollback(cx)));
        let show_diff = icon_button(
            "hunk-popup-diff",
            "general/openInToolWindow",
            "Show Diff",
            None,
            &theme,
        )
        .debug_selector(|| "hunk-popup-diff".to_owned())
        .on_click(cx.listener(|this, _, window, cx| this.show_diff(window, cx)));
        let copy = icon_button(
            "hunk-popup-copy",
            "general/copy",
            "Copy Old Text",
            None,
            &theme,
        )
        .debug_selector(|| "hunk-popup-copy".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.copy(cx)));
        let buttons = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .child(previous)
            .child(next)
            .child(rollback)
            .child(show_diff)
            .child(copy);
        let body = surface(&theme)
            .id("hunk-popup")
            .debug_selector(|| "hunk-popup".to_owned())
            .track_focus(&self.focus)
            .key_context("HunkPopup")
            .occlude()
            .w(px(WIDTH))
            .p(px(SPACE_M))
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .on_hover(cx.listener(|this, hovered, _, cx| this.hover(*hovered, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(title)
            .child(text)
            .child(buttons);
        div()
            .absolute()
            .size_full()
            .child(
                deferred(
                    anchored()
                        .position(position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(body),
                )
                .with_priority(1),
            )
            .into_any_element()
    }
}
