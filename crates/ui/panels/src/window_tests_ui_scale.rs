//! The scale of the interface: every logical pixel of the window grows with it, so the
//! text, the icons, the rows and the tabs grow together and the clicks keep landing.

use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, open_path, open_sample_window};

/// How far a size measured at two scales may be from the ratio of them: the rows round
/// their heights to whole logical pixels.
const TOLERANCE: f32 = 0.06;

fn set_ui_scale(cx: &mut TestAppContext, window: WindowHandle<Workspace>, scale: f32) {
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| *settings = settings.with_ui_scale(scale), cx)
        })
        .unwrap();
    cx.run_until_parked();
    // The frame of the test platform applies the scale, and the next one draws at it.
    draw(cx, window);
    draw(cx, window);
}

fn window_scale(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> f32 {
    window
        .update(cx, |_, window, _| window.scale_factor())
        .unwrap()
}

/// The height of an element on screen: the logical pixels of the frame times the scale
/// factor the platform draws them at.
fn height_on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> f32 {
    let height = f32::from(bounds_of(cx, window, selector).size.height);
    height * window_scale(cx, window)
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn the_scale_grows_the_rows_the_icons_and_the_tabs_alike(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "ui-scale-grows");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    draw(cx, window);

    let before_scale = window_scale(cx, window);
    let row_before = height_on_screen(cx, window, "tree-row-src");
    let icon_before = height_on_screen(cx, window, "stripe-s-project");
    let tab_before = height_on_screen(cx, window, "tab-0-0");

    set_ui_scale(cx, window, 1.5);

    let after_scale = window_scale(cx, window);
    assert!(
        (after_scale / before_scale - 1.5).abs() < 0.001,
        "the window scale grows by the interface scale: {before_scale} to {after_scale}"
    );
    for (name, before, after) in [
        (
            "the row of the project tree",
            row_before,
            height_on_screen(cx, window, "tree-row-src"),
        ),
        (
            "the icon of the panel",
            icon_before,
            height_on_screen(cx, window, "stripe-s-project"),
        ),
        (
            "the tab",
            tab_before,
            height_on_screen(cx, window, "tab-0-0"),
        ),
    ] {
        let ratio = after / before;
        assert!(
            (ratio - 1.5).abs() < TOLERANCE,
            "{name} grows about one and a half times: {before} to {after}"
        );
    }
}

#[ui_kit::gpui::test]
fn a_tree_row_answers_a_click_at_the_grown_scale(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "ui-scale-click");
    draw(cx, window);
    set_ui_scale(cx, window, 1.5);

    click(cx, window, "tree-row-src");
    draw(cx, window);
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let opened = visual.debug_bounds("tree-row-main.txt").is_some();
    assert!(
        opened,
        "the click opens the folder though the interface is at 150%: {:?}",
        root.join("src")
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_window_buttons_answer_a_click_at_the_grown_scale(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "ui-scale-buttons");
    draw(cx, window);
    set_ui_scale(cx, window, 1.5);
    crate::window_tests_close_cross::assert_caption_areas_match(cx, window, "at 150%");

    let before = cx.read(workspace::window_zoom_requests);
    click(cx, window, "w-max");
    let after = cx.read(workspace::window_zoom_requests);
    assert!(
        after > before,
        "the button of the window takes the click at 150%"
    );
}
