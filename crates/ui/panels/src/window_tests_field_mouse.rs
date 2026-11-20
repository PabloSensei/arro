//! T-0368: the mouse in the fields of the windows. The pointer is simulated, not the methods
//! of the field: a click puts the caret where the pointer is, a drag selects, and the keys
//! copy the selection. The carets are read from the screen, so no glyph width is assumed.
//!
//! `cargo test -p panels window_tests_field_mouse`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, TestAppContext,
    VisualTestContext, WindowHandle,
};
use workspace::COMMIT_TOOL;

use crate::database_sources::Field;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{
    bounds_of, click, draw, open_path, open_sample_window, open_window, press, type_text,
};
use crate::window_tests_database_data::{count, data_tab, owners, page, table, wait_for_rows};
use crate::window_tests_database_sources::{StandIn, field_text, on_screen, open, retype};

/// A click in the filter of the Database table puts the caret at the pointer, and typing
/// goes in there.
#[ui_kit::gpui::test]
fn a_click_in_the_filter_puts_the_caret_there_and_typing_goes_in_at_it(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "field-mouse-filter",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    click(cx, window, "database-data-filter");
    draw(cx, window);
    type_text(cx, window, "owner");
    draw(cx, window);
    press(cx, window, "home right right");
    draw(cx, window);
    // The caret bar stands on the boundary after "ow": its centre is the pointer's place.
    let boundary = bounds_of(cx, window, "database-data-filter-field-caret").center();
    press(cx, window, "end");
    draw(cx, window);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(boundary, Modifiers::default());
    cx.run_until_parked();
    type_text(cx, window, "X");
    draw(cx, window);

    assert_eq!(
        view.read_with(cx, |view, _| view.filter_for_test()),
        "owXner"
    );
}

/// A right click on the filter opens Cut, Copy, Paste and Select All, and Paste inserts the
/// text of the clipboard at the caret.
#[ui_kit::gpui::test]
fn a_right_click_opens_the_field_menu_and_paste_inserts_the_clipboard(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "field-mouse-menu",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);
    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("pasted".to_owned())));

    let centre = bounds_of(cx, window, "database-data-filter").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(centre, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(centre, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    for row in [
        "field-menu-cut",
        "field-menu-copy",
        "field-menu-paste",
        "field-menu-select-all",
    ] {
        assert!(on_screen(cx, window, row), "the menu has {row}");
    }

    click(cx, window, "field-menu-paste");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.filter_for_test()),
        "pasted"
    );
}

/// A drag over the User field selects the text between the two pointers, and Ctrl+C copies
/// exactly that selection.
#[ui_kit::gpui::test]
fn a_drag_selects_part_of_the_user_field_and_ctrl_c_copies_it(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "field-mouse-user",
        StandIn::support("field-mouse-user"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-2", "owners-main");

    press(cx, window, "home right right");
    draw(cx, window);
    let from = bounds_of(cx, window, "data-sources-field-caret").center();
    press(cx, window, "right right right right right");
    draw(cx, window);
    let to = bounds_of(cx, window, "data-sources-field-caret").center();

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        on_screen(cx, window, "data-sources-field-selection"),
        "the drag draws the selection"
    );

    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    // "owners-main": the selection is bytes 2 to 7.
    assert_eq!(copied.as_deref(), Some("ners-"));
    assert_eq!(
        field_text(cx, window, Field::User).as_deref(),
        Some("owners-main"),
        "a copy leaves the text"
    );
}

/// A double click selects the word under the pointer, and a Shift+click grows the selection
/// from the caret to the pointer. Both are read back with Ctrl+C.
#[ui_kit::gpui::test]
fn a_double_click_selects_a_word_and_shift_click_extends_the_selection(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "field-mouse-words",
        StandIn::support("field-mouse-words"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    retype(cx, window, "database-field-2", "owners-main");

    // Double click inside "owners": the word is bytes 0 to 6.
    press(cx, window, "home right right");
    draw(cx, window);
    let inside = bounds_of(cx, window, "data-sources-field-caret").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for click_count in [1, 2] {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: inside,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: inside,
            modifiers: Modifiers::default(),
            click_count,
        });
    }
    cx.run_until_parked();
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("owners"),
        "a double click takes the word"
    );

    // Shift+click from the caret at 0 to the boundary after "owners-".
    press(cx, window, "home");
    draw(cx, window);
    let start = bounds_of(cx, window, "data-sources-field-caret").center();
    press(cx, window, "right right right right right right right");
    draw(cx, window);
    let end = bounds_of(cx, window, "data-sources-field-caret").center();
    press(cx, window, "home");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(start, Modifiers::default());
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    visual.simulate_click(end, shift);
    cx.run_until_parked();
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("owners-"),
        "Shift+click extends the selection"
    );
}

