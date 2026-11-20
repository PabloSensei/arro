//! Find Action with an empty query, without a window: each command shows the main shortcut
//! that its menu row writes, the icon of its menu row, and the commands come in alphabetical
//! order, every one of them.

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::palette::Palette;
use crate::window_tests::{draw, open_palette, open_sample_window};

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

/// The commands on screen: the title, the key at the right edge and the icon at the left.
fn commands(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Vec<(String, Option<String>, &'static str)> {
    palette(cx, window).read_with(cx, |palette, _| palette.action_rows_text())
}

/// The key of the command with this title.
fn key_of(rows: &[(String, Option<String>, &'static str)], title: &str) -> Option<String> {
    rows.iter()
        .find(|(row, _, _)| row == title)
        .unwrap_or_else(|| panic!("no {title} in Find Action: {rows:?}"))
        .1
        .clone()
}

/// The icon of the command with this title.
fn icon_of(rows: &[(String, Option<String>, &'static str)], title: &str) -> &'static str {
    rows.iter()
        .find(|(row, _, _)| row == title)
        .unwrap_or_else(|| panic!("no {title} in Find Action: {rows:?}"))
        .2
}

#[ui_kit::gpui::test]
fn find_action_shows_the_main_shortcut_of_each_command(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-main-keys");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let rows = commands(cx, window);

    // The first key of the keymap file, as the menus write it, not the keymap spelling.
    assert_eq!(key_of(&rows, "Undo").as_deref(), Some("Ctrl+Z"), "{rows:?}");
    assert_eq!(
        key_of(&rows, "Redo").as_deref(),
        Some("Ctrl+Shift+Z"),
        "{rows:?}"
    );
    assert_eq!(key_of(&rows, "Find").as_deref(), Some("Ctrl+F"), "{rows:?}");
    for (title, key, _) in &rows {
        let Some(key) = key else { continue };
        let lower = key.to_lowercase();
        for part in ["ctrl-", "alt-", "shift-", "cmd-"] {
            assert!(!lower.contains(part), "{title} shows {key}");
        }
    }
}

#[ui_kit::gpui::test]
fn find_action_without_a_query_lists_the_commands_alphabetically(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-alphabet");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let titles: Vec<String> = commands(cx, window)
        .into_iter()
        .map(|(title, _, _)| title)
        .collect();

    for pair in titles.windows(2) {
        assert!(
            pair[0].to_lowercase() <= pair[1].to_lowercase(),
            "{} comes before {}: {titles:?}",
            pair[0],
            pair[1]
        );
    }
    // Every visible command is listed without a query, the end of the alphabet too.
    let visible = window
        .read_with(cx, |workspace, _| workspace.commands().visible().count())
        .unwrap();
    assert_eq!(
        titles.len(),
        visible,
        "every visible command is listed without a query"
    );
    assert!(
        titles.iter().any(|title| title == "Toggle Theme"),
        "{titles:?}"
    );
}

#[ui_kit::gpui::test]
fn find_action_shows_the_icon_of_the_menu_row(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-icons");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let rows = commands(cx, window);

    assert_eq!(icon_of(&rows, "Cut"), "general/cut", "{rows:?}");
    assert_eq!(icon_of(&rows, "Copy"), "general/copy", "{rows:?}");
    assert_eq!(icon_of(&rows, "Paste"), "general/paste", "{rows:?}");
    assert_eq!(icon_of(&rows, "Fetch"), "general/download", "{rows:?}");
    // Undo has no icon in its menu row, so the row has none.
    assert_eq!(icon_of(&rows, "Undo"), "", "{rows:?}");
}
