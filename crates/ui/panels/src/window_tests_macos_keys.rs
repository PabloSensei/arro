//! The macOS keys in a window, pressed on every runner: Command closes the tab of the
//! editor, the terminal takes its own Command keys and leaves Control to the shell, and
//! Windows keeps the Control keys it had.
//!
//! The platform of the keymap is process-wide, so a test here takes the macOS keys for
//! its own window only: choosing them for the process would change the windows of the
//! tests that run beside this one.

use keymap::Platform;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::{RUN_TOOL, TERMINAL_TOOL, WINDOW_KEYS};

use crate::terminal_panel::ActiveTerminal;
use crate::window_tests::{
    active_path, draw, open_path, open_sample_window, open_window, press, sample_project,
};

/// The menu of the application shows the first key of the action: ⌘, opens Settings.
#[test]
fn the_first_key_of_settings_on_macos_is_command_comma() {
    let row = WINDOW_KEYS
        .iter()
        .find(|row| row.action == "workspace::ShowSettings")
        .expect("Settings is a window action");
    assert_eq!(
        row.keys_for(Platform::MacOs).first().copied(),
        Some("cmd-,")
    );
    assert_eq!(
        keymap::shortcut_label("cmd-,", Platform::MacOs),
        "⌘,",
        "the label of the menu"
    );
    let idea =
        keymap::idea_shortcuts(keymap::IDEA_DEFAULT_MACOS).expect("idea-default-macos.json parses");
    let keys = idea.get("ShowSettings").expect("the action is in the file");
    assert_eq!(
        keys.first().map(ToString::to_string),
        Some("cmd-,".to_owned())
    );
}

#[ui_kit::gpui::test]
fn command_w_closes_the_tab_of_the_editor(cx: &mut TestAppContext) {
    let root = sample_project("macos-editor-tab");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, _, cx| {
            workspace.use_keys_of(Platform::MacOs, cx)
        })
        .unwrap();
    open_path(cx, window, &path);
    assert_eq!(active_path(cx, window), Some(path.clone()));
    assert_eq!(
        window
            .read_with(cx, |workspace, _| {
                workspace.shortcut_of(keymap::workspace::CLOSE_ACTIVE_TAB)
            })
            .unwrap()
            .as_deref(),
        Some("cmd-w"),
        "the window takes the keys of macOS"
    );

    press(cx, window, "cmd-w");
    assert_eq!(
        active_path(cx, window),
        None,
        "⌘W closes the tab of the editor"
    );
}

#[ui_kit::gpui::test]
fn command_w_closes_the_tab_of_the_terminal_and_control_reaches_the_shell(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "macos-terminal-tab");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TERMINAL_TOOL, window, cx)
        })
        .unwrap();
    let terminal = cx.read(|cx| cx.global::<ActiveTerminal>().0.clone());
    terminal.update(cx, |panel, _| panel.use_keys_of(Platform::MacOs));
    draw(cx, window);

    press(cx, window, "cmd-t");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local", "Local (2)"],
        "⌘T opens a tab of the terminal"
    );

    // Control belongs to the shell there: Ctrl+W is a word of the program and not the
    // window key that closes a tab.
    press(cx, window, "ctrl-w");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local", "Local (2)"],
        "Ctrl+W does not close a tab of the terminal"
    );

    press(cx, window, "cmd-w");
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_names()),
        ["Local"],
        "⌘W closes the tab of the terminal"
    );
}

#[ui_kit::gpui::test]
fn windows_keeps_its_control_keys_for_the_tab_and_the_tool_window(cx: &mut TestAppContext) {
    let root = sample_project("windows-close-tab");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    open_path(cx, window, &path);

    press(cx, window, "ctrl-f4");
    assert_eq!(active_path(cx, window), None, "Ctrl+F4 closes the tab");

    open_path(cx, window, &path);
    press(cx, window, "alt-4");
    press(cx, window, "ctrl-shift-f4");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap(),
        "Ctrl+Shift+F4 hides the tool window"
    );
    assert_eq!(
        active_path(cx, window),
        Some(path),
        "Ctrl+Shift+F4 leaves the tab of the editor open"
    );
}
