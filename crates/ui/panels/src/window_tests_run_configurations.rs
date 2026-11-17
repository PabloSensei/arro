//! Run configurations with their before launch steps, compound children, the question of
//! a second Run, the environment file, automatic tests and their history, without a real
//! window and with substitute processes.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::{
    BeforeLaunchStep, RunConfig, RunConfigProvider, RunPlan, TestListen, TestProvider, TestRequest,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{draw, press, sample_project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The setting that tells the substitute provider what its program does.
const PROGRAM: &str = "program";
/// The image of a long program, for this run of the tests only.
const IMAGE: &str = "image";

/// A provider whose program comes from the settings of a configuration, so one test can
/// store programs of several kinds in one file.
struct ScriptedRun {
    planned: Arc<Mutex<Vec<String>>>,
}

impl ScriptedRun {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let planned = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                planned: planned.clone(),
            },
            planned,
        )
    }
}

impl RunConfigProvider for ScriptedRun {
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
            .push(config.name.clone());
        let setting = |key: &str| {
            config
                .settings
                .get(key)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        let program = match setting(PROGRAM).as_str() {
            "echo" => ProcessSpec::new("cmd").args(["/d", "/c", "echo", "the program"]),
            "marker" => {
                let path = setting("marker");
                ProcessSpec::new("cmd").args(["/d", "/c", &format!("echo started>\"{path}\"")])
            }
            "env" => ProcessSpec::new("cmd")
                .args(["/d", "/c", "echo", "%RUN_MARK%", "%ORDER%"])
                .env("ORDER", "config"),
            "long" => ProcessSpec::new(setting(IMAGE))
                .args(["-n", "120", "127.0.0.1"])
                .ctrl_helper("no-such-helper.exe"),
            other => return Err(format!("no program {other}").into()),
        };
        Ok(RunPlan {
            before: None,
            program,
            grace: Duration::from_secs(1),
        })
    }
}

/// A build tool whose tasks are short echoes, a failing command or a long program, so a
/// test sees the order of the before launch steps.
struct StepTool {
    model: ProjectModel,
    long: PathBuf,
}

impl StepTool {
    fn new(root: &Path, long: &Path) -> Self {
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fixture".into(),
                resolved: true,
                ..ProjectModel::default()
            },
            long: long.to_path_buf(),
        }
    }
}

impl BuildSystem for StepTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", "build-step"]))
    }

    fn goal(
        &self,
        _: &Path,
        line: &str,
        _: project_model::TaskOptions,
    ) -> base::Result<ProcessSpec> {
        Ok(match line {
            "fail" => ProcessSpec::new("cmd").args(["/d", "/c", "exit", "7"]),
            "long" => ProcessSpec::new(&self.long)
                .args(["-n", "120", "127.0.0.1"])
                .ctrl_helper("no-such-helper.exe"),
            _ => ProcessSpec::new("cmd").args(["/d", "/c", "echo", "before-step"]),
        })
    }
}

fn config(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "fixture".into(),
        assemble_first: false,
        settings: Default::default(),
        ..RunConfig::default()
    }
}

fn program(name: &str, kind: &str) -> RunConfig {
    let mut config = config(name);
    config
        .settings
        .insert(PROGRAM.into(), kind.to_owned().into());
    config
}

fn store(root: &Path, configs: &[RunConfig]) {
    let file = runner::configs_file(root);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    runner::save_configs(&file, configs).unwrap();
}

/// A copy of a program under a name of this test binary, so parallel runs of the tests
/// never see each other's processes.
fn long_image(name: &str) -> PathBuf {
    let system_root = std::env::var_os("SystemRoot").expect("a Windows machine");
    let source = PathBuf::from(system_root).join("System32").join("ping.exe");
    let image = test_support::scratch("window-run-configurations")
        .join(format!("{name}-{}.exe", std::process::id()));
    std::fs::copy(&source, &image).expect("the program is copied");
    image
}

fn running_copies(image: &Path) -> usize {
    let name = image.file_name().unwrap().to_string_lossy().into_owned();
    let list = std::process::Command::new("tasklist")
        .args(["/fi", &format!("imagename eq {name}"), "/fo", "csv", "/nh"])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter(|line| line.contains(&name))
        .count()
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

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) -> bool {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

fn text(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| panel.text())
}

fn tabs(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| panel.content_tabs())
}

/// A test provider that plays the listener from a thread and numbers its tests by run, so
/// the tree of an earlier run is told apart from the one after it.
struct NumberedTests {
    requests: Arc<Mutex<Vec<TestRequest>>>,
    runs: Arc<Mutex<usize>>,
}

