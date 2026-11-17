//! The macOS table of the IDEA keymap: the same actions as the Windows one, with Command
//! where IDEA puts Command and the hints written as glyphs. The tests take the platform as
//! an argument, so they run on every system.

use std::collections::HashSet;

use keymap::{Keystroke, Platform, editor, workspace};

fn stroke(text: &str) -> Keystroke {
    Keystroke::parse(text).unwrap()
}

fn mac() -> (keymap::ActionRegistry, keymap::Keymap) {
    keymap::default_keymap_for(Platform::MacOs)
}

/// The actions the macOS table may leave without a key: their Windows key is Insert, which
/// an Apple keyboard has no key for, two actions that macOS gives no key of its own, and
/// the two selections to the ends of the text: IDEA gives them ⇧⌘↑ and ⇧⌘↓, the keys its
/// editor uses for moving the line, and this editor moves the line as IDEA does.
const NO_MAC_KEY: &[&str] = &[
    "EditorTextEndWithSelection",
    "EditorTextStartWithSelection",
    "EditorToggleInsertState",
    "MaximizeToolWindow",
    "ShowPopupMenu",
    "SwitcherIterateItems",
    "UsageView.Include",
    "XDebugger.NewWatch",
];

#[test]
fn the_mac_table_keeps_every_action_of_the_windows_one() {
    let windows = keymap::idea_shortcuts(keymap::IDEA_DEFAULT).unwrap();
    let mac = keymap::idea_shortcuts(keymap::IDEA_DEFAULT_MACOS).unwrap();

    let windows_ids: HashSet<&String> = windows.keys().collect();
    let mac_ids: HashSet<&String> = mac.keys().collect();
    assert_eq!(windows_ids, mac_ids);
    for (id, keys) in &mac {
        if keys.is_empty() {
            assert!(
                NO_MAC_KEY.contains(&id.as_str()) || id.starts_with("XDebugger."),
                "{id} has no key on macOS"
            );
        }
    }
}

#[test]
fn every_implemented_action_of_the_tables_has_its_mac_keys() {
    let mac = keymap::idea_shortcuts(keymap::IDEA_DEFAULT_MACOS).unwrap();
    for (id, action) in editor::IDEA_IDS
        .iter()
        .chain(workspace::IDEA_IDS)
        .chain(workspace::EXTRA_IDEA_IDS)
    {
        let keys = mac
            .get(*id)
            .unwrap_or_else(|| panic!("{id} is in the mac table"));
        assert!(
            !keys.is_empty() || NO_MAC_KEY.contains(id),
            "{action} ({id}) has no key on macOS"
        );
    }
}

#[test]
fn command_replaces_control_on_mac_where_idea_uses_command() {
    let (_, keymap) = mac();
    for (key, action) in [
        ("cmd-c", editor::COPY),
        ("cmd-v", editor::PASTE),
        ("cmd-x", editor::CUT),
        ("cmd-z", editor::UNDO),
        ("cmd-shift-z", editor::REDO),
        ("cmd-b", editor::GOTO_DECLARATION),
        ("cmd-alt-b", editor::GOTO_IMPLEMENTATION),
        ("cmd-n", editor::GENERATE),
        ("cmd-f", editor::FIND),
        ("cmd-p", editor::PARAMETER_INFO),
        ("cmd-d", editor::DUPLICATE),
        ("cmd-/", editor::TOGGLE_COMMENT),
        ("alt-up", editor::EXTEND_SELECTION),
        ("alt-left", editor::MOVE_WORD_LEFT),
        ("cmd-up", editor::MOVE_FILE_START),
        ("cmd-l", editor::GOTO_LINE),
        ("cmd-g", editor::FIND_NEXT),
        ("f3", editor::TOGGLE_BOOKMARK),
    ] {
        assert_eq!(
            keymap.resolve(&stroke(key), editor::CONTEXT),
            Some(action),
            "{key}"
        );
    }
    // The window actions of the editor context are looked up through the workspace one.
    assert_eq!(
        keymap.lookup(
            &[stroke("cmd-shift-o")],
            &[editor::CONTEXT, workspace::CONTEXT]
        ),
        keymap::Lookup::Action(workspace::GOTO_FILE)
    );
}

