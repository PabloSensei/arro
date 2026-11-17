//! The tool window of the build tool without a real window: a stand-in tool records what
//! the tree and the buttons ask it to run.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Module, ProjectModel, TaskOptions,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{TASKS_TOOL, Workspace};

use crate::build::TOOLTIPS;
use crate::run::ProjectSetup;
use crate::script::{ActiveBuildPanel, ActiveRunPanel};
use crate::window_tests::{draw, open_window_with, press, sample_project, scratch, type_text};
use crate::window_tests_run::wait_for;

/// What the tool was asked to run: the module id, the task and the Skip Tests state.
type Tasks = Arc<Mutex<Vec<(Option<String>, String, bool)>>>;

struct FakeTool {
    model: ProjectModel,
    syncs: Arc<AtomicUsize>,
    tasks: Tasks,
    goals: Arc<Mutex<Vec<String>>>,
}

impl FakeTool {
    fn new(root: &Path) -> Self {
        let module = |id: &str, name: &str, dir: PathBuf| Module {
            id: id.into(),
            name: name.into(),
            dir,
            ..Module::default()
        };
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fake".into(),
                resolved: true,
                modules: vec![
                    module("root", "demo", root.to_path_buf()),
                    module("app", "app", root.join("app")),
                ],
                ..ProjectModel::default()
            },
            syncs: Arc::new(AtomicUsize::new(0)),
            tasks: Arc::new(Mutex::new(Vec::new())),
            goals: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn task(&self, name: &str, program: bool) -> BuildTask {
        BuildTask {
            name: name.into(),
            program,
        }
    }
}

impl BuildSystem for FakeTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn task_tree(&self, _: &ProjectModel) -> Vec<BuildModule> {
        vec![
            BuildModule {
                id: "root".into(),
                name: "demo".into(),
                groups: vec![
                    BuildGroup {
                        name: "Lifecycle".into(),
                        tasks: vec![self.task("clean", false), self.task("install", false)],
                    },
                    BuildGroup {
                        name: "Plugins".into(),
                        tasks: vec![self.task("spring-boot:run", true)],
                    },
                ],
            },
            BuildModule {
                id: "app".into(),
                name: "app".into(),
                groups: vec![BuildGroup {
                    name: "Tasks".into(),
                    tasks: vec![self.task("compile", false), self.task("test", false)],
                }],
            },
        ]
    }

    fn task_in(
        &self,
        _: &Path,
        module: Option<&Module>,
        task: &str,
        options: TaskOptions,
    ) -> base::Result<ProcessSpec> {
        self.tasks.lock().unwrap().push((
            module.map(|module| module.id.clone()),
            task.to_owned(),
            options.skip_tests,
        ));
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", task]))
    }

    fn goal(&self, _: &Path, line: &str, _: TaskOptions) -> base::Result<ProcessSpec> {
        self.goals.lock().unwrap().push(line.to_owned());
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", line]))
    }

    fn supports_skip_tests(&self) -> bool {
        true
    }
}

fn setup(tool: Arc<FakeTool>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("build-tool-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    Arc<FakeTool>,
    ui_kit::gpui::Entity<crate::build::BuildPanel>,
) {
    let root = sample_project(name);
    let tool = Arc::new(FakeTool::new(&root));
    let window = open_window_with(cx, root, setup(tool.clone()));
    let panel = cx.read(|cx| cx.global::<ActiveBuildPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TASKS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the task tree", |cx| {
        !panel.read_with(cx, |panel, cx| panel.labels(cx)).is_empty()
    });
    (window, tool, panel)
}

fn labels(
    cx: &TestAppContext,
    panel: &ui_kit::gpui::Entity<crate::build::BuildPanel>,
) -> Vec<String> {
    panel.read_with(cx, |panel, cx| panel.labels(cx))
}

fn row(labels: &[String], label: &str) -> usize {
    labels
        .iter()
        .position(|line| line.trim() == label)
        .unwrap_or_else(|| panic!("no row {label} in {labels:?}"))
}

fn focus_panel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &ui_kit::gpui::Entity<crate::build::BuildPanel>,
) {
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
}

fn wait_idle(cx: &mut TestAppContext) {
    let run = cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone());
    wait_for(cx, "the task to finish", |cx| {
        !run.read_with(cx, |run, _| run.state().running)
    });
}

