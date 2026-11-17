//! The layout of the window title bar and of the tool window stripes: the groups of
//! controls, their gaps and the one place the metrics come from.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::sync::Arc;

use ui_kit::VisualTestContext;
use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, WindowHandle};
use ui_kit::metrics::*;

use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, sample_project};
use crate::window_tests_run::{
    EchoRun, MockService, debug_panel, open_with, run_panel, setup, snapshot, wait_for,
};
#[cfg(windows)]
use crate::window_tests_run::{LongRun, long_image};

/// The tolerance of a measured gap: layout rounds logical pixels to device pixels.
const TOLERANCE: f32 = 0.5;

fn close_to(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "{what}: {actual} instead of {expected}"
    );
}

fn gap(right: Bounds<Pixels>, left: Bounds<Pixels>) -> f32 {
    f32::from(left.left() - right.right())
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn size(bounds: Bounds<Pixels>) -> (f32, f32) {
    (f32::from(bounds.size.width), f32::from(bounds.size.height))
}

fn wait_for_config(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
}

#[ui_kit::gpui::test]
fn one_debug_button_starts_the_selected_configuration(cx: &mut TestAppContext) {
    let root = sample_project("chrome-debug");
    let service = MockService::new();
    let mut project = setup(&root);
    project.providers = vec![Arc::new(EchoRun)];
    project.debug = Some(service.clone());
    let window = open_with(cx, &root, project);
    let panel = run_panel(cx);

    wait_for_config(cx, &panel);
    draw(cx, window);
    assert!(has(cx, window, "t-debug"), "the toolbar has a Debug button");
    click(cx, window, "t-debug");
    wait_for(cx, "the debug start", |_| service.starts() == 1);
    assert_eq!(
        service.starts(),
        1,
        "one click starts one debug session of the selected configuration"
    );
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the debug session", |cx| {
        snapshot(cx).state == dap_client::DebugState::Terminated
            && !debug_panel(cx).read_with(cx, |panel, _| panel.state()).0
    });
    service.join();
}

#[ui_kit::gpui::test]
fn no_more_button_and_settings_opens_its_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "chrome-settings");
    click(cx, window, "t-settings");
    wait_for(cx, "the Settings window", |cx| {
        draw(cx, window);
        has(cx, window, "settings-font-up")
    });
    assert!(!has(cx, window, "t-more"), "the toolbar has no ⋮ button");
    assert!(
        has(cx, window, "t-settings"),
        "the toolbar keeps the Settings button"
    );
}

#[ui_kit::gpui::test]
fn the_toolbar_keeps_the_spacing_of_its_groups(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "chrome-spacing");
    let project = bounds_of(cx, window, "t-project");
    let channel = bounds_of(cx, window, "app-channel");
    let config = bounds_of(cx, window, "t-config");
    let run = bounds_of(cx, window, "t-run");
    let debug = bounds_of(cx, window, "t-debug");
    let search = bounds_of(cx, window, "t-search");
    let settings = bounds_of(cx, window, "t-settings");

    close_to(gap(project, channel), SPACE_XS, "project to channel");
    close_to(gap(config, run), SPACE_XS, "configuration to Run");
    close_to(gap(run, debug), SPACE_XS, "Run to Debug");
    close_to(gap(debug, search), SPACE_L, "run group to tools");
    close_to(gap(search, settings), SPACE_XS, "Search to Settings");
}

#[ui_kit::gpui::test]
fn the_toolbar_controls_share_their_sizes(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "chrome-sizes");
    for selector in ["t-run", "t-debug", "t-search", "t-settings"] {
        let (width, height) = size(bounds_of(cx, window, selector));
        close_to(width, ICON_BUTTON, &format!("{selector} width"));
        close_to(height, ICON_BUTTON, &format!("{selector} height"));
    }
    for selector in ["t-config", "t-project", "app-channel"] {
        let (_, height) = size(bounds_of(cx, window, selector));
        close_to(height, CONTROL_HEIGHT, &format!("{selector} height"));
    }
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn stop_appears_right_of_debug_without_moving_run_and_debug(cx: &mut TestAppContext) {
    let root = sample_project("chrome-stop");
    let image = long_image("chrome-stop");
    let mut project = setup(&root);
    project.providers = vec![Arc::new(LongRun {
        image: image.clone(),
    })];
    let window = open_with(cx, &root, project);
    let panel = run_panel(cx);

    wait_for_config(cx, &panel);
    draw(cx, window);
    let run_before = bounds_of(cx, window, "t-run");
    let debug_before = bounds_of(cx, window, "t-debug");

    ui_kit::gpui::TestAppContext::simulate_keystrokes(&mut *cx, window.into(), "shift-f10");
    cx.run_until_parked();
    wait_for(cx, "the long run", |cx| {
        panel.read_with(cx, |panel, _| panel.state().running)
    });
    wait_for(cx, "Stop in the toolbar", |cx| has(cx, window, "t-stop"));
    draw(cx, window);

    let run_after = bounds_of(cx, window, "t-run");
    let debug_after = bounds_of(cx, window, "t-debug");
    let stop = bounds_of(cx, window, "t-stop");
    close_to(
        f32::from(run_after.left() - run_before.left()),
        0.,
        "Run moved",
    );
    close_to(
        f32::from(debug_after.left() - debug_before.left()),
        0.,
        "Debug moved",
    );
    assert!(
        stop.left() >= debug_after.right(),
        "Stop is to the right of Debug"
    );
    close_to(gap(debug_after, stop), SPACE_XS, "Debug to Stop");

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped program", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
    });
    let _ = std::fs::remove_file(&image);
}

#[ui_kit::gpui::test]
fn the_tool_stripes_share_one_size_and_axis(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "chrome-stripes");
    let left = [
        "stripe-s-project",
        "stripe-s-commit",
        "stripe-s-log",
        "stripe-s-structure",
        "stripe-s-run",
        "stripe-s-debug",
        "stripe-s-terminal",
        "stripe-s-services",
        "stripe-s-todo",
        "stripe-s-problems",
        "stripe-s-usages",
    ];
    let right = [
        "stripe-s-notifications",
        "stripe-s-ai",
        "stripe-s-tasks",
        "stripe-s-bookmarks",
    ];
    for stripe in [&left[..], &right[..]] {
        let first = bounds_of(cx, window, stripe[0]);
        let axis = first.center().x;
        close_to(f32::from(first.size.width), ICON_BUTTON, "stripe width");
        close_to(f32::from(first.size.height), ICON_BUTTON, "stripe height");
        for selector in &stripe[1..] {
            let bounds = bounds_of(cx, window, selector);
            close_to(f32::from(bounds.size.width), ICON_BUTTON, selector);
            close_to(f32::from(bounds.size.height), ICON_BUTTON, selector);
            close_to(f32::from(bounds.center().x - axis), 0., selector);
        }
    }
}