/// A click on the second line of the commit message puts the caret on that line, and typing
/// goes in there.
#[ui_kit::gpui::test]
fn a_click_on_the_second_line_of_the_commit_message_puts_the_caret_there(cx: &mut TestAppContext) {
    let (root, _bare, _git) = crate::window_tests_git::repository_with_origin("field-mouse-commit");
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = crate::window_tests_git::git_state(cx);
    crate::window_tests_git::wait_for_repository(cx, &state);

    let panel = cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();

    type_text(cx, window, "one");
    press(cx, window, "enter");
    type_text(cx, window, "two");
    press(cx, window, "enter");
    type_text(cx, window, "three");
    press(cx, window, "up home");
    draw(cx, window);
    // The caret stands at the start of the second line; its bar is the pointer's place.
    let second_start = bounds_of(cx, window, "commit-message-caret").center();
    press(cx, window, "end");
    draw(cx, window);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(second_start, Modifiers::default());
    cx.run_until_parked();
    type_text(cx, window, "X");
    draw(cx, window);

    assert_eq!(
        panel.read_with(cx, |panel, _| panel.message().to_owned()),
        "one\nXtwo\nthree"
    );
}

/// Two clicks at `at`, the second one counted as a double click.
fn double_click<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, at: Point<Pixels>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for click_count in [1, 2] {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::default(),
            click_count,
        });
    }
    cx.run_until_parked();
}

/// Escape closes the field menu and nothing else: the filter keeps its text.
#[ui_kit::gpui::test]
fn escape_closes_the_field_menu_and_nothing_else(cx: &mut TestAppContext) {
    let (window, panel) = data_tab(
        cx,
        "field-mouse-escape",
        table(),
        vec![page(owners()), count(2)],
    );
    let view = wait_for_rows(cx, &panel, &table(), 2);

    let centre = bounds_of(cx, window, "database-data-filter").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(centre, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(centre, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(on_screen(cx, window, "field-menu-cut"), "the menu is open");

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "field-menu-cut"),
        "Escape closes the menu"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.filter_for_test()),
        "",
        "the filter keeps its text"
    );
}

/// A double click in the Find in Files field selects the word under the pointer, and Ctrl+C
/// copies it.
#[ui_kit::gpui::test]
fn a_double_click_in_the_find_in_files_field_selects_the_word(cx: &mut TestAppContext) {
    let window = crate::window_tests_finder_polish::open_finder(
        cx,
        "field-mouse-finder",
        &[("notes.txt", "owners-main\n")],
    );
    type_text(cx, window, "owners-main");
    draw(cx, window);
    press(cx, window, "home right right");
    draw(cx, window);
    let inside = bounds_of(cx, window, "finder-input-find-caret").center();
    double_click(cx, window, inside);
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    // The caret stands inside "owners", so the word is bytes 0 to 6.
    assert_eq!(
        copied.as_deref(),
        Some("owners"),
        "a double click takes the word"
    );
}

/// A click in a Settings value puts the caret there, and a Shift+click takes the selection to
/// the pointer.
#[ui_kit::gpui::test]
fn a_shift_click_in_a_settings_value_extends_the_selection(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-mouse-settings");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-terminal");
    draw(cx, window);
    click(cx, window, "settings-terminal-shell");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "bash");
    draw(cx, window);

    // The start is the boundary after "b", the end the boundary after "h".
    press(cx, window, "home right");
    draw(cx, window);
    let start = bounds_of(cx, window, "settings-field-caret").center();
    press(cx, window, "end");
    draw(cx, window);
    let end = bounds_of(cx, window, "settings-field-caret").center();

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(start, Modifiers::default());
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    visual.simulate_click(end, shift);
    cx.run_until_parked();
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("ash"),
        "Shift+click extends the selection to the pointer"
    );
}

/// A double click in the find bar selects the word under the pointer, and Ctrl+C copies it.
#[ui_kit::gpui::test]
fn a_double_click_in_the_find_bar_selects_the_word(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "field-mouse-find-bar");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "alpha\nbeta\ngamma alpha\n").unwrap();
    open_path(cx, window, &path);
    press(cx, window, "ctrl-f");
    // The editor selects the match of "gamma alpha"; the copy must show the query instead.
    type_text(cx, window, "gamma alpha");
    draw(cx, window);
    press(
        cx,
        window,
        "home right right right right right right right right",
    );
    draw(cx, window);
    let inside = bounds_of(cx, window, "bar-query-caret").center();
    double_click(cx, window, inside);
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("alpha"),
        "a double click takes the word"
    );
}

/// The first click on a Settings value starts editing and puts the caret where the pointer is,
/// in the same click: Shift+Home then copies the text before the pointer.
#[ui_kit::gpui::test]
fn a_first_click_on_a_settings_value_starts_editing_and_places_the_caret(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "field-mouse-first-click");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-terminal");
    draw(cx, window);
    click(cx, window, "settings-terminal-shell");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, "bash");
    press(cx, window, "home right right");
    draw(cx, window);
    // The boundary after "ba": the pointer goes there.
    let inside = bounds_of(cx, window, "settings-field-caret").center();
    // A page tab ends the editing; the text stays, since a value is applied as it is typed.
    click(cx, window, "settings-page-keymap");
    draw(cx, window);
    click(cx, window, "settings-page-terminal");
    draw(cx, window);
    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("none".to_owned())));

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(inside, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        on_screen(cx, window, "settings-field-caret"),
        "the first click starts the editing"
    );
    press(cx, window, "shift-home");
    press(cx, window, "ctrl-c");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some("ba"),
        "the caret stands where the first click was"
    );
}
