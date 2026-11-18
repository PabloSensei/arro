//! T-0306: Stop while a launch still prepares in the background. The plan of the
//! substitute run and of the substitute build comes after the test asks for Stop: neither
//! of them starts anything, and the console of the run says the launch was cancelled.
//!
//! cargo test -p panels window_tests_run_starting

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use runner::{RunConfig, RunConfigProvider, RunPlan, ServiceState};
use ui_kit::gpui;
use ui_kit::gpui::{Keystroke, OperatingSystem, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::sample_project;
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The program that would tell the test that something started after Stop.
fn start_marker(marker: &Path) -> ProcessSpec {
    ProcessSpec::new("cmd").args([
        "/d",
        "/c",
        &format!("echo started>\"{}\"", marker.display()),
    ])
}

/// A configuration whose plan comes only after the test asks for Stop.
struct WaitingRun {
    planned: Arc<Mutex<usize>>,
    marker: PathBuf,
}

impl RunConfigProvider for WaitingRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "App".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
            ..RunConfig::default()
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        *self
            .planned
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += 1;
        Ok(RunPlan {
            before: None,
            program: start_marker(&self.marker),
            grace: Duration::from_secs(1),
        })
    }
}

/// A build tool whose build comes only after the test asks for Stop.
struct WaitingBuild {
    model: ProjectModel,
    planned: Arc<Mutex<usize>>,
    marker: PathBuf,
}

impl BuildSystem for WaitingBuild {
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
        Ok(start_marker(&self.marker))
    }

    fn build(&self, _: &Path, _: bool) -> base::Result<ProcessSpec> {
        *self
            .planned
            .lock()
            .unwrap_or_else(|error| error.into_inner()) += 1;
        Ok(start_marker(&self.marker))
    }
}

fn model(root: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        ..ProjectModel::default()
    }
}

/// Sends a key without letting the background tasks it started finish: the test can ask
/// for Stop while the plan is still in the hands of the executor.
fn key(cx: &mut TestAppContext, window: WindowHandle<Workspace>, name: &str) {
    let stroke = Keystroke::parse_with_os(name, OperatingSystem::Windows).expect("a known key");
    cx.dispatch_keystroke(window.into(), stroke);
}

fn wait_for_config(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_for(cx, "the configuration", |cx| {
        panel.read_with(cx, |panel, _| !panel.state().configs.is_empty())
    });
}

#[ui_kit::gpui::test]
fn stop_while_a_run_is_planned_keeps_the_program_from_starting(cx: &mut TestAppContext) {
    let root = sample_project("run-stop-planning");
    let marker = root.join("program-started.txt");
    let planned = Arc::new(Mutex::new(0));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(WaitingRun {
        planned: planned.clone(),
        marker: marker.clone(),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for_config(cx, &panel);

    key(cx, window, "shift-f10");
    assert!(
        panel.read_with(cx, |panel, _| !panel.idle()),
        "the run is still preparing"
    );
    key(cx, window, "ctrl-f2");
    assert!(
        panel.read_with(cx, |panel, _| panel.idle()),
        "Stop ends the launch at once"
    );
    cx.run_until_parked();

    let planned = *planned.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(planned, 1, "the plan came after Stop");
    assert!(
        !marker.exists(),
        "the program started after Stop cancelled the launch"
    );
    let stopped = panel.read_with(cx, |panel, _| panel.service_snapshots());
    assert!(
        stopped
            .iter()
            .any(|(_, state)| matches!(state, ServiceState::Stopped { code: None })),
        "the tab of the run says it is stopped: {stopped:?}"
    );
    let text = panel.read_with(cx, |panel, _| panel.text());
    assert!(
        text.iter().any(|line| line.contains("Launch cancelled")),
        "the console says why nothing started: {text:?}"
    );
}

#[ui_kit::gpui::test]
fn stop_while_a_build_is_planned_keeps_the_build_from_starting(cx: &mut TestAppContext) {
    let root = sample_project("run-stop-build-planning");
    let marker = root.join("build-started.txt");
    let planned = Arc::new(Mutex::new(0));
    let mut setup = setup(&root);
    setup.tool = Some(Arc::new(WaitingBuild {
        model: model(&root),
        planned: planned.clone(),
        marker: marker.clone(),
    }));
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    panel.update(cx, |panel, cx| panel.build_project(cx));
    assert!(
        panel.read_with(cx, |panel, _| !panel.idle()),
        "the build is still preparing"
    );
    key(cx, window, "ctrl-f2");
    cx.run_until_parked();

    let planned = *planned.lock().unwrap_or_else(|error| error.into_inner());
    assert_eq!(planned, 1, "the build plan came after Stop");
    wait_for(cx, "the stopped build", |cx| {
        panel.read_with(cx, |panel, _| panel.idle())
    });
    assert!(
        !marker.exists(),
        "the build started after Stop cancelled it"
    );
}
