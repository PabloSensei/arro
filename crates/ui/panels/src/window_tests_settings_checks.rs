//! The look of the controls of the Settings pages that the window can show: the box of an empty
//! checkbox is on the screen and a click ticks it, an empty text field stands with its default
//! as a hint, and the window opens at its share of a large window. The colours of the borders
//! are not seen here: `ui-kit` checks their contrast on every theme.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::DIALOG_PADDING;
use ui_kit::{px, size};
use workspace::Workspace;

use crate::settings::{first_size, text_hint};
use crate::settings_catalog::{SettingField, SettingsPage, set_build_tool_names};
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press};

/// Whether the element with `selector` is on the screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Sets the size of the window and draws it at the new size.
fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32, height: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Opens Settings on the page of the first build tool. The page takes its name from the build
/// tool, as the application names it, so the test asks for the name the same way.
fn open_build_tool_page(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (first, second) = window_language::build_tool_names();
    set_build_tool_names(first, second);
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    let title = SettingsPage::BuildToolsFirst
        .title()
        .to_lowercase()
        .replace(' ', "-");
    let page: &'static str = Box::leak(format!("settings-page-{title}").into_boxed_str());
    click(cx, window, page);
    draw(cx, window);
    window
}

#[gpui::test]
fn window_tests_settings_checks_empty_box_is_drawn_and_a_click_ticks_it(cx: &mut TestAppContext) {
    let window = open_build_tool_page(cx, "settings-checks-box");
    let square = "settings-build-tool-first-offline-box";
    let mark = "settings-build-tool-first-offline-mark";
    let bounds = bounds_of(cx, window, square);
    assert_eq!(
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
        "the box of a checkbox is a square"
    );
    assert!(!on_screen(cx, window, mark), "an empty box draws no mark");
    click(cx, window, "settings-build-tool-first-offline");
    draw(cx, window);
    assert!(
        on_screen(cx, window, mark),
        "a click on the row ticks the box"
    );
}

#[gpui::test]
fn window_tests_settings_checks_empty_field_shows_its_default_as_a_hint(cx: &mut TestAppContext) {
    let window = open_build_tool_page(cx, "settings-checks-hint");
    assert!(
        on_screen(cx, window, "settings-build-tool-first-home"),
        "the field of the home is on the screen"
    );
    assert!(
        on_screen(cx, window, "settings-build-tool-first-home-hint"),
        "an empty field shows its default inside it"
    );
    assert_eq!(
        text_hint(SettingField::BuildToolFirstHome),
        "Wrapper or bundled"
    );
}

#[gpui::test]
fn window_tests_settings_checks_opens_at_its_share_of_a_large_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "settings-checks-size");
    resize(cx, window, 1600., 1000.);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    let width = f32::from(bounds_of(cx, window, "settings").size.width);
    assert!(
        (900. ..=1400.).contains(&width),
        "the Settings window is {width} px wide in a 1600 px window"
    );
}

#[test]
fn window_tests_settings_checks_first_size_is_a_share_kept_between_the_bounds() {
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    let (width, height) = first_size(1600., 1000.);
    assert!(
        close(width, 1120.) && close(height, 750.),
        "{width} x {height}"
    );
    let (width, height) = first_size(2560., 1440.);
    assert!(
        close(width, 1400.) && close(height, 1000.),
        "{width} x {height}"
    );
    let (width, _) = first_size(1000., 700.);
    assert!(
        close(width, 900.),
        "the least width of the first opening: {width}"
    );
    let (width, height) = first_size(700., 500.);
    assert!(
        width <= 700. - 2. * DIALOG_PADDING && height <= 500. - 2. * DIALOG_PADDING,
        "a window smaller than the minimum shows the dialog inside it: {width} x {height}"
    );
}
