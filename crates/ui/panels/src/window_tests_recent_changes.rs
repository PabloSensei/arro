//! Alt+Shift+C and the Recent Changes popup: the latest versions of the project from
//! Local History, the runs of saves joined, the filter, and the history window the Enter
//! key opens at the chosen version.

use std::path::{Path, PathBuf};

use local_history::Cause;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{PROJECT_TOOL, Workspace};

use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_palette, open_path, open_window, press, sample_project,
    type_text, wait_for_signal, with_window,
};

fn wait_for_history(cx: &mut TestAppContext) {
    wait_for_signal(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
}

fn saved_count(cx: &mut TestAppContext, path: &Path) -> usize {
    cx.update(|cx| {
        crate::history::get(cx).map_or(0, |history| {
            history
                .file_history(path)
                .iter()
                .filter(|revision| revision.cause == Cause::Saved)
                .count()
        })
    })
}

/// Saves the file twice with a text that differs each time, so the history holds two
/// saved versions of it.
fn save_twice(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    for (text, wanted) in [("first", 1), ("second", 2)] {
        view.update(cx, |view, cx| {
            view.replace_text(0..5, text, cx);
            assert!(view.save_blocking(cx).is_none());
        });
        wait_for_signal(cx, "the saved version", |cx| {
            saved_count(cx, path) >= wanted
        });
    }
}

/// Renames the file by the key of the project tree, as a user does.
fn rename_in_tree(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    new_name: &str,
) -> PathBuf {
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(path, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "shift-f6");
    draw(cx, window);
    let length = path
        .file_name()
        .map_or(0, |name| name.to_string_lossy().chars().count());
    let backspaces = vec!["backspace"; length].join(" ");
    press(cx, window, &backspaces);
    type_text(cx, window, new_name);
    press(cx, window, "enter");
    draw(cx, window);
    let renamed = path.with_file_name(new_name);
    wait_for_signal(cx, "the renamed file", |_| renamed.exists());
    renamed
}

/// A project whose one file has two saved versions and a rename after them.
fn saved_twice_and_renamed(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root.clone());
    wait_for_history(cx);
    save_twice(cx, window, &path);
    rename_in_tree(cx, window, &path, "renamed.txt");
    wait_for_signal(cx, "the rename in the history", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| {
                history
                    .folder_history(&root)
                    .iter()
                    .any(|revision| revision.cause == Cause::BeforeRename && revision.path == path)
            })
        })
    });
    (root, window, path)
}

#[gpui::test]
fn window_tests_recent_changes_puts_the_rename_first_and_joins_the_saves(cx: &mut TestAppContext) {
    let (root, window, _old) = saved_twice_and_renamed(cx, "recent-changes-rename");
    // The key works from the editor, where the user pressed it when the popup was missing.
    open_path(cx, window, &root.join("src").join("renamed.txt"));
    press(cx, window, "alt-shift-c");
    draw(cx, window);
    let rows = cx.update(|cx| crate::recent_changes::report(cx));
    assert!(
        rows.first()
            .is_some_and(|row| row.starts_with("recent-change Rename") && row.contains("main.txt")),
        "the rename is the first row: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.starts_with("recent-change Saved"))
            .count(),
        1,
        "the two saves of the file are one row: {rows:?}"
    );
}

#[gpui::test]
fn window_tests_recent_changes_enter_opens_the_history_at_that_version(cx: &mut TestAppContext) {
    let (_root, window, old) = saved_twice_and_renamed(cx, "recent-changes-enter");
    press(cx, window, "alt-shift-c");
    draw(cx, window);
    let (id, title) = cx
        .update(|cx| crate::recent_changes::selected(cx))
        .expect("the popup has a row to open");
    assert_eq!(title, "Rename");

    press(cx, window, "enter");
    draw(cx, window);
    wait_for_signal(cx, "the history window at the version", |cx| {
        cx.update(|cx| crate::history_view::selected_revision_id(cx)) == Some(id)
    });
    let lines = cx.update(|cx| crate::history_view::report(cx));
    assert!(
        lines.iter().any(|line| line == "history-version Rename"),
        "the history window shows the version of the rename: {lines:?}"
    );
    assert!(
        old.file_name().and_then(|name| name.to_str()) == Some("main.txt"),
        "the version belongs to the file that was renamed"
    );
}

#[gpui::test]
fn window_tests_recent_changes_empty_history_says_so(cx: &mut TestAppContext) {
    let root = sample_project("recent-changes-empty");
    let window = open_window(cx, root);
    wait_for_history(cx);
    press(cx, window, "alt-shift-c");
    draw(cx, window);
    let rows = cx.update(|cx| crate::recent_changes::report(cx));
    assert_eq!(rows, ["No recent changes"]);
    assert!(
        bounds_of(cx, window, "recent-changes-popup").size.height > ui_kit::px(0.),
        "the popup says the history is empty in place of its rows"
    );
}

#[gpui::test]
fn window_tests_recent_changes_letters_filter_rows_and_escape_closes(cx: &mut TestAppContext) {
    let (_root, window, _old) = saved_twice_and_renamed(cx, "recent-changes-filter");
    press(cx, window, "alt-shift-c");
    draw(cx, window);
    type_text(cx, window, "ren");
    draw(cx, window);
    let rows = cx.update(|cx| crate::recent_changes::report(cx));
    assert!(
        !rows.is_empty() && rows.iter().all(|row| row.contains("Rename")),
        "the letters keep the row of the rename: {rows:?}"
    );
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        cx.update(|cx| crate::recent_changes::report(cx)).is_empty(),
        "Escape closes the popup"
    );
}

#[gpui::test]
fn window_tests_recent_changes_is_in_find_action(cx: &mut TestAppContext) {
    let root = sample_project("recent-changes-find-action");
    let window = open_window(cx, root);
    wait_for_history(cx);
    open_palette(cx, window, "workspace.find_action");
    type_text(cx, window, "Recent Changes");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    let rows = cx.update(|cx| crate::recent_changes::report(cx));
    assert_eq!(
        rows,
        ["No recent changes"],
        "Find Action opens the Recent Changes popup"
    );
}
