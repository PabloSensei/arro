//! The Switcher of Ctrl+Tab in two columns: the tool windows on the left, the open files on the
//! right. Left and Right move between the columns, Tab and the arrow keys walk the column of
//! the selection, Enter and releasing Ctrl open the selected row, typing filters both columns
//! and shows the query line, and the popup is as tall as its taller column.
//!
//! `cargo test -p panels window_tests_switcher_columns`

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ROW_HEIGHT;
use ui_kit::{Modifiers, px};
use workspace::{COMMIT_TOOL, PROJECT_TOOL, Workspace};

use crate::palette::Palette;
use crate::window_tests::{
    active_path, bounds_of, draw, open_path, open_sample_window, open_window, press,
    sample_project, type_text,
};

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The Ctrl key up of the platform: the modifiers go back to none.
fn release_ctrl(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ModifiersChangedEvent {
        modifiers: Modifiers::default(),
        ..ModifiersChangedEvent::default()
    });
    cx.run_until_parked();
}

/// The rows of the palette on screen, by title and detail, and the index of the selected one.
fn rows_and_selected(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> (Vec<(String, String)>, usize) {
    palette(cx, window).read_with(cx, |palette, _| {
        (palette.rows_text(), palette.selected_row())
    })
}

/// The tool window the selected row opens, `None` when the selection is on a file.
fn selected_tool(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<&'static str> {
    palette(cx, window).read_with(cx, |palette, _| palette.selected_tool())
}

fn tool_open(cx: &TestAppContext, window: WindowHandle<Workspace>, tool: &str) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(tool))
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_tools_stand_left_and_the_files_right_with_the_previous_file_selected(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "switcher-columns-draw");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_path(cx, window, &root.join("src").join("notes.txt"));

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, selected) = rows_and_selected(cx, window);
    assert_eq!(
        rows[0].0, "notes.txt",
        "the file on screen comes first: {rows:?}"
    );
    assert_eq!(rows[1].0, "main.txt", "{rows:?}");
    assert_eq!(
        rows[2].0, "Project",
        "the tool windows follow the files: {rows:?}"
    );
    assert_eq!(selected, 1, "the previous file is selected");

    let tools = bounds_of(cx, window, "palette-tools-column");
    let files = bounds_of(cx, window, "palette-files-column");
    assert!(
        tools.right() <= files.left(),
        "the tool windows are left of the files: {tools:?} {files:?}"
    );
    let project = bounds_of(cx, window, "palette-row-2");
    let main = bounds_of(cx, window, "palette-row-1");
    assert!(
        project.left() >= tools.left() && project.right() <= tools.right(),
        "Project stands in the left column: {project:?}"
    );
    assert!(
        main.left() >= files.left() && main.right() <= files.right(),
        "main.txt stands in the right column: {main:?}"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn left_and_right_move_the_selection_between_the_columns(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-move");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_path(cx, window, &root.join("src").join("notes.txt"));
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    assert_eq!(
        selected_tool(cx, window),
        None,
        "the previous file is selected"
    );

    // Left takes the tool window at the same place of its column: the second one, Commit.
    press(cx, window, "left");
    assert_eq!(
        selected_tool(cx, window),
        Some(COMMIT_TOOL),
        "Left goes to the tool windows"
    );
    press(cx, window, "left");
    assert_eq!(
        selected_tool(cx, window),
        Some(COMMIT_TOOL),
        "Left in the tool windows changes nothing"
    );

    press(cx, window, "right");
    assert_eq!(
        selected_tool(cx, window),
        None,
        "Right goes back to the files"
    );
    assert_eq!(
        rows_and_selected(cx, window).1,
        1,
        "on the same file as before"
    );

    press(cx, window, "ctrl-left");
    assert_eq!(
        selected_tool(cx, window),
        Some(COMMIT_TOOL),
        "Ctrl+Left switches too"
    );
    press(cx, window, "ctrl-right");
    assert_eq!(selected_tool(cx, window), None, "Ctrl+Right switches back");
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn tab_and_the_arrows_stay_in_their_column_and_enter_opens_the_tool_window(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_sample_window(cx, "switcher-columns-column");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_path(cx, window, &root.join("src").join("notes.txt"));
    press(cx, window, "ctrl-tab");
    draw(cx, window);

    // Tab walks the files and wraps from the last file to the first.
    press(cx, window, "tab");
    assert_eq!(selected_tool(cx, window), None, "Tab stays with the files");
    assert_eq!(
        rows_and_selected(cx, window).1,
        0,
        "Tab wraps to the file on screen"
    );
    press(cx, window, "tab");
    assert_eq!(rows_and_selected(cx, window).1, 1, "the next file");

    // In the tool windows the arrows walk the column and stop at its ends.
    press(cx, window, "left");
    for _ in 0..30 {
        press(cx, window, "up");
    }
    assert_eq!(
        selected_tool(cx, window),
        Some(PROJECT_TOOL),
        "Up stops at the first tool window"
    );
    for _ in 0..30 {
        press(cx, window, "down");
    }
    assert!(
        selected_tool(cx, window).is_some(),
        "Down stops at the last tool window, which is still a tool window"
    );
    for _ in 0..30 {
        press(cx, window, "up");
    }
    press(cx, window, "down");
    assert_eq!(
        selected_tool(cx, window),
        Some(COMMIT_TOOL),
        "one step down is Commit"
    );

    assert!(
        !tool_open(cx, window, COMMIT_TOOL),
        "Commit is closed before Enter"
    );
    press(cx, window, "enter");
    assert!(
        tool_open(cx, window, COMMIT_TOOL),
        "Enter opens the selected tool window"
    );
}

#[ui_kit::gpui::test]
fn typing_filters_both_columns_and_the_query_line_shows_only_while_typing(cx: &mut TestAppContext) {
    let root = sample_project("switcher-columns-typing");
    fs::write(root.join("src").join("Owner.txt"), "owner\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    for file in ["src/Owner.txt", "src/main.txt", "src/notes.txt"] {
        open_path(cx, window, &root.join(file));
    }

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "palette-input"),
        "no query line before anything is typed"
    );

    type_text(cx, window, "own");
    draw(cx, window);
    assert!(
        has_selector(cx, window, "palette-input"),
        "the query line appears once typing starts"
    );
    let (rows, _) = rows_and_selected(cx, window);
    assert!(
        rows.iter().any(|(row, _)| row == "Owner.txt"),
        "the file that matches stays: {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|(row, _)| row != "main.txt" && row != "notes.txt"),
        "the other files are filtered out: {rows:?}"
    );
    assert!(
        rows.iter().all(|(row, _)| row != "Project"),
        "the tool windows are filtered too: {rows:?}"
    );
    let text = palette(cx, window).read_with(cx, |palette, _| palette.query_field().text.clone());
    assert_eq!(text, "own", "the query holds what was typed");

    for _ in 0..3 {
        press(cx, window, "backspace");
    }
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "palette-input"),
        "an empty query has no query line"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn releasing_ctrl_opens_the_selected_file(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-release");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);

    // The list is readme, notes, main: Ctrl+Tab selects notes, a second one selects main.
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    press(cx, window, "ctrl-tab");
    assert_eq!(
        rows_and_selected(cx, window).1,
        2,
        "the third file is selected"
    );
    release_ctrl(cx, window);
    assert!(!has_selector(cx, window, "palette"), "the list closes");
    assert_eq!(
        active_path(cx, window),
        Some(main),
        "the selected file opens"
    );
}

