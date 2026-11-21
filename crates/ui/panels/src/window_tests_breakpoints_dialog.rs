//! The Breakpoints window without a debug session: the tree of the project points with
//! their flags, the properties of the selected point and the line the double click opens.
//!
//! `cargo test -p panels window_tests_breakpoints_dialog`
//!
//! The window reads the points of the project, which a click in the gutter of the editor
//! puts there, and sends every change to the adapter once a session starts.

use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use workspace::Workspace;

use crate::window_tests::{
    active_path, bounds_of, click, click_gutter, draw, editor_of, open_path, open_sample_window,
    press, type_text,
};
use crate::window_tests_debug_actions::{debug_panel, shown};

fn open_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-shift-f8");
    draw(cx, window);
}

fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_line_point_shows_without_a_session_and_its_flag_greys_the_gutter(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "breakpoints-dialog-line");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    click_gutter(cx, window, 0, 1);
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.breakpoints().to_vec()),
        vec![1],
        "the gutter click put the point on line 2"
    );

    open_dialog(cx, window);
    assert!(
        shown(cx, window, "breakpoints-dialog"),
        "the window is open"
    );
    assert!(
        shown(cx, window, "breakpoints-line-row-0"),
        "the point of the project is in the tree without a session"
    );
    assert!(
        shown(cx, window, "breakpoints-exception-any"),
        "Any exception is listed without a session"
    );

    let debug = debug_panel(cx);
    click(cx, window, "breakpoints-line-enabled-0");
    draw(cx, window);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_enabled(&path, 2)),
        Some(false),
        "the flag of the row turned the point off"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.disabled_breakpoints().to_vec()),
        vec![1],
        "the gutter paints the disabled point grey"
    );
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![2],
        "the point stays in the project"
    );

    // A point the editor adds while the window is open reaches the tree at once.
    debug.update(cx, |panel, cx| panel.toggle_breakpoint(path.clone(), 3, cx));
    draw(cx, window);
    assert!(
        shown(cx, window, "breakpoints-line-row-1"),
        "a point put in the editor is in the open window"
    );
    debug.update(cx, |panel, cx| panel.toggle_breakpoint(path.clone(), 3, cx));
    draw(cx, window);
    assert!(
        !shown(cx, window, "breakpoints-line-row-1"),
        "a point taken away in the editor leaves the open window"
    );
}

#[ui_kit::gpui::test]
fn the_condition_of_a_point_survives_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "breakpoints-dialog-condition");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    click_gutter(cx, window, 0, 1);
    cx.run_until_parked();
    draw(cx, window);
    let debug = debug_panel(cx);

    open_dialog(cx, window);
    click(cx, window, "breakpoint-dialog-condition");
    draw(cx, window);
    type_text(cx, window, "x>1");
    press(cx, window, "enter");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.condition(&path, 2).map(str::to_owned)),
        Some("x>1".to_owned()),
        "the field writes the condition of the point"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !shown(cx, window, "breakpoints-dialog"),
        "Escape closes the window"
    );
    open_dialog(cx, window);
    assert!(
        shown(cx, window, "breakpoint-condition-value-x>1"),
        "the condition is in the field after the window is opened again"
    );
}

#[ui_kit::gpui::test]
fn any_exception_is_always_listed_and_off_by_default(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "breakpoints-dialog-exception");
    let debug = debug_panel(cx);
    open_dialog(cx, window);
    assert!(
        shown(cx, window, "breakpoints-exception-any"),
        "Any exception is in the tree with no point of the project"
    );
    assert!(
        !debug.read_with(cx, |panel, _| panel.any_exception_enabled()),
        "Any exception is off by default"
    );
    click(cx, window, "breakpoints-exception-any-enabled");
    draw(cx, window);
    assert!(
        debug.read_with(cx, |panel, _| panel.any_exception_enabled()),
        "the flag of the row turns the point on"
    );
    assert!(
        shown(cx, window, "breakpoint-dialog-caught"),
        "the properties of the exception point are on the right"
    );
    assert!(
        shown(cx, window, "breakpoint-dialog-uncaught"),
        "Caught and Uncaught describe the exception point"
    );
}

#[ui_kit::gpui::test]
fn the_remove_button_takes_the_selected_point_away(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "breakpoints-dialog-remove");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    click_gutter(cx, window, 0, 1);
    cx.run_until_parked();
    draw(cx, window);
    let debug = debug_panel(cx);

    open_dialog(cx, window);
    click(cx, window, "breakpoints-remove");
    draw(cx, window);
    assert!(
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .is_empty(),
        "the minus button removed the selected point"
    );
    assert!(
        view.read_with(cx, |view, _| view.breakpoints().is_empty()),
        "the gutter lost the point too"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_opens_the_line_of_the_point(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "breakpoints-dialog-open");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    click_gutter(cx, window, 0, 1);
    cx.run_until_parked();
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);

    open_dialog(cx, window);
    double_click(cx, window, "breakpoints-line-row-0");
    assert!(
        !shown(cx, window, "breakpoints-dialog"),
        "the window closes on the double click"
    );
    assert_eq!(active_path(cx, window), Some(path.clone()));
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(1),
        "the caret stands on the line of the point"
    );
}
