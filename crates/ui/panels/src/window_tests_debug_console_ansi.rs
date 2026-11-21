//! The console of the Debug tool window shows the output of a program the way the console
//! of Run does: the SGR colours of a line become colours and never text, and the search
//! reads the line without the codes.
//!
//! `cargo test -p panels window_tests_debug_console_ansi`

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{DEBUG_TOOL, Workspace};

use crate::debug::DebugPanel;
use crate::window_tests::{click, draw, open_sample_window, press, type_text};
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

/// Puts one line into the console the way a Spring Boot program writes it.
fn push_info(cx: &mut TestAppContext, debug: &gpui::Entity<DebugPanel>) {
    debug.update(cx, |panel, cx| {
        panel.push_output(
            "\u{1b}[2m2026-11-21T16:01:16.134\u{1b}[0;39m \u{1b}[32m INFO\u{1b}[0;39m 13708 --- [main] started",
            false,
            cx,
        );
    });
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_debug_console_line_shows_ansi_text_without_codes_and_in_its_color(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-ansi");
    show_console(cx, window);
    let debug = debug_panel(cx);
    push_info(cx, &debug);
    draw(cx, window);

    assert_eq!(
        debug.read_with(cx, |panel, _| panel.console_lines()),
        ["2026-11-21T16:01:16.134  INFO 13708 --- [main] started"],
        "the screen carries the text with no escape sequence"
    );
    let parts = debug.read_with(cx, |panel, _| panel.console_line_parts(0));
    assert!(
        parts
            .iter()
            .any(|(text, color)| text == " INFO" && *color == Some(2)),
        "the green of the level stays: {parts:?}"
    );
    assert!(
        parts.iter().all(|(text, _)| !text.contains('\u{1b}')),
        "no escape sequence reaches the screen: {parts:?}"
    );
}

#[ui_kit::gpui::test]
fn the_search_of_the_debug_console_finds_text_behind_ansi_codes(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-ansi-search");
    show_console(cx, window);
    let debug = debug_panel(cx);
    push_info(cx, &debug);
    draw(cx, window);

    press(cx, window, "ctrl-f");
    draw(cx, window);
    type_text(cx, window, "INFO");

    assert_eq!(
        debug.read_with(cx, |panel, _| panel.console_search_status()),
        "1/1",
        "the search finds the text of the line"
    );
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.visible_console_lines()),
        ["2026-11-21T16:01:16.134  INFO 13708 --- [main] started"],
        "the filter and the list see the text without the codes"
    );
}
