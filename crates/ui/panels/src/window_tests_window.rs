//! The window actions of T-0090 without a real window: the tabs, the tool windows,
//! the scale, the file path, the navigation bar, scratch files and the project tree.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, Theme};
use workspace::{
    DockSide, MAX_FONT_SIZE, MIN_UI_SCALE, PROJECT_TOOL, RUN_TOOL, Settings, USAGES_TOOL, Workspace,
};

use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    active_path, bounds_of, draw, editor_of, open_palette, open_path, open_sample_window, press,
    scratch, type_text,
};
use crate::window_tests_ui::{wait_for, wait_for_files};

fn workspace_settings(cx: &TestAppContext) -> Settings {
    cx.read(|cx| cx.global::<Settings>().clone())
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn focus_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    crate::window_tests_menus::show_project(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    window
        .update(cx, |_, window, cx| {
            window.focus(&panel.read(cx).focus(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn project_panel(cx: &TestAppContext) -> gpui::Entity<crate::project::ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

fn click_with(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    modifiers: Modifiers,
) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), modifiers);
    cx.run_until_parked();
}

fn selected_paths(cx: &TestAppContext) -> Vec<PathBuf> {
    let panel = project_panel(cx);
    cx.read(|cx| panel.read(cx).selected_paths())
}

fn clipboard_text(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

#[ui_kit::gpui::test]
fn alt_shift_arrows_walk_the_editor_tabs(cx: &mut TestAppContext) {
    let root = scratch("window-tabs");
    fs::create_dir_all(root.join("src")).unwrap();
    let first = root.join("src").join("first.txt");
    let second = root.join("src").join("second.txt");
    fs::write(&first, "first\n").unwrap();
    fs::write(&second, "second\n").unwrap();
    let window = crate::window_tests::open_window(cx, root);
    draw(cx, window);
    open_path(cx, window, &first);
    open_path(cx, window, &second);
    assert_eq!(active_path(cx, window), Some(second.clone()));

    press(cx, window, "alt-shift-right");
    assert_eq!(active_path(cx, window), Some(first.clone()));

    press(cx, window, "alt-shift-left");
    assert_eq!(active_path(cx, window), Some(second.clone()));
}

#[ui_kit::gpui::test]
fn shift_enter_in_the_tree_opens_the_file_in_a_split_on_the_right(cx: &mut TestAppContext) {
    let root = scratch("window-split");
    fs::create_dir_all(root.join("src")).unwrap();
    let main = root.join("src").join("main.txt");
    fs::write(&main, "main\n").unwrap();
    let window = crate::window_tests::open_window(cx, root.clone());
    draw(cx, window);

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&main, cx));
    focus_project(cx, window);
    draw(cx, window);
    press(cx, window, "shift-enter");

    let groups = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(groups.len(), 2, "Shift+Enter makes a second group");
    assert!(
        groups.iter().any(|tabs| tabs.contains(&main)),
        "the file is in the new group"
    );
    assert_eq!(active_path(cx, window), Some(main));
}

#[ui_kit::gpui::test]
fn shift_enter_in_go_to_file_opens_the_file_in_a_right_split(cx: &mut TestAppContext) {
    let root = scratch("window-split-palette");
    fs::create_dir_all(root.join("src")).unwrap();
    let alpha = root.join("src").join("alpha.txt");
    fs::write(&alpha, "alpha\n").unwrap();
    let window = crate::window_tests::open_window(cx, root);
    draw(cx, window);
    wait_for_files(cx, window, &alpha);
    // The file is open after the index check: close it, so Shift+Enter makes the split.
    press(cx, window, "ctrl-f4");

    open_palette(cx, window, keys::GOTO_FILE);
    type_text(cx, window, "alpha");
    draw(cx, window);
    press(cx, window, "shift-enter");

    let groups = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(groups.len(), 2, "Shift+Enter in Go to File makes a split");
    assert_eq!(active_path(cx, window), Some(alpha));
}

#[ui_kit::gpui::test]
fn ctrl_shift_f4_closes_the_active_tool_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-close-tab");
    press(cx, window, "alt-4");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap()
    );

    press(cx, window, "ctrl-shift-f4");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap(),
        "CloseActiveTab hides the tool window"
    );
}

