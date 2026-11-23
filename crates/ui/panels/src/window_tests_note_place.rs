//! A note of the editor at the caret of the last visible line opens above the line, so it
//! stays inside the text and never covers the breadcrumbs or the status bar.

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{bounds_of, draw, editor_of, open_path, press};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

#[gpui::test]
fn window_tests_note_on_the_last_visible_line_opens_above_the_line(cx: &mut TestAppContext) {
    let mut text = String::new();
    for line in 0..120 {
        text.push_str(&format!("body {line}\n"));
    }
    text.push_str("last");
    let root = project("note-place-last-line", &[("src/Main.txt", text.as_str())]);
    let main = root.join("src").join("Main.txt");
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    press(cx, window, "ctrl-end");
    draw(cx, window);
    press(cx, window, "ctrl-shift-p");
    draw(cx, window);

    let note = bounds_of(cx, window, "lsp-note");
    let area = view
        .read_with(cx, |view, _| view.editor_bounds())
        .expect("the frame laid out the text");
    let line = view
        .read_with(cx, |view, _| view.caret_line_bounds())
        .expect("the frame drew the row of the caret");
    assert!(
        note.top() >= area.top() && note.bottom() <= area.bottom(),
        "the note {note:?} is inside the text {area:?}"
    );
    assert!(
        note.bottom() <= line.top(),
        "the note {note:?} stands above the caret row {line:?} at the bottom edge"
    );
}
