//! The deeper debugger of the window without a window: Mark Object, Copy Stack, the
//! filter of library frames, inline watches and type renderers on the mock adapter, and
//! Mark Object and a renderer on the real debugger at the end.
//!
//! `cargo test -p panels window_tests_debug_depth`
//!
//! The ignored test needs `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_debug_depth -- --ignored --test-threads=1`

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dap_client::{BreakpointSpec, DebugService, DebugState, Debugger, Launch};
use proc_sup::ProcessSpec;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, press, scratch, type_text,
};
use crate::window_tests_debug_actions::{MockService, debug_panel, end_session, shown};
use crate::window_tests_lang::FakeLanguage;

/// How long a test waits for the panel or the adapter.
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

/// A method long enough that the line the mock adapter stops at names two locals.
fn source() -> String {
    let mut text = String::from("void main() {\n");
    for _ in 0..40 {
        text.push_str("    // a line of the method\n");
    }
    text.push_str("    text items\n}\n");
    text
}

/// The buffer line the mock adapter stops at, from zero: its frame names line 42.
const STOP_LINE: usize = 41;

struct Fixture {
    window: WindowHandle<Workspace>,
    view: gpui::Entity<editor_view::EditorView>,
    debug: gpui::Entity<crate::debug::DebugPanel>,
    service: Arc<MockService>,
}

