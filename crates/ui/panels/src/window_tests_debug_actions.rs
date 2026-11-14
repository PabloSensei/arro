//! The debugger actions of the window without a window: Run to Cursor, Show Execution
//! Point, Smart Step Into, Quick Evaluate, Set Value and the watch list on the mock
//! adapter, and the same three on the real debugger at the end.
//!
//! `cargo test -p panels window_tests_debug_actions`
//!
//! The ignored tests at the end need `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_debug_actions -- --ignored --test-threads=1`

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::{BreakpointSpec, DebugService, DebugState, Debugger, Launch};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel, Unit};
use runner::{RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::debug::ActiveDebug;
use crate::run::{ProjectSetup, RunPanel};
use crate::script::ActiveRunPanel;
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_window_with, press, sample_project,
    scratch, type_text,
};

/// How long a test waits for the panel, the adapter or the program.
const LIMIT: Duration = Duration::from_secs(20);
/// One turn of the simulated clock: the panels poll every 50 ms.
const TICK: Duration = Duration::from_millis(50);

fn wait_for(
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

pub(crate) fn shown(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &str,
) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The model of the scratch project, with the output folder the probes use.
fn model(root: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        modules: vec![Module {
            id: "demo".into(),
            name: "demo".into(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".into(),
                output_dirs: vec![root.join("classes")],
                ..Unit::default()
            }],
        }],
        ..ProjectModel::default()
    }
}

struct ProbeTool {
    model: ProjectModel,
}

impl BuildSystem for ProbeTool {
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
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn build(&self, _: &Path, _: bool) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// Two configurations and a counter per name, so a test sees which one started.
struct Configs {
    runs: Arc<Mutex<Vec<String>>>,
}

impl RunConfigProvider for Configs {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(["Alpha", "Beta"]
            .into_iter()
            .map(|name| RunConfig {
                name: name.into(),
                provider: "fixture".into(),
                assemble_first: false,
                settings: Default::default(),
            })
            .collect())
    }

    fn plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(config.name.clone());
        Ok(plan())
    }

    fn debug_plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(format!("debug {}", config.name));
        Ok(plan())
    }
}

fn plan() -> RunPlan {
    RunPlan {
        before: None,
        program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
        grace: Duration::from_secs(1),
    }
}

/// One mock adapter session per start or attach.
pub(crate) struct MockService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    starts: AtomicUsize,
    /// The file the stack frame of the scenario names.
    source: PathBuf,
    /// Whether the adapter advertises the method and field requests and the frame
    /// restart among its capabilities.
    breakpoint_kinds: bool,
    /// Whether the adapter supports logpoints: a breakpoint with a message prints it and
    /// does not stop the program.
    log_points: bool,
}

impl MockService {
    pub(crate) fn new(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            starts: AtomicUsize::new(0),
            source,
            breakpoint_kinds: false,
            log_points: false,
        })
    }

    /// The same session on an adapter that lists the requests of method breakpoints,
    /// field watchpoints and frame restarts.
    pub(crate) fn with_breakpoint_kinds(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            starts: AtomicUsize::new(0),
            source,
            breakpoint_kinds: true,
            log_points: false,
        })
    }

    /// The same session on an adapter that supports logpoints.
    pub(crate) fn with_log_points(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            starts: AtomicUsize::new(0),
            source,
            breakpoint_kinds: false,
            log_points: true,
        })
    }

    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        let server = if self.log_points {
            lang_fixture::MockDap::start_with_log_points(self.source.to_string_lossy())
        } else if self.breakpoint_kinds {
            lang_fixture::MockDap::start_with_breakpoint_kinds_for(self.source.to_string_lossy())
        } else {
            lang_fixture::MockDap::start_with_stack_source(self.source.to_string_lossy())
        }
        .map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let launch = Launch {
            address,
            adapter_id: "fixture".into(),
            name,
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

    pub(crate) fn requests(&self) -> Vec<serde_json::Value> {
        self.server
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(lang_fixture::MockDap::requests)
            .unwrap_or_default()
    }

    fn join(&self) {
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
        self.session(name, breakpoints, watch, exception_filters)
    }

    fn attach(
        &self,
        _host: String,
        _port: u16,
        name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name, breakpoints, watch, exception_filters)
    }
}

pub(crate) struct Setup {
    pub(crate) window: WindowHandle<Workspace>,
    pub(crate) service: Arc<MockService>,
    pub(crate) runs: Arc<Mutex<Vec<String>>>,
    pub(crate) root: PathBuf,
}

