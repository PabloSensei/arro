//! The Keymap page as the tree of IDEA: the place of an action in the menu of the window,
//! the search that opens the tree to what it found, the buttons of the chosen row and the
//! names the shortcuts are written with.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, type_text};

/// Opens the project and the Keymap page of its Settings window.
fn open_keymap_page(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-keymap");
    draw(cx, window);
    window
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The place a row of the tree was drawn in, as `Main Menu / Edit`.
fn place_of(action: &str) -> Option<String> {
    let name = format!("keymap-place-{action}");
    ui_kit::test_bounds()
        .into_iter()
        .find(|(drawn, _)| *drawn == name)
        .and_then(|(_, probe)| probe.label)
        .map(|label| label.to_string())
}

/// The shortcuts a row was drawn with, as one line of the column at the right.
fn shortcuts_of(action: &str) -> Option<String> {
    let name = format!("keymap-shortcuts-label-{action}");
    ui_kit::test_bounds()
        .into_iter()
        .find(|(drawn, _)| *drawn == name)
        .and_then(|(_, probe)| probe.label)
        .map(|label| label.to_string())
}

#[gpui::test]
fn undo_stands_in_the_edit_menu_and_the_search_opens_the_tree_to_it(cx: &mut TestAppContext) {
    let window = open_keymap_page(cx, "keymap-tree-undo");

    // The search shows what it found in the tree of the groups of the menu.
    type_text(cx, window, "undo");
    draw(cx, window);
    bounds_of(cx, window, "keymap-row-editor-undo");
    bounds_of(cx, window, "keymap-group-main-menu");
    bounds_of(cx, window, "keymap-group-main-menu-edit");
    assert_eq!(
        place_of("editor-undo").as_deref(),
        Some("Main Menu / Edit"),
        "Undo is a row of Edit in the main menu"
    );

    // A section closed by the user opens again while the search asks for what is in it.
    click(cx, window, "keymap-group-main-menu-edit");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    press(cx, window, "backspace");
    draw(cx, window);
    assert!(
        !shown(cx, window, "keymap-row-editor-undo"),
        "the closed menu hides its rows"
    );
    type_text(cx, window, "undo");
    draw(cx, window);
    assert!(
        shown(cx, window, "keymap-row-editor-undo"),
        "the search opens the tree to the row it found"
    );
}

#[gpui::test]
fn the_buttons_of_a_row_stand_on_the_chosen_row_only(cx: &mut TestAppContext) {
    let window = open_keymap_page(cx, "keymap-tree-buttons");
    type_text(cx, window, "copy");
    draw(cx, window);

    assert!(
        !shown(cx, window, "keymap-add-editor-copy"),
        "a row that is neither chosen nor under the pointer shows no buttons"
    );
    click(cx, window, "keymap-row-editor-copy");
    draw(cx, window);
    bounds_of(cx, window, "keymap-remove-editor-copy");
    bounds_of(cx, window, "keymap-add-editor-copy");
    bounds_of(cx, window, "keymap-reset-editor-copy");
}

#[gpui::test]
fn a_numpad_shortcut_is_written_as_the_menus_write_it(cx: &mut TestAppContext) {
    let window = open_keymap_page(cx, "keymap-tree-numpad");
    type_text(cx, window, "collapse region");
    draw(cx, window);
    bounds_of(cx, window, "keymap-row-editor-collapse_region_recursively");
    assert_eq!(
        shortcuts_of("editor-collapse_region_recursively").as_deref(),
        Some("Ctrl+Alt+NumPad -, Ctrl+Alt+-"),
        "the shortcuts of the row stand in one column, with the name of the keypad key"
    );
}
