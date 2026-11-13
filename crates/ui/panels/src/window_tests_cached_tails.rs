//! The tails of cached views: a finished shell, the delay of a tooltip and the state of
//! the inspection hub must reach a view that keeps its frame until it is told otherwise.
//!
//! The frames here go through `frame` from `window_tests_cached_views`, which draws
//! without `Window::refresh`, so the cache of the views stays in force.

use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{TOOLTIP_DELAY, clear_shown_tooltips, shown_tooltips};
use workspace::{INSPECTION_TOOL, TERMINAL_TOOL, Workspace, view_render_count};

use crate::inspect::ActiveInspect;
use crate::terminal_panel::ActiveTerminal;
use crate::window_tests::{bounds_of, open_sample_window};
use crate::window_tests_cached_views::{frame, show_tool};
use crate::window_tests_run::wait_for;

/// Moves the pointer to the middle of an element and lets the frames and the tooltip
/// timers run for `wait`.
fn hover(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    wait: Duration,
) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    drop(visual);
    cx.executor().advance_clock(wait);
    cx.run_until_parked();
    frame(cx, window);
}

fn last_tooltip() -> Option<String> {
    shown_tooltips().last().map(|(text, _)| text.clone())
}

#[ui_kit::gpui::test]
fn a_tooltip_shown_just_now_makes_its_neighbour_show_at_once_in_a_cached_view(
    cx: &mut TestAppContext,
) {
    let (_, window) = open_sample_window(cx, "cached-tooltip-warm");

    clear_shown_tooltips();
    hover(
        cx,
        window,
        "t-run",
        TOOLTIP_DELAY + Duration::from_millis(100),
    );
    assert_eq!(last_tooltip().as_deref(), Some("Run"), "the first tooltip");

    // The title bar keeps the frame of the first draw: its neighbour would have waited
    // half a second if the delay came from that frame.
    clear_shown_tooltips();
    hover(cx, window, "t-debug", Duration::from_millis(50));
    assert_eq!(
        last_tooltip().as_deref(),
        Some("Debug"),
        "a tooltip shown just now makes its neighbour show at once"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_terminal_panel_shows_the_exit_line_after_the_program_ends(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cached-terminal-exit");
    show_tool(cx, window, TERMINAL_TOOL);
    let terminal = cx.read(|cx| cx.global::<ActiveTerminal>().0.clone());
    wait_for(cx, "the shell", |cx| {
        terminal.read_with(cx, |panel, _| panel.session().is_some())
    });
    let session = terminal
        .read_with(cx, |panel, _| panel.session().cloned())
        .expect("the terminal has a session");
    session.send(b"exit 3\r".to_vec()).expect("input");

    wait_for(cx, "the exit line of the shell", |cx| {
        frame(cx, window);
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("terminal-exit").is_some()
    });
}

#[ui_kit::gpui::test]
fn a_change_of_the_inspection_hub_redraws_the_panel_without_a_new_title(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cached-inspection-rerun");
    show_tool(cx, window, INSPECTION_TOOL);
    cx.run_until_parked();
    frame(cx, window);

    let hub = cx.read(|cx| cx.global::<ActiveInspect>().0.clone());
    let before = cx.read(|cx| view_render_count(cx, "inspection"));
    assert!(before > 0, "the inspection panel drew itself");

    hub.update(cx, |hub, cx| hub.rerun_changed_for_test(true, cx));
    cx.run_until_parked();
    frame(cx, window);

    assert!(
        cx.read(|cx| view_render_count(cx, "inspection")) > before,
        "the panel hears the hub without a title or a progress update"
    );
    assert!(hub.read_with(cx, |hub, _| hub.can_rerun()));
}