#[ui_kit::gpui::test]
fn hide_all_windows_maximize_jump_and_default_layout(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-tools");
    press(cx, window, "alt-4");
    press(cx, window, "alt-3");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
    );

    press(cx, window, "ctrl-shift-f12");
    let hidden = window
        .read_with(cx, |workspace, _| {
            (
                workspace.all_windows_hidden(),
                workspace.dock_open(DockSide::Left),
                workspace.dock_open(DockSide::Bottom),
                workspace.dock_open(DockSide::Right),
            )
        })
        .unwrap();
    assert_eq!(hidden, (true, false, false, false));

    press(cx, window, "ctrl-shift-f12");
    let shown = window
        .read_with(cx, |workspace, _| {
            (
                workspace.all_windows_hidden(),
                workspace.dock_open(DockSide::Left),
                workspace.dock_open(DockSide::Bottom),
            )
        })
        .unwrap();
    assert_eq!(shown, (false, true, true));

    press(cx, window, "alt-4");
    press(cx, window, "ctrl-shift-'");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.maximized_tool_window())
            .unwrap(),
        Some(DockSide::Bottom)
    );
    press(cx, window, "ctrl-shift-'");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.maximized_tool_window())
            .unwrap(),
        None
    );

    // F12 goes to the tool window that had the focus last; the editor gets it back
    // from Escape without closing the tool window.
    press(cx, window, "f12");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap()
    );

    press(cx, window, "shift-f12");
    let layout = window
        .read_with(cx, |workspace, _| {
            (
                workspace.dock_open(DockSide::Left),
                workspace.dock_open(DockSide::Right),
                workspace.dock_open(DockSide::Bottom),
            )
        })
        .unwrap();
    assert_eq!(layout, (true, false, false));
}

#[ui_kit::gpui::test]
fn escape_in_a_tool_window_returns_the_focus_to_the_editor(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-escape");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    press(cx, window, "alt-4");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap()
    );

    press(cx, window, "escape");
    let (open, focused) = window
        .update(cx, |workspace, window, cx| {
            (
                workspace.tool_open(RUN_TOOL),
                workspace.editor_has_focus(window, cx),
            )
        })
        .unwrap();
    assert!(open, "Escape leaves the tool window open");
    assert!(focused, "Escape gives the editor the focus");
}

#[ui_kit::gpui::test]
fn zoom_and_editor_font_are_kept_in_settings(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-zoom");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    let before = view.read_with(cx, |view, _| view.font_size());

    press(cx, window, "alt-shift-=");
    let scale = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!((scale - 1.1).abs() < 0.001, "Zoom In Arro raises the scale");
    assert_eq!(workspace_settings(cx).ui_scale, scale);

    press(cx, window, "alt-shift-0");
    assert!(
        (window
            .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
            .unwrap()
            - 1.)
            .abs()
            < 0.001
    );

    press(cx, window, "alt-shift-.");
    let after = view.read_with(cx, |view, _| view.font_size());
    assert!(after > before, "the editor font grows");
    assert_eq!(workspace_settings(cx).font_size, after);

    press(cx, window, "alt-shift-,");
    assert!((view.read_with(cx, |view, _| view.font_size()) - before).abs() < 0.001);
    let settings = workspace_settings(cx);
    assert!(settings.font_size <= MAX_FONT_SIZE && settings.ui_scale >= MIN_UI_SCALE);
}

#[ui_kit::gpui::test]
fn file_path_popup_reveals_the_folder_and_the_navigation_bar_moves(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-path");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    ui_kit::clear_revealed_paths();

    press(cx, window, "ctrl-alt-f12");
    draw(cx, window);
    assert!(has_selector(cx, window, "file-path-popup"));
    press(cx, window, "enter");
    let revealed = ui_kit::revealed_paths();
    assert!(
        revealed.iter().any(|path| path == &root.join("src")),
        "Enter reveals the folder of the file: {revealed:?}"
    );
    assert!(!has_selector(cx, window, "file-path-popup"));

    ui_kit::clear_revealed_paths();
    press(cx, window, "alt-home");
    draw(cx, window);
    assert!(has_selector(cx, window, "nav-bar-popup"));
    // Walk back from the file to a folder and open that folder.
    press(cx, window, "left");
    press(cx, window, "enter");
    let revealed = ui_kit::revealed_paths();
    assert!(
        revealed.iter().any(|path| path.is_dir()),
        "the navigation bar reaches a folder: {revealed:?}"
    );
}

