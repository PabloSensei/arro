//! The console of Debug follows its output the way the console of Run does: the last
//! line stays in view while the reader stands at the end, and a reader who scrolled up
//! keeps the place they read until they come back.
//!
//! `cargo test -p panels window_tests_debug_console_follow`

use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, WindowHandle, point, px,
};
use workspace::{DEBUG_TOOL, Workspace};

use crate::debug::DebugPanel;
use crate::window_tests::{bounds_of, click, draw, open_sample_window};
use crate::window_tests_run::debug_panel;

fn show_console(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "debug-tab-console");
    cx.run_until_parked();
    draw(cx, window);
}

/// Puts `lines` lines into the console the way the debugged program writes them.
fn push_lines(cx: &mut TestAppContext, debug: &gpui::Entity<DebugPanel>, lines: usize) {
    let text = (0..lines)
        .map(|index| format!("line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    debug.update(cx, |panel, cx| panel.push_output(text, false, cx));
    cx.run_until_parked();
}

/// Moves the wheel over the console: a positive count scrolls up, a negative one down.
fn scroll_console(cx: &mut TestAppContext, window: WindowHandle<Workspace>, pixels: f32) {
    let area = bounds_of(cx, window, "scroll-area-debug-console-scroll");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: area.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(pixels))),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn drawn(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

fn offset(cx: &TestAppContext, debug: &gpui::Entity<DebugPanel>) -> f32 {
    debug.read_with(cx, |panel, _| panel.console_scroll_y())
}

#[ui_kit::gpui::test]
fn five_hundred_lines_leave_the_last_one_in_view(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-follow-many");
    show_console(cx, window);
    let debug = debug_panel(cx);

    push_lines(cx, &debug, 500);
    draw(cx, window);

    assert!(
        drawn(cx, window, "debug-console-line-499"),
        "the console shows its last line"
    );
    assert!(
        !drawn(cx, window, "debug-console-line-0"),
        "the console does not start at the top"
    );
}

#[ui_kit::gpui::test]
fn a_reader_who_scrolled_up_keeps_the_place(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-follow-up");
    show_console(cx, window);
    let debug = debug_panel(cx);
    push_lines(cx, &debug, 500);
    draw(cx, window);
    let end = offset(cx, &debug);

    scroll_console(cx, window, 440.);
    let before = offset(cx, &debug);
    assert!(
        before > end + 100.,
        "the wheel moved the console up: {before} against {end}"
    );

    push_lines(cx, &debug, 20);
    draw(cx, window);
    assert_eq!(
        offset(cx, &debug),
        before,
        "new lines leave the scrolled view alone"
    );
    assert!(
        !drawn(cx, window, "debug-console-line-519"),
        "the new last line stays out of view"
    );
}

#[ui_kit::gpui::test]
fn a_reader_back_at_the_end_follows_the_output_again(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-follow-down");
    show_console(cx, window);
    let debug = debug_panel(cx);
    push_lines(cx, &debug, 500);
    draw(cx, window);

    scroll_console(cx, window, 440.);
    // A whole page or more down: the end of the console is in view again.
    scroll_console(cx, window, -20000.);
    push_lines(cx, &debug, 1);
    draw(cx, window);

    assert!(
        drawn(cx, window, "debug-console-line-500"),
        "the console follows the line that came at the end"
    );
}

/// The empty console keeps its gray line, the way it did before the list.
#[ui_kit::gpui::test]
fn an_empty_console_says_there_is_no_output(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-follow-empty");
    show_console(cx, window);

    assert!(
        drawn(cx, window, "debug-empty-console"),
        "the empty console names its state"
    );
    let area = bounds_of(cx, window, "scroll-area-debug-console-scroll");
    assert!(
        area.size.height > px(0.),
        "the empty console still has its area"
    );
}
