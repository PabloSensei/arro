//! Find Action gives a command that opens a tool window the icon of that window, so Beans
//! shows its own leaf and not the class icon. A command without an icon of its window keeps
//! the icon of its menu row, or none.
//!
//! cargo test -p panels window_tests_find_action_tools

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

/// The icon of the command with this title.
fn icon_of(rows: &[(String, Option<String>, &'static str)], title: &str) -> &'static str {
    rows.iter()
        .find(|(row, _, _)| row == title)
        .unwrap_or_else(|| panic!("no {title} in Find Action: {rows:?}"))
        .2
}

#[ui_kit::gpui::test]
fn a_command_that_opens_a_tool_window_has_the_icon_of_that_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-tool-icons");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let rows = commands(cx, window);

    assert_eq!(
        icon_of(&rows, "Beans"),
        "run/springBoot",
        "Beans has the leaf of its window: {rows:?}"
    );
    assert_ne!(icon_of(&rows, "Beans"), "nodes/class", "{rows:?}");
    for (title, icon) in [
        ("Bookmarks", "toolwindows/bookmarks"),
        ("Database", "toolwindows/database"),
        ("Endpoints", "toolwindows/endpoints"),
        ("Git Log", "general/vcs"),
        ("Show History", "general/vcs"),
        ("Show in Console", "general/vcs"),
        ("Open Console", "toolwindows/run"),
        ("Project Tool Window", "toolwindows/project"),
        ("Terminal Tool Window", "toolwindows/terminal"),
        ("AI Assistant Tool Window", "own/aiSessions"),
    ] {
        assert_eq!(icon_of(&rows, title), icon, "{title}: {rows:?}");
    }
}

#[ui_kit::gpui::test]
fn a_command_without_an_icon_of_its_window_keeps_its_menu_icon_or_none(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-tool-fallback");
    open_palette(cx, window, keys::FIND_ACTION);
    draw(cx, window);
    let rows = commands(cx, window);

    assert_eq!(icon_of(&rows, "Cut"), "general/cut", "{rows:?}");
    assert_eq!(icon_of(&rows, "Documentation"), "", "{rows:?}");
    assert_eq!(icon_of(&rows, "Show Performance Monitor"), "", "{rows:?}");
}
