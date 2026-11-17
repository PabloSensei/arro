//! Settings -> Keymap without a real window: a shortcut file applies at once, the page
//! adds and removes keys, a conflict names the other action, and the file lands in the
//! settings folder of the test.

use std::fs;

use keymap::{editor as editor_keys, workspace as keys};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};

use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, open_path, open_sample_window,
    press, type_text, wait_for_signal,
};

/// Writes the keymap file of the test window and makes the window read it.
fn write_keymap(cx: &mut TestAppContext, window: WindowHandle<workspace::Workspace>, text: &str) {
    let path = window
        .read_with(cx, |workspace, _| workspace.keymap_path().to_path_buf())
        .unwrap();
    fs::write(&path, text).unwrap();
    let before = window
        .read_with(cx, |workspace, _| workspace.keymap_generation())
        .unwrap();
    window
        .update(cx, |workspace, _, cx| workspace.reload_keymap(cx))
        .unwrap();
    wait_for_signal(cx, "the keymap file", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.keymap_generation() > before)
            .unwrap()
    });
}

fn open_keymap_page(cx: &mut TestAppContext, window: WindowHandle<workspace::Workspace>) {
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-keymap");
    draw(cx, window);
}

#[gpui::test]
fn a_new_shortcut_from_the_file_runs_and_the_menu_and_find_action_show_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "keymap-add-file");
    write_keymap(
        cx,
        window,
        r#"{"version": 1, "actions": {"editor.duplicate": {"add": ["ctrl-alt-shift-d"]}}}"#,
    );
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n");

    press(cx, window, "ctrl-alt-shift-d");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nalpha\nbeta\n",
        "the shortcut of the file duplicates the line"
    );
    let shortcut = window
        .read_with(cx, |workspace, _| {
            workspace.shortcut_of(editor_keys::DUPLICATE)
        })
        .unwrap();
    assert_eq!(
        shortcut.as_deref(),
        Some("ctrl-alt-shift-d"),
        "a menu and Find Action write the first shortcut of the keymap"
    );
    let file = window
        .read_with(cx, |workspace, _| workspace.keymap_path().to_path_buf())
        .unwrap();
    assert!(file.ends_with("keymap.json"));
}

#[gpui::test]
fn a_removed_shortcut_no_longer_runs_and_a_reset_brings_it_back(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "keymap-remove-file");
    write_keymap(
        cx,
        window,
        r#"{"version": 1, "actions": {"editor.duplicate": {"remove": ["ctrl-d"]}}}"#,
    );
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-d");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\n",
        "the removed shortcut does nothing"
    );

    window
        .update(cx, |workspace, _, cx| {
            workspace.reset_keymap_action(editor_keys::DUPLICATE, cx)
        })
        .unwrap();
    press(cx, window, "ctrl-d");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nalpha\nbeta\n",
        "Reset gives the default shortcut back"
    );
}