fn stopped(cx: &mut TestAppContext, name: &str) -> Fixture {
    let root = crate::window_tests_lang::project(name, &[("src/main.txt", &source())]);
    let window = crate::window_tests_lang::open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let service = MockService::new(path.clone());
    let debug = debug_panel(cx);
    debug
        .update(cx, |panel, cx| {
            panel.start_attach(
                service.clone(),
                "127.0.0.1".into(),
                1,
                "fixture".into(),
                None,
                cx,
            )
        })
        .expect("the mock adapter attaches");
    wait_for(cx, "the first stop with its variables", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. }) && !snapshot.variables.is_empty()
    });
    draw(cx, window);
    Fixture {
        window,
        view,
        debug,
        service,
    }
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    // The bottom dock slides in: the click of a test lands on the settled frame.
    cx.executor().advance_clock(Duration::from_millis(300));
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

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    point: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(point, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(point, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
}

fn inline_text(cx: &TestAppContext, fixture: &Fixture) -> String {
    fixture
        .view
        .read_with(cx, |view, _| view.debug_values().to_vec())
        .into_iter()
        .map(|value| value.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn watch_text(cx: &TestAppContext, fixture: &Fixture) -> String {
    fixture
        .view
        .read_with(cx, |view, _| view.inline_watches().to_vec())
        .into_iter()
        .map(|watch| watch.text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[ui_kit::gpui::test]
fn mark_object_shows_its_label_in_variables_and_inline_values(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "debug-depth-mark");
    show_debug(cx, fixture.window);
    fixture.debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, fixture.window);
    focus_panel(cx, fixture.window, &fixture.debug);

    fixture
        .debug
        .update(cx, |panel, _| panel.select_variable_for_test(100, "items"));
    press(cx, fixture.window, "f11");
    draw(cx, fixture.window);
    type_text(cx, fixture.window, "mine");
    press(cx, fixture.window, "enter");

    wait_for(cx, "the label of the object", |cx| {
        fixture
            .debug
            .read_with(cx, |panel, _| panel.label_for_test(101))
            .is_some()
    });
    let value = fixture
        .debug
        .read_with(cx, |panel, _| panel.variable_value_for_test(100, "items"))
        .expect("the row of items");
    assert!(
        value.contains("mine_DebugLabel"),
        "the label stands beside the value: {value}"
    );
    let inline = inline_text(cx, &fixture);
    assert!(
        inline.contains("mine_DebugLabel"),
        "the label stands in the inline values: {inline}"
    );

    end_session(cx, &fixture.debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn copy_stack_puts_a_stack_trace_on_the_clipboard(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "debug-depth-copy");
    show_debug(cx, fixture.window);
    draw(cx, fixture.window);

    right_click(cx, fixture.window, "frame-12");
    draw(cx, fixture.window);
    assert!(
        shown(cx, fixture.window, "menu-popup"),
        "the frame menu opens"
    );
    click(cx, fixture.window, "menu-0");

    let text = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("the stack was copied");
    assert!(
        text.contains("at start(main.txt:42)"),
        "the trace names the frame: {text}"
    );
    assert!(
        text.contains("at run(Unknown Source)"),
        "the frame without a place is kept: {text}"
    );

    end_session(cx, &fixture.debug, &fixture.service);
}

/// One mock adapter session on the scenario with frames of libraries.
struct LibraryService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    source: PathBuf,
}

impl LibraryService {
    fn new(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            source,
        })
    }

    fn session(&self, breakpoints: Vec<BreakpointSpec>) -> Result<Debugger, String> {
        let server =
            lang_fixture::MockDap::start_with_library_frames(self.source.to_string_lossy())
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
            name: "fixture".into(),
            breakpoints,
            ..Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
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

impl DebugService for LibraryService {
    fn start(
        &self,
        _program: ProcessSpec,
        _name: String,
        _entry: Option<PathBuf>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Err("the library scenario only attaches".into())
    }

    fn attach(
        &self,
        _host: String,
        _port: u16,
        _name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(breakpoints)
    }
}

#[ui_kit::gpui::test]
fn the_filter_folds_the_frames_of_libraries_and_opens_them(cx: &mut TestAppContext) {
    let root =
        crate::window_tests_lang::project("debug-depth-libraries", &[("src/main.txt", &source())]);
    let window = crate::window_tests_lang::open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let service = LibraryService::new(path.clone());
    let debug = debug_panel(cx);
    debug
        .update(cx, |panel, cx| {
            panel.start_attach(
                service.clone(),
                "127.0.0.1".into(),
                1,
                "fixture".into(),
                None,
                cx,
            )
        })
        .expect("the library adapter attaches");
    wait_for(cx, "the frames of the library scenario", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. }) && snapshot.frames.len() == 4
    });
    show_debug(cx, window);
    draw(cx, window);

    assert!(
        shown(cx, window, "frames-group-22"),
        "the frames of libraries are folded"
    );
    assert!(
        !shown(cx, window, "frame-22"),
        "a frame of a library is hidden until the group opens"
    );
    assert!(shown(cx, window, "frame-21"), "the project frame stays");

    click(cx, window, "frames-group-22");
    draw(cx, window);
    wait_for(cx, "the group to open", |cx| {
        debug.read_with(cx, |panel, _| panel.library_group_open_for_test(22))
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "frame-22") && shown(cx, window, "frame-23"),
        "a click opens the group"
    );

    click(cx, window, "frames-group-22");
    draw(cx, window);
    assert!(
        !shown(cx, window, "frame-23"),
        "a second click folds the group again"
    );
    assert!(shown(cx, window, "debug-hide-libraries"));

    debug.update(cx, |panel, cx| panel.stop(cx));
    wait_for(cx, "the end of the session", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    service.join();
}

#[ui_kit::gpui::test]
fn an_inline_watch_is_evaluated_at_the_stop_and_its_cross_removes_it(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "debug-depth-watch");
    show_debug(cx, fixture.window);
    draw(cx, fixture.window);
    fixture
        .view
        .update(cx, |view, cx| view.restore_caret(STOP_LINE, 4, cx));
    fixture
        .window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, fixture.window);

    wait_for(cx, "the inline watch to be offered at the caret", |cx| {
        fixture
            .view
            .read_with(cx, |view, _| view.inline_watch_at_caret())
            .is_some()
    });
    press(cx, fixture.window, "alt-enter");
    wait_for(cx, "the action list", |cx| {
        shown(cx, fixture.window, "actions-popup")
    });
    press(cx, fixture.window, "enter");

    wait_for(cx, "the value of the inline watch", |cx| {
        watch_text(cx, &fixture).contains("text = \"hello\"")
    });
    assert_eq!(
        fixture
            .debug
            .read_with(cx, |panel, _| panel.inline_watches()),
        vec!["text".to_owned()]
    );

    draw(cx, fixture.window);
    fixture
        .window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    press(cx, fixture.window, "ctrl-end");
    draw(cx, fixture.window);
    let point = fixture
        .view
        .read_with(cx, |view, _| view.inline_watch_cross_point(STOP_LINE))
        .expect("the cross of the watch is laid out");
    click_at(cx, fixture.window, point);

    wait_for(cx, "the watch to go", |cx| {
        fixture
            .debug
            .read_with(cx, |panel, _| panel.inline_watches())
            .is_empty()
    });
    assert!(watch_text(cx, &fixture).is_empty());

    end_session(cx, &fixture.debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn a_type_renderer_changes_the_value_in_variables_and_inline_values(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "debug-depth-renderer");
    show_debug(cx, fixture.window);
    fixture.debug.update(cx, |panel, cx| panel.expand(100, cx));
    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_type_renderers = "ArrayList = size()".to_owned();
        cx.set_global(settings);
    });
    cx.run_until_parked();

    wait_for(cx, "the rendered value", |cx| {
        fixture.debug.update(cx, |panel, cx| panel.expand(100, cx));
        let snapshot = fixture
            .debug
            .read_with(cx, |panel, _| panel.snapshot().clone());
        snapshot.variables.get(&100).is_some_and(|variables| {
            variables
                .iter()
                .any(|variable| variable.name == "items" && variable.value == "2")
        })
    });
    let value = fixture
        .debug
        .read_with(cx, |panel, _| panel.variable_value_for_test(100, "items"))
        .expect("the row of items");
    assert_eq!(value, "2");
    draw(cx, fixture.window);
    assert!(
        inline_text(cx, &fixture).contains("items = 2"),
        "the inline value uses the renderer: {}",
        inline_text(cx, &fixture)
    );

    end_session(cx, &fixture.debug, &fixture.service);
}

