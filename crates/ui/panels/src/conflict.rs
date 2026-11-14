//! The Conflicts window: the files git left in a merge or a rebase, with a side to take,
//! a way to open the markers and a button that ends the operation.

use std::path::PathBuf;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{ConflictSide, Operation};
use workspace::Workspace;

use crate::git::GitState;
use crate::prompt::TextField;

const WIDTH: f32 = 760.;

pub struct ConflictView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    message: TextField,
    scroll: ScrollHandle,
    _git: Subscription,
}

impl ConflictView {
    pub fn open(
        git: Entity<GitState>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let message = TextField::from_text(git.read(cx).merge_message().to_string());
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            git: git.clone(),
            workspace: weak,
            focus: cx.focus_handle(),
            message,
            scroll: ScrollHandle::new(),
            _git: cx.observe(&git, |_, _, cx| cx.notify()),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn button(
        id: (&'static str, usize),
        label: &'static str,
        primary: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        let (hover, pressed) = (theme.hover, theme.pressed);
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(22.))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if primary {
                theme.accent
            } else {
                theme.popup_border
            })
            .when(primary, |this| {
                this.bg(theme.accent).text_color(theme.over_accent)
            })
            .when(!primary, |this| {
                this.hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(pressed))
            })
            .child(label)
    }

    fn accept(&mut self, path: String, side: ConflictSide, cx: &mut Context<Self>) {
        self.git
            .update(cx, |git, cx| git.accept_side(path, side, cx));
    }

    fn resolve(&mut self, path: String, cx: &mut Context<Self>) {
        self.git.update(cx, |git, cx| git.mark_resolved(path, cx));
    }

    fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.open_file(path, window, cx))
            .ok();
    }

    /// Opens the three panes of a conflicted file: ours, the result and theirs.
    fn open_merge(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.git.read(cx).merge_available(path) {
            return;
        }
        let Some(absolute) = self.git.read(cx).absolute(path) else {
            return;
        };
        let git = self.git.clone();
        let workspace = self.workspace.clone();
        crate::merge_view::MergeView::open(git, workspace, absolute, window, cx);
    }

    fn abort(&mut self, cx: &mut Context<Self>) {
        self.git.update(cx, |git, cx| git.abort_operation(cx));
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let message = self.message.text.clone();
        self.git
            .update(cx, |git, cx| git.finish_operation(message, cx));
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.key == "enter" && key.modifiers.control {
            cx.stop_propagation();
            self.finish(cx);
        } else if key.key == "escape" {
            cx.propagate();
        } else if self.message.key_with_clipboard(event, true, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn row(
        &self,
        index: usize,
        path: String,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let open_path = self
            .git
            .read(cx)
            .absolute(&path)
            .unwrap_or_else(|| PathBuf::from(&path));
        let yours = path.clone();
        let theirs = path.clone();
        let resolve = path.clone();
        let merge = path.clone();
        let double = path.clone();
        let merge_available = self.git.read(cx).merge_available(&path);
        div()
            .id(("conflict-row", index))
            .debug_selector(move || format!("conflict-row-{index}"))
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.click_count() >= 2 {
                    this.open_merge(&double, window, cx);
                }
            }))
            .child(div().flex_1().min_w_0().overflow_hidden().child(path))
            .child(
                Self::button(("conflict-yours", index), "Accept Yours", false, theme)
                    .debug_selector(move || format!("conflict-yours-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.accept(yours.clone(), ConflictSide::Yours, cx)
                    })),
            )
            .child(
                Self::button(("conflict-theirs", index), "Accept Theirs", false, theme)
                    .debug_selector(move || format!("conflict-theirs-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.accept(theirs.clone(), ConflictSide::Theirs, cx)
                    })),
            )
            .children(merge_available.then(|| {
                Self::button(("conflict-merge", index), "Merge...", false, theme)
                    .debug_selector(move || format!("conflict-merge-{index}"))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_merge(&merge, window, cx)),
                    )
            }))
            .child(
                Self::button(("conflict-open", index), "Open", false, theme)
                    .debug_selector(move || format!("conflict-open-{index}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_file(open_path.clone(), window, cx)
                    })),
            )
            .child(
                Self::button(("conflict-resolve", index), "Mark Resolved", true, theme)
                    .debug_selector(move || format!("conflict-resolve-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.resolve(resolve.clone(), cx))),
            )
    }
}

