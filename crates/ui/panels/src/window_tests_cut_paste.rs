//! The project tree's cut and paste with unsaved text, and a delete of several rows that fails
//! on one of them: the window without a window. Each scenario checks the tabs and the disk.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::project::{ActiveProjectPanel, ProjectPanel};
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window, press, type_text, wait_for_signal,
};
use crate::window_tests_copy_class::project;

fn project_panel(cx: &TestAppContext) -> gpui::Entity<ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

/// Puts the keyboard on the project tree, the way a click on it does.
fn focus_tree(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    crate::window_tests_menus::show_project(cx, window);
    let panel = project_panel(cx);
    window
        .update(cx, |_, window, cx| {
            window.focus(&panel.read(cx).focus(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Ctrl+X of a file with unsaved edits and Ctrl+V into another folder: the edit goes with the
/// file, into the tab that follows it and onto the disk.
#[ui_kit::gpui::test]
fn a_cut_file_keeps_its_unsaved_edit_in_the_tab_and_on_the_disk(cx: &mut TestAppContext) {
    let root = project(
        "cut-unsaved-edit",
        &[("src/a/Note.txt", "alpha\n"), ("src/b/Other.txt", "beta\n")],
    );
    let from = root.join("src/a/Note.txt");
    let folder = root.join("src/b");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &from);
    type_text(cx, window, "edit ");
    let view = editor_of(cx, window, &from);
    assert_eq!(
        fs::read_to_string(&from).unwrap(),
        "alpha\n",
        "the edit is not on the disk yet"
    );

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_tree(cx, window);
    press(cx, window, "ctrl-x");
    panel.update(cx, |panel, cx| panel.select_path(&folder, cx));
    focus_tree(cx, window);
    // Still unsaved right before the paste: the write must come from the paste itself.
    assert!(view.read_with(cx, |view, _| view.is_dirty()));
    press(cx, window, "ctrl-v");

    let moved = folder.join("Note.txt");
    wait_for_signal(cx, "the moved file", |_| moved.is_file() && !from.exists());
    cx.run_until_parked();
    draw(cx, window);
    let on_disk = fs::read_to_string(&moved).unwrap();
    assert!(
        on_disk.contains("edit "),
        "the disk has the edit: {on_disk:?}"
    );
    let moved_view = editor_of(cx, window, &moved);
    wait_for_signal(cx, "the new tab to load its file", |cx| {
        moved_view.read_with(cx, |view, _| view.text().is_some())
    });
    let text = buffer_of(cx, &moved_view);
    assert!(text.contains("edit "), "the new tab has the edit: {text:?}");
}

/// A delete of three rows fails on the second, because its file is gone from the disk already.
/// The first file went and its tab closes; the third is not reached and keeps its tab.
#[ui_kit::gpui::test]
fn a_failed_delete_of_several_rows_closes_the_tabs_of_the_files_already_gone(
    cx: &mut TestAppContext,
) {
    // The delete below must not put the scratch files into the Recycle Bin of the machine.
    crate::tree_ops::use_temporary_delete();
    let root = project(
        "delete-partial",
        &[
            ("src/a.txt", "a\n"),
            ("src/b.txt", "b\n"),
            ("src/c.txt", "c\n"),
        ],
    );
    let (a, b, c) = (
        root.join("src/a.txt"),
        root.join("src/b.txt"),
        root.join("src/c.txt"),
    );
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    for path in [&a, &b, &c] {
        open_path(cx, window, path);
    }

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&a, cx));
    for path in [&b, &c] {
        let index = panel
            .read_with(cx, |panel, _| panel.tree().index_of(path))
            .expect("the row is in the tree");
        panel.update(cx, |panel, cx| panel.toggle_selection(index, cx));
    }
    focus_tree(cx, window);
    press(cx, window, "delete");
    draw(cx, window);
    // The selection is taken when the question opens: the file goes after that, so the
    // tree's reload cannot drop its row before the delete reaches it.
    fs::remove_file(&b).unwrap();
    press(cx, window, "enter");
    wait_for_signal(cx, "the first file to go", |_| !a.exists());
    cx.run_until_parked();
    draw(cx, window);

    let open = |cx: &TestAppContext, path: &std::path::Path| {
        window
            .read_with(cx, |workspace, _| workspace.view_of(path).is_some())
            .unwrap()
    };
    assert!(!open(cx, &a), "the tab of the file that went is closed");
    assert!(c.is_file(), "the file after the failure stays");
    assert!(open(cx, &c), "its tab stays open");
}
