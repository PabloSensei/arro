//! The Run, Tests and Debug tool windows without a real window and with the outside
//! world replaced: short commands, a test listener in a thread and the mock adapter of
//! the fixture language.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::{BreakpointSpec, DebugService, Debugger, Launch, Snapshot};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::{
    RunConfig, RunConfigProvider, RunPlan, TestListen, TestProvider, TestRequest, TestSite,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::debug::ActiveDebug;
use crate::run::{ProjectSetup, RunPanel};
use crate::script::ActiveRunPanel;
#[cfg(windows)]
use crate::window_tests::{active_path, wait_for_process};
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_window_with, press, sample_project, scratch,
    type_text,
};

/// How long a test waits for an external process, a poll of the panel or the adapter.
const LIMIT: Duration = Duration::from_secs(15);
/// One turn of the simulated clock: the panels poll every 50 ms.
const TICK: Duration = Duration::from_millis(50);

/// Runs `check` until it holds, letting the real threads and the panel timers work.
pub(crate) fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
}

pub(crate) fn run_panel(cx: &TestAppContext) -> gpui::Entity<RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

pub(crate) fn debug_panel(cx: &TestAppContext) -> gpui::Entity<crate::debug::DebugPanel> {
    cx.read(|cx| cx.global::<ActiveDebug>().0.clone())
}

fn run_text(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| panel.text())
}

fn run_state(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> workspace::RunState {
    panel.read_with(cx, |panel, _| panel.state())
}

pub(crate) fn snapshot(cx: &TestAppContext) -> Snapshot {
    debug_panel(cx).read_with(cx, |panel, _| panel.snapshot().clone())
}

/// A build tool that answers at once with a model of the scratch project.
struct StaticTool(ProjectModel);

impl BuildSystem for StaticTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

pub(crate) fn setup(root: &Path) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(StaticTool(ProjectModel {
            root: root.to_path_buf(),
            tool: "fixture".into(),
            resolved: true,
            ..ProjectModel::default()
        }))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("run-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
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

/// A configuration whose program prints one line and exits.
pub(crate) struct EchoRun;

impl RunConfigProvider for EchoRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![config("Echo")])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "echo", "hello from run"]),
            grace: Duration::from_secs(1),
        })
    }
}

/// A configuration whose program runs far longer than the test.
pub(crate) struct LongRun {
    pub(crate) image: PathBuf,
}

impl RunConfigProvider for LongRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![config("Long")])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new(&self.image)
                .args(["-n", "120", "127.0.0.1"])
                // No helper in this test binary, so Stop goes straight to force.
                .ctrl_helper("no-such-helper.exe"),
            grace: Duration::from_secs(1),
        })
    }
}

/// A copy of a program under a name of its own: only this image tells the process apart
/// from the other copies of the same program. The folder of the test removes it.
pub(crate) fn long_image(name: &str) -> PathBuf {
    let system_root = std::env::var_os("SystemRoot").expect("a Windows machine");
    let source = PathBuf::from(system_root).join("System32").join("ping.exe");
    let image = test_support::scratch("window-run").join(format!("{name}.exe"));
    std::fs::copy(&source, &image).expect("the program is copied");
    image
}

fn is_running(image: &Path) -> bool {
    let name = image.file_name().unwrap().to_string_lossy().into_owned();
    let list = std::process::Command::new("tasklist")
        .args(["/fi", &format!("imagename eq {name}"), "/fo", "csv", "/nh"])
        .output()
        .expect("tasklist runs");
    String::from_utf8_lossy(&list.stdout).contains(&name)
}

/// The events of one test class: a passing case and a failing one with a frame in the
/// project.
const TEST_EVENTS: &[&str] = &[
    r#"{"event":"plan"}"#,
    r#"{"event":"started","id":"e","name":"Engine","kind":"suite"}"#,
    r#"{"event":"started","id":"e/c","parent":"e","name":"Vitest","kind":"suite","suite":"demo.Vitest"}"#,
    r#"{"event":"started","id":"e/c/passes","parent":"e/c","name":"passes()","kind":"case","suite":"demo.Vitest","case":"passes"}"#,
    r#"{"event":"finished","id":"e/c/passes","outcome":"passed","durationMs":2}"#,
    r#"{"event":"started","id":"e/c/fails","parent":"e/c","name":"fails()","kind":"case","suite":"demo.Vitest","case":"fails"}"#,
    r#"{"event":"finished","id":"e/c/fails","outcome":"failed","message":"expected 1, got 2","trace":"AssertionFailedError: expected\n\tat demo.Vitest.fails(Vitest.code:2)","durationMs":3}"#,
    r#"{"event":"planFinished"}"#,
];

