//! The visibility list of the Change Signature window offers only the levels that the place
//! allows: one level leaves the list closed and shows that level, two levels give two rows.
//! The word the server gave goes back unchanged until a level is chosen.

use std::path::Path;

use lang_api::{ChangeSignatureInfo, DocumentSpot, GraphVisibility, TextPoint};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::change_signature::ChangeSignatureDialog;
use crate::window_tests::{click, draw};
use crate::window_tests_lang::uri_of;
use crate::window_tests_signature::{FakeLanguage, open_form, preview_of, shown};

/// Sets the signature the language answers with, its word and the levels the place allows,
/// then opens the form again over the same place.
fn show_with(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    language: &FakeLanguage,
    open: &Path,
    word: &str,
    allowed: &[GraphVisibility],
) {
    let mut info: ChangeSignatureInfo = language.info.lock().unwrap().clone();
    if let Some(signature) = info.signature.as_mut() {
        signature.visibility = word.to_owned();
    }
    info.allowed_visibility = allowed.to_vec();
    *language.info.lock().unwrap() = info;
    let spot = DocumentSpot {
        uri: uri_of(open),
        version: 1,
        point: TextPoint {
            line: 1,
            character: 20,
        },
    };
    window
        .update(cx, |workspace, window, cx| {
            ChangeSignatureDialog::show(spot, workspace, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// The visibility word of the last refactoring that reached the language.
fn sent_visibility(language: &FakeLanguage) -> String {
    language
        .requests()
        .last()
        .expect("the refactoring reached the language")
        .signature
        .visibility
        .clone()
}

#[ui_kit::gpui::test]
fn one_allowed_level_is_shown_and_its_list_stays_closed(cx: &mut TestAppContext) {
    let (window, language, open) = open_form(cx, "change-visibility-one");
    show_with(
        cx,
        window,
        &language,
        &open,
        "",
        &[GraphVisibility::Private],
    );

    click(cx, window, "change-signature-visibility");
    draw(cx, window);
    assert!(
        !shown(cx, window, "change-signature-visibility-menu"),
        "a place with one allowed level has no list to open"
    );
    assert!(
        !shown(cx, window, "change-signature-visibility-option-private"),
        "the one level is shown in the field, not as a row of the list"
    );
    assert_eq!(
        preview_of(cx, window),
        "String combine(String left, String right)",
        "the word the server gave, none, is the preview"
    );

    click(cx, window, "change-signature-refactor");
    cx.run_until_parked();
    assert_eq!(
        sent_visibility(&language),
        "",
        "an untouched visibility goes back as the server gave it"
    );
}

#[ui_kit::gpui::test]
fn two_allowed_levels_give_two_rows_and_the_chosen_one_is_sent(cx: &mut TestAppContext) {
    let (window, language, open) = open_form(cx, "change-visibility-two");
    show_with(
        cx,
        window,
        &language,
        &open,
        "public",
        &[GraphVisibility::Public, GraphVisibility::Private],
    );

    click(cx, window, "change-signature-visibility");
    draw(cx, window);
    for (option, listed) in [
        ("change-signature-visibility-option-public", true),
        ("change-signature-visibility-option-protected", false),
        ("change-signature-visibility-option-package-private", false),
        ("change-signature-visibility-option-private", true),
    ] {
        assert_eq!(
            shown(cx, window, option),
            listed,
            "{option} is listed for two allowed levels"
        );
    }

    click(cx, window, "change-signature-visibility-option-private");
    draw(cx, window);
    assert_eq!(
        preview_of(cx, window),
        "private String combine(String left, String right)"
    );

    click(cx, window, "change-signature-refactor");
    cx.run_until_parked();
    assert_eq!(sent_visibility(&language), "private");
}

#[ui_kit::gpui::test]
fn the_level_of_the_server_is_chosen_when_the_place_allows_it(cx: &mut TestAppContext) {
    let (window, language, open) = open_form(cx, "change-visibility-current");
    show_with(
        cx,
        window,
        &language,
        &open,
        "private",
        &[GraphVisibility::Public, GraphVisibility::Private],
    );
    assert_eq!(
        preview_of(cx, window),
        "private String combine(String left, String right)"
    );
}
