//! The Debug console takes what the user types: a line reaches the program under the
//! debugger, Up and Down walk the lines sent before, Ctrl+V sends the complete lines of
//! the clipboard, and Ctrl+D or Send EOF ends the input of the program.
//!
//! `cargo test -p panels window_tests_debug_input`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dap_client::{BreakpointSpec, DebugService, Debugger, Launch};
use proc_sup::{Process, ProcessSpec};
use project_model::ProjectModel;
use runner::{RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::debug::DebugPanel;
use crate::window_tests::{bounds_of, click, draw, press, sample_project, type_text};
use crate::window_tests_run::{debug_panel, open_with, run_panel, setup, wait_for};

/// The program of the fixture: it appends every line it reads to the log, then `eof`.
fn reading_program(log: &Path) -> ProcessSpec {
    let log = log.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$path = '{log}'; \
         while (($line = [Console]::In.ReadLine()) -ne $null) {{ \
             [System.IO.File]::AppendAllText($path, $line + \"`n\") \
         }}; \
         [System.IO.File]::AppendAllText($path, \"eof`n\")"
    );
    ProcessSpec::new("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .input()
}

/// A configuration that starts the reading program under the debugger.
struct ReadingRun {
    log: PathBuf,
}

impl RunConfigProvider for ReadingRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Reading".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: reading_program(&self.log),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: reading_program(&self.log),
            grace: Duration::from_secs(1),
        })
    }
}

/// A debug service that starts the program of the plan and keeps its standard input, as
/// the language side does, and serves the mock adapter.
struct InputService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    process: Mutex<Option<Process>>,
}

impl InputService {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            process: Mutex::new(None),
        })
    }

    fn session(
        &self,
        program: ProcessSpec,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let mut process = program.start().map_err(|error| error.to_string())?;
        let stdin = process.take_stdin();
        *self
            .process
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(process);
        let server = lang_fixture::MockDap::start().map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let mut debugger = Debugger::new(commands, incoming);
        if let Some(stdin) = stdin {
            let (input, writer) = proc_sup::Input::channel();
            writer.start(stdin);
            debugger = debugger.with_input(input);
        }
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
        Ok(debugger)
    }
}

impl DebugService for InputService {
    fn start(
        &self,
        program: ProcessSpec,
        name: String,
        _entry: Option<PathBuf>,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(program, name, breakpoints, watch, exception_filters)
    }

    fn attach(
        &self,
        _host: String,
        _port: u16,
        _name: String,
        _project: Option<String>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Err("nothing to attach to in this test".into())
    }
}

struct Fixture {
    window: WindowHandle<Workspace>,
    log: PathBuf,
}

/// Opens the window and starts the reading program under the debugger.
fn started(cx: &mut TestAppContext, name: &str) -> (Fixture, gpui::Entity<DebugPanel>) {
    let root = sample_project(name);
    let log = root.join("input.log");
    let service = InputService::new();
    let mut project = setup(&root);
    project.providers = vec![Arc::new(ReadingRun { log: log.clone() })];
    project.debug = Some(service);
    let window = open_with(cx, &root, project);
    let run = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !run.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    assert!(
        run.update(cx, |panel, cx| panel.select_named("Reading", cx)),
        "the fixture configuration is there"
    );

    press(cx, window, "shift-f9");
    let debug = debug_panel(cx);
    wait_for(cx, "the console input", |cx| {
        debug.read_with(cx, |panel, _| panel.takes_input())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    wait_for(cx, "the first stop", |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            dap_client::DebugState::Stopped { .. }
        )
    });
    wait_for(cx, "the frames", |cx| {
        !debug
            .read_with(cx, |panel, _| panel.snapshot().frames.clone())
            .is_empty()
    });
    let focus = debug.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    (Fixture { window, log }, debug)
}

fn log_of(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for_line(cx: &mut TestAppContext, path: &Path, line: &str) {
    wait_for(cx, line, |_| log_of(path).lines().any(|read| read == line));
}

fn input_text(cx: &TestAppContext, panel: &gpui::Entity<DebugPanel>) -> String {
    panel.read_with(cx, |panel, _| panel.input_text().to_owned())
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_typed_line_reaches_the_program_and_its_history_stays(cx: &mut TestAppContext) {
    let (fixture, debug) = started(cx, "debug-input");
    let line = bounds_of(cx, fixture.window, "debug-console-input");
    assert!(
        line.size.height > px(0.),
        "the console shows the input line"
    );

    type_text(cx, fixture.window, "hel");
    assert_eq!(input_text(cx, &debug), "hel");
    type_text(cx, fixture.window, "lo");
    press(cx, fixture.window, "enter");
    wait_for_line(cx, &fixture.log, "hello");
    assert_eq!(input_text(cx, &debug), "", "Enter clears the line");

    // The history of this console: Up walks back through the lines sent here.
    type_text(cx, fixture.window, "second");
    press(cx, fixture.window, "enter");
    wait_for_line(cx, &fixture.log, "second");
    draw(cx, fixture.window);
    press(cx, fixture.window, "up");
    assert_eq!(input_text(cx, &debug), "second");
    press(cx, fixture.window, "up");
    assert_eq!(input_text(cx, &debug), "hello");
    press(cx, fixture.window, "down");
    assert_eq!(input_text(cx, &debug), "second");

    // A multi-line paste sends its complete lines and keeps the rest.
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(
            "one\ntwo\nrest".to_owned(),
        ))
    });
    press(cx, fixture.window, "ctrl-v");
    wait_for_line(cx, &fixture.log, "one");
    wait_for_line(cx, &fixture.log, "two");
    assert_eq!(input_text(cx, &debug), "rest");

    // Ctrl+D is Send EOF: the program sees the end of its input.
    press(cx, fixture.window, "ctrl-d");
    wait_for_line(cx, &fixture.log, "eof");
    assert!(
        !debug.read_with(cx, |panel, _| panel.takes_input()),
        "nothing is typed after the end of the input"
    );
}

#[ui_kit::gpui::test]
fn send_eof_in_the_console_menu_ends_the_input_of_the_program(cx: &mut TestAppContext) {
    let (fixture, debug) = started(cx, "debug-eof");

    let console = bounds_of(cx, fixture.window, "debug-console");
    let position = console.origin + ui_kit::point(px(20.), px(10.));
    right_click_at(cx, fixture.window, position);
    let entry = bounds_of(cx, fixture.window, "menu-0");
    assert!(
        entry.size.height > px(0.),
        "the console menu has a row: Send EOF"
    );
    click(cx, fixture.window, "menu-0");

    wait_for_line(cx, &fixture.log, "eof");
    assert!(
        !debug.read_with(cx, |panel, _| panel.takes_input()),
        "nothing is typed after the end of the input"
    );
}
