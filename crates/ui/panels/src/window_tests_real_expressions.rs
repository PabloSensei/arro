//! Introduce Variable on the real language server: the expressions around the caret inside a call.
//!
//! The corpus and the package cache are the only outside world; the project is copied, so
//! nothing changes in the corpus. The module is not part of a normal test run:
//!
//! `cargo test -p panels window_tests_real_expressions -- --ignored --test-threads=1`

use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_real::{build_window, close, finish, project, wait_for, wait_ready};

/// One answer of the server about the expressions, after the import.
const LIMIT: Duration = Duration::from_secs(120);

/// The line of `String lastName = owner.getLastName();` in OwnerController, counted from 0.
const CALL_LINE: usize = 97;
/// A column inside `owner` of that line, where the caret is put: the line is tab-indented.
const OWNER_COLUMN: usize = 22;
/// A column inside `getLastName` of that line, the place the walk of T-0447 had its caret.
const NAME_COLUMN: usize = 29;

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_call_lists_its_receiver_and_itself_on_the_real_server(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-expressions");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    // The language name is put together from two parts: this neutral crate does not spell it.
    let lang = format!("{}{}", "ja", "va");
    let path = root.join(format!(
        "src/main/{lang}/org/springframework/samples/petclinic/owner/OwnerController.{lang}"
    ));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let mut lists = Vec::new();
    for column in [OWNER_COLUMN, NAME_COLUMN] {
        view.update(cx, |view, cx| view.restore_caret(CALL_LINE, column, cx));
        draw(cx, window);
        press(cx, window, "ctrl-alt-v");
        wait_for(cx, "the list of expressions", LIMIT, |cx| {
            view.read_with(cx, |view, _| view.actions_visible().is_some())
        });
        draw(cx, window);
        let title = view.read_with(cx, |view, _| view.actions_title());
        let labels: Vec<String> = view.read_with(cx, |view, _| {
            view.actions_visible()
                .map(|rows| rows.rows.into_iter().map(|(_, label)| label).collect())
                .unwrap_or_default()
        });
        lists.push((column, title, labels));
        press(cx, window, "escape");
        draw(cx, window);
    }

    // The window closes before the asserts, so that a failure leaves no server running.
    close(cx, window, language, false);
    for (column, title, labels) in lists {
        assert_eq!(title, Some("Expressions"), "the caret at column {column}");
        assert_eq!(
            labels,
            ["owner", "owner.getLastName()"],
            "the caret at column {column}"
        );
    }
    finish(
        "the_call_lists_its_receiver_and_itself_on_the_real_server",
        started,
    );
}
