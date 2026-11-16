//! The column of the line numbers: it follows the digits of the file, so the number of
//! the last line of a long file shows whole and no mark of the gutter covers it.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::px;

use crate::window_tests::{draw, editor_of, open_path, open_window, sample_project};

/// The width of the marks left of the numbers in the gutter, as the window test counts
/// it: a breakpoint or a run mark lives there, and no digit may enter it.
const MARK_ROOM: f32 = 16.;

#[ui_kit::gpui::test]
fn the_number_column_holds_the_last_line_of_a_long_file(cx: &mut TestAppContext) {
    let root = sample_project("gutter-width");
    let path = root.join("src").join("big.txt");
    let text: String = (1..=5238).map(|line| format!("line {line}\n")).collect();
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root);
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.go_to_line(5237, cx));
    cx.run_until_parked();
    draw(cx, window);
    draw(cx, window);

    let editor = view
        .read_with(cx, |view, _| view.editor_bounds())
        .expect("the editor of the file is on screen");
    let gutter = view.read_with(cx, |view, _| view.gutter_width());
    let number = view
        .read_with(cx, |view, _| view.line_number_bounds(5237))
        .expect("the last line of the file is on screen");

    assert!(
        gutter > 64.,
        "four digits grow the column of the numbers: {gutter}"
    );
    assert!(
        number.left() >= editor.left() + px(MARK_ROOM),
        "the number starts after the marks of the gutter: {number:?} in {editor:?}"
    );
    assert!(
        number.right() <= editor.left() + px(gutter),
        "the whole number stands inside the gutter: {number:?} in {editor:?}"
    );
}
