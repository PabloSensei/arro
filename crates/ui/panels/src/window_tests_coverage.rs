//! Run with Coverage without a window: a configuration whose report is known in advance
//! paints the gutter of the file, fills the Coverage tool window and disappears with Hide
//! Coverage.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::CoverageState;
use lang_api::{
    BranchCoverage, Counters, CoverageReport, FileCoverage, LineCoverage, TypeCoverage,
};
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{CoveragePlan, RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{COVERAGE_TOOL, Workspace};

use crate::coverage::{ActiveCoverage, CoverageColumn};
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_window_with, sample_project, scratch,
};
use crate::window_tests_run::{run_panel, setup, wait_for};

const ALPHA: &str = "package demo;
class Alpha {
    int bump() { return 1; }
    int unused() { return 2; }
    int choose() { if (true) { return 1; } return 0; }
}
";

const BETA: &str = "package demo;
class Beta {
    int only() { return 1; }
}
";

/// The report of the fake run: three states in Alpha and a fully covered Beta.
fn report(_root: &Path, alpha: &Path, beta: &Path) -> CoverageReport {
    CoverageReport {
        root: alpha.parent().unwrap().parent().unwrap().to_owned(),
        files: vec![
            FileCoverage {
                path: alpha.to_owned(),
                lines: vec![
                    LineCoverage {
                        line: 3,
                        hits: 3,
                        branches: None,
                    },
                    LineCoverage {
                        line: 4,
                        hits: 0,
                        branches: None,
                    },
                    LineCoverage {
                        line: 5,
                        hits: 1,
                        branches: Some(BranchCoverage {
                            covered: 1,
                            total: 2,
                        }),
                    },
                ],
            },
            FileCoverage {
                path: beta.to_owned(),
                lines: vec![LineCoverage {
                    line: 3,
                    hits: 1,
                    branches: None,
                }],
            },
        ],
        types: vec![
            TypeCoverage {
                name: "demo.Alpha".into(),
                package: "demo".into(),
                source: PathBuf::from("src/Alpha.code"),
                class: Counters {
                    covered: 1,
                    total: 1,
                },
                method: Counters {
                    covered: 1,
                    total: 3,
                },
                line: Counters {
                    covered: 1,
                    total: 4,
                },
                branch: Counters {
                    covered: 1,
                    total: 2,
                },
            },
            TypeCoverage {
                name: "demo.Beta".into(),
                package: "demo".into(),
                source: PathBuf::from("src/Beta.code"),
                class: Counters {
                    covered: 1,
                    total: 1,
                },
                method: Counters {
                    covered: 1,
                    total: 1,
                },
                line: Counters {
                    covered: 2,
                    total: 2,
                },
                branch: Counters {
                    covered: 0,
                    total: 0,
                },
            },
        ],
    }
}

/// A configuration whose covered program prints nothing, and whose report is prepared.
struct FixtureCoverage {
    exec: PathBuf,
    report: CoverageReport,
}

impl RunConfigProvider for FixtureCoverage {
    fn id(&self) -> &'static str {
        "coverage-fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Covered".into(),
            provider: self.id().into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(self.plain())
    }

    fn coverage_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<CoveragePlan> {
        Ok(CoveragePlan {
            plan: self.plain(),
            exec: self.exec.clone(),
            note: None,
        })
    }

    fn coverage_report(&self, _exec: &Path, _model: &ProjectModel) -> base::Result<CoverageReport> {
        Ok(self.report.clone())
    }
}

impl FixtureCoverage {
    fn plain(&self) -> RunPlan {
        RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: std::time::Duration::from_secs(1),
        }
    }
}

/// A window with a project of two classes and a coverage configuration. The program of the
/// configuration exits at once, the report arrives from the provider.
fn open_coverage_window(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, PathBuf, PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let alpha = root.join("src/Alpha.code");
    let beta = root.join("src/Beta.code");
    fs::write(&alpha, ALPHA).unwrap();
    fs::write(&beta, BETA).unwrap();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureCoverage {
        exec: scratch(&format!("{name}-exec")).join("coverage/run.exec"),
        report: report(&root, &alpha, &beta),
    })];
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_for(cx, "the run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    open_path(cx, window, &alpha);
    (root, alpha, beta, window)
}