#[ui_kit::gpui::test]
fn the_window_shows_modules_with_their_groups_and_expands_them(cx: &mut TestAppContext) {
    let (window, _tool, panel) = open(cx, "build-tree");

    let lines = labels(cx, &panel);
    assert_eq!(lines[0], "demo", "the root module comes first");
    assert!(lines.contains(&"  Lifecycle".to_owned()), "{lines:?}");
    assert!(lines.contains(&"    clean".to_owned()), "{lines:?}");
    assert!(lines.contains(&"  Plugins".to_owned()), "{lines:?}");
    assert!(
        lines.contains(&"    spring-boot:run".to_owned()),
        "{lines:?}"
    );
    assert!(lines.contains(&"app".to_owned()), "{lines:?}");
    assert!(lines.contains(&"  Tasks".to_owned()), "{lines:?}");

    // The arrow keys close and open the root module; the group follows the same keys.
    focus_panel(cx, window, &panel);
    panel.update(cx, |panel, cx| panel.select_row(0, cx));
    press(cx, window, "left");
    let lines = labels(cx, &panel);
    assert!(!lines.contains(&"  Lifecycle".to_owned()), "{lines:?}");
    assert!(lines.contains(&"app".to_owned()), "another module stays");
    press(cx, window, "right");
    let lines = labels(cx, &panel);
    assert!(lines.contains(&"  Lifecycle".to_owned()), "{lines:?}");

    let group = row(&lines, "Lifecycle");
    panel.update(cx, |panel, cx| panel.select_row(group, cx));
    press(cx, window, "left");
    let lines = labels(cx, &panel);
    assert!(!lines.contains(&"    clean".to_owned()), "{lines:?}");
    press(cx, window, "right");
    assert!(labels(cx, &panel).contains(&"    clean".to_owned()));
}

#[ui_kit::gpui::test]
fn enter_and_a_double_click_run_the_task_of_the_module(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "build-run");
    let lines = labels(cx, &panel);
    let compile = row(&lines, "compile");

    focus_panel(cx, window, &panel);
    panel.update(cx, |panel, cx| panel.select_row(compile, cx));
    press(cx, window, "enter");
    wait_for(cx, "the task from Enter", |_| {
        !tool.tasks.lock().unwrap().is_empty()
    });
    assert_eq!(
        tool.tasks.lock().unwrap()[0],
        (Some("app".into()), "compile".into(), false)
    );
    wait_idle(cx);

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.activate_row(compile, window, cx))
        })
        .unwrap();
    wait_for(cx, "the task from a double click", |_| {
        tool.tasks.lock().unwrap().len() == 2
    });
    assert_eq!(
        tool.tasks.lock().unwrap()[1],
        (Some("app".into()), "compile".into(), false)
    );
}

#[ui_kit::gpui::test]
fn execute_runs_the_typed_line_in_the_root(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "build-execute");

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.open_execute(window, cx))
        })
        .unwrap();
    assert!(panel.read_with(cx, |panel, _| panel.execute_open()));
    type_text(cx, window, "clean install");
    press(cx, window, "enter");

    wait_for(cx, "the typed goal", |_| {
        !tool.goals.lock().unwrap().is_empty()
    });
    assert_eq!(tool.goals.lock().unwrap().as_slice(), ["clean install"]);
    assert!(
        !panel.read_with(cx, |panel, _| panel.execute_open()),
        "Enter closes the popup"
    );
}

#[ui_kit::gpui::test]
fn skip_tests_reaches_the_task_of_a_module(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "build-skip");

    panel.update(cx, |panel, cx| panel.toggle_skip_tests(cx));
    assert!(panel.read_with(cx, |panel, _| panel.skip_tests()));
    let lines = labels(cx, &panel);
    let test = row(&lines, "test");
    focus_panel(cx, window, &panel);
    panel.update(cx, |panel, cx| panel.select_row(test, cx));
    press(cx, window, "enter");

    wait_for(cx, "the task with Skip Tests", |_| {
        !tool.tasks.lock().unwrap().is_empty()
    });
    assert_eq!(
        tool.tasks.lock().unwrap()[0],
        (Some("app".into()), "test".into(), true)
    );
}

#[ui_kit::gpui::test]
fn reload_reads_the_model_again(cx: &mut TestAppContext) {
    let (_window, tool, panel) = open(cx, "build-reload");
    wait_for(cx, "the first sync", |_| {
        tool.syncs.load(Ordering::SeqCst) > 0
    });

    panel.update(cx, |panel, cx| panel.reload_now(cx));

    wait_for(cx, "the second sync", |_| {
        tool.syncs.load(Ordering::SeqCst) >= 2
    });
}

#[ui_kit::gpui::test]
fn every_button_has_a_hint(cx: &mut TestAppContext) {
    let (window, _tool, _panel) = open(cx, "build-tooltips");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, _| window.refresh());
    for &(selector, hint) in TOOLTIPS {
        assert!(!hint.is_empty(), "{selector} has a hint");
        assert!(
            visual.debug_bounds(selector).is_some(),
            "{selector} is on screen"
        );
    }
}
