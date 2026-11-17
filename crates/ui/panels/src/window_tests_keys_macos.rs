//! The macOS keys of the window and of the panels, read on every system: Command where
//! IDEA puts Command, Control where IDEA keeps it, and the tool window slots of macOS.

use keymap::Platform;
use workspace::WINDOW_KEYS;

/// The file is the source of the macOS keys, so the tests read it rather than spell a
/// list of their own.
fn idea_keys(id: &str) -> Vec<String> {
    keymap::idea_shortcuts(keymap::IDEA_DEFAULT_MACOS)
        .expect("idea-default-macos.json parses")
        .get(id)
        .unwrap_or_else(|| panic!("{id} is in the macOS file"))
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn window_keys(action: &str, platform: Platform) -> Vec<&'static str> {
    WINDOW_KEYS
        .iter()
        .find(|row| row.action == action)
        .unwrap_or_else(|| panic!("{action} is a window action"))
        .keys_for(platform)
}

fn panel_keys(
    rows: &'static [workspace::KeyRow],
    action: &str,
    platform: Platform,
) -> Vec<&'static str> {
    rows.iter()
        .find(|row| row.action == action)
        .unwrap_or_else(|| panic!("{action} is in the panel table"))
        .keys_for(platform)
}

#[test]
fn the_macos_file_has_the_tool_window_and_run_keys_of_idea() {
    assert_eq!(
        idea_keys("ActivateProjectToolWindow"),
        vec!["cmd-1".to_owned()]
    );
    assert_eq!(idea_keys("ActivateRunToolWindow"), vec!["cmd-4".to_owned()]);
    assert_eq!(
        idea_keys("ActivateDebugToolWindow"),
        vec!["cmd-5".to_owned()]
    );
    assert_eq!(idea_keys("Run"), vec!["ctrl-r".to_owned()]);
    assert_eq!(idea_keys("Debug"), vec!["ctrl-d".to_owned()]);
    assert_eq!(idea_keys("ToggleLineBreakpoint"), vec!["cmd-f8".to_owned()]);
    assert_eq!(idea_keys("Stop"), vec!["cmd-f2".to_owned()]);
}

#[test]
fn the_window_table_uses_the_macos_keys() {
    assert_eq!(
        window_keys("workspace::ToggleProject", Platform::MacOs),
        ["cmd-1"]
    );
    assert_eq!(
        window_keys("workspace::ToggleRun", Platform::MacOs),
        ["cmd-4"]
    );
    assert_eq!(
        window_keys("workspace::ToggleDebug", Platform::MacOs),
        ["cmd-5"]
    );
    assert_eq!(window_keys("workspace::Run", Platform::MacOs), ["ctrl-r"]);
    assert_eq!(window_keys("workspace::Debug", Platform::MacOs), ["ctrl-d"]);
    assert_eq!(
        window_keys("workspace::ToggleBreakpoint", Platform::MacOs),
        ["cmd-f8"]
    );
    assert_eq!(window_keys("workspace::Stop", Platform::MacOs), ["cmd-f2"]);
    assert_eq!(
        keymap::shortcut_label("cmd-1", Platform::MacOs),
        "⌘1",
        "the hint of a tool window"
    );
    assert_eq!(
        keymap::shortcut_label("ctrl-r", Platform::MacOs),
        "⌃R",
        "the hint of Run"
    );
}

#[test]
fn the_windows_table_keeps_its_keys() {
    assert_eq!(
        window_keys("workspace::ToggleProject", Platform::Windows),
        ["alt-1"]
    );
    assert_eq!(
        window_keys("workspace::Run", Platform::Windows),
        ["shift-f10"]
    );
    assert_eq!(
        window_keys("workspace::Debug", Platform::Windows),
        ["shift-f9"]
    );
    assert_eq!(
        window_keys("workspace::ToggleBreakpoint", Platform::Windows),
        ["ctrl-f8"]
    );
    assert_eq!(
        window_keys("workspace::Stop", Platform::Windows),
        ["ctrl-f2"]
    );
    for row in WINDOW_KEYS {
        assert_eq!(
            row.keys_for(Platform::Windows),
            [row.keys],
            "{} keeps its Windows key",
            row.action
        );
        assert_eq!(row.keys_for(Platform::Linux), [row.keys], "{}", row.action);
    }
}

#[test]
fn the_panel_tables_use_command_where_idea_does() {
    assert_eq!(
        panel_keys(
            crate::project::PANEL_KEYS,
            "project_panel::CopyRows",
            Platform::MacOs
        ),
        ["cmd-c"]
    );
    assert_eq!(
        panel_keys(
            crate::project::PANEL_KEYS,
            "project_panel::CompareTwoFiles",
            Platform::MacOs
        ),
        ["cmd-d"]
    );
    assert_eq!(
        panel_keys(
            crate::commit::KEYS,
            "commit_panel::ShowDiff",
            Platform::MacOs
        ),
        ["cmd-d"]
    );
    assert!(
        panel_keys(crate::debug::KEYS, "debug_panel::NewWatch", Platform::MacOs).is_empty(),
        "an Apple keyboard has no Insert key"
    );
    assert_eq!(
        panel_keys(crate::debug::KEYS, "debug_panel::SetValue", Platform::MacOs),
        ["f2"]
    );
}
