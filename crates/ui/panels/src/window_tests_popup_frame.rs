//! The search popups as floating windows: a drag by the top row, a resize by a corner, the
//! minimum, the place and the size kept for the session, a click in the search field, a
//! click outside that closes the popup, a drag released outside that does not, and the
//! pinned Find in Files.

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::finder::{self, ActiveFinder, Finder};
use crate::palette;
use crate::window_tests::{
    bounds_of, draw, open_palette, open_path, open_sample_window, press, type_text,
};

/// Whether the element with `selector` is on the screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Drags from `from` by (`dx`, `dy`) with the left button held, the way a pointer moves.
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
            gpui::point(from.x + px(dx * fraction), from.y + px(dy * fraction)),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(
        gpui::point(from.x + px(dx), from.y + px(dy)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// Clicks a point of the window, as the pointer does.
fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    at: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(at, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

fn finder(cx: &TestAppContext) -> gpui::Entity<Finder> {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
            .expect("Find in Files is open")
    })
}

/// The offset of the left and top edges of an element from the same edges before a drag.
fn moved_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    before: gpui::Bounds<gpui::Pixels>,
) -> (f32, f32) {
    let after = bounds_of(cx, window, selector);
    (
        f32::from(after.left()) - f32::from(before.left()),
        f32::from(after.top()) - f32::from(before.top()),
    )
}

/// The offset of the width and height of an element from the size before a drag.
fn grown_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    before: gpui::Bounds<gpui::Pixels>,
) -> (f32, f32) {
    let after = bounds_of(cx, window, selector);
    (
        f32::from(after.size.width) - f32::from(before.size.width),
        f32::from(after.size.height) - f32::from(before.size.height),
    )
}

#[gpui::test]
fn a_drag_of_the_top_row_moves_find_in_files_and_search_everywhere(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-drag");

    open_palette(cx, window, keys::FIND_IN_FILES);
    assert!(on_screen(cx, window, "finder"), "Find in Files opens");
    let before = bounds_of(cx, window, "finder");
    let title = bounds_of(cx, window, "finder-title");
    drag_by(cx, window, title.center(), 60., 40.);
    let (moved_x, moved_y) = moved_by(cx, window, "finder", before);
    assert!(
        (moved_x - 60.).abs() <= 4.,
        "Find in Files follows the pointer to the right: {moved_x}"
    );
    assert!(
        (moved_y - 40.).abs() <= 4.,
        "Find in Files follows the pointer down: {moved_y}"
    );
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "finder"),
        "Escape closes Find in Files"
    );

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    assert!(on_screen(cx, window, "palette"), "Search Everywhere opens");
    let before = bounds_of(cx, window, "palette");
    let header = bounds_of(cx, window, "palette-header");
    drag_by(cx, window, header.center(), -60., 40.);
    let (moved_x, moved_y) = moved_by(cx, window, "palette", before);
    assert!(
        (moved_x + 60.).abs() <= 4.,
        "Search Everywhere follows the pointer to the left: {moved_x}"
    );
    assert!(
        (moved_y - 40.).abs() <= 4.,
        "Search Everywhere follows the pointer down: {moved_y}"
    );
}

#[gpui::test]
fn a_drag_of_a_corner_resizes_find_in_files_and_search_everywhere(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-resize");

    open_palette(cx, window, keys::FIND_IN_FILES);
    let before = bounds_of(cx, window, "finder");
    let corner = bounds_of(cx, window, "finder-resize-top-left");
    drag_by(cx, window, corner.center(), -40., -30.);
    let (wider, taller) = grown_by(cx, window, "finder", before);
    assert!(
        (wider - 40.).abs() <= 4.,
        "the right edge of Find in Files stays and the left one moves: {wider}"
    );
    assert!(
        (taller - 30.).abs() <= 4.,
        "the bottom edge of Find in Files stays and the top one moves: {taller}"
    );
    press(cx, window, "escape");
    draw(cx, window);

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    let before = bounds_of(cx, window, "palette");
    let corner = bounds_of(cx, window, "palette-resize-top-left");
    drag_by(cx, window, corner.center(), -40., -30.);
    let (wider, taller) = grown_by(cx, window, "palette", before);
    assert!(
        (wider - 40.).abs() <= 4.,
        "the right edge of Search Everywhere stays: {wider}"
    );
    assert!(
        (taller - 30.).abs() <= 4.,
        "the bottom edge of Search Everywhere stays: {taller}"
    );
}

#[gpui::test]
fn a_resize_never_goes_below_the_minimum(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-min");

    open_palette(cx, window, keys::FIND_IN_FILES);
    let corner = bounds_of(cx, window, "finder-resize-corner");
    drag_by(cx, window, corner.center(), -5000., -5000.);
    let small = bounds_of(cx, window, "finder");
    assert!(
        (f32::from(small.size.width) - finder::MIN_WIDTH).abs() <= 4.,
        "Find in Files stops at its smallest width: {:?}",
        small.size.width
    );
    assert!(
        (f32::from(small.size.height) - finder::MIN_HEIGHT).abs() <= 4.,
        "Find in Files stops at its smallest height: {:?}",
        small.size.height
    );
    press(cx, window, "escape");
    draw(cx, window);

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    let corner = bounds_of(cx, window, "palette-resize-corner");
    drag_by(cx, window, corner.center(), -5000., -5000.);
    let small = bounds_of(cx, window, "palette");
    assert!(
        (f32::from(small.size.width) - palette::MIN_WIDTH).abs() <= 4.,
        "Search Everywhere stops at its smallest width: {:?}",
        small.size.width
    );
    assert!(
        (f32::from(small.size.height) - palette::MIN_HEIGHT).abs() <= 4.,
        "Search Everywhere stops at its smallest height: {:?}",
        small.size.height
    );
}

