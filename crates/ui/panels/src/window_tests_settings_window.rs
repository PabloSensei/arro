//! The Settings window as a real window: the title bar with its close button, the search
//! over the pages, and OK, Cancel and Apply. A change never reaches the window or the
//! settings file before Apply or OK, and Cancel throws the draft away.
//!
//! These tests describe the behaviour of T-0178 and fail until the window keeps a draft:
//! the controls of today apply every change at once, and the window has no title button,
//! no search field and no buttons of its own.

use std::fs;
use std::path::PathBuf;

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use workspace::{Settings, Workspace};

use crate::window_tests::{
    bounds_of, click, draw, open_sample_window, press, type_text, wait_for_signal,
};

/// The settings file the window writes.
fn settings_file(cx: &TestAppContext, window: WindowHandle<Workspace>) -> PathBuf {
    window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(workspace::SETTINGS_FILE)
        })
        .unwrap()
}

fn font_size(cx: &TestAppContext) -> f32 {
    cx.read(|cx| cx.global::<Settings>().font_size)
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Opens a project and Ctrl+Alt+S on it, and proves the Settings window is on screen.
fn open_settings(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    let file = settings_file(cx, window);
    (window, file)
}

/// Drags the pointer from `from` by `(dx, dy)` with the left button held.
fn drag_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    dx: f32,
    dy: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(from.x + px(dx * fraction), from.y + px(dy * fraction)),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(
        point(from.x + px(dx), from.y + px(dy)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[gpui::test]
fn cancel_keeps_the_settings_and_the_file(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-cancel");
    let before = fs::read_to_string(&file).ok();

    click(cx, window, "settings-font-up");
    draw(cx, window);
    click(cx, window, "settings-cancel");
    draw(cx, window);

    assert_eq!(
        font_size(cx),
        Settings::default().font_size,
        "Cancel throws the change away"
    );
    assert_eq!(
        fs::read_to_string(&file).ok(),
        before,
        "Cancel writes nothing to the settings file"
    );
    assert!(!has(cx, window, "settings"), "Cancel closes the window");
}

#[gpui::test]
fn apply_writes_the_settings_and_leaves_the_window_open(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-apply");

    click(cx, window, "settings-font-up");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);

    assert_eq!(font_size(cx), 14., "Apply reaches the window at once");
    assert!(has(cx, window, "settings"), "Apply leaves the window open");
    wait_for_signal(cx, "the applied settings file", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"font_size\": 14"))
    });
}

#[gpui::test]
fn ok_writes_the_settings_and_closes_the_window(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-ok");

    click(cx, window, "settings-font-up");
    draw(cx, window);
    click(cx, window, "settings-ok");
    draw(cx, window);

    assert_eq!(font_size(cx), 14., "OK keeps the change");
    assert!(!has(cx, window, "settings"), "OK closes the window");
    wait_for_signal(cx, "the settings file of OK", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"font_size\": 14"))
    });
}

#[gpui::test]
fn apply_does_nothing_while_nothing_changed(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-apply-idle");
    let text = "{\n  \"font_size\": 13.0\n}\n";
    fs::write(&file, text).unwrap();

    click(cx, window, "settings-apply");
    draw(cx, window);

    assert!(
        has(cx, window, "settings"),
        "a disabled Apply leaves the window open"
    );
    assert_eq!(font_size(cx), 13., "a disabled Apply changes no setting");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        text,
        "a disabled Apply writes no file"
    );
}

#[gpui::test]
fn the_close_button_and_escape_cancel(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-close");
    let before = fs::read_to_string(&file).ok();

    click(cx, window, "settings-font-up");
    draw(cx, window);
    click(cx, window, "settings-close");
    draw(cx, window);
    assert_eq!(
        font_size(cx),
        13.,
        "the close button throws the change away"
    );
    assert!(
        !has(cx, window, "settings"),
        "the close button closes the window"
    );

    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-font-up");
    draw(cx, window);
    press(cx, window, "escape");
    draw(cx, window);
    assert_eq!(font_size(cx), 13., "Escape throws the change away");
    assert!(!has(cx, window, "settings"), "Escape closes the window");
    assert_eq!(
        fs::read_to_string(&file).ok(),
        before,
        "neither way writes the settings file"
    );
}

#[gpui::test]
fn the_search_leaves_the_pages_with_font_settings(cx: &mut TestAppContext) {
    let (window, _file) = open_settings(cx, "settings-window-search");

    click(cx, window, "settings-search");
    type_text(cx, window, "font");
    draw(cx, window);

    assert!(
        has(cx, window, "settings-page-appearance"),
        "the page with the font settings stays"
    );
    assert!(
        !has(cx, window, "settings-page-project"),
        "a page without a matching setting leaves the tree"
    );
    assert!(
        has(cx, window, "settings-editor-font-family"),
        "the matched setting is on the page"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-page-project"),
        "Escape in the search field clears it"
    );
    assert!(
        has(cx, window, "settings"),
        "the first Escape does not close the window"
    );
}

#[gpui::test]
fn a_click_outside_keeps_the_window_and_the_draft(cx: &mut TestAppContext) {
    let (window, file) = open_settings(cx, "settings-window-outside");
    let before = Settings::default().font_size;

    click(cx, window, "settings-font-up");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(point(px(4.), px(4.)), Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        has(cx, window, "settings"),
        "a click outside does not close the window"
    );
    assert_eq!(
        font_size(cx),
        before,
        "a click outside does not apply the draft"
    );

    click(cx, window, "settings-apply");
    draw(cx, window);
    assert_eq!(
        font_size(cx),
        before + 1.,
        "the draft kept through the click outside reaches Apply"
    );
    wait_for_signal(cx, "the settings file after the click outside", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"font_size\": 14"))
    });
}

#[gpui::test]
fn dragging_the_corner_changes_the_size(cx: &mut TestAppContext) {
    let (window, _file) = open_settings(cx, "settings-window-resize");
    let before = bounds_of(cx, window, "settings");
    let corner = bounds_of(cx, window, "settings-resize-corner");

    drag_by(cx, window, corner.center(), 80., 50.);

    let after = bounds_of(cx, window, "settings");
    let wider = f32::from(after.size.width) - f32::from(before.size.width);
    let taller = f32::from(after.size.height) - f32::from(before.size.height);
    assert!(
        (wider - 80.).abs() <= 4.,
        "the right edge follows the pointer: {wider}"
    );
    assert!(
        (taller - 50.).abs() <= 4.,
        "the bottom edge follows the pointer: {taller}"
    );
}

#[gpui::test]
fn dragging_the_title_moves_the_window(cx: &mut TestAppContext) {
    let (window, _file) = open_settings(cx, "settings-window-drag");
    let before = bounds_of(cx, window, "settings");
    let title = bounds_of(cx, window, "settings-title");

    drag_by(cx, window, title.center(), 120., 60.);

    let after = bounds_of(cx, window, "settings");
    let moved_x = f32::from(after.left()) - f32::from(before.left());
    let moved_y = f32::from(after.top()) - f32::from(before.top());
    assert!(
        (moved_x - 120.).abs() <= 4.,
        "the window follows the pointer to the right: {moved_x}"
    );
    assert!(
        (moved_y - 60.).abs() <= 4.,
        "the window follows the pointer down: {moved_y}"
    );
}