impl NumberedTests {
    fn new() -> (Self, Arc<Mutex<Vec<TestRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                requests: requests.clone(),
                runs: Arc::new(Mutex::new(0)),
            },
            requests,
        )
    }

    fn events(&self) -> (Vec<String>, usize) {
        let mut runs = self.runs.lock().unwrap_or_else(|error| error.into_inner());
        *runs += 1;
        let number = *runs;
        let events = vec![
            r#"{"event":"plan"}"#.to_owned(),
            r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#.to_owned(),
            r#"{"event":"started","id":"e/c","parent":"e","name":"Vitest","kind":"suite","suite":"demo.Vitest"}"#
                .to_owned(),
            format!(
                r#"{{"event":"started","id":"e/c/passes-{number}","parent":"e/c","name":"passes-{number}()","kind":"case","suite":"demo.Vitest","case":"passes-{number}"}}"#
            ),
            format!(
                r#"{{"event":"finished","id":"e/c/passes-{number}","outcome":"passed","durationMs":2}}"#
            ),
            r#"{"event":"planFinished"}"#.to_owned(),
        ];
        (events, number)
    }
}

impl TestProvider for NumberedTests {
    fn plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        let (events, _) = self.events();
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) {
            let _ = writeln!(stream, "{}", listen.token);
            std::thread::spawn(move || {
                for line in events {
                    if writeln!(stream, "{line}").is_err() {
                        return;
                    }
                }
            });
        }
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