#[ui_kit::gpui::test]
fn menu_key_opens_the_context_menu_of_the_editor_and_the_tree(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-menu");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    draw(cx, window);

    press(cx, window, "menu");
    wait_for(cx, "the editor menu", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap_or(false)
    });
    let entries = window
        .read_with(cx, |workspace, _| workspace.menu_entries().map(<[_]>::len))
        .unwrap();
    assert!(
        entries.is_some_and(|count| count > 0),
        "the editor menu opens"
    );
    press(cx, window, "escape");

    focus_project(cx, window);
    draw(cx, window);
    press(cx, window, "menu");
    wait_for(cx, "the tree menu", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap_or(false)
    });
    let entries = window
        .read_with(cx, |workspace, _| workspace.menu_entries().map(<[_]>::len))
        .unwrap();
    assert!(
        entries.is_some_and(|count| count > 0),
        "the tree menu opens"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn quick_change_scheme_changes_the_scale(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-scheme");
    press(cx, window, "ctrl-`");
    draw(cx, window);
    assert!(has_selector(cx, window, "scheme-popup"));

    press(cx, window, "down");
    press(cx, window, "down");
    press(cx, window, "enter");
    let scale = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!(scale > 1., "the scheme popup raises the scale");
    assert!(!has_selector(cx, window, "scheme-popup"));
}

#[ui_kit::gpui::test]
fn new_scratch_file_opens_in_the_scratches_folder_and_shows_in_the_tree(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-scratch");
    press(cx, window, "ctrl-alt-shift-insert");
    draw(cx, window);
    assert!(has_selector(cx, window, "scratch-language-popup"));

    press(cx, window, "enter");
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let scratches = state_dir.join("scratches");
    wait_for(cx, "the scratch file", |cx| {
        let path = active_path(cx, window);
        path.as_ref()
            .is_some_and(|path| path.starts_with(&scratches))
    });
    let path = active_path(cx, window).unwrap();
    assert_eq!(path.extension().and_then(|e| e.to_str()), Some("txt"));
    assert!(path.is_file());

    let panel = project_panel(cx);
    let has_node = cx.read(|cx| {
        panel
            .read(cx)
            .tree()
            .rows()
            .iter()
            .any(|row| row.name == "Scratches" && row.folder)
    });
    assert!(has_node, "the project tree shows the Scratches folder");
}

#[ui_kit::gpui::test]
fn tree_expands_and_collapses_with_ctrl_equals_and_minus(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-tree-fold");
    focus_project(cx, window);
    draw(cx, window);

    press(cx, window, "ctrl-=");
    let expanded = cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .tree()
            .expanded()
    });
    assert!(
        expanded.len() > 1,
        "Expand All opens the folders of the project"
    );

    press(cx, window, "ctrl--");
    let open = cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .tree()
            .rows()
            .iter()
            .filter(|row| row.open)
            .count()
    });
    assert_eq!(open, 1, "Collapse All leaves only the root open");
}

#[ui_kit::gpui::test]
fn tree_multi_selection_copies_every_selected_path(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "window-tree-select");
    focus_project(cx, window);
    let src = root.join("src");
    crate::window_tests_menus::reveal(cx, window, &src);
    project_panel(cx).update(cx, |panel, cx| panel.toggle(&src, cx));
    draw(cx, window);
    wait_for(cx, "the open src folder", |cx| {
        cx.read(|cx| {
            cx.global::<ActiveProjectPanel>()
                .0
                .read(cx)
                .tree()
                .index_of(&root.join("src").join("main.txt"))
                .is_some()
        })
    });
    draw(cx, window);

    click_with(cx, window, "tree-row-main.txt", ctrl());
    let paths = selected_paths(cx);
    assert!(
        paths.len() >= 2,
        "Ctrl+click adds rows to the selection: {paths:?}"
    );

    press(cx, window, "ctrl-shift-c");
    let text = clipboard_text(cx).unwrap_or_default();
    assert!(text.contains("main.txt") && text.contains("src"));

    // Shift+click selects the whole run between the anchor and the clicked row.
    crate::window_tests_menus::reveal(cx, window, &src);
    project_panel(cx).update(cx, |panel, cx| {
        let open = panel
            .tree()
            .rows()
            .iter()
            .find(|row| row.path == src)
            .is_some_and(|row| row.open);
        if !open {
            panel.toggle(&src, cx);
        }
    });
    draw(cx, window);
    click_with(cx, window, "tree-row-main.txt", shift());
    let paths = selected_paths(cx);
    assert!(paths.len() >= 2, "Shift+click extends the selection");
    let all_under_src = paths.iter().all(|path| path.starts_with(root.join("src")));
    assert!(all_under_src, "the run stays inside the open folder");
}

