//! The Run window before anything ran in it names the selected configuration: Run starts it,
//! and a link opens the Run/Debug Configurations.
//!
//! cargo test -p panels window_tests_run_idle

#![cfg(windows)]

use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{RUN_TOOL, Workspace};

use crate::window_tests::{click, draw, sample_project, wait_for_process};
use crate::window_tests_run::{EchoRun, open_with, run_panel, setup, wait_for};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// A window of a project with the configuration `Echo`, with its Run window shown.
fn run_window(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let root = sample_project(name);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    window
}

#[ui_kit::gpui::test]
fn the_idle_run_window_names_the_configuration_and_run_starts_it(cx: &mut TestAppContext) {
    let window = run_window(cx, "run-idle-start");
    assert!(
        has(cx, window, "run-idle"),
        "a Run window with a configuration and no output names it"
    );
    assert!(has(cx, window, "run-idle-run"), "the name is a link");
    let panel = run_panel(cx);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.state().label().to_string()),
        "Echo",
        "the link names the selected configuration"
    );

    click(cx, window, "run-idle-run");
    wait_for_process(cx, |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .text()
                .iter()
                .any(|line| line.contains("hello from run"))
        })
    });
    draw(cx, window);
    assert!(
        !has(cx, window, "run-idle"),
        "the output of the run replaces the link"
    );
}

#[ui_kit::gpui::test]
fn the_idle_run_window_links_to_the_configurations(cx: &mut TestAppContext) {
    let window = run_window(cx, "run-idle-edit");
    assert!(has(cx, window, "run-idle-edit"), "the link is there");
    click(cx, window, "run-idle-edit");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("run-configs").is_some(),
        "Edit Configurations opens Run/Debug Configurations"
    );
}
