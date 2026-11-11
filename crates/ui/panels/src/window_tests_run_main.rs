//! Run marks of a program in the gutter, Run at the caret and temporary configurations,
//! without a real window: a substitute provider says which lines start a program.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Module, ProjectModel, TaskOptions,
};
use runner::{
    LaunchPoint, RunConfig, RunConfigProvider, RunPlan, TestListen, TestProvider, TestRequest,
    TestSite, configs_file, load_configs,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{TASKS_TOOL, Workspace};

use crate::run::RunPanel;
use crate::window_tests::{draw, editor_of, open_path, press, sample_project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The configurations this provider planned, so a test sees what started.
type Planned = Arc<Mutex<Vec<RunConfig>>>;
/// The test requests this provider received.
type Requests = Arc<Mutex<Vec<TestRequest>>>;

/// Starts the program of the line `main` is on and names it after the class.
struct MainRun {
    planned: Planned,
}

impl RunConfigProvider for MainRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        self.planned
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(config.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "echo", "started"]),
            grace: Duration::from_secs(1),
        })
    }

    fn launch_points(&self, text: &str, _: &Path, _: &ProjectModel) -> Vec<LaunchPoint> {
        if !text.contains("void main") {
            return Vec::new();
        }
        let lines: Vec<&str> = text.lines().collect();
        let name = lines
            .iter()
            .find_map(|line| {
                let mut words = line.split_whitespace();
                words
                    .position(|word| word == "class")
                    .and_then(|at| words.nth(at))
            })
            .unwrap_or("Main");
        let config = RunConfig {
            name: name.into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
        };
        let mut points = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if line.contains("class ") || line.contains("void main") {
                points.push(LaunchPoint {
                    line: index as u32 + 1,
                    config: config.clone(),
                });
            }
        }
        points
    }
}

/// A test provider that records the request and prints nothing.
struct CountingTests {
    requests: Requests,
}

impl TestProvider for CountingTests {
    fn plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        _: &TestListen,
    ) -> base::Result<RunPlan> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "echo", "tests"]),
            grace: Duration::from_secs(1),
        })
    }
}

fn config(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "fixture".into(),
        assemble_first: false,
        settings: Default::default(),
    }
}

fn main_source(root: &Path) -> PathBuf {
    let path = root.join("src").join("Main.code");
    std::fs::write(
        &path,
        "class Main {\n    void main() {\n        run();\n    }\n}\n",
    )
    .unwrap();
    path
}

fn test_source(root: &Path) -> PathBuf {
    let path = root.join("src").join("Vitest.code");
    std::fs::write(
        &path,
        "class Vitest {\n    void fails() {\n        check();\n    }\n}\n",
    )
    .unwrap();
    path
}

fn find_tests(text: &str, _: &Path) -> Vec<TestSite> {
    text.lines()
        .position(|line| line.contains("void fails"))
        .map(|index| {
            vec![TestSite {
                suite: "demo.Vitest".into(),
                case: Some("fails".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

fn labels(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .state()
            .configs
            .iter()
            .map(ToString::to_string)
            .collect()
    })
}

fn menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.menu_entries().is_some_and(|entries| {
                    entries.iter().any(|entry| entry.label.as_ref() == label)
                })
            })
            .unwrap()
    });
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    click_selector(cx, window, &format!("menu-{index}"));
}

fn open_main(cx: &mut TestAppContext, name: &str) -> (PathBuf, Planned, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let planned = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(MainRun {
        planned: planned.clone(),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    (root, planned, window)
}

#[ui_kit::gpui::test]
fn a_main_line_has_a_mark_that_opens_the_run_menu(cx: &mut TestAppContext) {
    let (_root, _planned, window) = open_main(cx, "run-main-mark");
    let source = main_source(&_root);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    wait_for(cx, "the marks of the program", |cx| {
        view.read_with(cx, |view, _| view.run_marks().len() >= 2)
    });
    let marks: Vec<u32> = view.read_with(cx, |view, _| {
        view.run_marks().iter().map(|mark| mark.line).collect()
    });
    assert!(marks.contains(&0), "the class line has a mark: {marks:?}");
    assert!(marks.contains(&1), "the main line has a mark: {marks:?}");

    window
        .update(cx, |workspace, window, cx| {
            crate::marks::show_launch_menu(&view, 1, workspace, window, cx);
        })
        .unwrap();
    draw(cx, window);
    let entries = menu_labels(cx, window);
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0], "Run 'Main'");
    assert_eq!(entries[1], "Debug 'Main'");
    assert_eq!(entries[2], "Modify Run Configuration...");
}

