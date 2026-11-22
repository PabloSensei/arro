//! The bar of Find and Replace: its field is 320 to 600 points wide and grows with its text,
//! the toggles sit inside the field at its right edge, the counter follows the field, the
//! close button stands at the right edge of the bar, and the Replace field lines up with the
//! Find field.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle, size};
use ui_kit::px;
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, open_path, open_window, press, scratch, type_text};

/// The window of the tests is wide, as the window of a user on a large display is.
const WIDE: f32 = 2560.;
const HEIGHT: f32 = 1200.;

/// A window of a project with one text file open in its editor, drawn at the wide size.
fn open_wide(cx: &mut TestAppContext, name: &str, text: &str) -> WindowHandle<Workspace> {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.txt"), "A sample project.\n").unwrap();
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(WIDE), px(HEIGHT)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    window
}

/// The width of a box, in points.
fn width(bounds: gpui::Bounds<gpui::Pixels>) -> f32 {
    f32::from(bounds.size.width)
}

#[ui_kit::gpui::test]
fn window_tests_find_field_is_capped_at_600_points_and_the_counter_follows_it(
    cx: &mut TestAppContext,
) {
    let window = open_wide(cx, "find-bar-cap", "alpha beta\n");
    press(cx, window, "ctrl-f");
    type_text(cx, window, &"alpha ".repeat(40));
    draw(cx, window);
    let field = bounds_of(cx, window, "bar-query");
    assert!(
        (599.5..=600.).contains(&width(field)),
        "a long query fills the field to its limit of 600 points, not {}",
        width(field)
    );
    let count = bounds_of(cx, window, "bar-count");
    let gap = f32::from(count.left() - field.right());
    assert!(
        (0. ..=48.).contains(&gap),
        "the counter stands {gap} points right of the field"
    );
}

#[ui_kit::gpui::test]
fn window_tests_find_field_starts_at_320_points_and_grows_with_its_text(cx: &mut TestAppContext) {
    let window = open_wide(cx, "find-bar-growth", "alpha\n");
    press(cx, window, "ctrl-f");
    draw(cx, window);
    let empty = width(bounds_of(cx, window, "bar-query"));
    type_text(cx, window, "alpha");
    draw(cx, window);
    let short = width(bounds_of(cx, window, "bar-query"));
    type_text(cx, window, " beta gamma delta epsilon zeta");
    draw(cx, window);
    let grown = width(bounds_of(cx, window, "bar-query"));
    assert!(
        (empty - 320.).abs() < 0.5,
        "an empty field is {empty} points wide"
    );
    assert!(
        (short - 320.).abs() < 0.5,
        "a short query keeps the field at 320 points, not {short}"
    );
    assert!(
        grown > short + 20.,
        "the field grows with its query: {short} points, then {grown}"
    );
}

#[ui_kit::gpui::test]
fn window_tests_find_toggles_sit_inside_the_field(cx: &mut TestAppContext) {
    let window = open_wide(cx, "find-bar-toggles", "alpha\n");
    press(cx, window, "ctrl-f");
    type_text(cx, window, "alpha");
    draw(cx, window);
    let field = bounds_of(cx, window, "bar-query");
    for toggle in ["bar-case", "bar-word", "bar-regex"] {
        let bounds = bounds_of(cx, window, toggle);
        let inside = bounds.left() >= field.left()
            && bounds.right() <= field.right()
            && bounds.top() >= field.top()
            && bounds.bottom() <= field.bottom();
        assert!(
            inside,
            "{toggle} at {bounds:?} lies outside the field at {field:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn window_tests_find_row_runs_field_counter_arrows_selection_then_close(cx: &mut TestAppContext) {
    let window = open_wide(cx, "find-bar-row", "alpha beta alpha\n");
    press(cx, window, "ctrl-f");
    type_text(cx, window, "alpha");
    draw(cx, window);
    let field = bounds_of(cx, window, "bar-query");
    let count = bounds_of(cx, window, "bar-count");
    let previous = bounds_of(cx, window, "bar-prev");
    let next = bounds_of(cx, window, "bar-next");
    let selection = bounds_of(cx, window, "bar-selection");
    let close = bounds_of(cx, window, "bar-close");
    assert!(
        field.right() <= count.left()
            && count.right() <= previous.left()
            && previous.right() <= next.left()
            && next.right() <= selection.left()
            && selection.right() <= close.left(),
        "the row runs field, counter, arrows, In Selection, close: {field:?} {count:?} \
         {previous:?} {next:?} {selection:?} {close:?}"
    );
    let body = bounds_of(cx, window, "editor-body-0");
    let edge = f32::from(body.right() - close.right());
    assert!(
        (0. ..=40.).contains(&edge),
        "the close button stands {edge} points from the right edge of the editor"
    );
}

#[ui_kit::gpui::test]
fn window_tests_find_replace_field_lines_up_with_the_find_field(cx: &mut TestAppContext) {
    let window = open_wide(cx, "find-bar-replace-line", "alpha\n");
    press(cx, window, "ctrl-r");
    type_text(
        cx,
        window,
        "a query that is long enough to matter for the width",
    );
    draw(cx, window);
    let query = bounds_of(cx, window, "bar-query");
    let replacement = bounds_of(cx, window, "bar-replacement");
    let left = f32::from(replacement.left() - query.left());
    assert!(
        left.abs() < 0.5,
        "the Replace field starts {left} points away from the Find field"
    );
    assert!(
        (width(replacement) - width(query)).abs() < 0.5,
        "the Replace field is {} points wide, the Find field {}",
        width(replacement),
        width(query)
    );
}