#[test]
fn control_keeps_the_keys_idea_keeps_on_mac() {
    let (_, keymap) = mac();
    for (key, action) in [
        ("ctrl-space", editor::COMPLETE),
        ("ctrl-shift-space", editor::SMART_TYPE_COMPLETION),
        ("ctrl-alt-space", editor::CLASS_NAME_COMPLETION),
        ("ctrl-alt-shift-t", editor::REFACTOR_THIS),
        ("ctrl-alt-o", editor::OPTIMIZE_IMPORTS),
        ("ctrl-h", editor::TYPE_HIERARCHY),
        ("ctrl-f7", editor::FIND_USAGES_IN_FILE),
        ("ctrl-shift-f7", editor::HIGHLIGHT_USAGES),
    ] {
        assert_eq!(
            keymap.resolve(&stroke(key), editor::CONTEXT),
            Some(action),
            "{key}"
        );
    }
    for (key, action) in [
        ("cmd-o", workspace::GOTO_CLASS),
        ("cmd-shift-o", workspace::GOTO_FILE),
        ("cmd-e", workspace::RECENT_FILES),
        ("ctrl-tab", workspace::SWITCHER),
        ("cmd-shift-t", workspace::GOTO_TEST),
        ("cmd-shift-a", workspace::FIND_ACTION),
        ("cmd-shift-f", workspace::FIND_IN_FILES),
        ("cmd-shift-r", workspace::REPLACE_IN_FILES),
    ] {
        assert_eq!(
            keymap.resolve(&stroke(key), workspace::CONTEXT),
            Some(action),
            "{key}"
        );
    }
    // Control+C belongs to the text field of a dialog on macOS; the editor copies with
    // Command.
    assert_ne!(
        keymap.resolve(&stroke("ctrl-c"), editor::CONTEXT),
        Some(editor::COPY)
    );
}

#[test]
fn the_windows_table_and_the_linux_one_stay_as_they_were() {
    for platform in [Platform::Windows, Platform::Linux] {
        let (_, keymap) = keymap::default_keymap_for(platform);
        assert_eq!(
            keymap.resolve(&stroke("ctrl-b"), editor::CONTEXT),
            Some(editor::GOTO_DECLARATION),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("ctrl-shift-n"), workspace::CONTEXT),
            Some(workspace::GOTO_FILE),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("cmd-b"), editor::CONTEXT),
            None,
            "{platform:?}"
        );
    }
}

#[test]
fn a_mac_shortcut_is_written_with_glyphs() {
    assert_eq!(
        keymap::shortcut_label("cmd-shift-f", Platform::MacOs),
        "⇧⌘F"
    );
    assert_eq!(keymap::shortcut_label("cmd-b", Platform::MacOs), "⌘B");
    assert_eq!(
        keymap::shortcut_label("ctrl-space", Platform::MacOs),
        "⌃Space"
    );
    assert_eq!(keymap::shortcut_label("alt-enter", Platform::MacOs), "⌥↩");
    assert_eq!(
        keymap::shortcut_label("ctrl-alt-shift-t", Platform::MacOs),
        "⌃⌥⇧T"
    );
    assert_eq!(
        keymap::shortcut_label("double-shift", Platform::MacOs),
        "Double Shift"
    );
    assert_eq!(
        keymap::shortcut_label("ctrl-shift-n", Platform::Windows),
        "Ctrl+Shift+N"
    );
    assert_eq!(
        keymap::shortcut_label("ctrl-k ctrl-c", Platform::Windows),
        "Ctrl+K, Ctrl+C"
    );
}

#[test]
fn the_platform_of_a_process_is_chosen_once() {
    // The default keeps the tests on the Windows keys on every runner.
    assert_eq!(keymap::platform(), Platform::Windows);
    keymap::set_platform(Platform::Linux);
    assert_eq!(keymap::platform(), Platform::Linux);
    assert_eq!(
        keymap::shortcut_label("ctrl-b", keymap::platform()),
        "Ctrl+B"
    );
    keymap::set_platform(Platform::Windows);
    assert_eq!(keymap::platform(), Platform::Windows);
}
