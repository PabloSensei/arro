//! Build Project and hot swap without a real window: the build tool and the adapter are
//! replaced, the Run panel and the Debug panel are the ones the application builds.

#![cfg(windows)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::{BreakpointSpec, DebugService, Debugger, Launch, ReloadOutcome};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel, Unit};
use runner::{RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::run::{ProjectSetup, RunPanel};
use crate::script::ActiveRunPanel;
use crate::window_tests::{draw, open_window_with, press, sample_project, scratch};

/// How long a test waits for the panel, the build or the adapter.
const LIMIT: Duration = Duration::from_secs(15);
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

fn run_panel(cx: &TestAppContext) -> gpui::Entity<RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

fn run_text(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| panel.text())
}

fn notification(cx: &TestAppContext, window: WindowHandle<Workspace>) -> String {
    window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(_, text)| text.to_string())
        .unwrap_or_default()
}

/// The model of the scratch project: one module with an output folder of its own.
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

/// A build tool that counts the builds Build Project asks for and succeeds at once.
struct CountingTool {
    model: ProjectModel,
    builds: Arc<AtomicUsize>,
}

impl BuildSystem for CountingTool {
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
        self.builds.fetch_add(1, Ordering::SeqCst);
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// A configuration that would run a program; the debug service replaces the program with
/// the mock adapter, so only the configuration matters here.
struct ProbeRun;

impl RunConfigProvider for ProbeRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Probe".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

/// A debug service that starts the mock adapter and answers `redefineClasses` from it.
struct HotSwapService {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
}

impl HotSwapService {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
        })
    }

    fn session(
        &self,
        name: String,
        breakpoints: Vec<BreakpointSpec>,
        watch: Vec<String>,
        exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start().map_err(|error| error.to_string())?;
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

    fn requests(&self) -> Vec<serde_json::Value> {
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

impl DebugService for HotSwapService {
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

    fn reload_classes(
        &self,
        debugger: Debugger,
        _outputs: Vec<PathBuf>,
        done: std::sync::mpsc::Sender<ReloadOutcome>,
    ) -> bool {
        self.runtime.spawn(async move {
            let outcome = match debugger
                .request("redefineClasses", serde_json::json!({}))
                .await
            {
                Ok(body) => ReloadOutcome {
                    reloaded: body
                        .get("changedClasses")
                        .and_then(serde_json::Value::as_array)
                        .map(|list| {
                            list.iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    error: body
                        .get("errorMessage")
                        .and_then(serde_json::Value::as_str)
                        .filter(|message| !message.is_empty())
                        .map(str::to_owned),
                },
                Err(error) => ReloadOutcome {
                    reloaded: Vec::new(),
                    error: Some(error),
                },
            };
            let _ = done.send(outcome);
        });
        true
    }
}

fn setup(
    root: &Path,
    builds: Arc<AtomicUsize>,
    debug: Option<Arc<dyn DebugService>>,
) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(CountingTool {
            model: model(root),
            builds,
        })),
        providers: vec![Arc::new(ProbeRun)],
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("hotswap-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug,
    }
}

fn open_with(cx: &mut TestAppContext, root: &Path, setup: ProjectSetup) -> WindowHandle<Workspace> {
    let window = open_window_with(cx, root.to_path_buf(), setup);
    cx.run_until_parked();
    draw(cx, window);
    window
}

fn wait_for_project(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn ctrl_f9_builds_the_project_without_debugging(cx: &mut TestAppContext) {
    let root = sample_project("hotswap-build");
    let builds = Arc::new(AtomicUsize::new(0));
    let window = open_with(cx, &root, setup(&root, builds.clone(), None));
    let panel = run_panel(cx);
    wait_for_project(cx, &panel);

    press(cx, window, "ctrl-f9");

    wait_for(cx, "the build to finish", |cx| {
        builds.load(Ordering::SeqCst) == 1
            && run_text(cx, &panel)
                .iter()
                .any(|line| line.contains("Process finished with exit code 0"))
    });
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(workspace::RUN_TOOL))
            .unwrap(),
        "Ctrl+F9 shows the build console"
    );
    assert_eq!(
        notification(cx, window),
        "",
        "a build without a debug session is not a hot swap"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn ctrl_f9_during_debugging_reloads_classes_and_tells_the_user(cx: &mut TestAppContext) {
    let root = sample_project("hotswap-debug");
    let builds = Arc::new(AtomicUsize::new(0));
    let service = HotSwapService::new();
    let window = open_with(
        cx,
        &root,
        setup(&root, builds.clone(), Some(service.clone())),
    );
    let panel = run_panel(cx);
    wait_for_project(cx, &panel);

    press(cx, window, "shift-f9");
    let debug = cx.update(|cx| cx.global::<crate::debug::ActiveDebug>().0.clone());
    wait_for(cx, "the mock session to stop", |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            dap_client::DebugState::Stopped { .. }
        )
    });

    let class = root.join("classes/demo/Probe.class");
    std::fs::create_dir_all(class.parent().unwrap()).unwrap();
    std::fs::write(&class, b"changed").unwrap();
    let serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();

    press(cx, window, "ctrl-f9");

    wait_for(cx, "the hot swap answer", |cx| {
        builds.load(Ordering::SeqCst) == 1
            && window
                .read_with(cx, |workspace, _| workspace.notification_serial())
                .unwrap()
                > serial
    });
    assert!(
        notification(cx, window).contains("Reloaded 2 classes"),
        "the notification names the classes: {:?}",
        notification(cx, window)
    );
    assert!(
        service
            .requests()
            .iter()
            .any(|request| request["command"] == "redefineClasses"),
        "the adapter received the hot swap request"
    );
    assert!(
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            dap_client::DebugState::Stopped { .. }
        ),
        "the session stays alive after the reload"
    );

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the session", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone())
            == dap_client::DebugState::Terminated
    });
    service.join();
}
