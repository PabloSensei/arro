//! The chrome has no button without an action and no duplicate of a tool window; a
//! menu that a button opens stands under that button, not at the point of the click.

use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, Pixels, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::*;
use ui_kit::{point, px, size};
use workspace::{
    AI_TOOL, COMMIT_TOOL, DEBUG_TOOL, DockSide, LOG_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, STRUCTURE_TOOL, TASKS_TOOL, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::run::RunPanel;
use crate::window_tests::{
    bounds_of, click, draw, git_repository, open_path, open_sample_window, open_window, press,
    sample_project, wait_for_signal,
};
use crate::window_tests_run::{EchoRun, MockService, open_with, run_panel, setup};
use crate::window_tests_ui::wait_for;

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, Modifiers::default());
    cx.run_until_parked();
}

fn dismiss(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| workspace.dismiss(window, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn tool_open(cx: &TestAppContext, window: WindowHandle<Workspace>, tool: &str) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(tool))
        .unwrap()
}

fn dock_open(cx: &TestAppContext, window: WindowHandle<Workspace>, side: DockSide) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.dock_open(side))
        .unwrap()
}

/// What a click may change: an open overlay or menu, a notification, a tool window or a
/// dock. Two different snapshots mean the button did something.
fn signature(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<bool> {
    window
        .read_with(cx, |workspace, _| {
            let mut state = vec![
                workspace.has_overlay(),
                workspace.menu_entries().is_some(),
                workspace.last_notification().is_some(),
            ];
            for tool in [
                PROJECT_TOOL,
                COMMIT_TOOL,
                LOG_TOOL,
                STRUCTURE_TOOL,
                RUN_TOOL,
                DEBUG_TOOL,
                TERMINAL_TOOL,
                SERVICES_TOOL,
                TODO_TOOL,
                PROBLEMS_TOOL,
                USAGES_TOOL,
                AI_TOOL,
                TASKS_TOOL,
            ] {
                state.push(workspace.tool_open(tool));
            }
            for side in [DockSide::Left, DockSide::Right, DockSide::Bottom] {
                state.push(workspace.dock_open(side));
            }
            state
        })
        .unwrap()
}

fn assert_click_changes(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) {
    let before = signature(cx, window);
    click(cx, window, selector);
    draw(cx, window);
    let after = signature(cx, window);
    assert_ne!(before, after, "{selector} changes nothing");
    dismiss(cx, window);
}

#[ui_kit::gpui::test]
fn the_duplicate_buttons_are_gone_and_ai_sessions_stays_on_the_right_stripe(
    cx: &mut TestAppContext,
) {
    let (_, window) = open_sample_window(cx, "dead-duplicates");
    assert!(!has(cx, window, "t-ai"), "the title bar has no AI button");
    assert!(
        !has(cx, window, "stripe-s-vcs"),
        "the left stripe has no Version Control button"
    );
    assert!(
        !has(cx, window, "stripe-s-more"),
        "the left stripe has no button without an action"
    );
    assert!(
        !has(cx, window, "tabs-more"),
        "the tab bar has no button without an action"
    );
    assert!(has(cx, window, "stripe-s-ai"), "AI Sessions stays");

    let open = dock_open(cx, window, DockSide::Right);
    click(cx, window, "stripe-s-ai");
    draw(cx, window);
    assert_ne!(
        dock_open(cx, window, DockSide::Right),
        open,
        "the button toggles AI Sessions"
    );
    assert!(
        tool_open(cx, window, AI_TOOL) || !dock_open(cx, window, DockSide::Right),
        "AI Sessions is the tool of the right dock"
    );

    // Alt+2 keeps working as it did.
    let open = dock_open(cx, window, DockSide::Right);
    press(cx, window, "alt-2");
    draw(cx, window);
    assert_ne!(
        dock_open(cx, window, DockSide::Right),
        open,
        "Alt+2 toggles the same window"
    );
}

#[ui_kit::gpui::test]
fn every_title_bar_button_changes_something(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "dead-title");
    for selector in [
        "t-search",
        "t-project",
        "t-config",
        "t-settings",
        "app-channel",
    ] {
        assert_click_changes(cx, window, selector);
    }
}

#[ui_kit::gpui::test]
fn the_branch_button_opens_the_branch_menu(cx: &mut TestAppContext) {
    let root = git_repository("dead-branch");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for(cx, "the branch button", |cx| {
        draw(cx, window);
        has(cx, window, "t-branch")
    });
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    wait_for_signal(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });

    click(cx, window, "t-branch");
    wait_for(cx, "the branch menu", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap()
    });
    dismiss(cx, window);
}

