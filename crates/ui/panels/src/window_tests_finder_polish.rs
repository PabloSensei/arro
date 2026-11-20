//! Find in Files of IDEA: the line under the fields counts the matches while the search
//! runs, and the preview under the list shows the row that is selected.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::finder::{ActiveFinder, Finder};
use crate::window_tests::{
    draw, open_window, press, scratch, type_text, wait_for_file_index, wait_for_signal,
};

fn finder(cx: &TestAppContext) -> Entity<Finder> {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
            .expect("Find in Files is open")
    })
}

/// The text the line under the fields shows in the last frame.
fn status_text() -> String {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(name, _)| name == "finder-status-text")
        .and_then(|(_, bounds)| bounds.label)
        .map_or_else(String::new, |label| label.to_string())
}

fn open_finder(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
) -> WindowHandle<Workspace> {
    let root = scratch(name);
    for (file, text) in files {
        fs::write(root.join(file), text).unwrap();
    }
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    press(cx, window, "ctrl-shift-f");
    wait_for_signal(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    draw(cx, window);
    window
}

#[gpui::test]
fn the_window_says_searching_until_the_search_answers(cx: &mut TestAppContext) {
    let window = open_finder(
        cx,
        "finder-polish-searching",
        &[("a.txt", "alpha\n"), ("b.txt", "beta\n")],
    );
    let finder = finder(cx);

    // A query that matches nothing: before the answer the window must not claim that.
    finder.update(cx, |finder, cx| finder.search("zzz", cx));
    draw(cx, window);
    assert_eq!(
        status_text(),
        "Searching…",
        "the running search says so, not that nothing was found"
    );
    assert!(finder.read_with(cx, |finder, _| finder.is_searching()));

    wait_for_signal(cx, "the answer of the search", |cx| {
        !finder.read_with(cx, |finder, _| finder.is_searching())
    });
    draw(cx, window);
    assert_eq!(
        status_text(),
        "Nothing found",
        "an empty answer after the search is Nothing found"
    );
}

#[gpui::test]
fn the_preview_shows_the_line_of_the_selected_row_and_down_moves_it(cx: &mut TestAppContext) {
    let root = scratch("finder-polish-preview");
    let first = root.join("a.txt");
    let second = root.join("b.txt");
    fs::write(&first, "first line\nalpha one\n").unwrap();
    fs::write(&second, "beta alpha\nlast line\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    press(cx, window, "ctrl-shift-f");
    wait_for_signal(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    draw(cx, window);
    type_text(cx, window, "alpha");
    let finder = finder(cx);
    let preview = finder.read_with(cx, |finder, _| finder.preview().clone());
    wait_for_preview(cx, &preview, &first, "alpha");

    press(cx, window, "down");
    wait_for_preview(cx, &preview, &second, "alpha");
    let editor = preview
        .read_with(cx, |preview, _| preview.editor().cloned())
        .expect("the preview has an editor");
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (0, 5),
        "the preview shows the match of the second file"
    );
    draw(cx, window);
    assert_eq!(
        status_text(),
        "2 matches in 2 files",
        "the line of the window counts the finished search"
    );
}

fn wait_for_preview(
    cx: &mut TestAppContext,
    preview: &Entity<crate::preview::FilePreview>,
    path: &std::path::Path,
    text: &'static str,
) {
    wait_for_signal(cx, "the preview of the selected row", |cx| {
        preview.read_with(cx, |preview, _| {
            preview.is_ready()
                && preview.path() == Some(path)
                && preview.highlight_text().as_deref() == Some(text)
        })
    });
}
