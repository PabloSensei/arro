//! The place of a stop in the window: the Debugger tab comes back at each new stop, the file
//! of the top frame opens at its line, the row of that line is tinted, and the values of the
//! frame stand after the line unless Settings hides them. The stub adapter moves its stop one
//! line down at every step, as a real debuggee does between two statements.
//!
//! `cargo test -p panels window_tests_debug_stop_view`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dap_client::{BreakpointSpec, DebugService, DebugState, Debugger, Launch};
use editor_view::EditorView;
use proc_sup::{Input, InputWriter, ProcessSpec};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::window_tests::{active_path, buffer_of, click, draw};
use crate::window_tests_debug_actions::{debug_panel, shown};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// How long a test waits for the panel, the adapter or the editor.
const LIMIT: Duration = Duration::from_secs(20);
/// One turn of the simulated clock: the panels poll every 50 ms.
const TICK: Duration = Duration::from_millis(50);
/// The buffer line of the first stop, from zero: the stub adapter stops at line 42.
const FIRST_STOP: usize = 41;

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

/// A method long enough that the first stop names two locals on line 42.
fn source() -> String {
    let mut text = String::from("void main() {\n");
    for _ in 0..40 {
        text.push_str("    // a line of the method\n");
    }
    text.push_str("    text items\n}\n");
    text
}

/// A stub adapter whose stack moves one line down at every step. With `reads_input` the
/// session holds the standard input of a program, as a launched program does, so that the
/// console takes what the user types.
struct StubService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    source: PathBuf,
    reads_input: bool,
    /// The writers of the input, kept alive for the session.
    writers: Mutex<Vec<InputWriter>>,
}

impl StubService {
    fn new(source: PathBuf, reads_input: bool) -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            source,
            reads_input,
            writers: Mutex::new(Vec::new()),
        })
    }

    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start_with_moving_line(self.source.to_string_lossy())
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
        let debugger = Debugger::new(commands, incoming);
        if !self.reads_input {
            return Ok(debugger);
        }
        let (input, writer) = Input::channel();
        self.writers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(writer);
        Ok(debugger.with_input(input))
    }

    /// Ends the adapter of the session once the session has ended.
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

impl DebugService for StubService {
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

struct Fixture {
    window: WindowHandle<Workspace>,
    path: PathBuf,
    debug: gpui::Entity<crate::debug::DebugPanel>,
    service: Arc<StubService>,
}

/// Opens the project with the stub adapter and starts its session. The file of the stop
/// opens by itself, as a stop shows its place.
fn start(cx: &mut TestAppContext, name: &str, reads_input: bool) -> Fixture {
    let root = project(name, &[("src/main.txt", &source())]);
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    let service = StubService::new(path.clone(), reads_input);
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
        .expect("the stub adapter attaches");
    wait_for(cx, "the first stop with its frames", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. }) && !snapshot.frames.is_empty()
    });
    Fixture {
        window,
        path,
        debug,
        service,
    }
}

/// Stops the session and waits until the stub adapter is free again.
fn end(cx: &mut TestAppContext, fixture: &Fixture) {
    fixture.debug.update(cx, |panel, cx| panel.stop(cx));
    wait_for(cx, "the end of the session", |cx| {
        fixture
            .debug
            .read_with(cx, |panel, _| panel.snapshot().state.clone())
            == DebugState::Terminated
    });
    fixture.service.join();
}

/// Opens the Debug tool window, as it opens when a session starts.
fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Gives the keyboard to the Debugger, as a click in its tree does.
fn focus_debug(cx: &mut TestAppContext, fixture: &Fixture) {
    let focus = fixture.debug.read_with(cx, |panel, _| panel.focus());
    fixture
        .window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

/// Whether the Debugger shows its frames, as the window draws it now.
fn frames_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    draw(cx, window);
    shown(cx, window, "debug-frames")
}

