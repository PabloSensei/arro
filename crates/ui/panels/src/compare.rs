//! The Compare window of two branches: the commits only one side has, in two columns,
//! and the files they differ in. A file opens the diff against the branch of the left.

use std::path::PathBuf;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Commit, Compare};
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::git::GitState;

const WIDTH: f32 = 900.;
const HEIGHT: f32 = 620.;
/// Characters of a commit subject a row shows.
const SUBJECT: usize = 64;

pub struct CompareView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    left: String,
    right: String,
    only_left: Vec<Commit>,
    only_right: Vec<Commit>,
    files: Vec<String>,
    working_tree: bool,
    scroll_left: ScrollHandle,
    scroll_right: ScrollHandle,
    scroll_files: ScrollHandle,
}

impl CompareView {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open(
        git: Entity<GitState>,
        report: Compare,
        left: String,
        right: String,
        working_tree: bool,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            git,
            workspace: weak,
            focus: cx.focus_handle(),
            left,
            right,
            only_left: report.only_base,
            only_right: report.only_current,
            files: report.files,
            working_tree,
            scroll_left: ScrollHandle::new(),
            scroll_right: ScrollHandle::new(),
            scroll_files: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn only_left(&self) -> &[Commit] {
        &self.only_left
    }

    #[cfg(test)]
    pub(crate) fn only_right(&self) -> &[Commit] {
        &self.only_right
    }

    #[cfg(test)]
    pub(crate) fn files(&self) -> &[String] {
        &self.files
    }

    #[cfg(test)]
    pub(crate) fn left(&self) -> &str {
        &self.left
    }

    #[cfg(test)]
    pub(crate) fn right(&self) -> &str {
        &self.right
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn open_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.files.get(index) else {
            return;
        };
        let Some(path) = self.git.read(cx).absolute(path) else {
            return;
        };
        let files: Vec<PathBuf> = self
            .files
            .iter()
            .filter_map(|file| self.git.read(cx).absolute(file))
            .collect();
        let index = files.iter().position(|file| *file == path).unwrap_or(0);
        let branch = self.left.clone();
        self.workspace
            .update(cx, |_workspace, cx| {
                let git = self.git.clone();
                let weak = cx.weak_entity();
                DiffView::open_branch(git, weak, files, index, branch, window, cx);
            })
            .ok();
    }

    fn commit_rows(commits: &[Commit], side: &'static str, theme: &Theme) -> Vec<AnyElement> {
        commits
            .iter()
            .enumerate()
            .map(|(index, commit)| {
                let id = &commit.id[..commit.id.len().min(7)];
                let subject: String = commit.subject.chars().take(SUBJECT).collect();
                div()
                    .id((side, index))
                    .debug_selector(move || format!("compare-{side}-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.text_secondary)
                            .child(id.to_owned()),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(subject))
                    .into_any_element()
            })
            .collect()
    }

    fn file_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        (0..self.files.len())
            .map(|index| {
                let path = self.files[index].clone();
                let hover = theme.hover;
                div()
                    .id(("compare-file", index))
                    .debug_selector(move || format!("compare-file-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover(move |style| style.bg(hover))
                    .child(path)
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_file(index, window, cx)),
                    )
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for CompareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = px(HEIGHT).min(viewport.height - px(40.));
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - height) / 2.;
        let left_rows = Self::commit_rows(&self.only_left, "left", &theme);
        let right_rows = Self::commit_rows(&self.only_right, "right", &theme);
        let left_empty = left_rows.is_empty();
        let right_empty = right_rows.is_empty();
        let files = self.file_rows(&theme, cx);
        let files_empty = files.is_empty();
        let heading = if self.working_tree {
            format!("Diff of '{}' with the Working Tree", self.left)
        } else {
            format!("Compare '{}' with '{}'", self.left, self.right)
        };
        let column = |index: usize,
                      title: String,
                      rows: Vec<AnyElement>,
                      empty: bool,
                      scroll: &ScrollHandle,
                      window: &mut Window,
                      cx: &mut App| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(SPACE_S))
                .child(
                    div()
                        .flex_none()
                        .h(px(CONTROL_HEIGHT))
                        .px(px(ROW_PADDING_X))
                        .flex()
                        .items_center()
                        .text_color(theme.text_secondary)
                        .child(title),
                )
                .child(
                    scrollable(
                        ("compare-column", index),
                        scroll,
                        div()
                            .id(("compare-column", index))
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .track_scroll(scroll)
                            .children(rows)
                            .when(empty, |this| {
                                this.text_color(theme.text_muted).child("No commits")
                            }),
                        &theme,
                        window,
                        cx,
                    )
                    .flex_1()
                    .min_h_0(),
                )
        };
        let body = surface(&theme)
            .id("compare")
            .debug_selector(|| "compare".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(height)
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .flex()
                    .items_center()
                    .text_size(px(TEXT_TITLE))
                    .child(heading),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(SPACE_M))
                    .child(column(
                        0,
                        format!("Only in '{}'", self.left),
                        left_rows,
                        left_empty,
                        &self.scroll_left,
                        window,
                        cx,
                    ))
                    .child(column(
                        1,
                        format!("Only in '{}'", self.right),
                        right_rows,
                        right_empty,
                        &self.scroll_right,
                        window,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child(format!("{} file(s) differ", self.files.len())),
            )
            .child(
                scrollable(
                    "compare-files",
                    &self.scroll_files,
                    div()
                        .id("compare-files")
                        .debug_selector(|| "compare-files".to_owned())
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll_files)
                        .children(files)
                        .when(files_empty, |this| {
                            this.text_color(theme.text_muted).child("No files differ")
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    this.close(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)));
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("compare-enter", body))
    }
}
