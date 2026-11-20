//! The Data Sources window copies the user name, and Ctrl+C in its password field puts nothing
//! on the clipboard: the password field takes the copy keys and copies nothing.
//!
//! cargo test -p panels window_tests_database_copy

use ui_kit::gpui::{self, ClipboardItem, TestAppContext};

use crate::database_sources::Field;
use crate::window_tests::{click, draw, press};
use crate::window_tests_database_sources::{StandIn, field_text, open, retype};

/// The text on the clipboard now.
fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

#[ui_kit::gpui::test]
fn ctrl_c_copies_the_user_name_and_nothing_from_the_password(cx: &mut TestAppContext) {
    let (window, _panel) = open(
        cx,
        "database-sources-copy",
        StandIn::support("database-sources-copy-jar"),
        Vec::new(),
    );
    // `+` opens a new connection in the Data Sources window.
    click(cx, window, "database-add");
    draw(cx, window);

    retype(cx, window, "database-field-2", "alice");
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-c");
    draw(cx, window);
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("alice"),
        "Ctrl+C in the user field copies the user name"
    );

    cx.write_to_clipboard(ClipboardItem::new_string("before".to_owned()));
    retype(cx, window, "database-field-3", "secret");
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-c");
    draw(cx, window);
    assert_eq!(
        field_text(cx, window, Field::Password).as_deref(),
        Some("secret"),
        "the password is typed into its field"
    );
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("before"),
        "Ctrl+C in the password field puts nothing on the clipboard"
    );
}
