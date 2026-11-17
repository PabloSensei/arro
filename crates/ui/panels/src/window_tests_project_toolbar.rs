//! The toolbar of the Project window, Select Opened File with its switch Always Select Opened
//! File, and the tab strip of a window with many tabs: a tab that opens or is switched to is
//! brought into view.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Entity, Pixels, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::project::{ActiveProjectPanel, ProjectPanel};
use crate::window_tests::{
    click, draw, open_path, open_window, press, scratch, wait_for_project_tree, wait_for_signal,
};
use crate::window_tests_menus::click_menu_row;

/// A project with a folder chain five levels deep down to one file, and a file at the top.
/// Returns the root and the deep file.
fn deep_project(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let deep = root.join("deep").join("a").join("b").join("c").join("d");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("Deep.txt"), "deep\n").unwrap();
    fs::write(root.join("top.txt"), "top\n").unwrap();
    (root, deep.join("Deep.txt"))
}

/// A project of `count` text files in its root, one tab each. Returns the root and the files.
fn tab_files(name: &str, count: usize) -> (PathBuf, Vec<PathBuf>) {
    let root = scratch(name);
    fs::create_dir_all(&root).unwrap();
    let files = (0..count)
        .map(|index| {
            let path = root.join(format!("tab{index:02}.txt"));
            fs::write(&path, format!("tab {index}\n")).unwrap();
            path
        })
        .collect();
    (root, files)
}

/// The window of a project, drawn once with its tree read.
fn project_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_tree(cx);
    window
}

fn project_panel(cx: &TestAppContext) -> Entity<ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

/// The names of the rows the tree shows, in order.
fn row_names(cx: &TestAppContext, panel: &Entity<ProjectPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .tree()
            .rows()
            .iter()
            .map(|row| row.name.clone())
            .collect()
    })
}

/// The file in which the window keeps its settings, written in the background.
fn settings_file(cx: &TestAppContext, window: WindowHandle<Workspace>) -> PathBuf {
    window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join("settings.json")
        })
        .unwrap()
}

/// The place of an element on screen, if the window draws it.
fn rect_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Option<Bounds<Pixels>> {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector)
}

/// The place of an element that must be on screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Bounds<Pixels> {
    rect_of(cx, window, selector).unwrap_or_else(|| panic!("{selector} is not on screen"))
}

/// Whether a tab lies inside the visible part of its strip, from edge to edge.
fn inside(tab: Bounds<Pixels>, strip: Bounds<Pixels>) -> bool {
    tab.left() >= strip.left() && tab.right() <= strip.right()
}

#[ui_kit::gpui::test]
fn select_opened_file_expands_the_tree_to_a_deep_file(cx: &mut TestAppContext) {
    let (root, deep) = deep_project("select-opened-file");
    let window = project_window(cx, root.clone());
    let panel = project_panel(cx);
    open_path(cx, window, &deep);

    click(cx, window, "project-collapse-all");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep)),
        None,
        "the deep file is hidden in a closed tree"
    );

    click(cx, window, "project-select-opened");
    wait_for_signal(cx, "the opened file in the tree", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.selected_path() == Some(deep.as_path()) && panel.tree().index_of(&deep).is_some()
        })
    });
    let expanded = panel.read_with(cx, |panel, _| panel.tree().expanded());
    for folder in ["deep", "deep/a", "deep/a/b", "deep/a/b/c", "deep/a/b/c/d"] {
        assert!(
            expanded.contains(&root.join(folder)),
            "{folder} is open on the way to the file"
        );
    }
}

#[ui_kit::gpui::test]
fn expand_all_shows_a_file_four_folders_down_after_one_click(cx: &mut TestAppContext) {
    let (root, deep) = deep_project("expand-all-one-click");
    let window = project_window(cx, root.clone());
    let panel = project_panel(cx);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep)),
        None,
        "the deep file is hidden in a closed tree"
    );

    // One click opens every folder on the way: no second click is needed.
    click(cx, window, "project-expand-all");
    wait_for_signal(cx, "the deep file in the tree", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep).is_some())
    });
    let expanded = panel.read_with(cx, |panel, _| panel.tree().expanded());
    for folder in ["deep", "deep/a", "deep/a/b", "deep/a/b/c", "deep/a/b/c/d"] {
        assert!(
            expanded.contains(&root.join(folder)),
            "{folder} is open after one Expand All"
        );
    }
}

#[ui_kit::gpui::test]
fn collapse_all_leaves_only_the_root_open(cx: &mut TestAppContext) {
    let (root, deep) = deep_project("collapse-all-root");
    let window = project_window(cx, root.clone());
    let panel = project_panel(cx);
    let closed = row_names(cx, &panel);

    click(cx, window, "project-expand-all");
    wait_for_signal(cx, "the deep file in the tree", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep).is_some())
    });

    click(cx, window, "project-collapse-all");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().expanded()),
        vec![root.clone()],
        "only the root stays open"
    );
    assert_eq!(
        row_names(cx, &panel),
        closed,
        "the tree shows the root and its children again"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep)),
        None,
        "the deep file is hidden again"
    );
}

