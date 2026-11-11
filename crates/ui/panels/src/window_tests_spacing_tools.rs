//! The spacing of the tool windows: the rows of their lists, the buttons of a panel
//! toolbar and the header of every tool window come from the same metrics.

use std::fs;

use ui_kit::VisualTestContext;
use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, WindowHandle};
use ui_kit::metrics::*;

use workspace::{COMMIT_TOOL, DEBUG_TOOL, LOG_TOOL, PROJECT_TOOL, RUN_TOOL, Workspace};

use crate::window_tests::{bounds_of, draw, open_path, open_window, press};
use crate::window_tests_git::{git_state, repository_with_origin};
use crate::window_tests_run::debug_panel;
use crate::window_tests_ui::wait_for;

/// The tolerance of a measured gap: layout rounds logical pixels to device pixels.
const TOLERANCE: f32 = 0.5;

fn close_to(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() <= TOLERANCE,
        "{what}: {actual} instead of {expected}"
    );
}

fn size(bounds: Bounds<Pixels>) -> (f32, f32) {
    (f32::from(bounds.size.width), f32::from(bounds.size.height))
}

/// The step between two buttons of a bar, whichever way the bar runs.
fn step(first: Bounds<Pixels>, next: Bounds<Pixels>) -> f32 {
    if next.left() >= first.right() {
        f32::from(next.left() - first.right())
    } else {
        f32::from(next.top() - first.bottom())
    }
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn row_height(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let (_, height) = size(bounds_of(cx, window, selector));
    close_to(height, ROW_HEIGHT, &format!("{selector} height"));
}

fn header_height(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let (_, height) = size(bounds_of(cx, window, selector));
    close_to(height, CONTROL_HEIGHT, &format!("{selector} height"));
}

fn button_bar(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selectors: &[&'static str],
) {
    let bounds: Vec<_> = selectors
        .iter()
        .map(|selector| bounds_of(cx, window, selector))
        .collect();
    for (selector, bounds) in selectors.iter().zip(&bounds) {
        let (width, height) = size(*bounds);
        close_to(width, ICON_BUTTON, &format!("{selector} width"));
        close_to(height, ICON_BUTTON, &format!("{selector} height"));
    }
    for pair in bounds.windows(2) {
        close_to(step(pair[0], pair[1]), SPACE_XS, "step between buttons");
    }
}

#[ui_kit::gpui::test]
fn tool_window_rows_buttons_and_headers_share_the_metrics(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("spacing-tools");
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let git = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    git.update(cx, |git, cx| git.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        git.read_with(cx, |git, _| !git.status().files.is_empty())
    });

    // Project: the header and a row of the tree.
    header_height(cx, window, "project-header");
    row_height(cx, window, "tree-row-main.txt");

    // Run: the header and the column of buttons of the console.
    show_tool(cx, window, RUN_TOOL);
    header_height(cx, window, "run-header");
    button_bar(cx, window, &["run-rerun", "run-stop"]);

    // Debug: its own bar of buttons and the row of a breakpoint.
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let debug = debug_panel(cx);
    press(cx, window, "ctrl-f8");
    wait_for(cx, "the breakpoint", |cx| {
        !debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty()
    });
    show_tool(cx, window, DEBUG_TOOL);
    header_height(cx, window, "debug-header");
    button_bar(cx, window, &["debug-continue", "debug-over", "debug-into"]);
    assert!(
        has(cx, window, "debug-breakpoint-0"),
        "the breakpoint row is on screen"
    );
    row_height(cx, window, "debug-breakpoint-0");

    // Commit: the header and the row of the changed file.
    show_tool(cx, window, COMMIT_TOOL);
    wait_for(cx, "the row of the file", |cx| {
        has(cx, window, "commit-row-0")
    });
    header_height(cx, window, "commit-header");
    row_height(cx, window, "commit-row-0");

    // Git Log: the header and the row of a commit.
    show_tool(cx, window, LOG_TOOL);
    wait_for(cx, "the commits", |cx| has(cx, window, "log-commit-0"));
    header_height(cx, window, "log-header");
    row_height(cx, window, "log-commit-0");
}

#[ui_kit::gpui::test]
fn every_tool_window_header_has_one_height(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("spacing-headers");
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let git = git_state(cx);
    wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    git.update(cx, |git, cx| git.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        git.read_with(cx, |git, _| !git.status().files.is_empty())
    });

    let mut heights = Vec::new();
    let selectors = [
        ("project-header", PROJECT_TOOL),
        ("run-header", RUN_TOOL),
        ("debug-header", DEBUG_TOOL),
        ("commit-header", COMMIT_TOOL),
        ("log-header", LOG_TOOL),
    ];
    for (selector, tool) in selectors {
        show_tool(cx, window, tool);
        let (_, height) = size(bounds_of(cx, window, selector));
        close_to(height, CONTROL_HEIGHT, &format!("{selector} height"));
        heights.push((selector, height));
    }
    let first = heights[0].1;
    for (selector, height) in &heights[1..] {
        close_to(
            *height,
            first,
            &format!("{selector} against the Project header"),
        );
    }
}