pub(crate) fn setup(cx: &mut TestAppContext, name: &str) -> Setup {
    setup_with_service(cx, name, false)
}

/// The same window on an adapter that advertises the requests of method breakpoints,
/// field watchpoints and frame restarts.
pub(crate) fn setup_with_breakpoint_kinds(cx: &mut TestAppContext, name: &str) -> Setup {
    setup_with_modes(cx, name, true, false)
}

/// The same window on an adapter that supports logpoints.
pub(crate) fn setup_with_log_points(cx: &mut TestAppContext, name: &str) -> Setup {
    setup_with_modes(cx, name, false, true)
}

fn setup_with_service(cx: &mut TestAppContext, name: &str, breakpoint_kinds: bool) -> Setup {
    setup_with_modes(cx, name, breakpoint_kinds, false)
}

fn setup_with_modes(
    cx: &mut TestAppContext,
    name: &str,
    breakpoint_kinds: bool,
    log_points: bool,
) -> Setup {
    let root = sample_project(name);
    let runs = Arc::new(Mutex::new(Vec::new()));
    let source = root.join("src").join("main.txt");
    let service = if log_points {
        MockService::with_log_points(source)
    } else if breakpoint_kinds {
        MockService::with_breakpoint_kinds(source)
    } else {
        MockService::new(source)
    };
    let project = ProjectSetup {
        tool: Some(Arc::new(ProbeTool {
            model: model(&root),
        })),
        providers: vec![Arc::new(Configs { runs: runs.clone() })],
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("debug-actions-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: Some(service.clone()),
    };
    let window = open_window_with(cx, root.clone(), project);
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    assert!(
        panel.update(cx, |panel, cx| panel.select_named("Alpha", cx)),
        "the fixture configuration is there"
    );
    Setup {
        window,
        service,
        runs,
        root,
    }
}

/// Starts the debug session and waits for the first stop of the mock scenario.
pub(crate) fn stop_at_start(
    cx: &mut TestAppContext,
    setup: &Setup,
) -> gpui::Entity<crate::debug::DebugPanel> {
    press(cx, setup.window, "shift-f9");
    let debug = debug_panel(cx);
    wait_for(cx, "the first stop", |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });
    wait_for(cx, "the frames and variables", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        !snapshot.frames.is_empty() && !snapshot.variables.is_empty()
    });
    debug
}