/// The editor of `path`, once the workspace has opened its tab.
fn editor(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Option<gpui::Entity<EditorView>> {
    let view = window
        .read_with(cx, |workspace, _| workspace.view_of(path))
        .ok()??;
    view.downcast::<EditorView>().ok()
}

/// The buffer line the editor of `path` tints for the stop, `None` without an editor.
fn tint(cx: &TestAppContext, window: WindowHandle<Workspace>, path: &Path) -> Option<usize> {
    editor(cx, window, path)?.read_with(cx, |view, _| view.execution_line())
}

/// The values the editor of `path` draws after the lines, empty without an editor.
fn values(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Vec<editor_view::InlineValue> {
    editor(cx, window, path)
        .map(|view| view.read_with(cx, |view, _| view.debug_values().to_vec()))
        .unwrap_or_default()
}

/// The line of the caret in the editor of `path`, from zero.
fn caret_line(cx: &TestAppContext, window: WindowHandle<Workspace>, path: &Path) -> usize {
    let view = editor(cx, window, path).expect("the file is open");
    let offset = view
        .read_with(cx, |view, _| view.head_offset())
        .expect("the caret is placed");
    buffer_of(cx, &view)[..offset].matches('\n').count()
}

#[ui_kit::gpui::test]
fn a_new_stop_brings_the_debugger_tab_back_after_the_console_was_chosen(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-tab", false);
    show_debug(cx, fixture.window);
    assert!(
        frames_shown(cx, fixture.window),
        "the Debugger shows the frames of the stop"
    );

    click(cx, fixture.window, "debug-tab-console");
    assert!(
        !frames_shown(cx, fixture.window),
        "the console is in front after the click"
    );

    fixture.debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(cx, "the Debugger tab after the step", |cx| {
        frames_shown(cx, fixture.window)
    });
    wait_for(cx, "the top frame of the stop selected", |cx| {
        fixture.debug.read_with(cx, |panel, _| {
            let snapshot = panel.snapshot();
            snapshot.selected_frame.is_some()
                && snapshot.selected_frame == snapshot.frames.first().map(|frame| frame.id)
        })
    });

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn a_stop_brings_the_debugger_tab_when_the_program_takes_input(cx: &mut TestAppContext) {
    // A program that reads its input keeps the console in front while it runs. A stop is
    // not running, so the Debugger tab of the stop shows.
    let fixture = start(cx, "stop-view-input", true);
    wait_for(cx, "the console input", |cx| {
        fixture.debug.read_with(cx, |panel, _| panel.takes_input())
    });
    show_debug(cx, fixture.window);
    wait_for(cx, "the Debugger tab of the stop", |cx| {
        frames_shown(cx, fixture.window)
    });

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn a_stop_opens_the_file_of_its_frame_at_the_stop_line(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-open", false);
    wait_for(cx, "the stop line of the frame's file", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });

    assert_eq!(
        active_path(cx, fixture.window),
        Some(fixture.path.clone()),
        "the file of the frame is in front"
    );
    assert_eq!(
        caret_line(cx, fixture.window, &fixture.path),
        FIRST_STOP,
        "the caret stands on the stop line"
    );

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn the_stop_line_is_tinted_and_a_step_moves_the_tint(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-step", false);
    wait_for(cx, "the tint of the first stop", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });

    fixture.debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(cx, "the tint of the line after the step", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP + 1)
    });

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn resume_takes_the_tint_away(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-resume", false);
    wait_for(cx, "the tint of the stop", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });

    // The stub adapter stops again at once, so the tint has to go with the command itself,
    // before the snapshot of the next stop can bring it back.
    fixture.debug.update(cx, |panel, cx| panel.resume(cx));
    assert_eq!(
        tint(cx, fixture.window, &fixture.path),
        None,
        "the tint stayed after Resume"
    );

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn the_values_stand_after_the_stop_line_and_the_setting_hides_them(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-values", false);
    wait_for(cx, "the values of the stop line", |cx| {
        values(cx, fixture.window, &fixture.path)
            .iter()
            .any(|value| value.line == FIRST_STOP)
    });

    let shown_values = values(cx, fixture.window, &fixture.path);
    let line = shown_values
        .iter()
        .find(|value| value.line == FIRST_STOP)
        .expect("the stop line names its values");
    assert_eq!(line.text, "text = \"hello\", items = ArrayList (2)");
    assert_eq!(
        shown_values.len(),
        1,
        "only the stop line names the variables"
    );

    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_show_values_inline = false;
        cx.set_global(settings);
    });
    cx.run_until_parked();
    assert!(
        values(cx, fixture.window, &fixture.path).is_empty(),
        "the values go when the setting is off"
    );

    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_show_values_inline = true;
        cx.set_global(settings);
    });
    cx.run_until_parked();
    assert!(
        !values(cx, fixture.window, &fixture.path).is_empty(),
        "the values come back when the setting is on"
    );

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn a_stop_that_jumps_to_its_line_keeps_the_keyboard_in_the_debugger(cx: &mut TestAppContext) {
    let fixture = start(cx, "stop-view-focus", false);
    show_debug(cx, fixture.window);
    wait_for(cx, "the tint of the stop", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });
    focus_debug(cx, &fixture);

    fixture.debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(cx, "the tint of the line after the step", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP + 1)
    });
    let focus = fixture.debug.read_with(cx, |panel, _| panel.focus());
    let focused = fixture
        .window
        .update(cx, |_, window, _| focus.is_focused(window))
        .unwrap();
    assert!(focused, "the jump took the keyboard from the Debugger");

    end(cx, &fixture);
}
