//! The Coverage tool window: the packages and types of the last run with their
//! percentages, and the stripes the editors show. The numbers come from the run panel,
//! which gets them from the provider of the language.

use std::path::PathBuf;
use std::sync::Arc;

use editor_view::{CoverageMark, CoverageState, EditorView};
use lang_api::{Counters, CoverageReport, LineState, TypeCoverage};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;
use crate::run::RunPanel;
use crate::script::ActiveRunPanel;

const GROUP: &str = "bottom-panel";
/// The width of a percentage column of the table.
const COLUMN_WIDTH: f32 = 64.;
const HEADER_HEIGHT: f32 = 24.;

const COVERED: (u32, u32) = (0x5fb865, 0x208a3c);
const PARTIAL: (u32, u32) = (0xf2c55c, 0xb59b1b);
const NOT_COVERED: (u32, u32) = (0xf75464, 0xc7222d);

/// The panel of the window, for the tests and the coverage stripes.
pub struct ActiveCoverage(pub Entity<CoveragePanel>);

impl Global for ActiveCoverage {}

/// A column of the table, which a click sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageColumn {
    Class,
    Method,
    Line,
    Branch,
}

impl CoverageColumn {
    fn label(self) -> &'static str {
        match self {
            Self::Class => "Class",
            Self::Method => "Method",
            Self::Line => "Line",
            Self::Branch => "Branch",
        }
    }
}

/// One row of the table: a package and then its types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageRow {
    pub label: String,
    pub depth: usize,
    /// The source of a type, absent for a package.
    pub path: Option<PathBuf>,
    pub class: Option<u32>,
    pub method: Option<u32>,
    pub line: Option<u32>,
    pub branch: Option<u32>,
}

impl CoverageRow {
    fn percent(&self, column: CoverageColumn) -> Option<u32> {
        match column {
            CoverageColumn::Class => self.class,
            CoverageColumn::Method => self.method,
            CoverageColumn::Line => self.line,
            CoverageColumn::Branch => self.branch,
        }
    }
}

/// A package of the tree with the counters of its types.
struct Package {
    name: String,
    class: Counters,
    method: Counters,
    line: Counters,
    branch: Counters,
    types: Vec<TypeCoverage>,
}

pub struct CoveragePanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    report: Option<Arc<CoverageReport>>,
    rows: Vec<CoverageRow>,
    column: Option<CoverageColumn>,
    descending: bool,
}

