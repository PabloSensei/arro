//! The Debug window and the tab in front: a build before a launch shows in the Console with
//! the status Building, a failed build keeps the Console and says so, a session without frames
//! names its state in the Debugger tab, and while a step runs the tab stays where the user left
//! it. A program that reads its input brings the Console only before its first stop.
//!
//! `cargo test -p panels window_tests_debug_build_front`

#![cfg(windows)]

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use dap_client::{BreakpointSpec, Command, DebugService, DebugState, Debugger, Frame, Snapshot};
use proc_sup::{Input, InputWriter, ProcessSpec};
use project_model::ProjectModel;
use runner::{RunConfig, RunConfigProvider, RunPlan, ServiceState};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{RUN_TOOL, Workspace};

use crate::debug::DebugPanel;
use crate::window_tests::{click, draw, press, sample_project};
use crate::window_tests_debug_actions::shown;
use crate::window_tests_run::{debug_panel, open_with, run_panel, setup, wait_for};

/// A debug service whose session the test scripts: the snapshots the test sends are the ones
/// the window polls, so a state lasts as long as the test keeps it.
struct ScriptedService {
    with_input: bool,
    /// The sender of the snapshots of the session, once the window starts it.
    snapshots: Mutex<Option<mpsc::Sender<Arc<Snapshot>>>>,
    /// The commands of the session, received and kept so that a command still sends.
    commands: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Command>>>,
    /// The writers of the input, kept alive for the session.
    writers: Mutex<Vec<InputWriter>>,
}

impl ScriptedService {
    fn new(with_input: bool) -> Arc<Self> {
        Arc::new(Self {
            with_input,
            snapshots: Mutex::new(None),
            commands: Mutex::new(None),
            writers: Mutex::new(Vec::new()),
        })
    }

    fn session(&self) -> Debugger {
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (sender, snapshots) = mpsc::channel();
        *lock(&self.commands) = Some(receiver);
        *lock(&self.snapshots) = Some(sender);
        let debugger = Debugger::new(commands, snapshots);
        if !self.with_input {
            return debugger;
        }
        let (input, writer) = Input::channel();
        lock(&self.writers).push(writer);
        debugger.with_input(input)
    }

    /// Whether the window has started the session.
    fn started(&self) -> bool {
        lock(&self.snapshots).is_some()
    }

    /// Sends a snapshot of the session; the window draws it at its next poll.
    fn send(&self, snapshot: Snapshot) {
        if let Some(sender) = lock(&self.snapshots).as_ref() {
            let _ = sender.send(Arc::new(snapshot));
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl DebugService for ScriptedService {
    fn start(
        &self,
        _program: ProcessSpec,
        _name: String,
        _entry: Option<PathBuf>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Ok(self.session())
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
        Ok(self.session())
    }
}

/// A configuration named `Launch`, whose build step is `build`. The program of it never
/// starts: the scripted service starts the session without it.
struct BuildRun {
    build: ProcessSpec,
}

impl BuildRun {
    fn run_plan(&self) -> RunPlan {
        RunPlan {
            before: Some(self.build.clone()),
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        }
    }
}

impl RunConfigProvider for BuildRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Launch".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
            ..RunConfig::default()
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(self.run_plan())
    }

    fn debug_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(self.run_plan())
    }
}

/// A build that prints a line and fails with exit code 3.
fn failing_build() -> ProcessSpec {
    ProcessSpec::new("powershell.exe").args([
        "-NoProfile",
        "-Command",
        "Write-Output 'compiling'; exit 3",
    ])
}

/// A build that prints a line and succeeds.
fn succeeding_build() -> ProcessSpec {
    ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"])
}

/// A build that runs until the test stops it.
fn long_build() -> ProcessSpec {
    ProcessSpec::new("powershell.exe").args([
        "-NoProfile",
        "-Command",
        "Write-Output 'compiling'; Start-Sleep -Seconds 120",
    ])
}

/// A snapshot of a stop with one frame, after `stops` stops of the session.
fn stopped(stops: u64) -> Snapshot {
    Snapshot {
        state: DebugState::Stopped {
            reason: "breakpoint".into(),
        },
        frames: vec![Frame {
            id: 1,
            name: "main".into(),
            source: None,
            line: 0,
            column: 0,
        }],
        selected_frame: Some(1),
        stops,
        ..Snapshot::default()
    }
}

/// A snapshot of a program that runs: it has no frames, and `marker` names the snapshot.
fn running(stops: u64, marker: u32) -> Snapshot {
    Snapshot {
        state: DebugState::Running,
        stops,
        process_id: Some(marker),
        ..Snapshot::default()
    }
}

/// Opens the project and starts a scripted session in its Debug window, as a launch would.
fn start_session(
    cx: &mut TestAppContext,
    name: &str,
    with_input: bool,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<DebugPanel>,
    Arc<ScriptedService>,
) {
    let root = sample_project(name);
    let window = open_with(cx, &root, setup(&root));
    let debug = debug_panel(cx);
    let service = ScriptedService::new(with_input);
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
        .expect("the scripted session attaches");
    wait_for(cx, "the session of the window", |_| service.started());
    show_debug(cx, window);
    (window, debug, service)
}