#[ui_kit::gpui::test]
fn each_tool_window_row_shows_the_key_that_opens_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-keys");
    open_path(cx, window, &root.join("src").join("main.txt"));
    press(cx, window, "ctrl-tab");
    draw(cx, window);

    let shortcuts = palette(cx, window).read_with(cx, |palette, _| palette.tool_shortcuts());
    let project = shortcuts
        .iter()
        .find(|(title, _)| title == "Project")
        .expect("the Project window is listed");
    assert!(project.1.is_some(), "Project has its key: {shortcuts:?}");
    #[cfg(windows)]
    assert_eq!(
        project.1.as_deref(),
        Some("Alt+1"),
        "the key as the menus spell it"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn the_popup_is_as_tall_as_its_taller_column(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-height");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_path(cx, window, &root.join("src").join("notes.txt"));
    press(cx, window, "ctrl-tab");
    draw(cx, window);

    // Two files and eighteen tool windows: the tool windows set the height, which is below
    // the limit of the popup, so the last tool window is on screen.
    let (rows, _) = rows_and_selected(cx, window);
    assert_eq!(
        rows[19].0, "Build",
        "the last tool window is the nineteenth row: {rows:?}"
    );
    let popup = bounds_of(cx, window, "palette");
    assert!(
        popup.size.height >= px(ROW_HEIGHT * 18.),
        "the popup holds the tool windows: {popup:?}"
    );
    assert!(
        popup.size.height < px(600.),
        "the popup stops at its limit: {popup:?}"
    );
    let build = bounds_of(cx, window, "palette-row-19");
    assert!(
        build.bottom() <= popup.bottom(),
        "the last tool window is inside the popup: {build:?} {popup:?}"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn a_long_path_in_the_files_column_is_cut_inside_its_row(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-long-path");
    let mut deep = root.clone();
    for depth in 1..=10 {
        deep = deep.join(format!("folder{depth:02}"));
    }
    fs::create_dir_all(&deep).unwrap();
    let file = deep.join("Deep.txt");
    fs::write(&file, "deep\n").unwrap();
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_path(cx, window, &file);

    // The file on screen is the first row of the Switcher.
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, _) = rows_and_selected(cx, window);
    assert_eq!(rows[0].0, "Deep.txt", "{rows:?}");
    let row = bounds_of(cx, window, "palette-row-0");
    let detail = bounds_of(cx, window, "palette-detail-0");
    assert!(
        detail.right() <= row.right(),
        "the path stays inside its row: {detail:?} {row:?}"
    );
    assert!(
        detail.size.height <= px(ROW_HEIGHT),
        "the path is one line: {detail:?}"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn a_long_file_list_scrolls_inside_its_column_under_the_limit(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-columns-scroll");
    for index in 0..30 {
        let file = root.join("src").join(format!("list{index:02}.txt"));
        fs::write(&file, "list\n").unwrap();
        open_path(cx, window, &file);
    }
    press(cx, window, "ctrl-tab");
    draw(cx, window);

    // Thirty files do not fit under the limit: the popup stops at it and the list scrolls.
    let popup = bounds_of(cx, window, "palette");
    assert!(
        popup.size.height <= px(600.),
        "the popup stops at its limit: {popup:?}"
    );
    let list = bounds_of(cx, window, "scroll-area-palette-list");
    assert!(
        list.bottom() <= popup.bottom(),
        "the list ends inside the popup: {list:?} {popup:?}"
    );
    assert!(
        list.size.height < px(ROW_HEIGHT * 30.),
        "the list is shorter than its rows: {list:?}"
    );

    // The selection walks past the bottom of the list, and the row it reaches comes on screen.
    for _ in 0..25 {
        press(cx, window, "down");
    }
    draw(cx, window);
    let selected = palette(cx, window).read_with(cx, |palette, _| palette.selected_row());
    assert_eq!(selected, 26, "the previous file moved down 25 rows");
    let row = bounds_of(cx, window, "palette-row-26");
    let list = bounds_of(cx, window, "scroll-area-palette-list");
    assert!(
        row.top() >= list.top() && row.bottom() <= list.bottom(),
        "the selected file is on screen: {row:?} {list:?}"
    );
    press(cx, window, "escape");
}