#[ui_kit::gpui::test]
fn run_from_the_menu_creates_a_selected_temporary_configuration(cx: &mut TestAppContext) {
    let (root, planned, window) = open_main(cx, "run-main-menu");
    let source = main_source(&root);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    wait_for(cx, "the mark of main", |cx| {
        view.read_with(cx, |view, _| view.run_marks().len() >= 2)
    });

    window
        .update(cx, |workspace, window, cx| {
            crate::marks::show_launch_menu(&view, 1, workspace, window, cx);
        })
        .unwrap();
    draw(cx, window);
    click_menu(cx, window, "Run 'Main'");
    wait_for(cx, "the program", |_cx| {
        planned
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 1
    });
    let panel = run_panel(cx);
    wait_for(cx, "the end of the program", |cx| {
        panel.read_with(cx, |panel, _| panel.idle())
    });
    let state = panel.read_with(cx, |panel, _| panel.state());
    assert_eq!(state.label().as_ref(), "Main");
    let selected = state.selected.expect("the new configuration is selected");
    assert!(
        state.is_temporary(selected),
        "the configuration is temporary"
    );
    assert!(
        !configs_file(&root).exists(),
        "a temporary configuration is not written"
    );
}

#[ui_kit::gpui::test]
fn the_sixth_temporary_configuration_pushes_the_first_out(cx: &mut TestAppContext) {
    let (root, _planned, window) = open_main(cx, "run-main-five");
    let panel = run_panel(cx);

    for number in 1..=6 {
        panel.update(cx, |panel, cx| {
            panel.choose_configuration(config(&format!("Temporary {number}")), cx)
        });
    }
    draw(cx, window);

    let state = panel.read_with(cx, |panel, _| panel.state());
    assert_eq!(
        labels(cx, &panel),
        vec![
            "Temporary 2",
            "Temporary 3",
            "Temporary 4",
            "Temporary 5",
            "Temporary 6"
        ]
    );
    assert!(state.temporary.iter().all(|temporary| *temporary));
    assert!(!configs_file(&root).exists());
}

#[ui_kit::gpui::test]
fn save_configuration_writes_the_file(cx: &mut TestAppContext) {
    let (root, _planned, window) = open_main(cx, "run-main-save");
    let panel = run_panel(cx);
    panel.update(cx, |panel, cx| {
        panel.choose_configuration(config("Saved"), cx)
    });
    draw(cx, window);

    click_selector(cx, window, "t-config");
    click_menu(cx, window, "Save 'Saved' Configuration");
    wait_for(cx, "the saved configuration", |_| {
        load_configs(&configs_file(&root))
            .is_ok_and(|configs| configs.iter().any(|config| config.name == "Saved"))
    });

    let state = panel.read_with(cx, |panel, _| panel.state());
    let selected = state.selected.unwrap();
    assert!(
        !state.is_temporary(selected),
        "it is a stored configuration now"
    );
}

#[ui_kit::gpui::test]
fn a_temporary_configuration_is_saved_when_the_dialog_edits_it(cx: &mut TestAppContext) {
    let (root, _planned, window) = open_main(cx, "run-main-dialog");
    let panel = run_panel(cx);
    panel.update(cx, |panel, cx| {
        panel.choose_configuration(config("Draft"), cx)
    });
    draw(cx, window);

    click_selector(cx, window, "t-config");
    click_menu(cx, window, "Edit Configurations...");
    draw(cx, window);
    click_selector(cx, window, "run-config-name");
    press(cx, window, "ctrl-a backspace");
    cx.simulate_input(window.into(), "Saved draft");
    click_selector(cx, window, "run-config-ok");
    wait_for(cx, "the edited configuration", |_| {
        load_configs(&configs_file(&root))
            .is_ok_and(|configs| configs.iter().any(|config| config.name == "Saved draft"))
    });
    let state = panel.read_with(cx, |panel, _| panel.state());
    let selected = state
        .selected
        .expect("the edited configuration is selected");
    assert!(!state.is_temporary(selected));
}