/// Stops the session and waits until the mock adapter is free again.
pub(crate) fn end_session(
    cx: &mut TestAppContext,
    debug: &gpui::Entity<crate::debug::DebugPanel>,
    service: &MockService,
) {
    debug.update(cx, |panel, cx| panel.stop(cx));
    wait_for(cx, "the end of the session", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    service.join();
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn focus_panel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    debug: &gpui::Entity<crate::debug::DebugPanel>,
) {
    let focus = debug.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

/// The breakpoints the adapter was last told about for `file`.
pub(crate) fn last_breakpoint_lines(
    requests: &[serde_json::Value],
    file: &Path,
) -> Option<Vec<i64>> {
    let path = file.to_string_lossy().into_owned();
    requests
        .iter()
        .rfind(|request| {
            request["command"] == "setBreakpoints" && request["arguments"]["source"]["path"] == path
        })
        .and_then(|request| request["arguments"]["breakpoints"].as_array().cloned())
        .map(|list| {
            list.iter()
                .filter_map(|item| item["line"].as_i64())
                .collect()
        })
}

#[ui_kit::gpui::test]
fn run_to_cursor_stops_on_the_caret_and_takes_the_breakpoint_away(cx: &mut TestAppContext) {
    let setup = setup(cx, "run-to-cursor");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, setup.window);
    let debug = stop_at_start(cx, &setup);

    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty(),
        "Run to Cursor needs no breakpoint of the user"
    );
    press(cx, setup.window, "alt-f9");

    wait_for(cx, "the temporary breakpoint and the continue", |_| {
        let requests = setup.service.requests();
        requests
            .iter()
            .any(|request| request["command"] == "continue")
            && requests.iter().any(|request| {
                request["command"] == "setBreakpoints"
                    && request["arguments"]["breakpoints"]
                        .as_array()
                        .is_some_and(|list| {
                            list.iter().any(|item| item["line"].as_i64() == Some(2))
                        })
            })
    });
    // The adapter stops again at once: the temporary breakpoint has done its work.
    wait_for(cx, "the temporary breakpoint to go", |_| {
        last_breakpoint_lines(&setup.service.requests(), &path)
            .is_some_and(|lines| lines.is_empty())
    });
    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty(),
        "the temporary breakpoint is not one of the user"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn force_run_to_cursor_ignores_the_other_breakpoints_and_gives_them_back(cx: &mut TestAppContext) {
    let setup = setup(cx, "force-run-to-cursor");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| panel.toggle_breakpoint(path.clone(), 1, cx));
    view.update(cx, |view, cx| view.restore_caret(2, 0, cx));
    draw(cx, setup.window);
    stop_at_start(cx, &setup);

    press(cx, setup.window, "ctrl-alt-f9");

    wait_for(cx, "the breakpoints off and the continue", |_| {
        let requests = setup.service.requests();
        let user_off = requests.iter().any(|request| {
            request["command"] == "setBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| {
                        !list.is_empty() && list.iter().all(|item| item["line"].as_i64() != Some(1))
                    })
        });
        let continued = requests
            .iter()
            .any(|request| request["command"] == "continue");
        user_off && continued
    });
    wait_for(cx, "the breakpoints back", |_| {
        setup.service.requests().iter().any(|request| {
            request["command"] == "setBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|item| item["line"].as_i64() == Some(1)))
        })
    });
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![1],
        "the breakpoint of the user stays"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn show_execution_point_opens_the_frame(cx: &mut TestAppContext) {
    let setup = setup(cx, "execution-point");
    let debug = stop_at_start(cx, &setup);
    let frame = debug
        .read_with(cx, |panel, _| panel.snapshot().clone())
        .frames
        .iter()
        .find(|frame| frame.source.is_some() && frame.line > 0)
        .cloned()
        .expect("a frame with a source");

    press(cx, setup.window, "alt-f10");

    wait_for(cx, "the file of the frame", |cx| {
        active_path(cx, setup.window) == frame.source.clone()
    });
    assert!(
        editor_of(cx, setup.window, frame.source.as_ref().unwrap())
            .read_with(cx, |view, _| view.caret_line())
            .is_some(),
        "the editor of the frame is open"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn smart_step_into_lists_the_calls_and_steps_into_the_chosen_one(cx: &mut TestAppContext) {
    let setup = setup(cx, "smart-step");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);

    press(cx, setup.window, "shift-f7");
    wait_for(cx, "the list of calls", |cx| {
        shown(cx, setup.window, "step-targets")
    });
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "step-target-2"));

    click(cx, setup.window, "step-target-2");
    wait_for(cx, "the step into the chosen call", |_| {
        setup.service.requests().iter().any(|request| {
            request["command"] == "stepIn" && request["arguments"]["targetId"].as_i64() == Some(2)
        })
    });
    assert!(matches!(
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
        DebugState::Stopped { .. } | DebugState::Running
    ));
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn force_step_into_steps_without_filters(cx: &mut TestAppContext) {
    let setup = setup(cx, "force-step");
    let debug = stop_at_start(cx, &setup);

    press(cx, setup.window, "alt-shift-f7");

    wait_for(cx, "the step into", |_| {
        setup
            .service
            .requests()
            .iter()
            .any(|request| request["command"] == "stepIn")
    });
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn quick_evaluate_shows_the_value_at_the_caret_and_expands_a_field(cx: &mut TestAppContext) {
    let setup = setup(cx, "quick-evaluate");
    let path = setup.root.join("src").join("main.txt");
    std::fs::write(&path, "alpha\nitems\n").unwrap();
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 2, cx));
    draw(cx, setup.window);
    let debug = stop_at_start(cx, &setup);

    press(cx, setup.window, "ctrl-alt-f8");

    wait_for(cx, "the value of the caret", |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().evaluation.clone())
            .is_some_and(|evaluation| {
                evaluation.expression == "items" && evaluation.value == "ArrayList (2)"
            })
    });
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "quick-evaluate"));
    assert!(shown(cx, setup.window, "quick-evaluate-value"));

    click(cx, setup.window, "quick-evaluate-value");
    wait_for(cx, "the fields of the object", |cx| {
        debug.read_with(cx, |panel, _| {
            panel.snapshot().variables.get(&101).map(Vec::len)
        }) == Some(2)
    });
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "quick-evaluate-row-0"));
    press(cx, setup.window, "escape");
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn set_value_edits_the_selected_local_variable(cx: &mut TestAppContext) {
    let setup = setup(cx, "set-value");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);
    debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, setup.window);
    focus_panel(cx, setup.window, &debug);

    debug.update(cx, |panel, _| panel.select_variable_for_test(100, "text"));
    draw(cx, setup.window);
    press(cx, setup.window, "f2");
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "variable-edit"));

    type_text(cx, setup.window, "bye");
    press(cx, setup.window, "enter");

    wait_for(cx, "the new value", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        snapshot.variables.get(&100).is_some_and(|variables| {
            variables
                .iter()
                .any(|variable| variable.name == "text" && variable.value == "bye")
        })
    });
    assert!(
        setup
            .service
            .requests()
            .iter()
            .any(|request| request["command"] == "setVariable"),
        "the adapter received the new value"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn a_refused_value_shows_the_message_at_the_row(cx: &mut TestAppContext) {
    let setup = setup(cx, "set-value-error");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);
    debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, setup.window);
    focus_panel(cx, setup.window, &debug);

    debug.update(cx, |panel, _| panel.select_variable_for_test(100, "items"));
    press(cx, setup.window, "f2");
    type_text(cx, setup.window, "x");
    press(cx, setup.window, "enter");

    wait_for(cx, "the refusal of the adapter", |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().variable_error.clone())
            .is_some_and(|error| error.name == "items")
    });
    draw(cx, setup.window);
    assert!(
        shown(cx, setup.window, "variable-error"),
        "the message is shown at the row"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn insert_adds_a_watch_and_delete_takes_it_away(cx: &mut TestAppContext) {
    let setup = setup(cx, "watches");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);
    focus_panel(cx, setup.window, &debug);

    press(cx, setup.window, "insert");
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "watch-input"));
    type_text(cx, setup.window, "text");
    press(cx, setup.window, "enter");

    wait_for(cx, "the watch value", |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().watch.clone())
            .iter()
            .any(|watch| watch.expression == "text" && watch.value == "\"hello\"")
    });

    press(cx, setup.window, "delete");
    wait_for(cx, "the watch to go", |cx| {
        debug
            .read_with(cx, |panel, _| panel.watch_expressions())
            .is_empty()
            && debug
                .read_with(cx, |panel, _| panel.snapshot().watch.clone())
                .is_empty()
    });
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn rerun_ctrl_f5_starts_the_configuration_again(cx: &mut TestAppContext) {
    let setup = setup(cx, "rerun");
    let runs = run_panel(cx);

    press(cx, setup.window, "ctrl-f5");
    wait_for(cx, "the first run", |cx| {
        runs.read_with(cx, |panel, _| panel.idle())
            && !setup
                .runs
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
    });
    press(cx, setup.window, "ctrl-f5");
    wait_for(cx, "the second run", |_| {
        setup
            .runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len()
            >= 2
    });
    assert_eq!(
        setup
            .runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        vec!["Alpha".to_owned(), "Alpha".to_owned()]
    );
}

