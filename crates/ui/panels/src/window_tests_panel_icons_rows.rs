//! T-0375: the file rows of the TODO, Local History and Bookmarks windows show the icon
//! of the file type, the one the Project tree and the tabs give them. The Project window
//! is closed and the editor tabs are closed before a frame is checked, so the box of the
//! icon belongs to the row of the window under test.
//!
//! `cargo test -p panels window_tests_panel_icons`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;

use local_history::Cause;
use ui_kit::gpui;
use ui_kit::gpui::{AssetSource as _, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ICON;
use ui_kit::{ARRO, Assets, INTELLIJ, Theme, icon_file};
use workspace::{PROJECT_TOOL, TODO_TOOL, Workspace};

use crate::items::PanelRow;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_window, scratch, wait_for_settled_bounds,
    wait_for_signal,
};

/// A debug selector built at run time. A test lives as long as the process, so a name
/// leaked here costs nothing that matters.
fn selector(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// The file the icon `name` is drawn from in the theme of the window.
fn icon_path(cx: &TestAppContext, name: &str) -> &'static str {
    let (set, dark) = cx.read(|cx| {
        let theme = cx.global::<Theme>();
        (theme.icon_set(), theme.dark)
    });
    selector(icon_file(set, name, dark, ICON).to_string())
}

/// The icon is drawn inside the row.
fn icon_in_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: &'static str,
    icon: &'static str,
) {
    let bounds = bounds_of(cx, window, row);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let drawn = visual
        .debug_bounds(icon)
        .unwrap_or_else(|| panic!("the window draws no {icon}"));
    assert!(
        bounds.contains(&drawn.center()),
        "the icon {icon} is outside the row {row}: {drawn:?} and {bounds:?}"
    );
}

/// Closes the Project window, so that its tree draws no icon of the file under test.
fn hide_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
        .unwrap();
    if !open {
        return;
    }
    window
        .update(cx, |workspace, window, cx| {
            workspace.toggle_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// The icons these windows ask for, in the Arro set and in the IntelliJ set: the folder,
/// module and package of the TODO groups, the TODO mark, the file type and the events of
/// the history and the bookmark.
const NEW_ICONS: [&str; 11] = [
    "nodes/module",
    "nodes/package",
    "nodes/folder",
    "toolwindows/todo",
    "fileTypes/text",
    "general/star",
    "general/check",
    "general/history",
    "general/vcs",
    "codeInsight/intentionBulb",
    "general/settings",
];

#[ui_kit::gpui::test]
fn window_tests_panel_icons_are_files_of_both_sets_and_looks(_cx: &mut TestAppContext) {
    for name in NEW_ICONS {
        for set in [ARRO, INTELLIJ] {
            for dark in [false, true] {
                let path = icon_file(set, name, dark, ICON).to_string();
                let data = Assets.load(&path).ok().flatten();
                assert!(
                    data.is_some_and(|data| !data.is_empty()),
                    "{name} of the {set} set has no file {path}"
                );
            }
        }
    }
}

#[ui_kit::gpui::test]
fn window_tests_panel_icons_todo_file_rows_show_the_file_type_icon(cx: &mut TestAppContext) {
    let root = scratch("panel-icons-todo");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("Main.txt"), "// TODO read me\n").unwrap();
    let window = open_window(cx, root);
    let todo = cx.update(|cx| cx.global::<crate::todo::ActiveTodo>().0.clone());
    wait_for_signal(cx, "the TODO scan", |cx| {
        todo.read_with(cx, |panel, _| !panel.items().is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TODO_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    hide_project(cx, window);
    wait_for_settled_bounds(cx, window, "item-group-0");

    // Without a grouping rule the list starts with the file of the matches.
    let rows = todo.read_with(cx, |panel, _| panel.rows().to_vec());
    assert!(
        matches!(&rows[0], PanelRow::Group { label, .. } if label == "Main.txt"),
        "the list starts with the file row: {rows:?}"
    );
    icon_in_row(cx, window, "item-group-0", icon_path(cx, "fileTypes/text"));
}

#[ui_kit::gpui::test]
fn window_tests_panel_icons_history_file_rows_show_the_file_type_icon(cx: &mut TestAppContext) {
    let root = scratch("panel-icons-history");
    fs::create_dir_all(root.join("src")).unwrap();
    let path = root.join("src").join("Gone.txt");
    fs::write(&path, "alpha\n").unwrap();
    let window = open_window(cx, root.clone());
    wait_for_signal(cx, "the local history", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "beta", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    wait_for_signal(cx, "the saved version", |cx| {
        cx.update(|cx| {
            crate::history::get(cx).is_some_and(|history| {
                history
                    .file_history(&path)
                    .iter()
                    .any(|revision| revision.cause == Cause::Saved)
            })
        })
    });
    fs::remove_file(&path).unwrap();
    // The tab of the file and the tree of the project would draw the same icon.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_tab_of(&path, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    hide_project(cx, window);
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("local_history.show_project", window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the deleted file row", |cx| {
        cx.update(|cx| {
            crate::history_view::report(cx)
                .iter()
                .any(|line| line.starts_with("history-deleted"))
        })
    });
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "history-deleted-row-0");

    icon_in_row(
        cx,
        window,
        "history-deleted-row-0",
        icon_path(cx, "fileTypes/text"),
    );
    // The newest version is the save of the file, and its row has the icon of that event.
    wait_for_settled_bounds(cx, window, "history-row-0");
    icon_in_row(cx, window, "history-row-0", icon_path(cx, "general/check"));
}

#[ui_kit::gpui::test]
fn window_tests_panel_icons_bookmarks_file_rows_show_the_file_type_icon(cx: &mut TestAppContext) {
    let root = scratch("panel-icons-bookmarks");
    fs::create_dir_all(root.join("src")).unwrap();
    let path = root.join("src").join("Marked.txt");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.restore_caret(0, 0, cx);
        view.toggle_bookmark(Some('M'), cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    // The tab of the file and the tree of the project would draw the same icon.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_tab_of(&path, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    hide_project(cx, window);
    click(cx, window, "stripe-s-bookmarks");
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "bookmark-file-row-1");

    icon_in_row(
        cx,
        window,
        "bookmark-file-row-1",
        icon_path(cx, "fileTypes/text"),
    );

    // The mnemonic of the bookmark stands on its icon, not in the caption.
    wait_for_settled_bounds(cx, window, "bookmark-item-row-2");
    let row = bounds_of(cx, window, "bookmark-item-row-2");
    let mnemonic = bounds_of(cx, window, "bookmark-mnemonic-2");
    assert!(
        row.contains(&mnemonic.center()),
        "the mnemonic is outside the bookmark row: {mnemonic:?} and {row:?}"
    );
}