#[ui_kit::gpui::test]
fn ctrl_shift_f10_runs_main_at_the_caret(cx: &mut TestAppContext) {
    let (root, planned, window) = open_main(cx, "run-main-caret");
    let source = main_source(&root);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    wait_for(cx, "the marks of the program", |cx| {
        view.read_with(cx, |view, _| view.run_marks().len() >= 2)
    });
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-shift-f10");
    wait_for(cx, "the program of main", |_| {
        planned
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 1
    });
    let panel = run_panel(cx);
    wait_for(cx, "the end of the program", |cx| {
        panel.read_with(cx, |panel, _| panel.idle())
    });
    assert_eq!(
        planned.lock().unwrap_or_else(|error| error.into_inner())[0].name,
        "Main"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_f10_runs_a_test_when_the_caret_is_in_one(cx: &mut TestAppContext) {
    let root = sample_project("run-main-test-caret");
    let source = test_source(&root);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(CountingTests {
        requests: requests.clone(),
    }));
    setup.test_sites = find_tests;
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-shift-f10");
    wait_for(cx, "the test run", |cx| {
        requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            == 1
            && panel.read_with(cx, |panel, _| panel.idle())
    });
    let first = requests.lock().unwrap_or_else(|error| error.into_inner())[0].clone();
    assert_eq!(first.suite.as_deref(), Some("demo.Vitest"));
    assert_eq!(first.case.as_deref(), Some("fails"));
}

/// A task the build tool was asked to run: the module and the task.
type Tasks = Arc<Mutex<Vec<(Option<String>, String)>>>;

/// A build tool with one module and one task.
struct TaskTool {
    model: ProjectModel,
    tasks: Tasks,
}

impl TaskTool {
    fn new(root: &Path) -> Self {
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fake".into(),
                resolved: true,
                modules: vec![Module {
                    id: "app".into(),
                    name: "app".into(),
                    dir: root.to_path_buf(),
                    ..Module::default()
                }],
                ..ProjectModel::default()
            },
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl BuildSystem for TaskTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &base::Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn task_tree(&self, _: &ProjectModel) -> Vec<BuildModule> {
        vec![BuildModule {
            id: "app".into(),
            name: "app".into(),
            groups: vec![BuildGroup {
                name: "Tasks".into(),
                tasks: vec![BuildTask {
                    name: "test".into(),
                    program: false,
                }],
            }],
        }]
    }

    fn task_in(
        &self,
        _: &Path,
        module: Option<&Module>,
        task: &str,
        _: TaskOptions,
    ) -> base::Result<ProcessSpec> {
        self.tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((module.map(|module| module.id.clone()), task.to_owned()));
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", task]))
    }
}

#[ui_kit::gpui::test]
fn create_run_configuration_from_the_build_window_makes_a_task(cx: &mut TestAppContext) {
    let root = sample_project("run-main-build");
    let tool = Arc::new(TaskTool::new(&root));
    let mut setup = setup(&root);
    setup.tool = Some(tool.clone());
    let window = open_with(cx, &root, setup);
    let panel = cx.read(|cx| cx.global::<crate::script::ActiveBuildPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TASKS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the task tree", |cx| {
        !panel.read_with(cx, |panel, cx| panel.labels(cx)).is_empty()
    });
    let rows = panel.read_with(cx, |panel, cx| panel.labels(cx));
    let task_row = rows
        .iter()
        .position(|row| row.trim() == "test")
        .expect("the task row");
    panel.update(cx, |panel, cx| {
        panel.show_menu(task_row, gpui::point(gpui::px(20.), gpui::px(20.)), cx)
    });
    draw(cx, window);
    let entries = menu_labels(cx, window);
    assert!(
        entries
            .iter()
            .any(|entry| entry == "Create Run Configuration 'app [test]'"),
        "{entries:?}"
    );
    click_menu(cx, window, "Create Run Configuration 'app [test]'");
    draw(cx, window);

    let run = run_panel(cx);
    let state = run.read_with(cx, |panel, _| panel.state());
    let selected = state.selected.expect("the configuration is selected");
    assert_eq!(state.configs[selected].as_ref(), "app [test]");
    assert!(!state.is_temporary(selected), "the build window stores it");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("run-configs").is_some(),
        "the dialog opens on the configuration"
    );

    click_selector(cx, window, "run-config-ok");
    wait_for(cx, "the saved task configuration", |_| {
        load_configs(&configs_file(&root))
            .is_ok_and(|configs| configs.iter().any(|config| config.name == "app [test]"))
    });
    press(cx, window, "shift-f10");
    wait_for(cx, "the task", |_| {
        !tool
            .tasks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    });
    assert_eq!(
        tool.tasks.lock().unwrap_or_else(|error| error.into_inner())[0],
        (Some("app".to_owned()), "test".to_owned())
    );
}

#[ui_kit::gpui::test]
fn a_run_file_changed_on_disk_reaches_the_list_without_polling(cx: &mut TestAppContext) {
    let root = sample_project("run-main-watch");
    // The metadata folder is usually ignored by Git, and the file table follows it.
    std::fs::write(root.join(".gitignore"), ".arro/\n").unwrap();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(MainRun {
        planned: Arc::new(Mutex::new(Vec::new())),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    let _ = window;

    let stored = config("From disk");
    std::fs::create_dir_all(configs_file(&root).parent().unwrap()).unwrap();
    runner::save_configs(&configs_file(&root), &[stored]).unwrap();

    wait_for(cx, "the configuration from the disk", |cx| {
        labels(cx, &panel).contains(&"From disk".to_owned())
    });
}
