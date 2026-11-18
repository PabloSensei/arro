//! The context menu of the editor in a window too low for the whole menu. The menu stays
//! inside the window, its rows scroll with the wheel and the arrows, and Enter runs the row
//! the arrows stop on. A tall window shows the same menu whole, where the click put it, and
//! the menu does not scroll there. No window of the machine is opened.

use ui_kit::gpui;
use ui_kit::gpui::{
    Bounds, MouseButton, Pixels, Point, ScrollDelta, ScrollEvent, TestAppContext,
    VisualTestContext, WindowHandle, size,
};
use ui_kit::metrics::{POPUP_PADDING, SPACE_M, UI_SIZE};
use ui_kit::{Modifiers, point, px};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, draw, line_height, open_path, open_window, press, sample_project,
};

/// A window tall enough for the whole menu of the sample file.
const TALL: f32 = 1400.;
const WIDE: f32 = 1100.;
/// The row the arrows walk to in the test of the low window.
const COPY_PATH: &str = "Copy Path";
/// Half a pixel: the layout rounds the edges of the rows.
const SLACK: f32 = 0.5;

/// A window of the sample project with its first file open in the editor.
fn open_sample_file(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    window
}

fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32, height: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Presses `keys` and draws the frame that shows what they did.
fn key(cx: &mut TestAppContext, window: WindowHandle<Workspace>, keys: &str) {
    press(cx, window, keys);
    draw(cx, window);
}

/// Turns the wheel down by `lines` with the pointer at `position`.
fn wheel_down(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: Point<Pixels>,
    lines: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position,
        delta: ScrollDelta::Lines(point(0., -lines)),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Right-clicks the text at `line` and `column`, where a click of the pointer lands.
fn right_click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) -> Point<Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    let position = point(
        bounds.left() + px(67. + column as f32 * 8.),
        bounds.top() + px(height * line as f32 + height / 2.),
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    position
}

/// The labels of the rows of the open menu, from the top.
fn labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

/// The top-level row that the arrows highlight in the open menu.
fn highlighted(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<usize> {
    window
        .read_with(cx, |workspace, _| workspace.menu_highlight())
        .unwrap()
        .and_then(|path| path.first().copied())
}

/// The bounds of the top-level row `index` of the open menu, where its frame draws it.
fn row_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    index: usize,
) -> Bounds<Pixels> {
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no row {index} on screen"))
}

