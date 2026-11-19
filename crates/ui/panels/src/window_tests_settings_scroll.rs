//! The content of a Settings page scrolls to its end above the row of OK, Cancel and Apply:
//! the last element of the page is drawn in full and the buttons are never drawn over it,
//! at every interface scale.

use ui_kit::gpui;
use ui_kit::gpui::{ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px, size};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, draw, open_sample_window, press};

/// Resizes the window and draws the frame at its new size.
fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32, height: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Sets the interface scale as the Appearance page does, and draws at it.
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

/// Turns the wheel down over the element with the debug selector, far past its end.
fn scroll_to_end(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: bounds.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
        modifiers: Modifiers::default(),
        ..ScrollEvent::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Opens the Settings window on `page` at `scale`, and scrolls the page to its end.
fn settings_at_end(
    cx: &mut TestAppContext,
    name: &str,
    page: &'static str,
    scale: f32,
) -> WindowHandle<Workspace> {
    let (_, window) = open_sample_window(cx, name);
    if scale != 1. {
        set_ui_scale(cx, window, scale);
    }
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, page);
    draw(cx, window);
    scroll_to_end(cx, window, "scroll-area-settings-content");
    window
}

/// The scrolled area of the content ends above the row of buttons, which is one row.
fn assert_area_above_buttons(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let area = bounds_of(cx, window, "scroll-area-settings-content");
    let buttons = bounds_of(cx, window, "settings-ok");
    assert!(
        area.bottom() <= buttons.top(),
        "the scrolled area ends at {:?}, under the buttons that start at {:?}",
        area.bottom(),
        buttons.top()
    );
}

/// The last section of the Appearance page, the icon sets, is drawn in full above the
/// buttons and inside the scrolled area.
fn assert_last_section_above_buttons(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    assert_area_above_buttons(cx, window);
    let area = bounds_of(cx, window, "scroll-area-settings-content");
    let buttons = bounds_of(cx, window, "settings-ok");
    for card in ["settings-icon-set-arro", "settings-icon-set-intellij"] {
        let card = bounds_of(cx, window, card);
        assert!(
            card.bottom() <= area.bottom(),
            "{card:?} ends below the scrolled area {area:?}"
        );
        assert!(
            card.bottom() <= buttons.top(),
            "{card:?} ends under the buttons that start at {:?}",
            buttons.top()
        );
    }
}

#[ui_kit::gpui::test]
fn the_appearance_page_scrolls_its_icon_sets_above_the_buttons(cx: &mut TestAppContext) {
    let window = settings_at_end(
        cx,
        "settings-scroll-appearance",
        "settings-page-appearance",
        1.,
    );
    assert_last_section_above_buttons(cx, window);
}

#[ui_kit::gpui::test]
fn the_appearance_page_scrolls_its_icon_sets_above_the_buttons_at_scale_1_5(
    cx: &mut TestAppContext,
) {
    let window = settings_at_end(cx, "settings-scroll-1-5", "settings-page-appearance", 1.5);
    assert_last_section_above_buttons(cx, window);
}

#[ui_kit::gpui::test]
fn the_appearance_page_scrolls_its_icon_sets_above_the_buttons_at_scale_2(cx: &mut TestAppContext) {
    let window = settings_at_end(cx, "settings-scroll-2", "settings-page-appearance", 2.);
    assert_last_section_above_buttons(cx, window);
}

#[ui_kit::gpui::test]
fn the_appearance_page_scrolls_its_icon_sets_above_the_buttons_in_a_short_window(
    cx: &mut TestAppContext,
) {
    let (_, window) = open_sample_window(cx, "settings-scroll-short-window");
    resize(cx, window, 1280., 620.);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-appearance");
    draw(cx, window);
    scroll_to_end(cx, window, "scroll-area-settings-content");
    assert_last_section_above_buttons(cx, window);
}

#[ui_kit::gpui::test]
fn the_inspections_page_scrolls_its_content_above_the_buttons(cx: &mut TestAppContext) {
    let window = settings_at_end(
        cx,
        "settings-scroll-inspections",
        "settings-page-inspections",
        1.,
    );
    assert_area_above_buttons(cx, window);
}
