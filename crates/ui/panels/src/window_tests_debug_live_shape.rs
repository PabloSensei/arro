//! The Debugger with the answers of a debug adapter that gives every `scopes` request new
//! references. The stub is `lang_fixture::MockDap` in that shape: the locals sit in a `Local`
//! scope, and the variables come by their own request after the frames.
//!
//! The file is open before the stop, under the spelling that the join of the project gives
//! (`src/app/main.txt` with a `/` in its folder), and the stop names it the way the live
//! adapter does, with backslashes. A stop that names the file by a `file:` URI is a test of
//! its own.
//!
//! `cargo test -p panels window_tests_debug_live_shape`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dap_client::{BreakpointSpec, DebugService, DebugState, Debugger, Launch};
use editor_view::EditorView;
use proc_sup::ProcessSpec;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{active_path, draw, open_path};
use crate::window_tests_debug_actions::{debug_panel, shown};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

/// How long a test waits for the panel, the adapter or the editor.
const LIMIT: Duration = Duration::from_secs(20);
/// One turn of the simulated clock: the panels poll every 50 ms.
const TICK: Duration = Duration::from_millis(50);
/// The buffer line of the first stop, from zero: the stub adapter stops at line 42.
const FIRST_STOP: usize = 41;
/// The buffer line above the stop that names `text`, from zero.
const ABOVE_STOP: usize = 2;

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

/// A method long enough that the first stop names two locals on line 42, and that names
/// `text` on line 3, above the stop.
fn source() -> String {
    let mut text = String::from("void main() {\n");
    for index in 0..40 {
        if index == 1 {
            text.push_str("    text.trim();\n");
        } else {
            text.push_str("    // a line of the method\n");
        }
    }
    text.push_str("    text items\n}\n");
    text
}

/// The `file:` URI of a local path, for an adapter that names its sources by URI. On Windows
/// the drive letter is in the other case than the path of the editor. Characters outside the
/// unreserved set are percent-encoded.
fn source_uri(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && text.as_bytes().get(1) == Some(&b':') {
        let lower = text[..1].to_ascii_lowercase();
        text.replace_range(..1, &lower);
    }
    let mut uri = String::from("file:///");
    for byte in text.trim_start_matches('/').bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                uri.push(byte as char)
            }
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri
}

/// A stub adapter with fresh scopes, whose stop names the file as `source` does.
struct ShapeService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    source: String,
}

impl ShapeService {
    fn new(source: String) -> Arc<Self> {
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

    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start_with_fresh_scopes(self.source.clone())
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

impl DebugService for ShapeService {
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
    /// The file as the editor has it.
    path: PathBuf,
    debug: gpui::Entity<crate::debug::DebugPanel>,
    service: Arc<ShapeService>,
}

/// Opens the project with the stub adapter and starts its session. The editor has the file
/// open before the stop, under the spelling of the join; the stop names it with backslashes,
/// or as a `file:` URI when `stop_as_uri` is set.
fn start(cx: &mut TestAppContext, name: &str, stop_as_uri: bool) -> Fixture {
    let root = project(name, &[("src/app/main.txt", &source())]);
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src/app/main.txt");
    open_path(cx, window, &path);
    let stop = root.join("src").join("app").join("main.txt");
    let source = if stop_as_uri {
        source_uri(&stop)
    } else {
        stop.to_string_lossy().into_owned()
    };
    let service = ShapeService::new(source);
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

/// Whether the Variables list shows the two locals of the stub, as the window draws it now.
fn locals_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    draw(cx, window);
    shown(cx, window, "var-text") && shown(cx, window, "var-items")
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

#[ui_kit::gpui::test]
fn a_stop_shows_the_locals_without_a_click(cx: &mut TestAppContext) {
    let fixture = start(cx, "live-shape-locals", false);
    show_debug(cx, fixture.window);
    wait_for(cx, "the locals of the stop on the screen", |cx| {
        locals_shown(cx, fixture.window)
    });

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn a_file_uri_with_another_drive_case_names_the_stop_line(cx: &mut TestAppContext) {
    let fixture = start(cx, "live-shape-uri", true);
    wait_for(cx, "the tint of the stop line", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });
    assert_eq!(
        active_path(cx, fixture.window),
        Some(fixture.path.clone()),
        "the file of the frame is in front"
    );

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn the_values_stand_at_the_end_of_the_stop_line(cx: &mut TestAppContext) {
    let fixture = start(cx, "live-shape-values", false);
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
    let above = shown_values
        .iter()
        .find(|value| value.line == ABOVE_STOP)
        .expect("the line above the stop names its value");
    assert_eq!(above.text, "text = \"hello\"");

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn a_step_moves_the_tint_and_the_locals_open_again(cx: &mut TestAppContext) {
    let fixture = start(cx, "live-shape-step", false);
    show_debug(cx, fixture.window);
    wait_for(cx, "the tint of the stop", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP)
    });

    fixture.debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(cx, "the tint of the line after the step", |cx| {
        tint(cx, fixture.window, &fixture.path) == Some(FIRST_STOP + 1)
    });
    wait_for(cx, "the locals of the new stop on the screen", |cx| {
        locals_shown(cx, fixture.window)
    });

    end(cx, &fixture);
}

#[ui_kit::gpui::test]
fn selecting_another_frame_opens_its_locals(cx: &mut TestAppContext) {
    let fixture = start(cx, "live-shape-frame", false);
    show_debug(cx, fixture.window);
    wait_for(cx, "the locals of the stop on the screen", |cx| {
        locals_shown(cx, fixture.window)
    });

    let other = fixture
        .debug
        .read_with(cx, |panel, _| {
            panel.snapshot().frames.get(1).map(|frame| frame.id)
        })
        .expect("the stack has a second frame");
    fixture
        .debug
        .update(cx, |panel, cx| panel.select_frame(other, cx));
    wait_for(cx, "the second frame selected", |cx| {
        fixture
            .debug
            .read_with(cx, |panel, _| panel.snapshot().selected_frame)
            == Some(other)
    });
    wait_for(cx, "the locals of the selected frame on the screen", |cx| {
        locals_shown(cx, fixture.window)
    });

    end(cx, &fixture);
}