/// Opens the project with the launch configuration and starts it with Shift+F9, under the
/// scripted service. The build of the launch is `build`.
fn launch(
    cx: &mut TestAppContext,
    name: &str,
    build: ProcessSpec,
) -> (WindowHandle<Workspace>, Arc<ScriptedService>) {
    let root = sample_project(name);
    let service = ScriptedService::new(false);
    let mut project = setup(&root);
    project.providers = vec![Arc::new(BuildRun { build })];
    project.debug = Some(service.clone());
    let window = open_with(cx, &root, project);
    let run = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !run.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    assert!(
        run.update(cx, |panel, cx| panel.select_named("Launch", cx)),
        "the launch configuration is there"
    );
    press(cx, window, "shift-f9");
    (window, service)
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

fn process_id(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> Option<u32> {
    debug.read_with(cx, |panel, _| panel.snapshot().process_id)
}

fn stops(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> u64 {
    debug.read_with(cx, |panel, _| panel.snapshot().stops)
}

/// The status of the toolbar of the Debug window, if it shows one.
fn status(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> Option<String> {
    debug.read_with(cx, |panel, _| panel.state_text().map(str::to_owned))
}

/// The line the Debug window shows last.
fn last_console_line(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> Option<String> {
    debug.read_with(cx, |panel, _| panel.visible_console_lines().last().cloned())
}

/// The line of the Debugger tab that stands in for the frames, if one does.
fn live_text(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> Option<&'static str> {
    debug.read_with(cx, |panel, _| panel.live_text())
}

/// The title and the labels of the balloon of the window, if one is on screen.
fn balloon(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<(String, Vec<String>)> {
    window
        .read_with(cx, |workspace, _| {
            workspace.last_notification().map(|(title, _)| {
                let labels = workspace
                    .last_notification_buttons()
                    .iter()
                    .map(|label| label.to_string())
                    .collect();
                (title.to_string(), labels)
            })
        })
        .ok()
        .flatten()
}

fn failure_balloon(cx: &TestAppContext, window: WindowHandle<Workspace>) -> bool {
    balloon(cx, window).is_some_and(|(title, _)| title == "Build failed")
}

/// Whether the Debugger tab is drawn: its frames and variables, or the line of a program
/// that runs without frames.
fn debugger_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    draw(cx, window);
    shown(cx, window, "debug-frames") || shown(cx, window, "debug-live")
}

#[ui_kit::gpui::test]
fn the_debugger_stays_in_front_while_a_step_runs_with_an_input_reading_program(
    cx: &mut TestAppContext,
) {
    let (window, debug, service) = start_session(cx, "build-front-step", true);

    // Before the first stop the program reads its input in the console, so the console is in
    // front while the program runs.
    service.send(running(0, 1));
    wait_for(cx, "the first snapshot of the program", |cx| {
        process_id(cx, &debug) == Some(1)
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "debug-console-body"),
        "the console is in front before the first stop"
    );

    // The first stop shows the Debugger tab. Each step after it runs, and the tab stays put.
    service.send(stopped(1));
    wait_for(cx, "the first stop", |cx| stops(cx, &debug) == 1);
    for number in 2u64..=3 {
        let marker = number as u32;
        assert!(
            debugger_shown(cx, window),
            "the Debugger shows stop {}",
            number - 1
        );
        debug.update(cx, |panel, cx| panel.step_over(cx));
        service.send(running(number - 1, marker));
        wait_for(cx, "the step to run", |cx| {
            process_id(cx, &debug) == Some(marker)
        });
        assert!(
            debugger_shown(cx, window),
            "the Debugger stays in front while the step runs"
        );
        draw(cx, window);
        assert!(
            !shown(cx, window, "debug-console-body"),
            "the console does not take the front after the first stop"
        );
        assert!(
            debug.read_with(cx, |panel, _| panel.takes_input()),
            "the program still reads its input"
        );
        service.send(stopped(number));
        wait_for(cx, "the stop of the step", |cx| stops(cx, &debug) == number);
    }
    debug.update(cx, |panel, cx| panel.stop(cx));
}

#[ui_kit::gpui::test]
fn the_build_before_a_launch_shows_in_the_console_and_says_building(cx: &mut TestAppContext) {
    let (window, service) = launch(cx, "build-front-running", long_build());
    let debug = debug_panel(cx);
    wait_for(cx, "the build to run", |cx| {
        status(cx, &debug).as_deref() == Some("Building…")
    });
    show_debug(cx, window);
    draw(cx, window);
    assert!(
        shown(cx, window, "debug-console-body"),
        "the console is in front while the build runs"
    );
    assert!(
        !service.started(),
        "the program does not start before its build"
    );

    click(cx, window, "debug-tab-debugger");
    assert!(
        shown(cx, window, "debug-live"),
        "the Debugger tab names the build"
    );
    assert!(
        !shown(cx, window, "debug-idle"),
        "the launch screen does not show during the build"
    );
    assert_eq!(live_text(cx, &debug), Some("Building…"));

    // Stop ends the build. A build that the user stopped is not a failure of the project.
    click(cx, window, "debug-stop");
    wait_for(cx, "the build to end", |cx| status(cx, &debug).is_none());
    assert_eq!(
        last_console_line(cx, &debug).as_deref(),
        Some("Build stopped")
    );
    assert!(
        !failure_balloon(cx, window),
        "a stopped build does not say that it failed"
    );
    assert!(!service.started(), "the program does not start after Stop");
}

#[ui_kit::gpui::test]
fn a_failed_build_keeps_the_console_in_front_and_says_build_failed(cx: &mut TestAppContext) {
    let (window, service) = launch(cx, "build-front-failed", failing_build());
    let debug = debug_panel(cx);
    wait_for(cx, "the build to fail", |cx| {
        status(cx, &debug).as_deref() == Some("Build failed")
    });
    wait_for(cx, "the balloon of the failure", |cx| {
        balloon(cx, window).is_some()
    });
    show_debug(cx, window);

    assert!(!service.started(), "the program does not start");
    draw(cx, window);
    assert!(
        shown(cx, window, "debug-console-body"),
        "the console stays in front"
    );
    assert_eq!(
        last_console_line(cx, &debug).as_deref(),
        Some("Build failed: exit code 3"),
        "the console ends with the failure"
    );

    let (active, running, rerun) = debug.read_with(cx, |panel, _| {
        (
            panel.state().0,
            matches!(panel.snapshot().state, DebugState::Running),
            panel.can_rerun(),
        )
    });
    assert!(!active, "Stop is not active after a failed build");
    assert!(!running, "Pause is not active after a failed build");
    assert!(rerun, "Rerun starts the launch again");

    let (title, labels) = balloon(cx, window).expect("the failure has a balloon");
    assert_eq!(title, "Build failed");
    assert!(
        labels.iter().any(|label| label == "Show Console"),
        "the balloon links to the Console: {labels:?}"
    );

    // The link shows the Console of the Debug window, also when the Debugger tab was chosen.
    click(cx, window, "debug-tab-debugger");
    draw(cx, window);
    assert!(!shown(cx, window, "debug-console-body"));
    click(cx, window, "notification-action");
    draw(cx, window);
    assert!(
        shown(cx, window, "debug-console-body"),
        "the link shows the Console of the Debug window"
    );
}

#[ui_kit::gpui::test]
fn a_session_without_frames_names_its_state_in_the_debugger_tab(cx: &mut TestAppContext) {
    let (window, debug, service) = start_session(cx, "build-front-live", false);
    click(cx, window, "debug-tab-debugger");
    draw(cx, window);
    assert!(
        shown(cx, window, "debug-live"),
        "the Debugger tab names the starting session"
    );
    assert!(
        !shown(cx, window, "debug-idle"),
        "a live session has no launch screen"
    );
    assert_eq!(live_text(cx, &debug), Some("Starting…"));

    service.send(running(0, 1));
    wait_for(cx, "the program runs", |cx| {
        process_id(cx, &debug) == Some(1)
    });
    draw(cx, window);
    assert_eq!(live_text(cx, &debug), Some("The program is running"));
    assert!(shown(cx, window, "debug-live"));
    debug.update(cx, |panel, cx| panel.stop(cx));
}

#[ui_kit::gpui::test]
fn a_stop_after_the_build_brings_the_debugger_tab_front(cx: &mut TestAppContext) {
    let (window, service) = launch(cx, "build-front-stop", succeeding_build());
    let debug = debug_panel(cx);
    show_debug(cx, window);
    wait_for(cx, "the program to start after its build", |_| {
        service.started()
    });

    service.send(stopped(1));
    wait_for(cx, "the first stop", |cx| stops(cx, &debug) == 1);
    assert!(
        debugger_shown(cx, window),
        "a stop brings the Debugger tab after the build"
    );
    debug.update(cx, |panel, cx| panel.stop(cx));
}

#[ui_kit::gpui::test]
fn a_failed_build_of_shift_f10_shows_the_error_and_the_stopped_run(cx: &mut TestAppContext) {
    let root = sample_project("build-front-run");
    let mut project = setup(&root);
    project.providers = vec![Arc::new(BuildRun {
        build: failing_build(),
    })];
    let window = open_with(cx, &root, project);
    let run = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !run.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    assert!(
        run.update(cx, |panel, cx| panel.select_named("Launch", cx)),
        "the launch configuration is there"
    );
    press(cx, window, "shift-f10");
    wait_for(cx, "the failed build to end the run", |cx| {
        run.read_with(cx, |panel, _| panel.service_snapshots())
            .iter()
            .any(|(name, state)| {
                name.as_str() == "Launch"
                    && matches!(state, ServiceState::Stopped { code: Some(3) })
            })
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        !shown(cx, window, "run-idle"),
        "the Run window shows the failed build, not the idle line"
    );
    let text = run.read_with(cx, |panel, _| panel.text());
    assert!(
        text.iter()
            .any(|line| line.as_str() == "Build failed: exit code 3"),
        "the error is in the Run console: {text:?}"
    );
}