#[ui_kit::gpui::test]
fn every_stripe_button_changes_something(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dead-stripes");
    open_path(cx, window, &root.join("src").join("main.txt"));
    draw(cx, window);

    for (selector, tool) in [
        ("stripe-s-project", PROJECT_TOOL),
        ("stripe-s-commit", COMMIT_TOOL),
        ("stripe-s-log", LOG_TOOL),
        ("stripe-s-structure", STRUCTURE_TOOL),
        ("stripe-s-run", RUN_TOOL),
        ("stripe-s-debug", DEBUG_TOOL),
        ("stripe-s-terminal", TERMINAL_TOOL),
        ("stripe-s-services", SERVICES_TOOL),
        ("stripe-s-todo", TODO_TOOL),
        ("stripe-s-problems", PROBLEMS_TOOL),
        ("stripe-s-usages", USAGES_TOOL),
    ] {
        let before = tool_open(cx, window, tool);
        click(cx, window, selector);
        draw(cx, window);
        assert_ne!(
            tool_open(cx, window, tool),
            before,
            "{selector} toggles {tool}"
        );
        dismiss(cx, window);
    }

    let (selector, tool) = ("stripe-s-tasks", TASKS_TOOL);
    let before = tool_open(cx, window, tool);
    click(cx, window, selector);
    draw(cx, window);
    assert_ne!(
        tool_open(cx, window, tool),
        before,
        "{selector} toggles {tool}"
    );
    dismiss(cx, window);

    let before = notification_serial(cx, window);
    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    assert!(
        notification_serial(cx, window) > before
            || window
                .read_with(cx, |workspace, _| workspace.has_overlay())
                .unwrap(),
        "s-bookmarks opens the bookmark list or says it is empty"
    );
    dismiss(cx, window);

    // Notifications need no text to show their balloon.
    let visible = has(cx, window, "notification");
    click(cx, window, "stripe-s-notifications");
    draw(cx, window);
    assert_ne!(
        has(cx, window, "notification"),
        visible,
        "s-notifications toggles the balloon"
    );
    click(cx, window, "stripe-s-notifications");
    draw(cx, window);
}

fn notification_serial(cx: &TestAppContext, window: WindowHandle<Workspace>) -> usize {
    window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_run_and_debug_buttons_show_their_windows(cx: &mut TestAppContext) {
    let root = sample_project("dead-run");
    let mut project = setup(&root);
    project.providers = vec![Arc::new(EchoRun)];
    let service = MockService::new();
    project.debug = Some(service.clone());
    let window = open_with(cx, &root, project);
    let panel: gpui::Entity<RunPanel> = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    click(cx, window, "t-run");
    wait_for(cx, "the Run window", |cx| {
        draw(cx, window);
        tool_open(cx, window, RUN_TOOL)
    });
    press(cx, window, "ctrl-f2");

    click(cx, window, "t-debug");
    wait_for(cx, "the Debug window", |cx| {
        draw(cx, window);
        tool_open(cx, window, DEBUG_TOOL)
    });
    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the debug session", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
    });
    service.join();
}

#[ui_kit::gpui::test]
fn a_toolbar_menu_stands_under_its_button(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "dead-config-menu");
    let button = bounds_of(cx, window, "t-config");
    let expected = point(button.left(), button.bottom() + px(SPACE_XS));

    let left = point(button.left() + px(2.), button.top() + px(2.));
    click_at(cx, window, left);
    draw(cx, window);
    let first = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the configuration menu is open");
    let popup = bounds_of(cx, window, "menu-popup");
    assert_eq!(first, expected, "the menu starts under the button");
    assert_eq!(popup.origin, expected, "the menu is drawn under the button");
    dismiss(cx, window);

    let right = point(button.right() - px(2.), button.bottom() - px(2.));
    click_at(cx, window, right);
    draw(cx, window);
    let second = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the configuration menu is open");
    assert_eq!(
        second, first,
        "another point of the button opens the menu in the same place"
    );
    dismiss(cx, window);
}

#[ui_kit::gpui::test]
fn a_status_menu_stands_under_its_widget(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dead-status-menu");
    open_path(cx, window, &root.join("src").join("main.txt"));
    draw(cx, window);
    let widget = bounds_of(cx, window, "st-eol");

    let left = point(widget.left() + px(2.), widget.top() + px(2.));
    click_at(cx, window, left);
    draw(cx, window);
    let first = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the line separator menu is open");
    let popup = bounds_of(cx, window, "menu-popup");
    // The status bar is at the bottom, so the menu opens above the widget.
    assert_eq!(first, popup.origin, "the menu is drawn where it was placed");
    let left_edges = (f32::from(first.x) - f32::from(widget.left())).abs();
    let right_edges = (f32::from(first.x + popup.size.width) - f32::from(widget.right())).abs();
    assert!(
        left_edges <= 1. || right_edges <= 1.,
        "the menu is aligned with the widget: {first:?} at {widget:?}"
    );
    assert_eq!(
        first.y,
        widget.top() - px(SPACE_XS) - popup.size.height,
        "its bottom edge is a hair above the widget"
    );
    dismiss(cx, window);

    let right = point(widget.right() - px(2.), widget.bottom() - px(2.));
    click_at(cx, window, right);
    draw(cx, window);
    let second = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the line separator menu is open");
    assert_eq!(
        second, first,
        "another point of the widget opens the menu in the same place"
    );
    dismiss(cx, window);
}

#[ui_kit::gpui::test]
fn a_menu_at_the_right_edge_stays_inside_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dead-right-edge");
    open_path(cx, window, &root.join("src").join("main.txt"));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(480.), px(600.)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);

    click(cx, window, "st-indent");
    draw(cx, window);
    let popup = bounds_of(cx, window, "menu-popup");
    let width = px(480.);
    assert!(
        popup.left() >= px(SPACE_M),
        "the menu keeps the left margin: {popup:?}"
    );
    assert!(
        popup.right() <= width - px(SPACE_M),
        "the menu keeps the right margin: {popup:?}"
    );
    dismiss(cx, window);
}