#[ui_kit::gpui::test]
fn the_dialog_stores_the_before_launch_list_the_flag_and_the_environment_file(
    cx: &mut TestAppContext,
) {
    let root = sample_project("run-config-dialog-steps");
    let mut app = program("App", "echo");
    app.assemble_first = true;
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    crate::window_tests::open_palette(cx, window, "run.edit_configurations");
    draw(cx, window);
    assert!(
        has(cx, window, "run-config-step-0"),
        "the build is a step of the list"
    );
    click_selector(cx, window, "run-config-step-add-task");
    draw(cx, window);
    click_selector(cx, window, "run-config-step-target-1");
    crate::window_tests::type_text(cx, window, "package");
    click_selector(cx, window, "run-config-environment");
    crate::window_tests::type_text(cx, window, ".env");
    click_selector(cx, window, "run-config-multiple");
    draw(cx, window);
    click_selector(cx, window, "run-config-ok");
    wait_for(cx, "the saved configuration", |_| {
        runner::load_configs(&runner::configs_file(&root)).is_ok_and(|configs| {
            configs.first().is_some_and(|config| {
                config.before_launch
                    == Some(vec![
                        BeforeLaunchStep::Build,
                        BeforeLaunchStep::Task {
                            target: "package".into(),
                        },
                    ])
            })
        })
    });
    let saved = runner::load_configs(&runner::configs_file(&root)).unwrap();
    assert!(saved[0].allow_multiple_instances);
    assert_eq!(
        saved[0].environment_file.as_deref(),
        Some(Path::new(".env"))
    );
    assert!(!saved[0].assemble_first, "the list owns the build now");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn before_launch_steps_run_before_the_program(cx: &mut TestAppContext) {
    let root = sample_project("run-before-launch");
    let long = long_image("before-launch");
    let mut app = program("App", "echo");
    app.before_launch = Some(vec![BeforeLaunchStep::Task {
        target: "ok".into(),
    }]);
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.tool = Some(Arc::new(StepTool::new(&root, &long)));
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the program", |cx| {
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("the program"))
    });
    let lines = text(cx, &panel);
    let step = lines
        .iter()
        .position(|line| line.contains("before-step"))
        .expect("the step printed its line");
    let program = lines
        .iter()
        .position(|line| line.contains("the program"))
        .expect("the program printed its line");
    assert!(step < program, "the step goes first: {lines:?}");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_failed_before_launch_step_keeps_the_program_from_starting(cx: &mut TestAppContext) {
    let root = sample_project("run-before-launch-fails");
    let long = long_image("before-launch-fails");
    let marker = root.join("program-started.txt");
    let mut app = program("App", "marker");
    app.settings.insert(
        "marker".into(),
        marker.to_string_lossy().into_owned().into(),
    );
    app.before_launch = Some(vec![BeforeLaunchStep::Task {
        target: "fail".into(),
    }]);
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.tool = Some(Arc::new(StepTool::new(&root, &long)));
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the failed step", |cx| {
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("before launch step failed"))
    });
    assert!(!marker.exists(), "the program started after the failure");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn compound_runs_two_configurations_and_stop_ends_both(cx: &mut TestAppContext) {
    let root = sample_project("run-compound");
    let first = long_image("compound-first");
    let second = long_image("compound-second");
    let mut one = program("One", "long");
    one.settings
        .insert(IMAGE.into(), first.to_string_lossy().into_owned().into());
    let mut two = program("Two", "long");
    two.settings
        .insert(IMAGE.into(), second.to_string_lossy().into_owned().into());
    let mut both = config("Both");
    both.provider = runner::COMPOUND_PROVIDER.into();
    both.compound = vec!["One".into(), "Two".into()];
    store(&root, &[one, two, both]);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the compound configuration", |cx| {
        panel.read_with(cx, |panel, _| panel.state().configs.len()) == 3
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("Both", cx)));

    press(cx, window, "shift-f10");
    wait_for(cx, "both children", |_| {
        running_copies(&first) == 1 && running_copies(&second) == 1
    });
    let tabs = tabs(cx, &panel);
    assert!(
        tabs.iter().any(|tab| tab.contains("One")) && tabs.iter().any(|tab| tab.contains("Two")),
        "each child has a tab: {tabs:?}"
    );

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped children", |_| {
        running_copies(&first) == 0 && running_copies(&second) == 0
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_second_run_asks_and_restarts(cx: &mut TestAppContext) {
    let root = sample_project("run-second-asks");
    let image = long_image("second-asks");
    let mut app = program("App", "long");
    app.settings
        .insert(IMAGE.into(), image.to_string_lossy().into_owned().into());
    store(&root, &[app]);
    let (provider, planned) = ScriptedRun::new();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(provider)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the long program", |_| running_copies(&image) == 1);
    press(cx, window, "shift-f10");
    draw(cx, window);
    assert!(
        has(cx, window, "run-confirm"),
        "the second Run asks before it stops the program"
    );
    assert_eq!(
        planned.lock().unwrap().len(),
        1,
        "nothing starts before the answer"
    );

    click_selector(cx, window, "run-confirm-rerun");
    wait_for(cx, "the restarted program", |_| {
        planned.lock().unwrap().len() == 2 && running_copies(&image) == 1
    });
    assert!(!has(cx, window, "run-confirm"), "the question is gone");
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped program", |_| running_copies(&image) == 0);
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn allow_multiple_instances_opens_another_tab(cx: &mut TestAppContext) {
    let root = sample_project("run-multiple");
    let image = long_image("multiple");
    let mut app = program("App", "long");
    app.allow_multiple_instances = true;
    app.settings
        .insert(IMAGE.into(), image.to_string_lossy().into_owned().into());
    store(&root, &[app]);
    let (provider, planned) = ScriptedRun::new();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(provider)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the first instance", |_| running_copies(&image) == 1);
    press(cx, window, "shift-f10");
    wait_for(cx, "the second instance", |_| {
        planned.lock().unwrap().len() == 2 && running_copies(&image) == 2
    });
    let tabs = tabs(cx, &panel);
    assert_eq!(
        tabs.iter().filter(|tab| tab.contains("App")).count(),
        2,
        "each instance has a tab: {tabs:?}"
    );

    press(cx, window, "ctrl-f2");
    wait_for(cx, "both stopped", |_| running_copies(&image) == 0);
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_environment_file_reaches_the_program(cx: &mut TestAppContext) {
    let root = sample_project("run-environment-file");
    let file = root.join(".env");
    std::fs::write(
        &file,
        "# the values of the file\nRUN_MARK=from-file\nexport ORDER=from-file\nEMPTY=\n",
    )
    .unwrap();
    let mut app = program("App", "env");
    app.environment_file = Some(".env".into());
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the program", |cx| {
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("Process finished"))
    });
    let lines = text(cx, &panel);
    assert!(
        lines.iter().any(|line| line.contains("from-file")),
        "the value of the file is in the environment: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("config")),
        "the value of the configuration wins over the file: {lines:?}"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_broken_environment_file_keeps_the_program_from_starting(cx: &mut TestAppContext) {
    let root = sample_project("run-environment-broken");
    let marker = root.join("program-started.txt");
    std::fs::write(root.join(".env"), "GOOD=1\nnot a variable\n").unwrap();
    let mut app = program("App", "marker");
    app.settings.insert(
        "marker".into(),
        marker.to_string_lossy().into_owned().into(),
    );
    app.environment_file = Some(".env".into());
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the parse error", |cx| {
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("line 2") && line.contains("NAME=value"))
    });
    assert!(!marker.exists(), "the program started with a broken file");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn stop_during_before_launch_keeps_the_program_from_starting(cx: &mut TestAppContext) {
    let root = sample_project("run-stop-before-launch");
    let long = long_image("stop-before-launch");
    let marker = root.join("program-started.txt");
    let mut app = program("App", "marker");
    app.settings.insert(
        "marker".into(),
        marker.to_string_lossy().into_owned().into(),
    );
    app.before_launch = Some(vec![BeforeLaunchStep::Task {
        target: "long".into(),
    }]);
    store(&root, &[app]);
    let mut setup = setup(&root);
    setup.tool = Some(Arc::new(StepTool::new(&root, &long)));
    setup.providers = vec![Arc::new(ScriptedRun::new().0)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    press(cx, window, "shift-f10");
    wait_for(cx, "the long step", |_| running_copies(&long) == 1);
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped step", |cx| {
        running_copies(&long) == 0 && panel.read_with(cx, |panel, _| panel.idle())
    });
    assert!(!marker.exists(), "the program started after Stop");
    assert!(
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("Launch cancelled")),
        "the console says why nothing started"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn auto_test_runs_again_after_a_save(cx: &mut TestAppContext) {
    let root = sample_project("run-auto-test");
    let source = root.join("src").join("main.txt");
    let (tests, requests) = NumberedTests::new();
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(tests));
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project", |cx| {
        panel.read_with(cx, |panel, _| panel.can_test())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::RUN_TOOL, window, cx)
        })
        .unwrap();
    crate::window_tests::open_path(cx, window, &source);
    let view = crate::window_tests::editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);

    panel.update(cx, |panel, cx| {
        panel.run_tests(
            TestRequest {
                suite: Some("demo.Vitest".into()),
                ..TestRequest::default()
            },
            cx,
        )
    });
    wait_for(cx, "the first test run", |cx| {
        !requests.lock().unwrap().is_empty() && panel.read_with(cx, |panel, _| panel.idle())
    });
    draw(cx, window);
    click_selector(cx, window, "tests-auto");
    assert!(panel.read_with(cx, |panel, _| panel.auto_test()));

    // The chip takes the focus; a click in the editor gives it back before typing.
    crate::window_tests::click(cx, window, "editor-body-0");
    crate::window_tests::type_text(cx, window, "changed ");
    press(cx, window, "ctrl-s");
    assert!(
        std::fs::read_to_string(&source)
            .unwrap()
            .contains("changed "),
        "the save reached the disk"
    );
    wait_for(cx, "the test run after the save", |cx| {
        requests.lock().unwrap().len() == 2 && panel.read_with(cx, |panel, _| panel.idle())
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_history_keeps_runs_and_opens_an_earlier_one(cx: &mut TestAppContext) {
    let root = sample_project("run-test-history");
    let (tests, requests) = NumberedTests::new();
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(tests));
    let state = test_support::scratch("run-test-history-state");
    setup.cache_root = state.clone();
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project", |cx| {
        panel.read_with(cx, |panel, _| panel.can_test())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::RUN_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    for _ in 0..2 {
        panel.update(cx, |panel, cx| {
            panel.run_tests(
                TestRequest {
                    suite: Some("demo.Vitest".into()),
                    ..TestRequest::default()
                },
                cx,
            )
        });
        let expected = requests.lock().unwrap().len() + 1;
        wait_for(cx, "a test run", |cx| {
            requests.lock().unwrap().len() == expected
                && panel.read_with(cx, |panel, _| panel.idle())
        });
    }
    wait_for(cx, "two stored runs", |cx| {
        panel.read_with(cx, |panel, _| panel.test_history().len()) == 2
    });
    let file = runner::test_history_file(state.as_path());
    assert_eq!(runner::load_test_history(&file).unwrap().len(), 2);

    draw(cx, window);
    click_selector(cx, window, "tests-history");
    draw(cx, window);
    assert!(
        has(cx, window, "tests-history-1"),
        "the list shows both runs"
    );
    click_selector(cx, window, "tests-history-1");
    draw(cx, window);
    let report = panel.read_with(cx, |panel, _| panel.test_report());
    assert!(
        report.iter().any(|line| line.contains("passes-1")),
        "the tree of the earlier run is shown: {report:?}"
    );
}

#[test]
fn the_before_launch_model_keeps_its_order() {
    // The dialog writes the list in its order; the runner reads it back unchanged.
    let config = RunConfig {
        before_launch: Some(vec![
            BeforeLaunchStep::Task {
                target: "package".into(),
            },
            BeforeLaunchStep::Build,
            BeforeLaunchStep::Configuration {
                name: "Other".into(),
            },
        ]),
        ..RunConfig::default()
    };
    assert_eq!(config.before_launch_steps()[1], BeforeLaunchStep::Build);
}