/// A test provider that plays the listener from a thread and keeps every request.
struct FixtureTests {
    requests: Arc<Mutex<Vec<TestRequest>>>,
    image: PathBuf,
}

impl TestProvider for FixtureTests {
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
        let listen = listen.clone();
        let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) else {
            return Ok(RunPlan {
                before: None,
                program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
                grace: Duration::from_secs(1),
            });
        };
        let _ = writeln!(stream, "{}", listen.token);
        std::thread::spawn(move || {
            for line in TEST_EVENTS {
                if writeln!(stream, "{line}").is_err() {
                    return;
                }
            }
        });
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(
        &self,
        _: &ProjectModel,
        _: &TestRequest,
        _: &TestListen,
        _: &runner::TestDebug,
    ) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new(&self.image)
                .args(["-n", "120", "127.0.0.1"])
                .ctrl_helper("no-such-helper.exe"),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_ready(&self) -> Option<&'static str> {
        Some("Listening for transport dt_socket at address:")
    }
}

fn find_tests(text: &str, _: &Path) -> Vec<TestSite> {
    let found = text.lines().position(|line| line.contains("void fails"));
    found
        .map(|index| {
            vec![TestSite {
                suite: "demo.Vitest".into(),
                case: Some("fails".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

fn test_source(root: &Path) -> PathBuf {
    let directory = root.join("src").join("demo");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("Vitest.code");
    std::fs::write(
        &path,
        "class Vitest {\n    void fails() {\n        check();\n    }\n}\n",
    )
    .unwrap();
    path
}

/// A debug service that starts the mock adapter of the fixture language and records what
/// the window asked it to debug.
pub(crate) struct MockService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    starts: Mutex<Vec<Vec<BreakpointSpec>>>,
}

impl MockService {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            starts: Mutex::new(Vec::new()),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
        attach: Option<(String, u16)>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start().map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        self.starts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(breakpoints.clone());
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let (attach_host, attach_port) = match attach {
            Some((host, port)) => (Some(host), Some(port)),
            None => (None, None),
        };
        let launch = Launch {
            address,
            adapter_id: "fixture".into(),
            name,
            attach_host,
            attach_port,
            breakpoints,
            exception_filters,
            watch,
            ..Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
    }

    pub(crate) fn starts(&self) -> usize {
        self.starts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
    }

    fn last_breakpoints(&self) -> Vec<BreakpointSpec> {
        self.starts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last()
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn join(&self) {
        if let Some(server) = self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            server.join();
        }
    }
}

impl DebugService for MockService {
    fn start(
        &self,
        _program: ProcessSpec,
        name: String,
        _entry: Option<PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name, breakpoints, watch, exception_filters, None)
    }

    fn attach(
        &self,
        host: String,
        port: u16,
        name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(
            name,
            breakpoints,
            watch,
            exception_filters,
            Some((host, port)),
        )
    }
}

pub(crate) fn open_with(
    cx: &mut TestAppContext,
    root: &Path,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    let window = open_window_with(cx, root.to_path_buf(), setup);
    cx.run_until_parked();
    draw(cx, window);
    window
}

fn wait_for_config(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_for(cx, "a run configuration", |cx| {
        !run_state(cx, panel).configs.is_empty()
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_shift_f10_shows_the_output_and_the_exit_code(cx: &mut TestAppContext) {
    let root = sample_project("run-output");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    wait_for_config(cx, &panel);
    press(cx, window, "shift-f10");
    wait_for_process(cx, "the end of the program", |cx| {
        let text = run_text(cx, &panel);
        text.iter().any(|line| line.contains("hello from run"))
            && text
                .iter()
                .any(|line| line.contains("Process finished with exit code 0"))
    });
    assert!(!run_state(cx, &panel).running, "the program has finished");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn stop_ctrl_f2_ends_a_long_run(cx: &mut TestAppContext) {
    let root = sample_project("run-stop");
    let image = long_image("stop");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(LongRun {
        image: image.clone(),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    wait_for_config(cx, &panel);
    press(cx, window, "shift-f10");
    wait_for(cx, "the long program", |cx| run_state(cx, &panel).running);
    wait_for(cx, "the long program in the process list", |_| {
        is_running(&image)
    });

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped program", |cx| {
        !run_state(cx, &panel).running
            && run_text(cx, &panel)
                .iter()
                .any(|line| line.contains("Process finished with exit code"))
    });
    assert!(!is_running(&image), "the stopped process still runs");
    let _ = std::fs::remove_file(&image);
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn ctrl_shift_f10_runs_the_test_at_the_caret_and_the_tree_shows_the_results(
    cx: &mut TestAppContext,
) {
    crate::tree_ops::use_temporary_delete();
    let root = sample_project("tests-tree");
    let source = test_source(&root);
    let image = long_image("tests-tree");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(FixtureTests {
        requests: requests.clone(),
        image: image.clone(),
    }));
    setup.test_sites = find_tests;
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-shift-f10");
    wait_for(cx, "the tests to finish", |cx| {
        runs(&requests).len() == 1 && panel.read_with(cx, |panel, _| panel.idle())
    });
    let first = runs(&requests)[0].clone();
    assert_eq!(first.suite.as_deref(), Some("demo.Vitest"));
    assert_eq!(first.case.as_deref(), Some("fails"));
    let report = panel.read_with(cx, |panel, _| panel.test_report());
    assert!(
        report
            .iter()
            .any(|line| line.contains("passed") && line.contains("passes")),
        "the passing test is in the tree: {report:?}"
    );
    assert!(
        report
            .iter()
            .any(|line| line.contains("failed") && line.contains("fails")),
        "the failing test is in the tree: {report:?}"
    );

    // The "Rerun failed" chip runs only the failed case of the last run.
    draw(cx, window);
    click(cx, window, "tests-rerun-failed");
    wait_for(cx, "the failed tests to run again", |cx| {
        runs(&requests).len() == 2 && panel.read_with(cx, |panel, _| panel.idle())
    });
    let second = runs(&requests)[1].clone();
    assert_eq!(
        second.cases,
        vec![("demo.Vitest".to_owned(), "fails".to_owned())]
    );

    // A click on the failing test and on its frame opens the place in the editor.
    draw(cx, window);
    click(cx, window, "test-row-e/c/fails");
    draw(cx, window);
    let frame = panel
        .read_with(cx, |panel, _| panel.first_frame())
        .expect("the failure points into the project");
    click(
        cx,
        window,
        Box::leak(format!("test-frame-{frame}").into_boxed_str()),
    );
    assert_eq!(active_path(cx, window), Some(source.clone()));
    let view = editor_of(cx, window, &source);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 0));
    let _ = std::fs::remove_file(&image);
}

fn runs(requests: &Arc<Mutex<Vec<TestRequest>>>) -> Vec<TestRequest> {
    requests
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

#[ui_kit::gpui::test]
fn debug_shift_f9_stops_on_a_breakpoint_and_the_window_shows_the_session(cx: &mut TestAppContext) {
    let root = sample_project("debug-session");
    let source = root.join("src").join("main.txt");
    let service = MockService::new();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    setup.debug = Some(service.clone());
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    let debug = debug_panel(cx);

    wait_for_config(cx, &panel);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "ctrl-f8");
    wait_for(cx, "the breakpoint", |cx| {
        debug.read_with(&*cx, |panel, _| panel.breakpoint_lines(&source)) == vec![2]
    });

    press(cx, window, "shift-f9");
    wait_for(cx, "the breakpoint stop", |cx| {
        matches!(snapshot(cx).state, dap_client::DebugState::Stopped { .. })
    });
    let seen = snapshot(cx);
    assert_eq!(seen.threads.len(), 2, "the panel lists the threads");
    assert_eq!(seen.frames.len(), 2, "the panel lists the stack");
    assert_eq!(seen.scopes.len(), 2, "the panel lists the scopes");
    assert!(
        seen.variables
            .get(&100)
            .is_some_and(|variables| variables.iter().any(|item| item.name == "items")),
        "the local variables are there"
    );
    assert_eq!(
        service.last_breakpoints()[0].line,
        2,
        "the session started with the breakpoint of the window"
    );

    draw(cx, window);
    assert!(bounds_of(cx, window, "thread-1").size.height > ui_kit::px(0.));
    assert!(bounds_of(cx, window, "frame-12").size.height > ui_kit::px(0.));
    assert!(bounds_of(cx, window, "scope-100").size.height > ui_kit::px(0.));

    // A scope opens and a variable with children expands, as their clicks do.
    debug.update(cx, |panel, cx| panel.expand(100, cx));
    wait_for(cx, "the scope to open", |cx| {
        debug.read_with(&*cx, |panel, _| panel.is_expanded(100))
    });
    debug.update(cx, |panel, cx| panel.toggle_expanded(101, cx));
    wait_for(cx, "the children of the variable", |cx| {
        snapshot(cx)
            .variables
            .get(&101)
            .is_some_and(|items| items.len() == 2)
    });
    draw(cx, window);
    assert!(bounds_of(cx, window, "var-items").size.height > ui_kit::px(0.));

    // Add Watch asks for an expression and the adapter answers it.
    click(cx, window, "debug-watch");
    draw(cx, window);
    assert!(bounds_of(cx, window, "prompt").size.height > ui_kit::px(0.));
    type_text(cx, window, "1+1");
    press(cx, window, "enter");
    wait_for(cx, "the watch value", |cx| {
        snapshot(cx)
            .watch
            .iter()
            .any(|watch| watch.expression == "1+1" && watch.value == "42")
    });

    // F8, F7 and Shift+F8 step and stop again; F9 continues to the next breakpoint.
    for keys in ["f8", "f7", "shift-f8"] {
        press(cx, window, keys);
        wait_for(
            cx,
            "a step to stop",
            |cx| matches!(&snapshot(cx).state, dap_client::DebugState::Stopped { reason } if reason == "step"),
        );
    }
    press(cx, window, "f9");
    wait_for(
        cx,
        "the next breakpoint stop",
        |cx| matches!(&snapshot(cx).state, dap_client::DebugState::Stopped { reason } if reason == "breakpoint"),
    );

    // Alt+F8 evaluates in the selected frame and the answer reaches the console.
    press(cx, window, "alt-f8");
    draw(cx, window);
    type_text(cx, window, "6*7");
    press(cx, window, "enter");
    wait_for(cx, "the evaluation", |cx| {
        let seen = snapshot(cx);
        seen.console.iter().any(|line| line.text == "> 6*7")
            && seen.console.iter().any(|line| line.text == "42")
    });

    // Stop ends the session.
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the session", |cx| {
        snapshot(cx).state == dap_client::DebugState::Terminated
            && !debug.read_with(cx, |panel, _| panel.state()).0
    });
    service.join();
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn stop_during_the_debug_start_of_a_test_from_the_tree(cx: &mut TestAppContext) {
    crate::tree_ops::use_temporary_delete();
    let root = sample_project("debug-test-stop");
    let source = test_source(&root);
    let image = long_image("debug-test-stop");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let service = MockService::new();
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(FixtureTests {
        requests: requests.clone(),
        image: image.clone(),
    }));
    setup.test_sites = find_tests;
    setup.debug = Some(service.clone());
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    wait_for(cx, "the project to load", |cx| {
        run_state(cx, &panel).hint.is_none()
    });
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-shift-f10");
    wait_for(cx, "the tree of tests", |cx| {
        panel.read_with(cx, |panel, _| panel.idle()) && runs(&requests).len() == 1
    });

    // The selected failing test starts under the debugger from the tree. The action is
    // called directly: a click or a key runs the executor, and the debug start waits for
    // its port there, so Stop could not come while the window is waiting.
    let selected = panel.update(cx, |panel, cx| panel.select_test("fails", cx));
    assert!(selected, "the failed test is in the tree");
    draw(cx, window);
    assert!(bounds_of(cx, window, "tests-debug-selected").size.height > ui_kit::px(0.));
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.debug_selected_test(window, cx));
        })
        .unwrap();
    assert!(
        run_state(cx, &panel).running,
        "the window waits for the test JVM"
    );

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped debug start", |cx| {
        panel.read_with(cx, |panel, _| panel.idle())
    });
    assert!(!is_running(&image), "the stopped test process still runs");
    assert_eq!(service.starts(), 0, "the window attached after Stop");
    assert!(
        run_text(cx, &panel)
            .iter()
            .any(|line| line.contains("Stopped by the user")),
        "the window says why the debug start ended"
    );
    let _ = std::fs::remove_file(&image);
}
