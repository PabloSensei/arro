//! The Build tool window: the tree of the errors and warnings of the last build with the
//! raw output beside it. A double click or Enter on a message opens the file at its line
//! and column; the buttons run the build again, stop it, hide the warnings or open and
//! close the tree.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use editor_view::Spot;
use tool_gateway::{BuildProblem, ProblemSeverity};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{BUILD_TOOL, Workspace};

use crate::navigation;
use crate::run::{BuildView, RunPanel};

const GROUP: &str = "bottom-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// The part of the window the tree takes; the output fills the rest.
const TREE_WIDTH: f32 = 360.;

/// The buttons of the window and the hint each of them shows. The tests read the same
/// table the render does.
pub(crate) const TOOLTIPS: &[(&str, &str)] = &[
    ("build-output-restart", "Restart Build"),
    ("build-output-stop", "Stop Build"),
    ("build-output-errors-only", "Show Errors Only"),
    ("build-output-expand-all", "Expand All"),
    ("build-output-collapse-all", "Collapse All"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowKind {
    Root,
    File(PathBuf),
    /// One problem of the build with its index in the list of the view.
    Problem(PathBuf, usize),
}

/// One line of the visible tree.
#[derive(Clone, Debug)]
struct Row {
    kind: RowKind,
    depth: usize,
    label: String,
    hint: Option<String>,
    icon: &'static str,
    expandable: bool,
    open: bool,
}

pub struct BuildOutputPanel {
    run: Entity<RunPanel>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    selected: Option<usize>,
    only_errors: bool,
    root_open: bool,
    /// The files the user closed; everything is open until then.
    closed_files: HashSet<PathBuf>,
    scroll: ScrollHandle,
    output_scroll: UniformListScrollHandle,
    /// The start of the build the window shows, so a new one resets the tree.
    seen: Option<Instant>,
    /// The first error of that build was selected already.
    finished_seen: bool,
    _run: Subscription,
}

/// The Build window of the project, for the tests of the window.
#[cfg(test)]
pub struct ActiveBuildOutput(pub Entity<BuildOutputPanel>);

#[cfg(test)]
impl Global for ActiveBuildOutput {}

impl BuildOutputPanel {
    pub fn create(
        run: Entity<RunPanel>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let _run = cx.observe(&run, |panel: &mut Self, run, cx| {
                let (started, finished) = {
                    let view = run.read(cx).build_view();
                    (view.started, view.finished)
                };
                if started != panel.seen {
                    panel.seen = started;
                    panel.finished_seen = false;
                    panel.root_open = true;
                    panel.closed_files.clear();
                    panel.selected = None;
                }
                if finished && !panel.finished_seen {
                    panel.finished_seen = true;
                    panel.select_first(cx);
                }
                cx.notify();
            });
            let mut panel = Self {
                run,
                workspace,
                focus: cx.focus_handle(),
                selected: None,
                only_errors: false,
                root_open: true,
                closed_files: HashSet::new(),
                scroll: ScrollHandle::new(),
                output_scroll: UniformListScrollHandle::new(),
                seen: None,
                finished_seen: false,
                _run,
            };
            panel.select_first(cx);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The lines of the visible tree, for tests.
    #[cfg(test)]
    pub(crate) fn labels(&self, cx: &App) -> Vec<String> {
        self.rows(cx)
            .into_iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.label))
            .collect()
    }

    /// The grey marks of the visible tree, next to the row that has them, for tests.
    #[cfg(all(test, windows))]
    pub(crate) fn hints(&self, cx: &App) -> Vec<(String, String)> {
        self.rows(cx)
            .into_iter()
            .filter_map(|row| row.hint.map(|hint| (row.label, hint)))
            .collect()
    }

    /// The problems of the last build the tree shows, for tests.
    #[cfg(all(test, windows))]
    pub(crate) fn problems(&self, cx: &App) -> Vec<BuildProblem> {
        self.run.read(cx).build_view().problems.clone()
    }

    /// Selects a line as a click does, for tests.
    #[cfg(all(test, windows))]
    pub(crate) fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.rows(cx).len() {
            self.selected = Some(index);
            cx.notify();
        }
    }

    #[cfg(all(test, windows))]
    pub(crate) fn only_errors(&self) -> bool {
        self.only_errors
    }

    #[cfg(all(test, windows))]
    pub(crate) fn restart_now(&mut self, cx: &mut Context<Self>) {
        self.run.update(cx, |run, cx| run.restart_build(cx));
    }

    #[cfg(all(test, windows))]
    pub(crate) fn stop_now(&mut self, cx: &mut Context<Self>) {
        self.run.update(cx, |run, cx| run.stop_build(cx));
    }

    /// The rows of the tree, in the order they are drawn.
    fn rows(&self, cx: &App) -> Vec<Row> {
        let view = self.run.read(cx).build_view();
        let problems = &view.problems;
        let errors = self.count(problems, ProblemSeverity::Error);
        let warnings = self.count(problems, ProblemSeverity::Warning);
        let mut rows = vec![Row {
            kind: RowKind::Root,
            depth: 0,
            label: if view.title.is_empty() {
                "Build".to_string()
            } else {
                view.title.clone()
            },
            hint: Some(status_text(view, errors, warnings)),
            icon: "toolwindows/tasks",
            expandable: true,
            open: self.root_open,
        }];
        if !self.root_open {
            return rows;
        }
        for (path, indices) in self.files(problems) {
            let file_errors = indices
                .iter()
                .filter(|index| problems[**index].severity == ProblemSeverity::Error)
                .count();
            let file_warnings = indices.len() - file_errors;
            let open = !self.closed_files.contains(&path);
            rows.push(Row {
                kind: RowKind::File(path.clone()),
                depth: 1,
                label: relative(&path, self.run.read(cx).root()),
                hint: Some(problems_text(file_errors, file_warnings)),
                icon: "fileTypes/text",
                expandable: true,
                open,
            });
            if !open {
                continue;
            }
            for index in indices {
                let problem = &problems[index];
                rows.push(Row {
                    kind: RowKind::Problem(path.clone(), index),
                    depth: 2,
                    label: problem_label(problem),
                    hint: None,
                    icon: match problem.severity {
                        ProblemSeverity::Error => "status/error",
                        ProblemSeverity::Warning => "status/warning",
                    },
                    expandable: false,
                    open: false,
                });
            }
        }
        rows
    }

    /// The files with the problems the filter keeps, in the order the build named them.
    fn files(&self, problems: &[BuildProblem]) -> Vec<(PathBuf, Vec<usize>)> {
        let mut files: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for (index, problem) in problems.iter().enumerate() {
            if self.only_errors && problem.severity == ProblemSeverity::Warning {
                continue;
            }
            match files.iter_mut().find(|(path, _)| *path == problem.path) {
                Some((_, indices)) => indices.push(index),
                None => files.push((problem.path.clone(), vec![index])),
            }
        }
        files
    }

    fn count(&self, problems: &[BuildProblem], severity: ProblemSeverity) -> usize {
        problems
            .iter()
            .filter(|problem| problem.severity == severity)
            .filter(|_| !self.only_errors || severity == ProblemSeverity::Error)
            .count()
    }

    /// Selects the first error of the tree, or the first message when the build named no
    /// error. The focus stays in the editor, so a build cannot take the keys away.
    fn select_first(&mut self, cx: &App) {
        let rows = self.rows(cx);
        self.selected = rows
            .iter()
            .position(|row| {
                matches!(&row.kind, RowKind::Problem(_, index)
                    if self.run.read(cx).build_view().problems[*index].severity
                        == ProblemSeverity::Error)
            })
            .or_else(|| {
                rows.iter()
                    .position(|row| matches!(row.kind, RowKind::Problem(..)))
            });
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows(cx).into_iter().nth(index) else {
            return;
        };
        self.selected = Some(index);
        match row.kind {
            RowKind::Root => {
                self.root_open = !self.root_open;
                cx.notify();
            }
            RowKind::File(path) => {
                if row.open {
                    self.closed_files.insert(path);
                } else {
                    self.closed_files.remove(&path);
                }
                cx.notify();
            }
            RowKind::Problem(path, problem_index) => {
                let Some(problem) = self
                    .run
                    .read(cx)
                    .build_view()
                    .problems
                    .get(problem_index)
                    .cloned()
                else {
                    return;
                };
                let spot = Spot::Line {
                    line: problem.line.saturating_sub(1) as usize,
                    column: problem.column.unwrap_or(1).saturating_sub(1) as usize,
                };
                self.workspace
                    .update(cx, |workspace, cx| {
                        navigation::go_to(workspace, path, Some(spot), true, window, cx)
                    })
                    .ok();
            }
        }
    }

    fn click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        if event.click_count() >= 2 {
            self.activate(index, window, cx);
            return;
        }
        self.selected = Some(index);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "left" => {
                cx.stop_propagation();
                self.collapse_or_parent(cx);
            }
            "right" => {
                cx.stop_propagation();
                self.expand_or_child(cx);
            }
            "enter" => {
                cx.stop_propagation();
                if let Some(index) = self.selected {
                    self.activate(index, window, cx);
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.rows(cx).len();
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        self.selected = Some(next as usize);
        cx.notify();
    }

    fn collapse_or_parent(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let rows = self.rows(cx);
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        if row.expandable && row.open {
            self.close_row(&row);
            cx.notify();
            return;
        }
        if let Some(parent) = rows[..index]
            .iter()
            .rposition(|candidate| candidate.depth < row.depth)
        {
            self.selected = Some(parent);
            cx.notify();
        }
    }

    fn expand_or_child(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let rows = self.rows(cx);
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        if row.expandable && !row.open {
            self.open_row(&row);
            cx.notify();
            return;
        }
        if rows
            .get(index + 1)
            .is_some_and(|child| child.depth > row.depth)
        {
            self.selected = Some(index + 1);
            cx.notify();
        }
    }

    fn close_row(&mut self, row: &Row) {
        match &row.kind {
            RowKind::Root => self.root_open = false,
            RowKind::File(path) => {
                self.closed_files.insert(path.clone());
            }
            RowKind::Problem(..) => {}
        }
    }

    fn open_row(&mut self, row: &Row) {
        match &row.kind {
            RowKind::Root => self.root_open = true,
            RowKind::File(path) => {
                self.closed_files.remove(path);
            }
            RowKind::Problem(..) => {}
        }
    }

    fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.root_open = true;
        self.closed_files.clear();
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.root_open = false;
        cx.notify();
    }

    pub(crate) fn set_only_errors(&mut self, on: bool, cx: &mut Context<Self>) {
        self.only_errors = on;
        self.select_first(cx);
        cx.notify();
    }

    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let restart = tool_button("build-output-restart", "general/refresh", 0, theme);
        let restart = if self.run.read(cx).build_view().running {
            restart.opacity(0.4)
        } else {
            restart.on_click(
                cx.listener(|this, _, _, cx| this.run.update(cx, |run, cx| run.restart_build(cx))),
            )
        };
        let stop = tool_button("build-output-stop", "run/stop", 1, theme).on_click(
            cx.listener(|this, _, _, cx| this.run.update(cx, |run, cx| run.stop_build(cx))),
        );
        let only = tool_button("build-output-errors-only", "general/filter", 2, theme)
            .when(self.only_errors, |this| this.bg(theme.pressed))
            .on_click(cx.listener(|this, _, _, cx| this.set_only_errors(!this.only_errors, cx)));
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .child(restart)
            .child(stop)
            .child(only)
            .child(
                tool_button("build-output-expand-all", "general/expandAll", 3, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.expand_all(cx))),
            )
            .child(
                tool_button("build-output-collapse-all", "general/collapseAll", 4, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            )
    }

    fn output_rows(&mut self, range: Range<usize>, theme: &Theme, cx: &App) -> Vec<AnyElement> {
        let view = self.run.read(cx).build_view();
        range
            .filter_map(|index| view.lines.get(index))
            .map(|line| {
                div()
                    .w_full()
                    .whitespace_nowrap()
                    .text_color(theme.text)
                    .child(line.clone())
                    .into_any_element()
            })
            .collect()
    }

    fn tree(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.rows(cx);
        let selected = self.selected;
        let children: Vec<_> = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let style = TreeRow {
                    id: ("build-output-row", index).into(),
                    depth: row.depth,
                    label: row.label.clone().into(),
                    hint: row.hint.clone().map(Into::into),
                    icon: row.icon,
                    expandable: row.expandable,
                    open: row.open,
                    selected: selected == Some(index),
                };
                let element = tree_row(style, theme)
                    .debug_selector(move || format!("build-output-row-{index}"))
                    .on_click(cx.listener(move |this, event, window, cx| {
                        this.click(index, event, window, cx)
                    }));
                div().w_full().px(px(ROW_MARGIN)).child(element)
            })
            .collect();
        let empty = rows_is_empty(self.run.read(cx).build_view());
        scrollable(
            "build-output-tree",
            &self.scroll,
            div()
                .id("build-output-tree")
                .flex()
                .flex_col()
                .flex_1()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(children)
                .when(empty, |this| {
                    this.child(
                        div()
                            .px(px(ROW_MARGIN + SPACE_M))
                            .py(px(SPACE_S))
                            .text_color(theme.text_secondary)
                            .child("No builds yet."),
                    )
                }),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    fn output(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *theme;
        let count = self.run.read(cx).build_view().lines.len();
        let list = div()
            .flex()
            .flex_col()
            .w_full()
            .min_h_0()
            .font_family(MONO_FONT)
            .text_size(px(CODE_SIZE))
            .child(
                uniform_list(
                    "build-output-console",
                    count,
                    cx.processor(move |this, range, _, cx| this.output_rows(range, &theme, cx)),
                )
                .track_scroll(&self.output_scroll)
                .size_full(),
            );
        scrollable(
            "build-output-output",
            &self.output_scroll,
            list,
            &theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0()
        .into_any_element()
    }
}

impl Render for BuildOutputPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::BUILD_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(BUILD_TOOL, window, cx)
                })
                .ok();
        };
        let tree = self.tree(&theme, window, cx);
        let output = self.output(&theme, window, cx);
        island(&theme)
            .id(BUILD_TOOL)
            .debug_selector(|| "build-output".to_owned())
            .key_context("BuildOutputPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(
                "Build", GROUP, BUILD_TOOL, options, hide, &theme,
            ))
            .child(self.toolbar(&theme, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(div().flex_none().w(px(TREE_WIDTH)).h_full().child(tree))
                    .child(
                        div()
                            .flex_none()
                            .w(px(DIVIDER_WIDTH))
                            .h_full()
                            .bg(theme.border),
                    )
                    .child(div().flex_1().min_w_0().h_full().child(output)),
            )
    }
}