#[ui_kit::gpui::test]
fn alt_f1_selects_the_open_file_in_the_tree(cx: &mut TestAppContext) {
    let (root, deep) = deep_project("select-opened-alt-f1");
    let window = project_window(cx, root.clone());
    let panel = project_panel(cx);
    open_path(cx, window, &deep);
    click(cx, window, "project-collapse-all");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep)),
        None,
        "the file is hidden before Alt+F1"
    );

    // The key that the tooltip of Select Opened File shows.
    press(cx, window, "alt-f1");
    wait_for_signal(cx, "the open file selected by Alt+F1", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.selected_path() == Some(deep.as_path()) && panel.tree().index_of(&deep).is_some()
        })
    });
}

#[test]
fn the_select_opened_file_hint_names_the_key_of_the_window_table() {
    assert_eq!(
        workspace::window_key("workspace::SelectInProject", keymap::Platform::Windows),
        Some("alt-f1"),
        "the window binds Alt+F1"
    );
    assert_eq!(
        keymap::shortcut_label("alt-f1", keymap::Platform::Windows),
        "Alt+F1"
    );
}

#[ui_kit::gpui::test]
fn always_select_opened_file_is_a_switch_saved_in_the_settings_file(cx: &mut TestAppContext) {
    let (root, _) = deep_project("always-select-saved");
    let window = project_window(cx, root);
    let file = settings_file(cx, window);

    click(cx, window, "project-view-options");
    click_menu_row(cx, window, "Always Select Opened File");
    wait_for_signal(cx, "the switch written to the settings file", |_| {
        Settings::load(&file).project_always_select_opened_file
    });
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("\"project_always_select_opened_file\""),
        "the settings file names the switch"
    );

    click(cx, window, "project-view-options");
    click_menu_row(cx, window, "✓ Always Select Opened File");
    wait_for_signal(cx, "the switch off in the settings file", |_| {
        !Settings::load(&file).project_always_select_opened_file
    });
}

#[ui_kit::gpui::test]
fn always_select_opened_file_follows_the_tabs_once_it_is_on(cx: &mut TestAppContext) {
    let (root, deep) = deep_project("always-select-opened-file");
    let top = root.join("top.txt");
    let window = project_window(cx, root.clone());
    let panel = project_panel(cx);

    // Off by default: a tab that becomes active does not move the selection.
    open_path(cx, window, &deep);
    assert_ne!(
        panel.read_with(cx, |panel, _| panel.selected_path().map(Path::to_path_buf)),
        Some(deep.clone()),
        "the tree does not follow the editor by default"
    );

    // Switching it on selects the file of the active tab at once.
    click(cx, window, "project-view-options");
    click_menu_row(cx, window, "Always Select Opened File");
    wait_for_signal(cx, "the file of the active tab selected", |cx| {
        panel.read_with(cx, |panel, _| panel.selected_path() == Some(deep.as_path()))
    });

    // Every later tab that becomes active is selected in the tree.
    open_path(cx, window, &top);
    wait_for_signal(cx, "the file of the new tab selected", |cx| {
        panel.read_with(cx, |panel, _| panel.selected_path() == Some(top.as_path()))
    });
}

#[ui_kit::gpui::test]
fn a_new_tab_is_in_view_when_thirty_tabs_are_open(cx: &mut TestAppContext) {
    let (root, files) = tab_files("tabs-new-in-view", 31);
    let window = project_window(cx, root);
    for file in &files[..30] {
        open_path(cx, window, file);
    }
    open_path(cx, window, &files[30]);

    let strip = on_screen(cx, window, "tabs-0");
    let tab = on_screen(cx, window, "tab-0-30");
    assert!(
        inside(tab, strip),
        "the tab that opened last is inside the strip: tab {tab:?}, strip {strip:?}"
    );
}

#[ui_kit::gpui::test]
fn switching_to_the_first_tab_from_the_keyboard_scrolls_the_strip_back(cx: &mut TestAppContext) {
    let (root, files) = tab_files("tabs-keyboard-back", 30);
    let window = project_window(cx, root);
    for file in &files {
        open_path(cx, window, file);
    }
    // The last tab is the one in view, so the first one has been scrolled out.
    let strip = on_screen(cx, window, "tabs-0");
    let first = rect_of(cx, window, "tab-0-0");
    assert!(
        first.is_none_or(|first| first.left() < strip.left()),
        "the first tab is out of view before the keyboard switches to it"
    );

    for _ in 0..29 {
        press(cx, window, "alt-left");
    }
    draw(cx, window);
    let strip = on_screen(cx, window, "tabs-0");
    let first = on_screen(cx, window, "tab-0-0");
    assert!(inside(first, strip), "the first tab is in view again");
    assert!(
        (f32::from(first.left()) - f32::from(strip.left())).abs() < 0.5,
        "the strip is back at its start: tab {first:?}, strip {strip:?}"
    );
}
