//! The MCP build of a project whose process ends before the Run window looks at it. The
//! answer is the result of the build, and "the run did not start" only when the process
//! never started. The test holds the first look of the window at the run until the run is
//! over, so the poll that reads the build finds all of its events at once, as a loaded
//! machine makes it do.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, ProjectModel};
use serde_json::json;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::mcp_approval::Reply;
use crate::window_tests::sample_project;
use crate::window_tests_mcp_runs::{
    answer, failing_tool, gateway, scope, wait_for_call, wait_for_question,
};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The panels poll every 50 ms of the simulated clock.
const POLL: Duration = Duration::from_millis(50);

/// A build tool whose command is a program that is not installed.
struct MissingTool(ProjectModel);

impl BuildSystem for MissingTool {
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
        Ok(ProcessSpec::new("arro-no-such-build-tool"))
    }
}

/// Calls `build_project` of a window whose build tool is `tool`, and returns the answer of
/// the call. The first poll after the answer takes the job and starts the build; the build
/// ends on its own, and the next poll is the first look at it.
#[cfg(windows)]
fn build_that_ends_unseen(
    cx: &mut TestAppContext,
    root: &Path,
    tool: Arc<dyn BuildSystem>,
) -> Result<String, String> {
    let mut setup = setup(root);
    setup.tool = Some(tool);
    open_with(cx, root, setup);
    let panel = run_panel(cx);
    panel.update(cx, |panel, _| panel.hold_observation());
    let (gateway, approvals) = gateway(cx);

    let project = scope(root);
    let call = thread::spawn(move || {
        gateway
            .call(&project, "build_project", &json!({}))
            .map_err(|error| error.to_string())
    });
    wait_for_question(cx, &approvals);
    answer(cx, Reply::Once);
    wait_for(cx, "the console of the build", |cx| {
        panel.read_with(cx, |panel, _| panel.console_started())
    });
    cx.executor().advance_clock(POLL);
    cx.run_until_parked();
    wait_for_call(cx, "the answer of the build", call)
}

/// A build that prints a compile error and fails, and ends before the window looks at it,
/// answers the error and the outcome of the build, not that the run did not start.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_build_that_ends_unseen_answers_the_compile_error(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-unseen-build");
    let text = build_that_ends_unseen(cx, &root, failing_tool(&root)).expect("the build answer");

    assert!(text.contains("build of the project failed"), "{text}");
    assert!(text.contains("src/Foo.txt:3: cannot find symbol"), "{text}");
    assert!(text.contains("output: "), "{text}");
}

/// A program that is not installed never starts: the answer says that the run did not
/// start, and it does not say that a process finished.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_program_that_is_missing_answers_that_the_run_did_not_start(cx: &mut TestAppContext) {
    let root = sample_project("mcp-runs-unseen-missing");
    let tool = Arc::new(MissingTool(ProjectModel {
        root: root.clone(),
        tool: "fixture".into(),
        resolved: true,
        ..ProjectModel::default()
    }));
    let refusal = build_that_ends_unseen(cx, &root, tool).expect_err("the refusal");

    assert!(refusal.starts_with("the run did not start"), "{refusal}");
    assert!(!refusal.contains("Process finished"), "{refusal}");
}
