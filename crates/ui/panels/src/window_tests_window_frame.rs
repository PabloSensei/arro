//! The frame of the window: a double click on the empty caption maximizes the window and
//! gives the old size back, and the areas of the buttons and of the caption follow a
//! resize, so the three window buttons still answer at the place they drew.
//!
//! The test platform changes no window: the window asks for the maximized state and the
//! test counts the requests, while the areas of the caption are the ones the platform of
//! Windows reads in the frame it drew last.

#![cfg(windows)]

use std::cell::Cell;
use std::rc::Rc;

use ui_kit::gpui;
use ui_kit::gpui::{
    App, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, TestAppContext,
    VisualTestContext, WindowHandle, px, size,
};
use workspace::{Workspace, view_render_count, window_zoom_requests};

use crate::window_tests::{bounds_of, click, open_path, open_sample_window};
use crate::window_tests_cached_views::frame;
use crate::window_tests_close_cross::{assert_caption_areas_match, caption_areas, draw_and_settle};

/// A double click of the pointer: two presses and releases at the same place, the second
/// one counted as the second click of the pair, as the platform counts it.
fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, position: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for click_count in [1, 2] {
        visual.simulate_event(MouseDownEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            position,
            modifiers: Modifiers::default(),
            button: MouseButton::Left,
            click_count,
        });
    }
    drop(visual);
    cx.run_until_parked();
}

/// How many times the window asked for the maximized state or for its old size back.
fn zoom_requests(cx: &TestAppContext) -> usize {
    cx.read(window_zoom_requests)
}

/// The empty caption maximizes the window, and the next double click gives back the size
/// it had before.
#[ui_kit::gpui::test]
fn a_double_click_on_the_caption_toggles_the_zoom(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "frame-zoom");
    draw_and_settle(cx, window);
    assert_eq!(zoom_requests(cx), 0, "the window starts as it was opened");

    let caption = bounds_of(cx, window, "w-drag-space");
    double_click(cx, window, caption.center());
    assert_eq!(
        zoom_requests(cx),
        1,
        "the double click on the empty caption asks for the maximized window"
    );

    // The platform changes no window of the test, so the caption stays where it drew. The
    // frame of a keystroke keeps the cached title bar, and its press is still there.
    let title = cx.read(|cx| view_render_count(cx, "title"));
    frame(cx, window);
    assert_eq!(
        cx.read(|cx| view_render_count(cx, "title")),
        title,
        "the title bar of the frame is the cached one"
    );
    let caption = bounds_of(cx, window, "w-drag-space");
    double_click(cx, window, caption.center());
    assert_eq!(
        zoom_requests(cx),
        2,
        "the second double click asks for the size the window had before"
    );
}

/// A double click on a control of the caption is the control's own presses: the empty
/// caption is the only part of the title bar that toggles the window.
#[ui_kit::gpui::test]
fn a_double_click_on_a_button_of_the_caption_toggles_nothing(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "frame-button");
    draw_and_settle(cx, window);

    let search = bounds_of(cx, window, "t-search");
    double_click(cx, window, search.center());

    assert_eq!(
        zoom_requests(cx),
        0,
        "a double click on a button of the caption is not a double click on the caption"
    );
}

/// A resize moves the buttons, and the areas the platform reads are the ones of the new
/// frame: the cross keeps working and the empty caption at the new place still toggles
/// the window.
#[ui_kit::gpui::test]
fn a_resize_moves_the_areas_of_the_caption_and_keeps_the_buttons(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "frame-resize");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    // A tab that may not close keeps the window open for the check of the cross.
    let asked = Rc::new(Cell::new(0));
    let asked_by_the_tab = asked.clone();
    window
        .update(cx, |workspace, _, _| {
            workspace.set_tab_can_close(
                &file,
                Rc::new(move |_: &mut App| {
                    asked_by_the_tab.set(asked_by_the_tab.get() + 1);
                    false
                }),
            );
        })
        .unwrap();
    draw_and_settle(cx, window);
    assert_caption_areas_match(cx, window, "before the resize");
    let before = caption_areas(cx, window);
    assert!(before.drag.is_some(), "the caption has its drag area");

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let opened = visual.update(|window, _| window.bounds().size);
    visual.simulate_resize(size(opened.width - px(120.), opened.height - px(80.)));
    drop(visual);
    cx.run_until_parked();
    // The frame after a resize honors the cache of the title bar, as the window of a user
    // draws it: the bar redraws for the new size of the window.
    frame(cx, window);
    draw_and_settle(cx, window);

    assert_caption_areas_match(cx, window, "after the resize");
    let after = caption_areas(cx, window);
    assert_eq!(
        after.drag,
        Some(bounds_of(cx, window, "w-drag-space")),
        "the drag area of the caption followed the window"
    );
    assert_ne!(
        after.close, before.close,
        "the buttons of the caption moved with the resize"
    );
    assert_ne!(
        after.drag, before.drag,
        "the drag area of the caption took the width the window gave it"
    );

    // The empty caption at the new place still toggles the zoom.
    let caption = caption_areas(cx, window)
        .drag
        .expect("the caption keeps its drag area");
    double_click(cx, window, caption.center());
    assert_eq!(
        zoom_requests(cx),
        1,
        "the double click on the caption of the resized window asks for the maximized window"
    );

    // The maximize button of the resized window takes its own press at its new place.
    let zoom_before = zoom_requests(cx);
    click(cx, window, "w-max");
    assert_eq!(
        zoom_requests(cx),
        zoom_before + 1,
        "the maximize button of the resized window asks for the maximized window"
    );

    // The cross of the resized window reaches the tab at the place it drew.
    let asked_before = asked.get();
    click(cx, window, "w-close");
    cx.run_until_parked();
    assert_eq!(
        asked.get(),
        asked_before + 1,
        "the cross after a resize asks the tab of the window"
    );
    assert!(!cx.windows().is_empty(), "the window of the tab stays");
}
