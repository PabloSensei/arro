//! ⇧⌘↑ and ⇧⌘↓ in the editor on macOS: IDEA gives them to both the line move and the
//! selection to the end of the text, and its editor moves the line. The tests take the
//! platform as an argument, so they run on every system.

use keymap::{Keystroke, Platform, editor};

fn stroke(text: &str) -> Keystroke {
    Keystroke::parse(text).unwrap()
}

fn keys(action: &str, platform: Platform) -> Vec<String> {
    keymap::default_keymap_for(platform)
        .1
        .shortcuts_for(action)
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn the_mac_editor_moves_the_line_with_shift_command_up_and_down() {
    let (_, keymap) = keymap::default_keymap_for(Platform::MacOs);
    for (key, action) in [
        ("cmd-shift-up", editor::MOVE_LINE_UP),
        ("cmd-shift-down", editor::MOVE_LINE_DOWN),
    ] {
        assert_eq!(
            keymap.resolve(&stroke(key), editor::CONTEXT),
            Some(action),
            "{key}"
        );
    }
    assert_eq!(
        keys(editor::MOVE_LINE_UP, Platform::MacOs),
        ["shift-cmd-up"],
        "the move keeps the key IDEA gives it"
    );
    assert_eq!(
        keys(editor::MOVE_LINE_DOWN, Platform::MacOs),
        ["shift-cmd-down"]
    );
}

/// The selections to the ends of the text lose the keys the move takes, and have no other
/// key on macOS: IDEA's macOS keymap has no other key for them.
#[test]
fn the_mac_editor_has_no_key_for_the_selection_to_the_ends_of_the_text() {
    let (_, keymap) = keymap::default_keymap_for(Platform::MacOs);
    for action in [editor::SELECT_FILE_START, editor::SELECT_FILE_END] {
        assert!(
            keys(action, Platform::MacOs).is_empty(),
            "{action} has a key on macOS"
        );
    }
    assert_eq!(
        keymap.resolve(&stroke("cmd-shift-up"), editor::CONTEXT),
        Some(editor::MOVE_LINE_UP),
        "⇧⌘↑ moves the line, it does not select to the start"
    );
    assert_eq!(
        keymap.resolve(&stroke("cmd-shift-down"), editor::CONTEXT),
        Some(editor::MOVE_LINE_DOWN)
    );
    assert_eq!(
        keys(editor::SELECT_FILE_START, Platform::Windows),
        ["ctrl-shift-home"],
        "the Windows key of the selection stays"
    );
}

#[test]
fn the_windows_editor_keeps_its_arrows() {
    for platform in [Platform::Windows, Platform::Linux] {
        let (_, keymap) = keymap::default_keymap_for(platform);
        assert_eq!(
            keymap.resolve(&stroke("ctrl-shift-home"), editor::CONTEXT),
            Some(editor::SELECT_FILE_START),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("ctrl-shift-end"), editor::CONTEXT),
            Some(editor::SELECT_FILE_END),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("alt-shift-up"), editor::CONTEXT),
            Some(editor::MOVE_LINE_UP),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("alt-shift-down"), editor::CONTEXT),
            Some(editor::MOVE_LINE_DOWN),
            "{platform:?}"
        );
        // ⇧⌘↑ is only a macOS key: the Windows table moves a statement with it.
        assert_eq!(
            keymap.resolve(&stroke("ctrl-shift-up"), editor::CONTEXT),
            Some(editor::MOVE_STATEMENT_UP),
            "{platform:?}"
        );
        assert_eq!(
            keymap.resolve(&stroke("cmd-shift-up"), editor::CONTEXT),
            None,
            "{platform:?}"
        );
    }
}