/// One mock adapter session of the scenario of reused handles: two stops with values of
/// the same type, where a handle of the first stop names another object at the second.
struct IdentityService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    source: PathBuf,
}

impl IdentityService {
    fn new(source: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            source,
        })
    }

    fn session(&self, breakpoints: Vec<BreakpointSpec>) -> Result<Debugger, String> {
        let server =
            lang_fixture::MockDap::start_with_identity_objects(self.source.to_string_lossy())
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
            name: "fixture".into(),
            breakpoints,
            ..Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
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

impl DebugService for IdentityService {
    fn start(
        &self,
        _program: ProcessSpec,
        _name: String,
        _entry: Option<PathBuf>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Err("the identity scenario only attaches".into())
    }

    fn attach(
        &self,
        _host: String,
        _port: u16,
        _name: String,
        _project: Option<String>,
        breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(breakpoints)
    }

    fn object_identity_expression(&self, evaluate_name: &str) -> Option<String> {
        Some(format!("identityHashCode({evaluate_name})"))
    }
}

#[ui_kit::gpui::test]
fn a_mark_follows_the_object_across_a_step_and_not_a_reused_handle(cx: &mut TestAppContext) {
    let root =
        crate::window_tests_lang::project("debug-depth-identity", &[("src/main.txt", &source())]);
    let window = crate::window_tests_lang::open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let service = IdentityService::new(path.clone());
    let debug = debug_panel(cx);
    debug
        .update(cx, |panel, cx| {
            panel.start_attach(
                service.clone(),
                "127.0.0.1".into(),
                1,
                "fixture".into(),
                None,
                cx,
            )
        })
        .expect("the identity adapter attaches");
    wait_for(cx, "the first stop with two values", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot.variables.get(&100).is_some_and(|variables| {
                variables.iter().any(|variable| variable.name == "a")
                    && variables.iter().any(|variable| variable.name == "b")
            })
    });
    show_debug(cx, window);
    debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, window);
    focus_panel(cx, window, &debug);

    debug.update(cx, |panel, _| panel.select_variable_for_test(100, "a"));
    press(cx, window, "f11");
    draw(cx, window);
    type_text(cx, window, "mine");
    press(cx, window, "enter");
    wait_for(cx, "the label of the first object", |cx| {
        debug
            .read_with(cx, |panel, _| panel.variable_value_for_test(100, "a"))
            .is_some_and(|value| value.contains("mine_DebugLabel"))
    });

    debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(
        cx,
        "the second stop with the same object and another one",
        |cx| {
            let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
            matches!(snapshot.state, DebugState::Stopped { .. })
                && snapshot.variables.get(&100).is_some_and(|variables| {
                    variables.iter().any(|variable| variable.name == "same")
                        && variables.iter().any(|variable| variable.name == "other")
                })
        },
    );
    wait_for(cx, "the identities of both values", |cx| {
        debug.read_with(cx, |panel, _| {
            panel.identity_for_test(101).is_some() && panel.identity_for_test(103).is_some()
        })
    });

    let same = debug
        .read_with(cx, |panel, _| panel.variable_value_for_test(100, "same"))
        .expect("the row of the same object");
    assert!(
        same.contains("mine_DebugLabel"),
        "the mark follows the object to its new handle: {same}"
    );
    let other = debug
        .read_with(cx, |panel, _| panel.variable_value_for_test(100, "other"))
        .expect("the row of the other object");
    assert!(
        !other.contains("mine_DebugLabel"),
        "the mark does not follow the reused handle: {other}"
    );

    debug.update(cx, |panel, cx| panel.stop(cx));
    wait_for(cx, "the end of the session", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    service.join();
}