/// The frame of the open menu, with its border.
fn frame_of(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Bounds<Pixels> {
    bounds_of(cx, window, "menu-popup")
}

/// The area the rows show in: the frame less its border and its padding.
fn rows_area(frame: Bounds<Pixels>) -> Bounds<Pixels> {
    let inset = px(1. + POPUP_PADDING);
    Bounds::new(
        point(frame.left(), frame.top() + inset),
        size(frame.size.width, frame.size.height - inset * 2.),
    )
}

/// Whether a row is drawn inside the area of the rows, where a user can see and press it.
fn shows(area: Bounds<Pixels>, row: Bounds<Pixels>) -> bool {
    let slack = px(SLACK);
    row.top() + slack >= area.top() && row.bottom() <= area.bottom() + slack
}

#[ui_kit::gpui::test]
fn the_editor_menu_stays_in_a_low_window_and_its_rows_scroll(cx: &mut TestAppContext) {
    let window = open_sample_file(cx, "menu-scroll-low");
    // The window is tall enough for the whole menu, so its height is the menu's own.
    resize(cx, window, WIDE, TALL);
    right_click_text(cx, window, 0, 2);
    let labels = labels(cx, window);
    let whole = frame_of(cx, window).size.height;
    assert!(
        whole > px(400.),
        "the menu of the sample file is taller than a low window"
    );
    press(cx, window, "escape");

    // A window of 60% of that height cannot show the whole menu.
    let low = f32::from(whole) * 0.6;
    resize(cx, window, WIDE, low);
    right_click_text(cx, window, 0, 2);
    let frame = frame_of(cx, window);
    assert!(
        frame.top() >= px(SPACE_M - SLACK),
        "the menu starts under the top margin"
    );
    assert!(
        frame.bottom() <= px(low - SPACE_M + SLACK),
        "the menu ends above the bottom margin"
    );
    assert!(
        frame.size.height <= px(low - 2. * SPACE_M + SLACK),
        "the menu is no taller than the window less the margins"
    );
    let last = labels.len() - 1;
    assert_eq!(
        labels[last], "Ask AI...",
        "the sample menu ends with its disabled row"
    );
    let area = rows_area(frame);
    assert!(
        !shows(area, row_at(cx, window, last)),
        "the last row is cut off before the wheel"
    );

    // The wheel brings the last row into view.
    wheel_down(cx, window, frame.center(), 60.);
    let area = rows_area(frame_of(cx, window));
    assert!(
        shows(area, row_at(cx, window, last)),
        "the wheel brings the last row into view"
    );

    // Home shows the first row, End the last row that can run.
    key(cx, window, "home");
    assert_eq!(
        highlighted(cx, window),
        Some(0),
        "Home highlights the first row"
    );
    assert!(
        shows(area, row_at(cx, window, 0)),
        "Home shows the first row"
    );
    // A separator and the disabled Ask AI come after Local History, the last row that runs.
    let runs = last - 2;
    key(cx, window, "end");
    assert_eq!(
        labels[runs], "Local History",
        "the last row that runs is Local History"
    );
    assert_eq!(
        highlighted(cx, window),
        Some(runs),
        "End highlights the last row that runs"
    );
    assert!(
        shows(area, row_at(cx, window, runs)),
        "End shows the last row that runs"
    );

    // The arrows walk down to Copy Path, and every row they stop on shows; Enter runs it.
    key(cx, window, "home");
    let target = labels
        .iter()
        .position(|label| label == COPY_PATH)
        .expect("the sample menu has Copy Path");
    for _ in 0..labels.len() {
        if highlighted(cx, window) == Some(target) {
            break;
        }
        key(cx, window, "down");
        let now = highlighted(cx, window).expect("a row stays highlighted");
        assert!(
            shows(area, row_at(cx, window, now)),
            "the highlighted row {} shows",
            labels[now]
        );
    }
    assert_eq!(
        highlighted(cx, window),
        Some(target),
        "Down reaches Copy Path"
    );
    key(cx, window, "enter");
    let clipboard = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert!(
        clipboard.is_some_and(|text| text.ends_with("main.txt")),
        "Enter runs Copy Path, which copies the path of the file"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_none())
            .unwrap(),
        "the menu closes after its row runs"
    );
}

#[ui_kit::gpui::test]
fn the_editor_menu_of_a_tall_window_keeps_its_place_and_does_not_scroll(cx: &mut TestAppContext) {
    let window = open_sample_file(cx, "menu-scroll-tall");
    resize(cx, window, WIDE, TALL);
    let position = right_click_text(cx, window, 0, 2);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.menu_position())
            .unwrap(),
        Some(position),
        "the menu stands where the click was"
    );
    let frame = frame_of(cx, window);
    assert_eq!(frame.origin, position, "the frame starts at the click");
    assert!(
        frame.bottom() <= px(TALL - SPACE_M),
        "the whole menu is in the window"
    );

    // The frame has the size that the menu measures for its own placement.
    let measured = window
        .update(cx, |workspace, window, _| {
            let entries = workspace
                .menu_entries()
                .map(|entries| entries.to_vec())
                .unwrap_or_default();
            ui_kit::menu_size(window, &entries, px(UI_SIZE))
        })
        .unwrap();
    assert!(
        (f32::from(frame.size.width) - f32::from(measured.width)).abs() < 1.,
        "the width is the one the menu measures: {:?} and {:?}",
        frame.size.width,
        measured.width
    );
    assert!(
        (f32::from(frame.size.height) - f32::from(measured.height)).abs() < 1.,
        "the height is the one the menu measures: {:?} and {:?}",
        frame.size.height,
        measured.height
    );

    // Nothing scrolls: the last row stays where it is after the wheel.
    let last = labels(cx, window).len() - 1;
    let before = row_at(cx, window, last);
    assert!(
        shows(rows_area(frame), before),
        "the last row shows without a scroll"
    );
    wheel_down(cx, window, frame.center(), 10.);
    assert_eq!(
        row_at(cx, window, last).origin,
        before.origin,
        "the menu does not scroll"
    );
}