#[ui_kit::gpui::test]
fn alt_shift_f10_filters_the_configurations_and_enter_runs_the_chosen_one(cx: &mut TestAppContext) {
    let setup = setup(cx, "choose-run");
    press(cx, setup.window, "alt-shift-f10");
    draw(cx, setup.window);
    assert!(
        shown(cx, setup.window, "menu-popup"),
        "Alt+Shift+F10 opens the configuration menu"
    );

    press(cx, setup.window, "down");
    press(cx, setup.window, "enter");

    wait_for(cx, "Beta to run", |_| {
        setup
            .runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|name| name == "Beta")
    });
}

#[ui_kit::gpui::test]
fn alt_shift_f9_starts_the_chosen_configuration_under_the_debugger(cx: &mut TestAppContext) {
    let setup = setup(cx, "choose-debug");
    press(cx, setup.window, "alt-shift-f9");
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "palette"));

    type_text(cx, setup.window, "beta");
    press(cx, setup.window, "shift-enter");

    let debug = debug_panel(cx);
    wait_for(cx, "the mock session of Beta", |cx| {
        setup.service.starts.load(Ordering::SeqCst) == 1
            && matches!(
                debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
                DebugState::Stopped { .. }
            )
    });
    assert!(
        setup
            .runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|name| name == "debug Beta"),
        "the debug plan of Beta was used"
    );
    end_session(cx, &debug, &setup.service);
}