fn start_coverage(cx: &mut TestAppContext, window: WindowHandle<Workspace>, alpha: &Path) {
    let view = editor_of(cx, window, alpha);
    click(cx, window, "t-coverage");
    wait_for(cx, "the coverage of the open file", |cx| {
        view.read_with(cx, |view, _| view.coverage().len() == 3)
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_coverage_run_paints_three_kinds_of_lines_and_shows_percentages(cx: &mut TestAppContext) {
    let (_root, alpha, beta, window) = open_coverage_window(cx, "coverage-gutter");
    start_coverage(cx, window, &alpha);
    let view = editor_of(cx, window, &alpha);

    let marks = view.read_with(cx, |view, _| view.coverage().to_vec());
    assert_eq!(
        marks.iter().map(|mark| mark.line).collect::<Vec<_>>(),
        vec![2, 3, 4],
        "the marks are on the lines of the report"
    );
    assert_eq!(marks[0].state, CoverageState::Covered);
    assert_eq!(marks[0].hint(), "Hits: 3");
    assert_eq!(marks[1].state, CoverageState::NotCovered);
    assert_eq!(marks[1].hint(), "Hits: 0");
    assert_eq!(marks[2].state, CoverageState::Partial);
    assert_eq!(marks[2].hint(), "Hits: 1\nCovered 1 of 2 branches");

    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(COVERAGE_TOOL))
            .unwrap(),
        "the Coverage window opened after the run"
    );
    let panel = cx.read(|cx| cx.global::<ActiveCoverage>().0.clone());
    let rows = panel.read_with(cx, |panel, _| panel.rows().to_vec());
    let package = rows
        .iter()
        .find(|row| row.label == "demo")
        .expect("the package row");
    assert_eq!(package.depth, 0);
    assert_eq!(package.line, Some(50), "three of six lines of the package");
    let alpha_row = rows
        .iter()
        .find(|row| row.label == "Alpha")
        .expect("the type row");
    assert_eq!(alpha_row.depth, 1);
    assert_eq!(alpha_row.class, Some(100));
    assert_eq!(alpha_row.method, Some(33));
    assert_eq!(alpha_row.line, Some(25));
    assert_eq!(alpha_row.branch, Some(50));

    panel.update(cx, |panel, cx| panel.sort_by(CoverageColumn::Line, cx));
    let sorted: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .rows()
            .iter()
            .filter(|row| row.depth == 1)
            .map(|row| row.label.clone())
            .collect()
    });
    assert_eq!(sorted, vec!["Beta".to_owned(), "Alpha".to_owned()]);

    let index = panel.read_with(cx, |panel, _| {
        panel
            .rows()
            .iter()
            .position(|row| row.label == "Beta")
            .unwrap()
    });
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.open_row(index, window, cx))
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(active_path(cx, window), Some(beta));
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn hide_coverage_removes_the_marks_and_the_rows(cx: &mut TestAppContext) {
    let (_root, alpha, _beta, window) = open_coverage_window(cx, "coverage-hide");
    start_coverage(cx, window, &alpha);
    let panel = cx.read(|cx| cx.global::<ActiveCoverage>().0.clone());
    assert!(!panel.read_with(cx, |panel, _| panel.rows().is_empty()));

    window
        .update(cx, |workspace, window, cx| {
            workspace.hide_coverage(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let view = editor_of(cx, window, &alpha);
    assert!(
        view.read_with(cx, |view, _| view.coverage().is_empty()),
        "the gutter keeps no marks"
    );
    assert!(
        panel.read_with(cx, |panel, _| panel.rows().is_empty()),
        "the window keeps no rows"
    );
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(COVERAGE_TOOL))
            .unwrap(),
        "the Coverage window closed"
    );
}