fn tool_button(
    id: &'static str,
    icon: &'static str,
    tooltip_index: usize,
    theme: &Theme,
) -> Stateful<Div> {
    icon_button(id, icon, TOOLTIPS[tooltip_index].1, None, theme)
        .debug_selector(move || id.to_owned())
}

fn rows_is_empty(view: &BuildView) -> bool {
    view.problems.is_empty() && view.lines.is_empty() && !view.running && !view.finished
}

fn relative(path: &Path, root: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let root = root.display().to_string().replace('\\', "/");
    let root = root
        .strip_prefix("//?/")
        .unwrap_or(&root)
        .trim_end_matches('/');
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    if text.len() >= root.len() && text[..root.len()].eq_ignore_ascii_case(root) {
        text[root.len()..].trim_start_matches('/').to_string()
    } else {
        text.to_string()
    }
}

fn problem_label(problem: &BuildProblem) -> String {
    match problem.column {
        Some(column) => format!("{}:{} {}", problem.line, column, problem.message),
        None => format!("{} {}", problem.line, problem.message),
    }
}

fn problems_text(errors: usize, warnings: usize) -> String {
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!("{errors} {}", error_word(errors)));
    }
    if warnings > 0 {
        parts.push(format!("{warnings} {}", warning_word(warnings)));
    }
    parts.join(", ")
}