#[ui_kit::gpui::test]
fn tree_cut_copy_paste_and_delete_work_on_the_selection(cx: &mut TestAppContext) {
    crate::tree_ops::use_temporary_delete();
    let (root, window) = open_sample_window(cx, "window-tree-clipboard");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&main, cx));
    focus_project(cx, window);
    draw(cx, window);
    press(cx, window, "ctrl-c");
    panel.update(cx, |panel, cx| panel.select_path(&root, cx));
    draw(cx, window);
    press(cx, window, "ctrl-v");
    wait_for(cx, "the copied file", |_| root.join("main.txt").is_file());

    panel.update(cx, |panel, cx| panel.select_path(&notes, cx));
    draw(cx, window);
    press(cx, window, "ctrl-x");
    panel.update(cx, |panel, cx| panel.select_path(&root, cx));
    draw(cx, window);
    press(cx, window, "ctrl-v");
    wait_for(cx, "the moved file", |_| {
        !notes.is_file() && root.join("notes.txt").is_file()
    });

    // Delete takes every selected file.
    let extra = root.join("src").join("extra.txt");
    fs::write(&extra, "extra\n").unwrap();
    panel.update(cx, |panel, cx| panel.reload(cx));
    wait_for(cx, "the new row", |cx| {
        cx.read(|cx| {
            cx.global::<ActiveProjectPanel>()
                .0
                .read(cx)
                .tree()
                .index_of(&extra)
                .is_some()
        })
    });
    draw(cx, window);
    click_with(cx, window, "tree-row-main.txt", Modifiers::default());
    draw(cx, window);
    click_with(cx, window, "tree-row-extra.txt", ctrl());
    press(cx, window, "delete");
    draw(cx, window);
    press(cx, window, "enter");
    wait_for(cx, "the deleted files", |_| {
        !root.join("main.txt").is_file() && !extra.is_file()
    });
}

#[ui_kit::gpui::test]
fn save_as_writes_the_buffer_to_the_chosen_path(cx: &mut TestAppContext) {
    let root = scratch("window-save-as");
    let source = root.join("source.txt");
    fs::write(&source, "the text to keep\n").unwrap();
    let window = crate::window_tests::open_window(cx, root.clone());
    draw(cx, window);
    open_path(cx, window, &source);

    let target = root.join("copy.txt");
    press(cx, window, "ctrl-shift-s");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for(cx, "the saved file", |_| target.is_file());
    assert_eq!(fs::read_to_string(&target).unwrap(), "the text to keep\n");
    wait_for(cx, "the new tab", |cx| {
        active_path(cx, window).as_deref() == Some(target.as_path())
    });
}

#[ui_kit::gpui::test]
fn zoom_out_never_goes_below_the_limit(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-zoom-limit");
    for _ in 0..30 {
        press(cx, window, "alt-shift--");
    }
    let scale = window
        .read_with(cx, |workspace, cx| workspace.ui_scale(cx))
        .unwrap();
    assert!((scale - MIN_UI_SCALE).abs() < 0.001);
    assert!((workspace_settings(cx).ui_scale - MIN_UI_SCALE).abs() < 0.001);
}

#[ui_kit::gpui::test]
fn the_theme_scheme_row_toggles_the_theme(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-scheme-theme");
    let before = cx.read(|cx| cx.global::<Theme>().dark);
    press(cx, window, "ctrl-`");
    draw(cx, window);
    press(cx, window, "enter");
    // The theme fades, so the window has to draw a few frames.
    let started = std::time::Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        draw(cx, window);
        let now = cx.read(|cx| cx.global::<Theme>().dark);
        if now != before {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the theme row did not change the theme");
}

#[ui_kit::gpui::test]
fn commands_of_the_window_are_registered_for_find_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "window-commands");
    for id in [
        keys::CLOSE_ACTIVE_TAB,
        keys::NEXT_EDITOR_TAB,
        keys::PREVIOUS_EDITOR_TAB,
        keys::OPEN_IN_RIGHT_SPLIT,
        keys::SAVE_AS,
        keys::HIDE_ALL_WINDOWS,
        keys::MAXIMIZE_TOOL_WINDOW,
        keys::JUMP_TO_LAST_WINDOW,
        keys::RESTORE_DEFAULT_LAYOUT,
        keys::FOCUS_EDITOR,
        keys::ZOOM_IN_IDE,
        keys::ZOOM_OUT_IDE,
        keys::RESET_IDE_SCALE,
        keys::EDITOR_FONT_INCREASE,
        keys::EDITOR_FONT_DECREASE,
        keys::SHOW_FILE_PATH,
        keys::SHOW_NAV_BAR,
        keys::SHOW_POPUP_MENU,
        keys::QUICK_CHANGE_SCHEME,
        keys::NEW_SCRATCH_FILE,
        keys::EXPAND_ALL,
        keys::COLLAPSE_ALL,
        keys::EXPAND_TREE_NODE,
        keys::COLLAPSE_TREE_NODE,
    ] {
        let known = window
            .read_with(cx, |workspace, _| workspace.commands().get(id).is_some())
            .unwrap_or(false);
        assert!(known, "{id} is registered");
    }
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
            .unwrap(),
        "the project tool window is still there"
    );
}
