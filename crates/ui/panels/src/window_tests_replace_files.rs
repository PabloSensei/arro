//! Replace in Files with In Selection, and with a file whose unsaved buffer no tab shows: the
//! replacement changes only the matches the search found, and an unsaved buffer takes the
//! replacement in its text while the disk keeps its own text until Save All.

use std::fs;

use editor_view::EditorView;
use text_core::Edit;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, press, scratch, type_text,
    wait_for_file_index, wait_for_signal,
};
use crate::window_tests_finder_scope::finder;

#[ui_kit::gpui::test]
fn replace_in_files_in_selection_changes_only_the_selected_match(cx: &mut TestAppContext) {
    let root = scratch("replace-files-selection");
    let path = root.join("a.txt");
    fs::write(&path, "one\none\none\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    // The first match is selected.
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    for _ in 0..3 {
        press(cx, window, "shift-right");
    }
    let selected = view.read_with(cx, |view, _| view.selection().expect("the file is read").0);
    assert_eq!(selected, 0..3);

    press(cx, window, "ctrl-shift-r");
    draw(cx, window);
    press(cx, window, "ctrl-alt-e");
    type_text(cx, window, "one");
    press(cx, window, "tab");
    type_text(cx, window, "two");
    wait_for_signal(cx, "the search of the selection", |cx| {
        finder(cx).read_with(cx, |finder, _| finder.export_text().contains("a.txt"))
    });
    draw(cx, window);
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the replaced file", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "two\none\none\n")
    });
    assert_eq!(buffer_of(cx, &view), "two\none\none\n");
}

#[ui_kit::gpui::test]
fn replace_in_files_changes_an_unsaved_buffer_and_save_all_keeps_the_change(
    cx: &mut TestAppContext,
) {
    let root = scratch("replace-files-buffer");
    let path = root.join("a.txt");
    fs::write(&path, "alpha beta\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    // The buffer of the file, with unsaved text, and no tab that shows it.
    let shown = window
        .update(cx, |workspace, window, cx| {
            workspace.open_buffer(path.clone(), window, cx)
        })
        .expect("the window is open");
    let Ok(buffer) = shown.downcast::<EditorView>() else {
        panic!("the buffer is an editor");
    };
    wait_for_signal(cx, "the text of the buffer", |cx| {
        buffer.read_with(cx, |view, _| view.text().is_some())
    });
    buffer.update(cx, |view, cx| {
        view.apply_edits(vec![Edit::new(0..0, "gamma ")], cx)
    });
    assert!(buffer.read_with(cx, |view, _| view.is_dirty()));

    press(cx, window, "ctrl-shift-r");
    draw(cx, window);
    type_text(cx, window, "alpha");
    press(cx, window, "tab");
    type_text(cx, window, "omega");
    wait_for_signal(cx, "the search of the project", |cx| {
        finder(cx).read_with(cx, |finder, _| finder.export_text().contains("a.txt"))
    });
    draw(cx, window);
    press(cx, window, "ctrl-enter");
    assert_eq!(
        buffer_of(cx, &buffer),
        "gamma omega beta\n",
        "the replacement goes into the unsaved text"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "alpha beta\n",
        "the disk keeps its text until the buffer is saved"
    );

    window
        .update(cx, |workspace, _, cx| workspace.save_all(cx))
        .expect("the window is open");
    wait_for_signal(cx, "the saved buffer", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "gamma omega beta\n")
    });
}
