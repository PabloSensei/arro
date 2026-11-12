//! Search Everywhere groups and `/` commands, the Switcher of Ctrl+Tab, the checkbox of
//! Recent Files and Show Content of a tool window, without a real window.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{ModifiersChangedEvent, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::{COMMIT_TOOL, Workspace};

use crate::palette::Palette;
use crate::window_tests::{
    active_path, bounds_of, click, draw, open_path, open_sample_window, press, type_text,
    wait_for_signal,
};
use crate::window_tests_ui::{double_shift, navigation_project, wait_for_symbols};

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

/// The index of a group header in the rows of the palette.
fn header(rows: &[(String, String)], title: &str) -> usize {
    rows.iter()
        .position(|(row, detail)| row == title && detail.is_empty())
        .unwrap_or_else(|| panic!("no group {title} in {rows:?}"))
}

#[ui_kit::gpui::test]
fn group_keys_walk_the_groups_of_search_everywhere(cx: &mut TestAppContext) {
    let root = navigation_project("navigation-groups");
    fs::write(root.join("src").join("Owner.txt"), "alpha\n").unwrap();
    let window = crate::window_tests_ui::open_test_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let sample = root.join("src").join("Sample.txt");
    wait_for_symbols(cx, window, &sample);

    // A query that finds a class and a file has both groups, in this order.
    double_shift(cx, window);
    type_text(cx, window, "Owner");
    wait_for_signal(cx, "the groups of Search Everywhere", |cx| {
        if !has_selector(cx, window, "palette") {
            return false;
        }
        let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
        rows.iter().any(|(row, _)| row == "Classes") && rows.iter().any(|(row, _)| row == "Files")
    });
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    let classes = header(&rows, "Classes");
    let files = header(&rows, "Files");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        classes + 1,
        "the first row of the first group is selected"
    );

    press(cx, window, "pagedown");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        files + 1,
        "PgDn goes to the first row of the next group"
    );
    press(cx, window, "pageup");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        classes + 1,
        "PgUp goes back"
    );
    press(cx, window, "ctrl-down");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        files + 1,
        "Ctrl+Down is the same key"
    );
    press(cx, window, "ctrl-up");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        classes + 1,
        "Ctrl+Up is the same key"
    );
    press(cx, window, "escape");

    // A group that finds nothing is not a step of the walk: the Files group is the only
    // one of this query, so PgDn keeps the first row where it is.
    double_shift(cx, window);
    type_text(cx, window, "notes");
    wait_for_signal(cx, "the single group", |cx| {
        !palette(cx, window)
            .read_with(cx, |palette, _| palette.rows_text())
            .is_empty()
    });
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    let files = header(&rows, "Files");
    assert!(
        !rows.iter().any(|(row, _)| row == "Classes"),
        "an empty group is not listed: {rows:?}"
    );
    let selected = palette(cx, window).read_with(cx, |palette, _| palette.selected_row());
    assert_eq!(selected, files + 1);
    press(cx, window, "pagedown");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        selected,
        "there is no next group"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn tab_completes_a_slash_command(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "complete-command");
    double_shift(cx, window);
    type_text(cx, window, "/ru");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert_eq!(rows[0].0, "/run", "the command list completes the name");
    press(cx, window, "tab");
    let text = palette(cx, window).read_with(cx, |palette, _| palette.query_field().text.clone());
    assert_eq!(text, "/run", "Tab completes the command in the query");
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn ctrl_tab_goes_back_at_once_and_walks_the_list_when_held(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-hold");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let readme = root.join("README.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);
    assert_eq!(active_path(cx, window), Some(readme.clone()));

    // A quick Ctrl+Tab opens the previous tab when Ctrl goes up.
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    assert!(has_selector(cx, window, "palette"));
    release_ctrl(cx, window);
    assert!(!has_selector(cx, window, "palette"), "the list closes");
    assert_eq!(active_path(cx, window), Some(notes.clone()));

    // While Ctrl is held, tabs and arrows walk the list; Ctrl opens the selected one.
    open_path(cx, window, &readme);
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        1,
        "the previous file is selected"
    );
    press(cx, window, "ctrl-down");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        2,
        "the next file of the list is selected"
    );
    press(cx, window, "ctrl-up");
    assert_eq!(
        palette(cx, window).read_with(cx, |palette, _| palette.selected_row()),
        1,
        "the arrow keys walk back"
    );
    press(cx, window, "ctrl-down");
    release_ctrl(cx, window);
    assert_eq!(active_path(cx, window), Some(main.clone()));
}

#[ui_kit::gpui::test]
fn the_switcher_lists_the_tool_windows(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "switcher-tools");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    let commit = rows
        .iter()
        .position(|(row, _)| row == "Commit")
        .expect("the tool windows are in the list");
    let selected = palette(cx, window).read_with(cx, |palette, _| palette.selected_row());
    for _ in selected..commit {
        press(cx, window, "ctrl-tab");
    }
    release_ctrl(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(COMMIT_TOOL))
            .unwrap(),
        "the selected tool window opens"
    );
}

#[ui_kit::gpui::test]
fn recent_files_shows_only_edited_files_with_the_checkbox(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "recent-edited");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    type_text(cx, window, "X");

    press(cx, window, "ctrl-e");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "palette-edited-only").size.height > px(0.),
        "Recent Files has the checkbox"
    );
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(rows.iter().any(|(row, _)| row == "main.txt"), "{rows:?}");
    assert!(rows.iter().any(|(row, _)| row == "notes.txt"), "{rows:?}");

    // Ctrl+E inside the list is the checkbox key.
    press(cx, window, "ctrl-e");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.iter().all(|(row, _)| row != "main.txt"),
        "only the file with unsaved text is left: {rows:?}"
    );
    assert!(rows.iter().any(|(row, _)| row == "notes.txt"), "{rows:?}");

    press(cx, window, "ctrl-e");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.iter().any(|(row, _)| row == "main.txt"),
        "the checkbox shows every recent file again: {rows:?}"
    );

    // A click on the box does the same.
    click(cx, window, "palette-edited-only");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.iter().all(|(row, _)| row != "main.txt"),
        "the click filters the list: {rows:?}"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn show_content_lists_the_tabs_of_the_tool_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "show-content");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "alt-down");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "menu-0").size.height > px(0.),
        "Alt+Down lists the tabs of the window in front"
    );
    let labels = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .unwrap_or_default()
                .iter()
                .map(|entry| entry.label.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(labels, vec!["Commit".to_owned(), "Shelf".to_owned()]);
    click(cx, window, "menu-1");
    draw(cx, window);

    let panel = cx.read(|cx| cx.global::<crate::script::ActiveCommitPanel>().0.clone());
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tab()),
        crate::commit::Tab::Shelf,
        "the row opens the tab"
    );
}
