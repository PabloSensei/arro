//! Every icon button of the title bar, the tool window stripes and the Run and Debug
//! panels names itself in a tooltip; a button with a key shows the key too.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
#[cfg(windows)]
use std::sync::Arc;
use std::time::Duration;

use ui_kit::gpui::{self, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{TOOLTIP_DELAY, clear_shown_tooltips, shown_tooltips};
use workspace::{DEBUG_TOOL, RUN_TOOL, Workspace};

use crate::window_tests::{bounds_of, draw, open_sample_window};
#[cfg(windows)]
use crate::window_tests::{press, sample_project};
#[cfg(windows)]
use crate::window_tests_run::{LongRun, long_image, open_with, run_panel, setup, wait_for};

/// Hovers the middle of an element and returns the tooltip it shows.
fn hover(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> (String, Option<String>) {
    clear_shown_tooltips();
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    drop(visual);
    cx.executor()
        .advance_clock(TOOLTIP_DELAY + Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    shown_tooltips()
        .into_iter()
        .last()
        .unwrap_or_else(|| panic!("{selector} shows no tooltip"))
}

fn assert_tooltip(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    name: &str,
    key: Option<&str>,
) {
    let (shown, shown_key) = hover(cx, window, selector);
    assert_eq!(shown, name, "{selector} names itself");
    assert_eq!(shown_key.as_deref(), key, "{selector} shows its key");
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn title_bar_buttons_name_themselves_and_run_shows_shift_f10(cx: &mut TestAppContext) {
    let root = sample_project("tooltips-title");
    let image = long_image("tooltips-title");
    let mut project = setup(&root);
    project.providers = vec![Arc::new(LongRun {
        image: image.clone(),
    })];
    let window = open_with(cx, &root, project);
    let panel = run_panel(cx);
    draw(cx, window);

    assert_tooltip(cx, window, "t-run", "Run", Some("Shift+F10"));
    assert_tooltip(cx, window, "t-debug", "Debug", Some("Shift+F9"));
    assert_tooltip(
        cx,
        window,
        "t-search",
        "Search Everywhere",
        Some("Double Shift"),
    );
    assert_tooltip(cx, window, "t-settings", "Settings", Some("Ctrl+Alt+S"));

    wait_for(cx, "a run configuration", |cx| {
        !panel
            .read_with(cx, |panel, _| panel.state())
            .configs
            .is_empty()
    });
    press(cx, window, "shift-f10");
    wait_for(cx, "the long run", |cx| {
        panel.read_with(cx, |panel, _| panel.state().running)
    });
    wait_for(cx, "Stop in the toolbar", |cx| {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("t-stop").is_some()
    });
    draw(cx, window);
    assert_tooltip(cx, window, "t-stop", "Stop", Some("Ctrl+F2"));

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped program", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
    });
    let _ = std::fs::remove_file(&image);
}

#[ui_kit::gpui::test]
fn stripes_and_the_run_and_debug_bars_name_themselves(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "tooltips-stripes");
    draw(cx, window);
    assert_tooltip(cx, window, "stripe-s-project", "Project", Some("Alt+1"));

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert_tooltip(cx, window, "run-rerun", "Rerun", Some("Ctrl+F5"));
    assert_tooltip(cx, window, "run-stop", "Stop", Some("Ctrl+F2"));

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert_tooltip(cx, window, "debug-continue", "Resume", Some("F9"));
}
