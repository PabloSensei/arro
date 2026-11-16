//! The cross of the window: a press on it closes the window, asks a tab that may not
//! close, and writes what the window holds, as File > Exit does.
//!
//! The title bar is a cached view: it draws on its own changes only, and a frame that
//! only the editor redrew keeps its last one. The test draws such a frame before the
//! press, because the cross has to close the window on it too.

use std::cell::Cell;
use std::fs;
use std::rc::Rc;

use ui_kit::gpui;
#[cfg(windows)]
use ui_kit::gpui::{Bounds, Pixels, px, size};
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
#[cfg(windows)]
use workspace::{
    TOGGLE_DISTRACTION_FREE, TOGGLE_FULL_SCREEN, TOGGLE_PRESENTATION_MODE, TOGGLE_ZEN_MODE,
};
use workspace::{Workspace, view_render_count};

#[cfg(windows)]
use crate::window_tests::press;
use crate::window_tests::{bounds_of, click, draw, open_path, open_sample_window, type_text};
use crate::window_tests_cached_views::frame;

/// Draws the frame of a keystroke in the editor: the title bar keeps the one it drew last,
/// and the window controls of that frame are the ones the platform has.
fn a_frame_the_editor_drew_alone(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let title = cx.read(|cx| view_render_count(cx, "title"));
    frame(cx, window);
    assert_eq!(
        cx.read(|cx| view_render_count(cx, "title")),
        title,
        "the title bar of the frame is the cached one"
    );
}

/// Puts the caret into the editor and types there, so the window holds an edit the close
/// has to write.
fn type_in_the_editor(cx: &mut TestAppContext, window: WindowHandle<Workspace>, text: &str) {
    let body = bounds_of(cx, window, "editor-body-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(body.center(), Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    type_text(cx, window, text);
}

#[ui_kit::gpui::test]
fn the_cross_closes_the_window_and_writes_its_unsaved_text(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "cross-close");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.autosave = false, cx)
        })
        .unwrap();
    let typed = "saved by the cross\n";
    type_in_the_editor(cx, window, typed);
    assert!(
        !fs::read_to_string(&file)
            .unwrap()
            .contains("saved by the cross"),
        "the edit is in the window only"
    );

    a_frame_the_editor_drew_alone(cx, window);
    click(cx, window, "w-close");
    cx.run_until_parked();

    assert!(
        cx.windows().is_empty(),
        "the cross closes the window of the test"
    );
    assert!(
        fs::read_to_string(&file)
            .unwrap()
            .contains("saved by the cross"),
        "the close writes the text of the window, as File > Exit does"
    );
}

#[ui_kit::gpui::test]
fn the_cross_asks_a_tab_that_may_not_close(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "cross-ask");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    let asked = Rc::new(Cell::new(0));
    let allow = Rc::new(Cell::new(false));
    let asked_by_the_tab = asked.clone();
    let allow_by_the_tab = allow.clone();
    window
        .update(cx, |workspace, _, _| {
            workspace.set_tab_can_close(
                &file,
                Rc::new(move |_: &mut gpui::App| {
                    asked_by_the_tab.set(asked_by_the_tab.get() + 1);
                    allow_by_the_tab.get()
                }),
            );
        })
        .unwrap();

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        !visual.simulate_close(),
        "the window waits for the answer of the tab"
    );
    drop(visual);
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "the close asked the tab of the window");
    assert!(!cx.windows().is_empty(), "the window stays");

    allow.set(true);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.simulate_close(),
        "the tab agrees and the window closes"
    );
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(cx.windows().is_empty(), "the window is gone");
}

#[ui_kit::gpui::test]
fn the_cross_closes_a_window_with_nothing_unsaved(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cross-plain");

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.simulate_close(),
        "nothing asks and the window closes"
    );
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(cx.windows().is_empty(), "the window is gone");
}

