//! The window of one hunk an agent changed: the text the lines had before, and Accept,
//! Reject and the keys between the hunks. The state is the one of the review panel: both
//! move the base of the change tracker, so an accept in one place shows in the other.

use std::path::PathBuf;
use std::sync::Arc;

use change_tracker::{ChangeTracker, FileReview};
use editor_view::EditorView;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::chat::text_button;

const WIDTH: f32 = 460.;
/// A hunk longer than this keeps the rest out of the window.
const MAX_LINES: usize = 12;
/// The room between the window and the changed lines.
const GAP: f32 = 6.;
/// The width of the number column of an editor, so the window starts at the text.
const GUTTER_ROOM: f32 = 64.;

pub(crate) const TAG: &str = "agent-hunk-popup";

pub struct AgentHunkPopup {
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    focus: FocusHandle,
    /// The project the changed files live in.
    root: PathBuf,
    /// Relative to the project, with forward slashes.
    path: String,
    editor: Entity<EditorView>,
    tracker: Arc<ChangeTracker>,
    review: FileReview,
    /// The hunk the window shows.
    index: usize,
    /// Why the last action left the file alone.
    notice: Option<SharedString>,
}

impl AgentHunkPopup {
    /// Shows the window of hunk `index` next to its lines.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        root: PathBuf,
        path: String,
        editor: Entity<EditorView>,
        tracker: Arc<ChangeTracker>,
        review: FileReview,
        index: usize,
    ) -> Option<Entity<Self>> {
        review.hunks.get(index)?;
        let weak = cx.weak_entity();
        let handle = window.window_handle();
        let view = cx.new(|cx| Self {
            workspace: weak,
            window: handle,
            focus: cx.focus_handle(),
            root,
            path,
            editor,
            tracker,
            review,
            index,
            notice: None,
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
        if target != self.index && target < self.review.hunks.len() {
            self.index = target;
            self.reveal(cx);
            cx.notify();
        }
    }

    /// Takes the new text as the base of the session, so the hunk leaves the review.
    pub(crate) fn accept(&mut self, cx: &mut Context<Self>) {
        if !self.text_is_current(cx) {
            self.notice = Some("The file changed, look at it again.".into());
            cx.notify();
            return;
        }
        let Some(base) = self.review.accepted_hunk(self.index) else {
            self.notice = Some("The hunk is gone, look again.".into());
            cx.notify();
            return;
        };
        self.tracker.accept(&self.path, base);
        self.close(cx);
    }

    /// Puts the old text of the hunk back on disk as one step the undo of the editor takes
    /// back, as the review panel does.
    pub(crate) fn reject(&mut self, cx: &mut Context<Self>) {
        if self.editor.read(cx).is_dirty() {
            self.notice =
                Some("Not reverted: the editor has unsaved edits. Save or undo them first.".into());
            cx.notify();
            return;
        }
        if !self.text_is_current(cx) {
            self.notice = Some("The file changed, look at it again.".into());
            cx.notify();
            return;
        }
        let Some(text) = self.review.reverted_hunk(self.index) else {
            self.notice = Some("The hunk is gone, look again.".into());
            cx.notify();
            return;
        };
        let root = self.root.clone();
        let path = self.path.clone();
        let job = cx
            .background_executor()
            .spawn(async move { crate::ai_review::write(&root, &path, text) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.tracker.refresh();
                    this.editor.update(cx, |view, cx| view.reload(cx));
                    this.close(cx);
                }
                Err(error) => {
                    this.notice = Some(format!("Not reverted: {error}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Whether the buffer still has the text the review was read from.
    fn text_is_current(&self, cx: &App) -> bool {
        self.editor.read(cx).text().as_deref() == self.review.current.as_deref()
    }

    fn reveal(&mut self, cx: &mut Context<Self>) {
        let Some(hunk) = self.review.hunks.get(self.index) else {
            return;
        };
        let line = hunk.after.start as usize;
        self.editor
            .update(cx, |editor, cx| editor.go_to_line(line, cx));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let window = self.window;
        window
            .update(cx, |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| workspace.close_menu(window, cx))
                    .ok();
            })
            .ok();
    }

    /// Where the window stands: below the changed lines when there is room, above them
    /// otherwise, so it never covers them.
    fn place(&self, viewport: Size<Pixels>, cx: &App) -> Point<Pixels> {
        let editor = self.editor.read(cx);
        let bounds = editor.editor_bounds();
        let line_height = editor.line_height().max(1.);
        let hunk = &self.review.hunks[self.index];
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
        let height = f32::from(self.height(cx));
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

    fn height(&self, cx: &App) -> Pixels {
        let line_height = self.editor.read(cx).line_height().max(1.);
        let lines = self.lines().len().clamp(1, MAX_LINES) as f32;
        px(lines * line_height + CONTROL_HEIGHT + 3. * SPACE_M)
    }

    /// The old text of the hunk; the new lines stand there when nothing stood before.
    fn lines(&self) -> Vec<String> {
        let Some(hunk) = self.review.hunks.get(self.index) else {
            return Vec::new();
        };
        if hunk.removed.is_empty() {
            hunk.added.clone()
        } else {
            hunk.removed.clone()
        }
    }
}

impl Render for AgentHunkPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(hunk) = self.review.hunks.get(self.index) else {
            return div().into_any_element();
        };
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let position = self.place(viewport, cx);
        let (label, color) = if hunk.before.is_empty() {
            ("Added lines", theme.success)
        } else if hunk.after.is_empty() {
            ("Removed lines", theme.close_hover)
        } else {
            ("Changed lines", theme.info)
        };
        let title = div()
            .debug_selector(|| "agent-hunk-title".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .child(div().w(px(3.)).h(px(14.)).rounded(px(1.)).bg(color))
            .child(label);
        let mut old = div().flex().flex_col().font_family(MONO_FONT);
        let removed = !hunk.removed.is_empty();
        for line in self.lines().into_iter().take(MAX_LINES) {
            old = old.child(
                div()
                    .text_color(if removed {
                        theme.close_hover
                    } else {
                        theme.string
                    })
                    .child(line),
            );
        }
        let notice = self.notice.clone().map(|notice| {
            div()
                .debug_selector(|| "agent-hunk-notice".to_owned())
                .text_color(theme.warning_mark)
                .child(notice)
        });
        let previous = icon_button(
            "agent-hunk-previous",
            "general/arrowLeft",
            "Previous Change",
            None,
            &theme,
        )
        .debug_selector(|| "agent-hunk-previous".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.step(false, cx)));
        let next = icon_button(
            "agent-hunk-next",
            "general/arrowRight",
            "Next Change",
            None,
            &theme,
        )
        .debug_selector(|| "agent-hunk-next".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.step(true, cx)));
        let accept = text_button("agent-hunk-accept", "Accept", &theme)
            .debug_selector(|| "agent-hunk-accept".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.accept(cx)));
        let reject = text_button("agent-hunk-reject", "Reject", &theme)
            .debug_selector(|| "agent-hunk-reject".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.reject(cx)));
        let buttons = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .child(previous)
            .child(next)
            .child(div().flex_1())
            .child(accept)
            .child(reject);
        let body = surface(&theme)
            .id("agent-hunk-popup")
            .debug_selector(|| "agent-hunk-popup".to_owned())
            .track_focus(&self.focus)
            .key_context("AgentHunkPopup")
            .occlude()
            .w(px(WIDTH))
            .p(px(SPACE_M))
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close(cx)))
            .child(title)
            .child(old)
            .children(notice)
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