// The same three actions on the real debugger, on a copy of PetClinic.

use crate::window_tests_real::{build_window, project, wait_for as wait_real, wait_ready};

fn real_setup(
    language: &window_language::RealLanguage,
    probe: &window_language::Probe,
) -> ProjectSetup {
    ProjectSetup {
        tool: Some(language.probe_build()),
        providers: language.probe_providers(probe),
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("debug-actions-probe-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    }
}

fn real_stop(
    cx: &mut TestAppContext,
    _window: WindowHandle<Workspace>,
    debug: &gpui::Entity<crate::debug::DebugPanel>,
) {
    wait_real(cx, "the stop at the breakpoint", LIMIT, |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });
    wait_real(cx, "the stack and the variables", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        !snapshot.frames.is_empty() && !snapshot.variables.is_empty()
    });
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn run_to_cursor_reaches_the_line_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-run-to-cursor");
    let probe = language.install_probe().expect("the probe compiles");
    let setup = real_setup(&language, &probe);
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = run_panel(cx);
    wait_real(cx, "the probe configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "DebugProbe")
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("DebugProbe", cx)));
    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });
    press(cx, window, "shift-f9");
    real_stop(cx, window, &debug);

    // The loop calls add() three times; Run to Cursor on the call line stops there.
    let view = editor_of(cx, window, &probe.path);
    view.update(cx, |view, cx| view.restore_caret(9, 12, cx));
    draw(cx, window);
    debug.update(cx, |panel, cx| {
        panel.remove_breakpoint(probe.path.clone(), probe.line, cx)
    });
    press(cx, window, "alt-f9");
    wait_real(cx, "the stop at the caret line", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line == 10)
    });
    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&probe.path))
            .is_empty(),
        "the temporary breakpoint is gone"
    );

    crate::window_tests_real::close(cx, window, language, true);
    crate::window_tests_real::finish(
        "run_to_cursor_reaches_the_line_on_the_real_adapter",
        started,
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn quick_evaluate_and_set_value_work_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-evaluate");
    let probe = language.install_probe().expect("the probe compiles");
    let setup = real_setup(&language, &probe);
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = run_panel(cx);
    wait_real(cx, "the probe configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "DebugProbe")
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("DebugProbe", cx)));
    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });
    press(cx, window, "shift-f9");
    real_stop(cx, window, &debug);

    // The caret is on the first parameter of `int sum = a + b;`.
    let view = editor_of(cx, window, &probe.path);
    view.update(cx, |view, cx| view.restore_caret(19, 18, cx));
    draw(cx, window);
    press(cx, window, "ctrl-alt-f8");
    wait_real(cx, "the quick value of a", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().evaluation.clone())
            .is_some_and(|evaluation| evaluation.expression == "a" && evaluation.error.is_none())
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "quick-evaluate"),
        "the popup at the caret is open"
    );
    press(cx, window, "escape");

    // F2 in the Debug window gives the local variable another value.
    show_debug(cx, window);
    let scopes: Vec<i64> = debug.read_with(cx, |panel, _| {
        panel
            .snapshot()
            .scopes
            .iter()
            .map(|scope| scope.reference)
            .collect()
    });
    for scope in scopes {
        debug.update(cx, |panel, cx| panel.expand(scope, cx));
    }
    draw(cx, window);
    focus_panel(cx, window, &debug);
    let reference = debug.read_with(cx, |panel, _| {
        let snapshot = panel.snapshot();
        snapshot
            .scopes
            .iter()
            .find(|scope| {
                snapshot
                    .variables
                    .get(&scope.reference)
                    .is_some_and(|variables| variables.iter().any(|variable| variable.name == "a"))
            })
            .map(|scope| scope.reference)
            .expect("a scope with a")
    });
    debug.update(cx, |panel, _| {
        panel.select_variable_for_test(reference, "a")
    });
    press(cx, window, "f2");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "7");
    press(cx, window, "enter");
    wait_real(cx, "the new value of a", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        snapshot
            .variables
            .values()
            .flatten()
            .any(|variable| variable.name == "a" && variable.value == "7")
    });

    crate::window_tests_real::close(cx, window, language, true);
    crate::window_tests_real::finish(
        "quick_evaluate_and_set_value_work_on_the_real_adapter",
        started,
    );
}