#[gpui::test]
fn the_page_adds_a_shortcut_and_a_conflict_names_the_other_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "keymap-page-add");
    open_keymap_page(cx, window);
    type_text(cx, window, "copy");
    draw(cx, window);

    click(cx, window, "keymap-add-editor-copy");
    draw(cx, window);
    // Ctrl+D is Duplicate already: the page asks what to do with it.
    press(cx, window, "ctrl-d");
    draw(cx, window);
    bounds_of(cx, window, "keymap-capture");
    click(cx, window, "keymap-capture-confirm");
    draw(cx, window);
    bounds_of(cx, window, "keymap-conflict-editor-duplicate");

    click(cx, window, "keymap-conflict-remove");
    draw(cx, window);
    let (copy, duplicate) = window
        .read_with(cx, |workspace, _| {
            (
                workspace
                    .keymap()
                    .shortcuts_for(editor_keys::COPY)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                workspace
                    .keymap()
                    .shortcuts_for(editor_keys::DUPLICATE)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
    assert!(copy.contains(&"ctrl-d".to_owned()), "{copy:?}");
    assert!(!duplicate.contains(&"ctrl-d".to_owned()), "{duplicate:?}");

    cx.run_until_parked();
    let file = window
        .read_with(cx, |workspace, _| workspace.keymap_path().to_path_buf())
        .unwrap();
    let saved = fs::read_to_string(&file).unwrap();
    assert!(saved.contains("editor.copy"), "{saved}");
    assert!(saved.contains("editor.duplicate"), "{saved}");
    assert!(
        file.starts_with(
            window
                .read_with(cx, |workspace, _| workspace.settings_dir().to_path_buf())
                .unwrap()
        ),
        "the file is written to the settings folder of the test"
    );
}

#[gpui::test]
fn find_by_shortcut_lists_only_the_actions_on_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "keymap-find-shortcut");
    open_keymap_page(cx, window);
    click(cx, window, "keymap-find-by-shortcut");
    draw(cx, window);
    press(cx, window, "ctrl-d");
    click(cx, window, "keymap-capture-confirm");
    draw(cx, window);

    bounds_of(cx, window, "keymap-row-editor-duplicate");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("keymap-row-editor-paste").is_none(),
        "an action on another shortcut is not listed"
    );
}

#[gpui::test]
fn reset_all_asks_first_and_returns_the_default_layout(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "keymap-reset-all");
    write_keymap(
        cx,
        window,
        r#"{"version": 1, "actions": {"editor.duplicate": {"remove": ["ctrl-d"]}}}"#,
    );
    open_keymap_page(cx, window);
    click(cx, window, "keymap-reset-all");
    draw(cx, window);
    bounds_of(cx, window, "keymap-reset-all-confirm");
    click(cx, window, "keymap-reset-all-yes");
    draw(cx, window);

    let duplicate = window
        .read_with(cx, |workspace, _| {
            workspace
                .keymap()
                .shortcuts_for(editor_keys::DUPLICATE)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(duplicate.contains(&"ctrl-d".to_owned()), "{duplicate:?}");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace
                .keymap_file()
                .actions
                .is_empty())
            .unwrap(),
        "Reset All drops every change"
    );
}

#[gpui::test]
fn a_shortcut_added_on_the_page_runs_in_the_window(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "keymap-page-runs");
    open_keymap_page(cx, window);
    type_text(cx, window, "duplicate");
    draw(cx, window);
    click(cx, window, "keymap-add-editor-duplicate");
    draw(cx, window);
    press(cx, window, "ctrl-alt-shift-d");
    click(cx, window, "keymap-capture-confirm");
    draw(cx, window);

    window
        .update(cx, |workspace, window, cx| workspace.dismiss(window, cx))
        .unwrap();
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-d");
    assert_eq!(buffer_of(cx, &view), "alpha\nalpha\nbeta\n");
    assert_eq!(active_path(cx, window), Some(path));
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace
                .shortcut_of(editor_keys::DUPLICATE))
            .unwrap()
            .as_deref(),
        Some("ctrl-alt-shift-d")
    );
}

#[gpui::test]
fn a_workspace_action_takes_a_shortcut_from_the_file(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "keymap-workspace");
    write_keymap(
        cx,
        window,
        &format!(
            r#"{{"version": 1, "actions": {{"{}": {{"add": ["ctrl-alt-shift-g"]}}}}}}"#,
            keys::GOTO_FILE
        ),
    );
    let shortcut = window
        .read_with(cx, |workspace, _| workspace.shortcut_of(keys::GOTO_FILE))
        .unwrap();
    assert_eq!(shortcut.as_deref(), Some("ctrl-alt-shift-g"));
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace
                .shortcut_of_key(keys::GOTO_FILE))
            .unwrap()
            .as_deref(),
        Some("Ctrl+Alt+Shift+G"),
        "a menu label is built from the keymap"
    );
}
