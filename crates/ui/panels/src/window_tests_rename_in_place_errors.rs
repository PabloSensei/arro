//! Rename in place while the language cannot answer about the name, and when it refuses it.
//! The editor gets the answer of the language, not its error text: an error that says the
//! name cannot be renamed comes as a refusal, and any other error comes as "unknown". The
//! router's share of that is in `semantic-router/tests/prepare_rename.rs`; these tests show
//! what the window does with each answer.
//!
//! cargo test -p panels window_tests_rename_in_place_errors

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use lang_api::Confidence;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, type_text};
use crate::window_tests_copy_class::shown;
use crate::window_tests_rename_in_place::{METHOD, RENAMED, RenameLanguage, open_window, project};

/// The message of the status bar when the language refuses the name.
const REFUSED: &str = "This name cannot be renamed";

#[ui_kit::gpui::test]
fn a_language_that_cannot_answer_yet_keeps_the_mode_and_the_name_follows_the_typing(
    cx: &mut TestAppContext,
) {
    let root = project(
        "rename-in-place-not-ready-yet",
        &[("src/Owner.txt", METHOD)],
    );
    let owner = root.join("src").join("Owner.txt");
    let language = RenameLanguage::new();
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    type_text(cx, window, "Zz");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), RENAMED);

    // The language is not ready: its answer is "unknown", which is not a refusal.
    language.answer_held(None, Confidence::Unknown);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "the mode goes on after the language said it is not ready"
    );
    assert_eq!(
        buffer_of(cx, &view),
        RENAMED,
        "the typed name stays in place"
    );
    type_text(cx, window, "x");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        RENAMED.replace("Zz", "Zzx"),
        "every place still follows the typing"
    );
    assert_ne!(
        window
            .read_with(cx, |workspace, _| {
                workspace.language_status().map(ToString::to_string)
            })
            .unwrap()
            .as_deref(),
        Some(REFUSED),
        "the status bar does not refuse the name"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), METHOD, "Escape puts the text back");
    assert!(language.queries().is_empty(), "nothing was asked to rename");
}

#[ui_kit::gpui::test]
fn a_refusal_that_says_the_name_cannot_be_renamed_ends_the_mode_at_once(cx: &mut TestAppContext) {
    let root = project(
        "rename-in-place-refusal-at-once",
        &[("src/Owner.txt", METHOD)],
    );
    let owner = root.join("src").join("Owner.txt");
    let language = RenameLanguage::new();
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "the mode starts without the answer of the language"
    );

    language.answer_held(None, Confidence::Certain);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "the refusal ends the mode"
    );
    assert_eq!(buffer_of(cx, &view), METHOD, "the name was never changed");
    assert!(
        !view.read_with(&*cx, |view, _| view.is_dirty()),
        "the file is not changed"
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| {
                workspace.language_status().map(ToString::to_string)
            })
            .unwrap()
            .as_deref(),
        Some(REFUSED),
        "the status bar says why"
    );
}
