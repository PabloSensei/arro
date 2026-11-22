//! Find Action lists the rows of the New menu of the project tree. A query finds a row by
//! the words of its title and of its group, and Enter opens the window of the row in the
//! folder of the active file.
//!
//! cargo test -p panels window_tests_find_action_new

use std::path::{Path, PathBuf};

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::px;
use workspace::Workspace;

use crate::palette::Palette;
use crate::tree_ops::{Creates, FileTemplate};
use crate::window_tests::{
    bounds_of, draw, open_palette, open_path, open_window_with, press, project_setup,
    sample_project, scratch, type_text, wait_for_signal,
};

/// The text of a new class of the language of the test.
fn class_text(_: &Path, _: &Path, name: &str) -> String {
    format!("class {name}\n")
}

/// The templates of a language without a server, as the application gives them to the
/// project tree. The label of the main kind belongs to the test alone.
fn templates() -> Vec<FileTemplate> {
    vec![
        FileTemplate {
            label: "Test Class",
            icon: "nodes/class",
            extension: "txt",
            creates: Creates::File,
            content: class_text,
        },
        FileTemplate {
            label: "Package",
            icon: "nodes/package",
            extension: "",
            creates: Creates::Folder { dotted: true },
            content: |_, _, _| String::new(),
        },
        FileTemplate {
            label: "package-info",
            icon: "fileTypes/text",
            extension: "txt",
            creates: Creates::File,
            content: |_, _, _| "package demo;\n".to_owned(),
        },
    ]
}

fn open_project(cx: &mut TestAppContext, name: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let mut setup = project_setup(scratch("find-action-new-cache"));
    setup.templates = templates();
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    (root, window)
}

/// The popup that Find Action opened.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

/// The rows of the New menu with the group label at their right edge and the icon at
/// their left.
fn new_rows(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Vec<(String, String, &'static str)> {
    palette(cx, window).read_with(cx, |palette, _| palette.new_rows_text())
}

fn close(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "escape");
    draw(cx, window);
}

fn search(cx: &mut TestAppContext, window: WindowHandle<Workspace>, query: &str) {
    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, query);
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn find_action_lists_the_rows_of_the_new_menu(cx: &mut TestAppContext) {
    let (_root, window) = open_project(cx, "find-action-new-list");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let rows = new_rows(cx, window);
    for (title, icon) in [
        ("Test Class", "nodes/class"),
        ("File", "fileTypes/text"),
        ("Scratch File", "general/newFile"),
        ("Directory", "nodes/folder"),
        ("Package", "nodes/package"),
        ("HTTP Request", "fileTypes/text"),
    ] {
        let found = rows
            .iter()
            .find(|(row, _, _)| row == title)
            .unwrap_or_else(|| panic!("the New menu has {title}: {rows:?}"));
        assert_eq!(
            found.1, "New",
            "the group stands at the right of the row: {rows:?}"
        );
        assert_eq!(found.2, icon, "the icon of {title}: {rows:?}");
    }
}

#[ui_kit::gpui::test]
fn a_new_row_is_found_by_its_title_and_by_the_group_word(cx: &mut TestAppContext) {
    let (_root, window) = open_project(cx, "find-action-new-words");
    for query in ["New Test Class", "new class", "class"] {
        search(cx, window, query);
        let rows = new_rows(cx, window);
        let found = rows
            .iter()
            .find(|(title, _, _)| title == "Test Class")
            .unwrap_or_else(|| panic!("{query} finds the class of the New menu: {rows:?}"));
        assert_eq!(
            found.1, "New",
            "the group stands at the right of the row: {rows:?}"
        );
        assert_eq!(
            found.2, "nodes/class",
            "the row has the icon of its template: {rows:?}"
        );
        close(cx, window);
    }
}

#[ui_kit::gpui::test]
fn directory_is_found_by_its_name(cx: &mut TestAppContext) {
    let (_root, window) = open_project(cx, "find-action-new-directory");
    search(cx, window, "directory");
    let rows = new_rows(cx, window);
    let found = rows
        .iter()
        .find(|(title, _, _)| title == "Directory")
        .unwrap_or_else(|| panic!("directory finds the New row: {rows:?}"));
    assert_eq!(
        found.1, "New",
        "the group stands at the right of the row: {rows:?}"
    );
    assert_eq!(
        found.2, "nodes/folder",
        "the row has the icon of the template: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn enter_creates_in_the_folder_of_the_active_file(cx: &mut TestAppContext) {
    let (root, window) = open_project(cx, "find-action-new-folder");
    let active = root.join("src").join("main.txt");
    open_path(cx, window, &active);
    search(cx, window, "New Test Class");

    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "prompt").size.height > px(0.),
        "Enter opens the window of the row"
    );

    type_text(cx, window, "Order");
    press(cx, window, "enter");
    draw(cx, window);
    let made = root.join("src").join("Order.txt");
    wait_for_signal(cx, "the new class in the folder of the active file", |_| {
        made.is_file()
    });
}