/// The areas the window root gave the platform in the frame the window drew last: what the
/// title bar left there when it drew.
#[cfg(windows)]
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CaptionAreas {
    pub(crate) min: Option<Bounds<Pixels>>,
    pub(crate) max: Option<Bounds<Pixels>>,
    pub(crate) close: Option<Bounds<Pixels>>,
    pub(crate) drag: Option<Bounds<Pixels>>,
}

#[cfg(windows)]
pub(crate) fn caption_areas(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> CaptionAreas {
    let shown = window
        .update(cx, |workspace, _, _| workspace.caption_areas())
        .unwrap();
    let mut areas = CaptionAreas::default();
    for (name, bounds) in shown {
        match name {
            "w-area-min" => areas.min = Some(bounds),
            "w-area-max" => areas.max = Some(bounds),
            "w-area-close" => areas.close = Some(bounds),
            "w-area-drag" => areas.drag = Some(bounds),
            other => panic!("the window root gave the platform an area it does not know: {other}"),
        }
    }
    areas
}

/// Draws the frame of a change and the frame after it, so that a check reads a settled
/// window: the modes of the window and the size of it settle on the first frame, and the
/// second one is what the pointer of a user would find.
#[cfg(windows)]
pub(crate) fn draw_and_settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    draw(cx, window);
    draw(cx, window);
}

#[cfg(windows)]
pub(crate) fn assert_caption_areas_match(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    what: &str,
) {
    let buttons = [
        bounds_of(cx, window, "w-min"),
        bounds_of(cx, window, "w-max"),
        bounds_of(cx, window, "w-close"),
    ];
    let areas = caption_areas(cx, window);
    for (area, button, name) in [
        (areas.min, buttons[0], "minimize"),
        (areas.max, buttons[1], "maximize"),
        (areas.close, buttons[2], "close"),
    ] {
        assert_eq!(
            area,
            Some(button),
            "{what}: the caption area of {name} has to be the button"
        );
    }
}

/// The caption areas are a part of every frame, not a part of the frames the title bar
/// happens to draw: the root puts them back from the bounds of the last drawn frame.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_caption_areas_are_there_on_every_frame(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cross-areas-frame");
    draw_and_settle(cx, window);
    assert_caption_areas_match(cx, window, "the first frame");
    let before = caption_areas(cx, window);
    assert!(before.drag.is_some(), "the caption has its drag area");

    a_frame_the_editor_drew_alone(cx, window);
    assert_eq!(
        caption_areas(cx, window),
        before,
        "a frame that only the editor drew keeps the areas of the caption"
    );
    assert_caption_areas_match(cx, window, "the frame of the editor alone");
}

/// The modes of View | Appearance take the tools and the bars away, not the caption: its
/// buttons stay on screen, and the areas of the platform stay on the buttons, so the cross
/// of a window in Distraction Free, Presentation, Full Screen or Zen still closes it.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_caption_areas_stay_with_the_buttons_of_the_modes(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "cross-areas-modes");
    let file = root.join("src").join("main.txt");
    open_path(cx, window, &file);
    let asked = Rc::new(Cell::new(0));
    let asked_by_the_tab = asked.clone();
    window
        .update(cx, |workspace, _, _| {
            workspace.set_tab_can_close(
                &file,
                Rc::new(move |_: &mut gpui::App| {
                    asked_by_the_tab.set(asked_by_the_tab.get() + 1);
                    false
                }),
            );
        })
        .unwrap();
    draw_and_settle(cx, window);

    for (id, name) in [
        (TOGGLE_DISTRACTION_FREE, "Distraction Free"),
        (TOGGLE_PRESENTATION_MODE, "Presentation"),
        (TOGGLE_FULL_SCREEN, "Full Screen"),
        (TOGGLE_ZEN_MODE, "Zen"),
    ] {
        assert!(
            window
                .update(cx, |workspace, window, cx| workspace
                    .run_command(id, window, cx))
                .unwrap(),
            "{name} runs"
        );
        cx.run_until_parked();
        draw_and_settle(cx, window);
        assert_caption_areas_match(cx, window, name);
        assert!(
            caption_areas(cx, window).drag.is_some(),
            "{name} keeps the drag area of the caption"
        );

        // The cross reaches the window as in the plain mode: the tab that may not close
        // keeps it open and is asked.
        let before = asked.get();
        click(cx, window, "w-close");
        cx.run_until_parked();
        assert_eq!(asked.get(), before + 1, "the cross of {name} asks the tab");
        assert!(!cx.windows().is_empty(), "the window of {name} stays");

        assert!(
            window
                .update(cx, |workspace, window, cx| workspace
                    .run_command(id, window, cx))
                .unwrap(),
            "the way out of {name} runs"
        );
        cx.run_until_parked();
        draw_and_settle(cx, window);
        assert_caption_areas_match(cx, window, "the caption after the mode");
    }
}

