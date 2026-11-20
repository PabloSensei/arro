//! Alt+Insert keeps the Getter, Setter and Implement Methods rows in a class. Chosen with
//! nothing to write, such a row says so in the status bar, and the text does not change.
//!
//! cargo test -p panels window_tests_generate_status

use std::path::Path;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{buffer_of, draw, editor_of, press};
use crate::window_tests_generate_list::{GenerateLanguage, alt_insert, open_window, project};
use crate::window_tests_ui::wait_for;

/// The message of the status bar, once a row wrote it.
fn status_message(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.status_message().map(|text| text.to_string())
        })
        .unwrap()
}

/// Opens the Alt+Insert list at the caret inside the class and chooses the row `label`.
fn choose(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path, label: &str) {
    let rows = alt_insert(cx, window, path, 0, 9).expect("the list opens");
    let index = rows
        .iter()
        .position(|row| row == label)
        .unwrap_or_else(|| panic!("no {label} row in {rows:?}"));
    for _ in 0..index {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
}

#[ui_kit::gpui::test]
fn a_getter_row_with_nothing_to_write_says_so_and_changes_no_text(cx: &mut TestAppContext) {
    let text = "class A {\n    private String name;\n    public String getName() {\n        return name;\n    }\n}\n";
    let (root, path) = project("generate-status-getter", text);
    let window = open_window(cx, root, GenerateLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    choose(cx, window, &path, "Getter");
    wait_for(cx, "the status message", |cx| {
        status_message(cx, window).is_some()
    });
    assert_eq!(
        status_message(cx, window).as_deref(),
        Some("No fields without getter were found")
    );
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "choosing the Getter row changes no text"
    );
}

#[ui_kit::gpui::test]
fn implement_methods_with_nothing_to_implement_says_so(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("generate-status-implement", text);
    let window = open_window(cx, root, GenerateLanguage::new());
    cx.run_until_parked();
    draw(cx, window);

    choose(cx, window, &path, "Implement Methods");
    wait_for(cx, "the status message", |cx| {
        status_message(cx, window).is_some()
    });
    assert_eq!(
        status_message(cx, window).as_deref(),
        Some("No methods to implement")
    );
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "choosing Implement Methods changes no text"
    );
}
