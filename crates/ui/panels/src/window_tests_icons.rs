//! The icons of the tool windows and the menus: the buttons of the Git Log and of the
//! Database toolbar show an icon and name themselves in a tooltip, a file group of Find in
//! Files has the icon of its file type, and the rows of the Run menu show their icons in
//! places of one width. The icons are found by the file they are drawn from.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ICON;
use ui_kit::{Theme, icon_file};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::DatabasePanel;
use crate::window_tests::{
    bounds_of, click, draw, open_sample_window, open_window, press, sample_project, scratch,
    type_text, wait_for_file_index, wait_for_settled_bounds,
};
use crate::window_tests_git::{git_state, has_selector, wait_for, wait_for_repository};
use crate::window_tests_git_log_icons::{open_window_with_icons, repository};
use crate::window_tests_git_window::open_log;
use crate::window_tests_tooltips::assert_tooltip;

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

/// The icon `icon` is drawn inside one of the `places` that show it. A window can draw the
/// same icon in several controls, and the frame keeps one box per icon, so the box is
/// checked against every place that shows the icon.
fn icon_in(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    places: &[&'static str],
    icon: &'static str,
) {
    let mut boxes = Vec::new();
    for &place in places {
        if has_selector(cx, window, place) {
            boxes.push((place, bounds_of(cx, window, place)));
        }
    }
    assert!(!boxes.is_empty(), "none of {places:?} is on screen");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let drawn = visual
        .debug_bounds(icon)
        .unwrap_or_else(|| panic!("the window draws no {icon}"));
    assert!(
        boxes
            .iter()
            .any(|(_, bounds)| bounds.contains(&drawn.center())),
        "the icon {icon} is outside {boxes:?}: {drawn:?}"
    );
}

/// One button of a toolbar: the selector of the button, the icon it shows, the name and the
/// key its tooltip shows, and the controls of the window that show the same icon.
type IconButton<'a> = (
    &'static str,
    &'static str,
    &'a str,
    Option<&'a str>,
    &'static [&'static str],
);

/// The buttons of a toolbar: each one shows its icon and names itself in its tooltip. The
/// controls of the window that show the same icon are listed after the button. The key of
/// each button is the one its tooltip shows, or none.
fn assert_icon_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    buttons: &[IconButton<'_>],
) {
    for &(button, icon, name, key, shared) in buttons {
        let mut places = vec![button];
        places.extend_from_slice(shared);
        icon_in(cx, window, &places, icon_path(cx, icon));
        assert_tooltip(cx, window, button, name, key);
    }
}

#[ui_kit::gpui::test]
fn the_git_log_buttons_are_icons_that_name_themselves(cx: &mut TestAppContext) {
    let root = repository("icons-git-log-buttons");
    let window = open_window_with_icons(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-commit-0");
    click(cx, window, "log-commit-0");
    wait_for(cx, "the file bar of the commit", |cx| {
        has_selector(cx, window, "log-files-diff")
    });
    draw(cx, window);

    assert_icon_buttons(
        cx,
        window,
        &[
            (
                "log-refresh",
                "general/refresh",
                "Refresh",
                None,
                &["log-action-update"],
            ),
            (
                "log-cherry-pick",
                "general/cherryPick",
                "Cherry-pick",
                None,
                &[],
            ),
            (
                "log-files-diff",
                "general/diff",
                "Diff",
                None,
                &["log-action-compare"],
            ),
            ("log-files-revert", "general/rollback", "Revert", None, &[]),
            (
                "log-files-expand",
                "general/expandAll",
                "Expand",
                None,
                &["project-expand-all"],
            ),
            (
                "log-files-collapse",
                "general/collapseAll",
                "Collapse",
                None,
                &["project-collapse-all"],
            ),
        ],
    );
}

#[ui_kit::gpui::test]
fn the_database_copy_buttons_are_icons_that_name_themselves(cx: &mut TestAppContext) {
    let root = sample_project("icons-database-copy");
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let copy = DatabasePanel::copy_cell_key();

    assert_icon_buttons(
        cx,
        window,
        &[
            (
                "database-copy-cell",
                "general/copy",
                "Copy Cell",
                Some(copy.as_str()),
                &["database-copy-row"],
            ),
            (
                "database-copy-row",
                "general/copy",
                "Copy Row",
                None,
                &["database-copy-cell"],
            ),
        ],
    );
}

#[ui_kit::gpui::test]
fn a_file_group_of_find_in_files_has_the_icon_of_its_type(cx: &mut TestAppContext) {
    let root = scratch("icons-find-in-files");
    fs::create_dir_all(root.join("notes")).unwrap();
    fs::write(root.join("notes").join("app.xml"), "<app>alpha</app>\n").unwrap();
    let window = open_window_with_icons(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    type_text(cx, window, "alpha");
    wait_for(cx, "the file group of the hit", |cx| {
        has_selector(cx, window, "find-file-0")
    });
    draw(cx, window);

    // The icon of the file is the one the application gives its type, not a fixed one.
    let icon = window
        .read_with(cx, |workspace, _| {
            workspace
                .icons()
                .icon_for(&root.join("notes").join("app.xml"))
        })
        .unwrap();
    assert_eq!(icon, "fileTypes/xml", "the window maps .xml to its icon");
    icon_in(cx, window, &["find-file-0"], icon_path(cx, icon));
}

#[ui_kit::gpui::test]
fn the_rows_of_the_run_menu_have_icons_in_places_of_one_width(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "icons-run-menu");
    let sections = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let index = sections
        .iter()
        .position(|section| section.label.as_ref() == "Run")
        .expect("the menu bar has a Run section");
    let run = sections[index].submenu.clone();
    click(cx, window, selector(format!("menu-bar-{index}")));
    draw(cx, window);

    let mut widths = Vec::new();
    for (row, entry) in run.iter().enumerate() {
        if entry.label.is_empty() {
            continue;
        }
        let place = selector(format!("menu-icon-{row}"));
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        let width = visual
            .debug_bounds(place)
            .unwrap_or_else(|| panic!("the row {} has no place for its icon", entry.label))
            .size
            .width;
        drop(visual);
        widths.push(width);
        // The debugger icons are drawn by no other control of this window, so a row that
        // shows one is found by its own box.
        if let Some(name) = entry.icon.filter(|name| name.starts_with("debug/")) {
            icon_in(
                cx,
                window,
                &[selector(format!("menu-{row}"))],
                icon_path(cx, name),
            );
        }
    }
    assert!(widths.len() > 3, "the Run menu has rows");
    assert!(
        widths.iter().all(|width| *width == widths[0]),
        "every row keeps a place of one width for its icon: {widths:?}"
    );
    for label in ["Run", "Debug", "Stop", "Step Over", "Toggle Breakpoint"] {
        let entry = run
            .iter()
            .find(|entry| entry.label.as_ref() == label)
            .unwrap_or_else(|| panic!("the Run menu has {label}"));
        assert!(entry.icon.is_some(), "{label} has an icon");
    }
}