// The same two actions on the real debugger: a program with an ArrayList.
use crate::window_tests_real::{build_window, project, wait_for as wait_real, wait_ready};

fn real_setup(
    language: &window_language::RealLanguage,
    probe: &window_language::Probe,
) -> crate::run::ProjectSetup {
    crate::run::ProjectSetup {
        tool: Some(language.probe_build()),
        providers: language.probe_providers(probe),
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("debug-depth-probe-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    }
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn mark_object_and_a_renderer_work_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-debug-depth");
    let probe = language
        .install_identity_probe()
        .expect("the probe compiles");
    let setup = real_setup(&language, &probe);
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = crate::window_tests_debug_actions::run_panel(cx);
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

    let scope_with = |cx: &mut TestAppContext, name: &str| {
        debug.read_with(cx, |panel, _| {
            let snapshot = panel.snapshot();
            snapshot
                .scopes
                .iter()
                .find(|scope| {
                    snapshot
                        .variables
                        .get(&scope.reference)
                        .is_some_and(|variables| variables.iter().any(|v| v.name == name))
                })
                .map(|scope| scope.reference)
        })
    };
    let reference = scope_with(cx, "first").expect("a scope with the marked value");
    for scope in debug.read_with(cx, |panel, _| {
        panel
            .snapshot()
            .scopes
            .iter()
            .map(|scope| scope.reference)
            .collect::<Vec<_>>()
    }) {
        debug.update(cx, |panel, cx| panel.expand(scope, cx));
    }
    draw(cx, window);
    focus_panel(cx, window, &debug);
    debug.update(cx, |panel, _| {
        panel.select_variable_for_test(reference, "first")
    });
    press(cx, window, "f11");
    draw(cx, window);
    type_text(cx, window, "mine");
    press(cx, window, "enter");
    wait_real(cx, "the label of the object", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| {
                panel.variable_value_for_test(reference, "first")
            })
            .is_some_and(|value| value.contains("mine_DebugLabel"))
    });

    // A step keeps both values of the type alive; only the marked object keeps its name.
    debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_real(cx, "the second stop with both values", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot.stops >= 2
            && snapshot
                .variables
                .values()
                .flatten()
                .any(|v| v.name == "first")
            && snapshot
                .variables
                .values()
                .flatten()
                .any(|v| v.name == "second")
    });
    let reference = scope_with(cx, "first").expect("a scope at the second stop");
    debug.update(cx, |panel, cx| panel.expand(reference, cx));
    draw(cx, window);
    wait_real(cx, "the identities of both values", LIMIT, |cx| {
        let handles = debug.read_with(cx, |panel, _| {
            let snapshot = panel.snapshot();
            let mut handles = Vec::new();
            for variable in snapshot.variables.values().flatten() {
                if variable.name == "first" || variable.name == "second" {
                    handles.push(variable.reference);
                }
            }
            handles
        });
        handles.len() == 2
            && debug.read_with(cx, |panel, _| {
                handles
                    .iter()
                    .all(|handle| panel.identity_for_test(*handle).is_some())
            })
    });
    let first = debug
        .read_with(cx, |panel, _| {
            panel.variable_value_for_test(reference, "first")
        })
        .expect("the row of the marked value");
    assert!(
        first.contains("mine_DebugLabel"),
        "the mark stays on the object after the step: {first}"
    );
    let second = debug
        .read_with(cx, |panel, _| {
            panel.variable_value_for_test(reference, "second")
        })
        .expect("the row of the other value");
    assert!(
        !second.contains("mine_DebugLabel"),
        "the mark does not go to another object of the type: {second}"
    );

    let first_type = debug
        .read_with(cx, |panel, _| {
            panel
                .snapshot()
                .variables
                .get(&reference)?
                .iter()
                .find(|variable| variable.name == "first")
                .map(|variable| variable.type_name.clone())
        })
        .unwrap_or_default();
    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_type_renderers = format!("{first_type} = size()");
        cx.set_global(settings);
    });
    cx.run_until_parked();
    debug.update(cx, |panel, cx| panel.expand(reference, cx));
    wait_real(cx, "the rendered value", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| {
                panel.variable_value_for_test(reference, "first")
            })
            .is_some_and(|value| value.contains('1'))
    });

    crate::window_tests_real::close(cx, window, language, true);
    crate::window_tests_real::finish(
        "mark_object_and_a_renderer_work_on_the_real_adapter",
        started,
    );
}