impl Render for ConflictView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let (operation, conflicts, autostash) = {
            let git = self.git.read(cx);
            (git.operation(), git.conflicts(), git.autostash())
        };
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.6;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.16;
        let title = match operation {
            Operation::Merge => "Merge Conflicts",
            Operation::Rebase => "Rebase Conflicts",
            Operation::CherryPick => "Cherry-Pick Conflicts",
            Operation::Revert => "Revert Conflicts",
            Operation::None => "Conflicts",
        };
        let rows: Vec<_> = conflicts
            .iter()
            .enumerate()
            .map(|(index, path)| self.row(index, path.clone(), &theme, cx))
            .collect();
        let none = rows.is_empty();
        let message_empty = self.message.text.is_empty();
        let message_color = if message_empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let footer = match operation {
            Operation::Merge if none => div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .id("conflict-message")
                        .debug_selector(|| "conflict-message".to_owned())
                        .h(px(72.))
                        .p(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .border_1()
                        .border_color(theme.popup_border)
                        .overflow_hidden()
                        .flex()
                        .flex_col()
                        .text_color(message_color)
                        .children(text_field_lines_named(
                            "conflict-message-field",
                            &self.message,
                            theme.text,
                            theme.text,
                            theme.selection,
                            theme.caret,
                            18.,
                            true,
                        ))
                        .when(message_empty, |this| this.child("Merge message")),
                )
                .child(
                    Self::button(("conflict-commit", 0), "Commit Merge", true, &theme)
                        .debug_selector(|| "conflict-commit".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                )
                .into_any_element(),
            Operation::Rebase if none => div()
                .child(
                    Self::button(("conflict-continue", 0), "Continue Rebase", true, &theme)
                        .debug_selector(|| "conflict-continue".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                )
                .into_any_element(),
            Operation::CherryPick if none => div()
                .child(
                    Self::button(
                        ("conflict-continue", 0),
                        "Continue Cherry-Pick",
                        true,
                        &theme,
                    )
                    .debug_selector(|| "conflict-continue".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                )
                .into_any_element(),
            Operation::Revert if none => div()
                .child(
                    Self::button(("conflict-continue", 0), "Continue Revert", true, &theme)
                        .debug_selector(|| "conflict-continue".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.finish(cx))),
                )
                .into_any_element(),
            _ => div()
                .text_color(theme.text_secondary)
                .child(if none {
                    "No conflicts"
                } else {
                    "Take a side or edit a file, then mark it resolved"
                })
                .into_any_element(),
        };
        let abort = (operation != Operation::None).then(|| {
            Self::button(("conflict-abort", 0), "Abort", false, &theme)
                .debug_selector(|| "conflict-abort".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.abort(cx)))
        });
        let stash_note = autostash.then(|| {
            div()
                .id("conflict-autostash")
                .debug_selector(|| "conflict-autostash".to_owned())
                .text_color(theme.text_secondary)
                .child("Local changes will be restored when the operation ends.")
        });
        let body = surface(&theme)
            .id("conflicts")
            .debug_selector(|| "conflicts".to_owned())
            .key_context("Conflicts ToolWindow")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(height)
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(40.))
                    .px(px(DIALOG_PADDING))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title)
                    .child(
                        div()
                            .id("conflict-close")
                            .debug_selector(|| "conflict-close".to_owned())
                            .px(px(8.))
                            .rounded(px(CONTROL_RADIUS))
                            .hover({
                                let hover = theme.hover;
                                move |style| style.bg(hover)
                            })
                            .child("Close")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    ),
            )
            .child(
                scrollable(
                    "conflict-files",
                    &self.scroll,
                    div()
                        .id("conflict-files")
                        .flex()
                        .flex_col()
                        .px(px(8.))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(SPACE_M))
                    .p(px(SPACE_M))
                    .child(footer)
                    .children(stash_note)
                    .children(abort),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("conflicts-enter", body))
    }
}
