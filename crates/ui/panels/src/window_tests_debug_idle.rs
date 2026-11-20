//! The Debug window before a session names the selected configuration, which Shift+F9
//! debugs, and links to the Run/Debug Configurations.
//!
//! cargo test -p panels window_tests_debug_idle

#![cfg(windows)]

use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DEBUG_TOOL, Workspace};

use crate::window_tests::{click, draw, sample_project};
use crate::window_tests_run::{EchoRun, open_with, run_panel, setup, wait_for};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn the_debug_window_names_the_configuration_and_links_to_the_configurations(
    cx: &mut TestAppContext,
) {
    let root = sample_project("debug-idle");
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);

    assert!(
        has(cx, window, "debug-idle-debug"),
        "a Debug window with a configuration names it"
    );
    assert!(
        !has(cx, window, "debug-empty-debug"),
        "the chooser link is for a window without a configuration"
    );
    click(cx, window, "debug-idle-edit");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("run-configs").is_some(),
        "Edit Configurations opens Run/Debug Configurations"
    );
}