#[gpui::test]
fn a_search_window_opens_again_at_the_place_and_size_it_had(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-remember");

    open_palette(cx, window, keys::FIND_IN_FILES);
    let title = bounds_of(cx, window, "finder-title");
    drag_by(cx, window, title.center(), 50., 30.);
    let corner = bounds_of(cx, window, "finder-resize-top-left");
    drag_by(cx, window, corner.center(), -30., -20.);
    let kept = bounds_of(cx, window, "finder");
    press(cx, window, "escape");
    draw(cx, window);

    open_palette(cx, window, keys::FIND_IN_FILES);
    let again = bounds_of(cx, window, "finder");
    assert!(
        (f32::from(again.left()) - f32::from(kept.left())).abs() <= 1.,
        "Find in Files opens where it was"
    );
    assert!(
        (f32::from(again.top()) - f32::from(kept.top())).abs() <= 1.,
        "Find in Files opens at the same height"
    );
    assert_eq!(
        again.size, kept.size,
        "Find in Files opens at the size it had"
    );
    press(cx, window, "escape");
    draw(cx, window);

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    let header = bounds_of(cx, window, "palette-header");
    drag_by(cx, window, header.center(), 50., 30.);
    let corner = bounds_of(cx, window, "palette-resize-top-left");
    drag_by(cx, window, corner.center(), -30., -20.);
    let kept = bounds_of(cx, window, "palette");
    press(cx, window, "escape");
    draw(cx, window);

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    let again = bounds_of(cx, window, "palette");
    assert!(
        (f32::from(again.left()) - f32::from(kept.left())).abs() <= 1.,
        "Search Everywhere opens where it was"
    );
    assert!(
        (f32::from(again.top()) - f32::from(kept.top())).abs() <= 1.,
        "Search Everywhere opens at the same height"
    );
    assert_eq!(
        again.size, kept.size,
        "Search Everywhere opens at the size it had"
    );
}

#[gpui::test]
fn a_click_in_the_search_field_does_not_start_a_drag(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-field");

    open_palette(cx, window, keys::FIND_IN_FILES);
    let before = bounds_of(cx, window, "finder");
    let field = bounds_of(cx, window, "finder-input-find");
    click_at(cx, window, field.center());

    let after = bounds_of(cx, window, "finder");
    assert_eq!(
        after.origin, before.origin,
        "a press on the field does not move the window"
    );
    type_text(cx, window, "alpha");
    draw(cx, window);
    let text = finder(cx).read_with(cx, |finder, _| finder.find_field().text.clone());
    assert_eq!(text, "alpha", "the press puts the caret into the field");
}

#[gpui::test]
fn a_click_outside_closes_the_search_windows(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "popup-frame-outside");
    open_path(cx, window, &root.join("src").join("main.txt"));

    open_palette(cx, window, keys::FIND_IN_FILES);
    let popup = bounds_of(cx, window, "finder");
    let editor = bounds_of(cx, window, "editor-body-0");
    let outside = gpui::point(editor.left() + px(6.), editor.top() + px(6.));
    assert!(
        !popup.contains(&outside),
        "the test point is outside Find in Files"
    );
    click_at(cx, window, outside);
    assert!(
        !on_screen(cx, window, "finder"),
        "a click outside closes Find in Files"
    );

    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    let popup = bounds_of(cx, window, "palette");
    let editor = bounds_of(cx, window, "editor-body-0");
    let outside = gpui::point(editor.left() + px(6.), editor.top() + px(6.));
    assert!(
        !popup.contains(&outside),
        "the test point is outside Search Everywhere"
    );
    click_at(cx, window, outside);
    assert!(
        !on_screen(cx, window, "palette"),
        "a click outside closes Search Everywhere"
    );
}

#[gpui::test]
fn a_drag_released_outside_does_not_close_the_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "popup-frame-release-outside");

    open_palette(cx, window, keys::FIND_IN_FILES);
    let title = bounds_of(cx, window, "finder-title");
    drag_by(cx, window, title.center(), -4000., 0.);
    assert!(
        on_screen(cx, window, "finder"),
        "a drag of the title bar released outside keeps Find in Files open"
    );

    let corner = bounds_of(cx, window, "finder-resize-corner");
    drag_by(cx, window, corner.center(), 4000., 4000.);
    assert!(
        on_screen(cx, window, "finder"),
        "a resize released outside keeps Find in Files open"
    );
}

#[gpui::test]
fn a_pinned_find_in_files_stays_open_on_a_click_outside(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "popup-frame-pin");
    open_path(cx, window, &root.join("src").join("main.txt"));

    open_palette(cx, window, keys::FIND_IN_FILES);
    let pin = bounds_of(cx, window, "finder-pin");
    click_at(cx, window, pin.center());
    assert!(
        finder(cx).read_with(cx, |finder, _| finder.is_pinned()),
        "the pin is on"
    );

    let popup = bounds_of(cx, window, "finder");
    let editor = bounds_of(cx, window, "editor-body-0");
    let outside = gpui::point(editor.left() + px(6.), editor.top() + px(6.));
    assert!(
        !popup.contains(&outside),
        "the test point is outside Find in Files"
    );
    click_at(cx, window, outside);
    assert!(
        on_screen(cx, window, "finder"),
        "a pinned Find in Files stays open"
    );
}