impl CoveragePanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        runs: Entity<RunPanel>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.subscribe(
                &runs,
                |panel: &mut Self, runs, _: &crate::run::CoverageChanged, cx| {
                    let report = runs.read(cx).coverage();
                    panel.set_report(report, cx);
                },
            )
            .detach();
            Self {
                workspace,
                focus: cx.focus_handle(),
                scroll: UniformListScrollHandle::new(),
                report: None,
                rows: Vec::new(),
                column: None,
                descending: true,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The rows as the window shows them: every package, then its types.
    pub fn rows(&self) -> &[CoverageRow] {
        &self.rows
    }

    fn set_report(&mut self, report: Option<Arc<CoverageReport>>, cx: &mut Context<Self>) {
        self.report = report;
        self.rebuild();
        cx.notify();
    }

    /// Sorts the types of every package by a column, the way a click on its header does.
    pub fn sort_by(&mut self, column: CoverageColumn, cx: &mut Context<Self>) {
        if self.column == Some(column) {
            self.descending = !self.descending;
        } else {
            self.column = Some(column);
            self.descending = true;
        }
        self.rebuild();
        cx.notify();
    }

    fn rebuild(&mut self) {
        let mut packages = packages_of(self.report.as_deref());
        for package in &mut packages {
            match self.column {
                Some(column) => package.types.sort_by(|left, right| {
                    let order = type_percent(right, column).cmp(&type_percent(left, column));
                    if self.descending {
                        order
                    } else {
                        order.reverse()
                    }
                }),
                None => package
                    .types
                    .sort_by(|left, right| left.name.cmp(&right.name)),
            }
        }
        let root = self.report_root();
        let mut rows = Vec::new();
        for package in packages {
            rows.push(CoverageRow {
                label: package.name.clone(),
                depth: 0,
                path: None,
                class: package.class.percent(),
                method: package.method.percent(),
                line: package.line.percent(),
                branch: package.branch.percent(),
            });
            for kind in package.types {
                rows.push(CoverageRow {
                    label: simple_name(&kind.name),
                    depth: 1,
                    path: Some(root.join(&kind.source)),
                    class: kind.class.percent(),
                    method: kind.method.percent(),
                    line: kind.line.percent(),
                    branch: kind.branch.percent(),
                });
            }
        }
        self.rows = rows;
    }

    fn report_root(&self) -> PathBuf {
        self.report
            .as_ref()
            .map_or_else(PathBuf::new, |report| report.root.clone())
    }

    /// Opens the source of a type row, as a double click does.
    pub fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.rows.get(index).and_then(|row| row.path.clone()) else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path, None, true, window, cx)
                })
                .ok();
        });
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
            self.open_row(index, window, cx);
        }
    }

    fn row_element(
        &self,
        index: usize,
        row: CoverageRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hint = row
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let label = row.label.clone();
        let depth = row.depth;
        div()
            .id(("coverage-row", index))
            .debug_selector(move || format!("coverage-row-{label}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_L + depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .hover(move |style| style.bg(theme.selection_hover))
            .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .on_click(
                cx.listener(move |this, event, window, cx| this.click(index, event, window, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if depth == 0 {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(row.label.clone()),
            )
            .children(
                [
                    CoverageColumn::Class,
                    CoverageColumn::Method,
                    CoverageColumn::Line,
                    CoverageColumn::Branch,
                ]
                .map(|column| self.cell(row.percent(column), theme, column)),
            )
    }

    fn cell(&self, percent: Option<u32>, theme: &Theme, column: CoverageColumn) -> Div {
        let selected = self.column == Some(column);
        div()
            .w(px(COLUMN_WIDTH))
            .flex_none()
            .text_color(if selected {
                theme.text
            } else {
                theme.text_secondary
            })
            .child(percent_text(percent))
    }

    fn rows_element(
        &self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .filter_map(|index| self.rows.get(index).cloned().map(|row| (index, row)))
            .map(|(index, row)| self.row_element(index, row, theme, cx))
            .collect()
    }
}

fn percent_text(percent: Option<u32>) -> String {
    match percent {
        Some(percent) => format!("{percent}%"),
        None => "n/a".to_owned(),
    }
}

fn simple_name(name: &str) -> String {
    name.rsplit(['.', '$']).next().unwrap_or(name).to_owned()
}

fn packages_of(report: Option<&CoverageReport>) -> Vec<Package> {
    let mut packages: Vec<Package> = Vec::new();
    let Some(report) = report else {
        return packages;
    };
    for kind in &report.types {
        let at = match packages
            .iter()
            .position(|package| package.name == kind.package)
        {
            Some(at) => at,
            None => {
                packages.push(Package {
                    name: kind.package.clone(),
                    class: Counters::default(),
                    method: Counters::default(),
                    line: Counters::default(),
                    branch: Counters::default(),
                    types: Vec::new(),
                });
                packages.len() - 1
            }
        };
        let package = &mut packages[at];
        add(&mut package.class, kind.class);
        add(&mut package.method, kind.method);
        add(&mut package.line, kind.line);
        add(&mut package.branch, kind.branch);
        package.types.push(kind.clone());
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages
}

fn add(total: &mut Counters, part: Counters) {
    total.covered += part.covered;
    total.total += part.total;
}

fn type_percent(kind: &TypeCoverage, column: CoverageColumn) -> Option<u32> {
    match column {
        CoverageColumn::Class => kind.class.percent(),
        CoverageColumn::Method => kind.method.percent(),
        CoverageColumn::Line => kind.line.percent(),
        CoverageColumn::Branch => kind.branch.percent(),
    }
}

fn color((dark, light): (u32, u32), is_dark: bool) -> Hsla {
    rgb(if is_dark { dark } else { light }).into()
}

/// The stripe of a line in the colour of its state.
pub fn mark_color(state: CoverageState, dark: bool) -> Hsla {
    match state {
        CoverageState::Covered => color(COVERED, dark),
        CoverageState::Partial => color(PARTIAL, dark),
        CoverageState::NotCovered => color(NOT_COVERED, dark),
    }
}

/// Puts the stripes of the last run in the gutter of the view, or removes them when the
/// run is hidden or the file has no data.
pub(crate) fn refresh(view: &mut EditorView, cx: &mut Context<EditorView>) {
    let Some(panel) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    let report = panel.read(cx).coverage();
    let dark = cx.global::<Theme>().dark;
    let Some(report) = report else {
        view.set_coverage(Vec::new(), cx);
        return;
    };
    let path = view.path().to_path_buf();
    let marks: Vec<CoverageMark> = report
        .file(&path)
        .map(|file| {
            file.lines
                .iter()
                .map(|line| {
                    let state = match line.state() {
                        LineState::Covered => CoverageState::Covered,
                        LineState::Partial => CoverageState::Partial,
                        LineState::NotCovered => CoverageState::NotCovered,
                    };
                    CoverageMark {
                        line: line.line.saturating_sub(1),
                        state,
                        hits: line.hits,
                        branches: line
                            .branches
                            .map(|branches| (branches.covered, branches.total)),
                        color: mark_color(state, dark),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    view.set_coverage(marks, cx);
}

impl Render for CoveragePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "coverage");
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide_coverage = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| workspace.hide_coverage(window, cx))
                .ok();
        };
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .child("Coverage")
            .child(
                icon_button(
                    "coverage-hide",
                    "general/hide",
                    "Hide Coverage",
                    None,
                    &theme,
                )
                .debug_selector(|| "coverage-hide".to_owned())
                .on_click(hide_coverage),
            );
        let columns = div()
            .flex()
            .items_center()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .pl(px(SPACE_L + COLUMN_WIDTH))
            .pr(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .child(div().flex_1().min_w_0().child("Package"))
            .children(
                [
                    CoverageColumn::Class,
                    CoverageColumn::Method,
                    CoverageColumn::Line,
                    CoverageColumn::Branch,
                ]
                .into_iter()
                .enumerate()
                .map(|(index, column)| {
                    let label = column.label();
                    div()
                        .id(("coverage-column", index))
                        .debug_selector(move || format!("coverage-column-{label}"))
                        .w(px(COLUMN_WIDTH))
                        .flex_none()
                        .text_color(if self.column == Some(column) {
                            theme.text
                        } else {
                            theme.text_secondary
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.sort_by(column, cx)))
                        .child(label)
                }),
            );
        let empty = self.rows.is_empty();
        let hint = empty.then(|| {
            div()
                .px(px(SPACE_L))
                .py(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child("No coverage data")
        });
        island(&theme)
            .id("coverage-panel")
            .key_context("CoveragePanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(header)
            .child(columns)
            .children(hint)
            .child(
                scrollable(
                    "coverage-rows",
                    &self.scroll,
                    div().size_full().child(
                        uniform_list(
                            "coverage-rows",
                            self.rows.len(),
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.rows_element(range, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
    }
}
