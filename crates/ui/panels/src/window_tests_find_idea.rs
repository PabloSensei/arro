//! Find and Replace of one file and of the project as IDEA does it: Find in Selection,
//! the history of Alt+Down and Preserve Case.

use std::fs;
use std::ops::Range;
use std::path::PathBuf;

use editor_view::EditorView;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::finder::ActiveFinder;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, scratch, type_text,
    wait_for_file_index, wait_for_signal,
};

/// A project of one text file, its window and the editor of the file.
fn open_text(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, Entity<EditorView>) {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("README.txt"), "A sample project.\n").unwrap();
    let path = root.join("src").join("main.txt");
    fs::write(&path, text).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (root, window, view)
}

fn selected(cx: &TestAppContext, view: &Entity<EditorView>) -> Range<usize> {
    view.read_with(cx, |view, _| view.selection().expect("the file is read").0)
}

/// Puts the caret at the top and selects the first `lines` lines of the file.
fn select_lines(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &Entity<EditorView>,
    lines: usize,
) {
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    for _ in 0..lines {
        press(cx, window, "shift-down");
    }
}

fn finder(cx: &TestAppContext) -> Entity<crate::finder::Finder> {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
            .expect("Find in Files is open")
    })
}

#[ui_kit::gpui::test]
fn find_and_replace_in_selection_stay_inside_three_lines(cx: &mut TestAppContext) {
    let (_root, window, view) = open_text(cx, "find-in-selection", "alpha\nbeta\nalpha\nalpha\n");

    select_lines(cx, window, &view, 3);
    assert_eq!(selected(cx, &view), 0..17, "three lines are selected");

    press(cx, window, "ctrl-f");
    type_text(cx, window, "alpha");
    assert_eq!(
        selected(cx, &view),
        0..5,
        "the first match of the selection"
    );
    press(cx, window, "enter");
    assert_eq!(
        selected(cx, &view),
        11..16,
        "the second match is in the selection"
    );

    // Ctrl+Alt+E turns In Selection off, so the match after the selection is reached.
    press(cx, window, "ctrl-alt-e");
    press(cx, window, "enter");
    press(cx, window, "enter");
    assert_eq!(
        selected(cx, &view),
        17..22,
        "without In Selection the line outside is found"
    );
    press(cx, window, "escape");

    // Replace All changes the same three lines and leaves the fourth one.
    select_lines(cx, window, &view, 3);
    press(cx, window, "ctrl-r");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "alpha");
    press(cx, window, "tab");
    type_text(cx, window, "omega");
    press(cx, window, "ctrl-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "omega\nbeta\nomega\nalpha\n",
        "Replace All replaces only inside the selection"
    );
}

#[ui_kit::gpui::test]
fn alt_down_brings_back_a_past_query(cx: &mut TestAppContext) {
    let (_root, window, view) = open_text(cx, "find-history", "alpha\nbeta\n");
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();

    press(cx, window, "ctrl-f");
    type_text(cx, window, "alpha");
    press(cx, window, "escape");
    // The list lives in the state folder, so the next start of the window has it.
    let path = state.join("find-history.txt");
    wait_for_signal(cx, "the saved history", |_| {
        fs::read_to_string(&path).is_ok_and(|body| body.lines().any(|line| line == "alpha"))
    });

    press(cx, window, "ctrl-f");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "beta");
    press(cx, window, "escape");

    // The list is the newest first: beta, then alpha.
    press(cx, window, "ctrl-f");
    press(cx, window, "alt-down");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    assert_eq!(
        selected(cx, &view),
        0..5,
        "the chosen query is alpha, not the remembered beta"
    );

    // Up and Down in an empty field open the same list.
    press(cx, window, "ctrl-f");
    press(cx, window, "ctrl-a");
    press(cx, window, "backspace");
    press(cx, window, "down");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(
        selected(cx, &view),
        6..10,
        "the newest query, beta, comes back from an empty field"
    );
}

#[ui_kit::gpui::test]
fn replace_preserves_the_case_of_every_match(cx: &mut TestAppContext) {
    let (_root, window, view) =
        open_text(cx, "find-preserve-case", "alpha ALPHA Alpha alphaBeta\n");

    press(cx, window, "ctrl-r");
    type_text(cx, window, "alpha");
    press(cx, window, "tab");
    type_text(cx, window, "omega");
    draw(cx, window);
    click(cx, window, "bar-preserve-case");
    draw(cx, window);
    press(cx, window, "ctrl-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "omega OMEGA Omega omegaBeta\n",
        "each replacement takes the case of its match"
    );
}

#[ui_kit::gpui::test]
fn replace_in_files_preserves_the_case_of_every_match(cx: &mut TestAppContext) {
    let root = scratch("find-preserve-case-files");
    let path = root.join("a.txt");
    fs::write(&path, "foo FOO Foo fooBar\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    press(cx, window, "ctrl-shift-r");
    draw(cx, window);
    type_text(cx, window, "foo");
    press(cx, window, "tab");
    type_text(cx, window, "bar");
    wait_for_signal(cx, "the search of the project", |cx| {
        finder(cx).read_with(cx, |finder, _| finder.export_text().contains("a.txt"))
    });
    draw(cx, window);
    click(cx, window, "replace-preserve-case");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the replaced file", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "bar BAR Bar barBar\n")
    });
}