fn error_word(count: usize) -> &'static str {
    if count == 1 { "error" } else { "errors" }
}

fn warning_word(count: usize) -> &'static str {
    if count == 1 { "warning" } else { "warnings" }
}

fn status_text(view: &BuildView, errors: usize, warnings: usize) -> String {
    let counts = problems_text(errors, warnings);
    let state = if view.running {
        "running".to_string()
    } else if view.stopped {
        "stopped".to_string()
    } else if view.finished {
        let time = view.duration.map_or_else(String::new, duration_text);
        let verdict = if errors > 0 {
            "with errors"
        } else if !view.success {
            "failed"
        } else if warnings > 0 {
            "successful with warnings"
        } else {
            "successful"
        };
        if time.is_empty() {
            verdict.to_string()
        } else {
            format!("{verdict} in {time}")
        }
    } else if counts.is_empty() {
        return "no builds yet".to_string();
    } else {
        return counts;
    };
    if counts.is_empty() {
        state
    } else {
        format!("{state} · {counts}")
    }
}

fn duration_text(duration: Duration) -> String {
    let milliseconds = duration.as_millis();
    if milliseconds >= 1000 {
        format!("{:.1} s", duration.as_secs_f64())
    } else {
        format!("{milliseconds} ms")
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn a_short_build_is_named_in_milliseconds() {
        assert_eq!(
            super::duration_text(std::time::Duration::from_millis(120)),
            "120 ms"
        );
        assert_eq!(
            super::duration_text(std::time::Duration::from_millis(1250)),
            "1.2 s"
        );
    }
}