/// A window without a caption of its own has no areas for the platform: the bounds of a
/// caption that is not drawn are forgotten, and nothing stands where a button is not drawn.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_window_without_the_caption_has_no_areas(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cross-areas-none");
    draw_and_settle(cx, window);
    assert!(caption_areas(cx, window).close.is_some());

    window
        .update(cx, |workspace, _, _| workspace.forget_caption_areas())
        .unwrap();
    assert_eq!(
        caption_areas(cx, window),
        CaptionAreas::default(),
        "the areas of a caption that is not there are gone"
    );

    // The caption draws again on the next frame it has to draw, and its areas come back
    // with it.
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.font_size += 1., cx)
        })
        .unwrap();
    draw(cx, window);
    assert_caption_areas_match(cx, window, "the caption that came back");
}

/// The drag area is the empty part of the toolbar: it never covers a control that takes a
/// press, and it is a part of every frame as well.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_drag_area_of_the_caption_spares_the_controls_on_every_frame(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cross-drag-areas");
    draw_and_settle(cx, window);
    let drag = caption_areas(cx, window)
        .drag
        .expect("the caption has a drag area");
    assert_eq!(
        drag,
        bounds_of(cx, window, "w-drag-space"),
        "the drag area is the empty space of the toolbar"
    );
    let overlaps = |first: Bounds<Pixels>, second: Bounds<Pixels>| {
        first.left() < second.right()
            && second.left() < first.right()
            && first.top() < second.bottom()
            && second.top() < first.bottom()
    };
    for selector in [
        "menu-bar-0",
        "menu-bar-1",
        "t-project",
        "t-search",
        "t-settings",
        "t-run",
        "t-debug",
        "t-config",
        "w-min",
        "w-max",
        "w-close",
    ] {
        let control = bounds_of(cx, window, selector);
        assert!(!overlaps(drag, control), "the drag area covers {selector}");
    }

    a_frame_the_editor_drew_alone(cx, window);
    assert_eq!(
        caption_areas(cx, window).drag,
        Some(drag),
        "a frame that only the editor drew keeps the drag area"
    );
}

/// Zoom and a resize move the buttons: the areas follow the bounds the title bar drew, not
/// a geometry fixed in the root.
#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_caption_areas_follow_the_buttons_of_the_zoom(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "cross-areas-zoom");
    draw_and_settle(cx, window);
    assert_caption_areas_match(cx, window, "before the zoom");

    press(cx, window, "ctrl-alt-shift-=");
    // One frame: the bar draws its buttons at the new scale first, and the areas of the
    // same frame already sit there.
    draw(cx, window);
    assert_caption_areas_match(cx, window, "the first frame after the zoom");
    draw_and_settle(cx, window);
    assert_caption_areas_match(cx, window, "after the zoom");

    let visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_resize(size(px(900.), px(700.)));
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    assert_caption_areas_match(cx, window, "the first frame after the resize");
    draw_and_settle(cx, window);
    assert_caption_areas_match(cx, window, "after the resize");
    let drag = caption_areas(cx, window)
        .drag
        .expect("the caption keeps its drag area");
    assert_eq!(
        drag,
        bounds_of(cx, window, "w-drag-space"),
        "the drag area follows the toolbar"
    );
}
