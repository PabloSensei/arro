//! The cost of a frame of the merge window on a big file. Typing in the result of a
//! 5000-line merge paints the three panes twice per key, and a frame must not walk every
//! line of the text for each of those panes: the widest row is counted once per text and
//! read from that count while the text stays as it is.

use std::time::Duration;

use editor_view::{EditorView, reset_widest_row_counts, widest_row_counts};
use ui_kit::Focusable;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{click, draw, press, with_window};
use crate::window_tests_git::start_conflicting_merge;
use crate::window_tests_merge::{conflict_repository, merge_view};

/// The three texts of a big merge: 5000 lines, both sides changed at the top and the end.
fn big_texts() -> (String, String, String) {
    let base: String = (0..5000).map(|line| format!("line {line}\n")).collect();
    let ours =
        base.replacen("line 0\n", "ours at the top\n", 1)
            .replacen("line 10\n", "ours at ten\n", 1);
    let theirs = base
        .replacen("line 0\n", "theirs at the top\n", 1)
        .replacen("line 4999\n", "theirs at the end\n", 1);
    (base, ours, theirs)
}

/// Opens the merge window of a big file and puts the focus into its result.
fn open_big_merge(cx: &mut TestAppContext) -> (WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let (base, ours, theirs) = big_texts();
    let (root, _bare, _git) = conflict_repository(
        "merge-growth",
        "main.txt",
        base.as_bytes(),
        ours.as_bytes(),
        theirs.as_bytes(),
    );
    let (window, _state) = start_conflicting_merge(cx, root);
    click(cx, window, "conflict-merge-0");
    draw(cx, window);
    let editor = merge_view(cx, window).read_with(cx, |view, _| view.result_editor());
    with_window(cx, window, |window, cx| {
        window.focus(&editor.read(cx).focus_handle(cx), cx);
    });
    (window, editor)
}

#[ui_kit::gpui::test]
fn a_frame_of_a_big_merge_does_not_walk_the_whole_text(cx: &mut TestAppContext) {
    const KEYS: usize = 20;
    let (window, editor) = open_big_merge(cx);
    let initial = editor
        .read_with(cx, |editor, _| editor.text())
        .unwrap_or_default();

    reset_widest_row_counts();
    let mut counted = widest_row_counts();
    for _ in 0..KEYS {
        press(cx, window, "a");
        // A loaded machine puts more time between two keys than a typing run takes, so
        // every key becomes an undo step of its own; the work of a frame must not follow
        // the time.
        std::thread::sleep(Duration::from_millis(2));
        let after = widest_row_counts();
        assert!(
            after > counted,
            "a new text was not counted: the horizontal bar cannot follow the typing"
        );
        assert!(
            after <= counted + 1,
            "the new text was counted {} times for one key",
            after - counted
        );
        counted = after;
    }
    draw(cx, window);
    assert!(
        counted <= KEYS + 2,
        "the widest row was counted {counted} times for {KEYS} keys: a frame walks the \
         lines of the text again"
    );
    let text = editor
        .read_with(cx, |editor, _| editor.text())
        .unwrap_or_default();
    assert_eq!(
        text.len(),
        initial.len() + KEYS,
        "the keys reached the result"
    );

    // A repaint of the same text, as the caret blink of the window asks for, reads the
    // count of the last text and does not start another walk.
    for _ in 0..4 {
        draw(cx, window);
    }
    assert_eq!(
        widest_row_counts(),
        counted,
        "a frame without a text change walked the lines again"
    );
}
