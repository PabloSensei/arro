//! The preview of Find in Files marks the bytes of a match alone, even when the query is
//! a part of a longer word, and the word at the caret keeps no mark of its own.
//!
//! `cargo test -p panels window_tests_find_in_files_preview_match`

use std::path::Path;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext};

use crate::preview::FilePreview;
use crate::window_tests::{draw, wait_for_signal};
use crate::window_tests_finder_scope::{finder, open_finder, search};

/// Waits until the preview shows `path` with the match of the selected row highlighted.
fn wait_for_preview(
    cx: &mut TestAppContext,
    preview: &Entity<FilePreview>,
    path: &Path,
    text: &str,
) {
    wait_for_signal(cx, "the preview of the selected row", |cx| {
        preview.read_with(cx, |preview, _| {
            preview.is_ready()
                && preview.path() == Some(path)
                && preview.highlight_text().as_deref() == Some(text)
        })
    });
}

/// A query that matches a part of an identifier paints only those bytes, with the colour
/// of the current match; the word at the caret is not painted over them.
#[gpui::test]
fn the_preview_marks_the_match_inside_a_word_and_not_the_word(cx: &mut TestAppContext) {
    let query = "findPaginated";
    let line = "Page<Owner> ownersResults = findPaginatedForOwnersLastName(page, lastName);\n";
    let window = open_finder(
        cx,
        "find-preview-part-of-word",
        &[("OwnerController.txt", line)],
    );
    let finder = finder(cx);
    search(cx, &finder, query);
    draw(cx, window);

    let preview = finder.read_with(cx, |finder, _| finder.preview().clone());
    let path = finder.read_with(cx, |finder, _| finder.root().join("OwnerController.txt"));
    wait_for_preview(cx, &preview, &path, query);
    draw(cx, window);

    let (current, all) = preview.read_with(cx, |preview, _| {
        (preview.highlight_text(), preview.match_highlight_texts())
    });
    assert_eq!(
        current.as_deref(),
        Some(query),
        "the current match is the found bytes, not the whole identifier"
    );
    assert_eq!(
        all,
        vec![query.to_owned()],
        "every mark of the search is the found bytes, not the word around them"
    );

    let editor = preview
        .read_with(cx, |preview, _| preview.editor().cloned())
        .expect("the preview has an editor");
    assert!(
        !editor.read_with(cx, |editor, _| editor.options().highlight_occurrences),
        "the preview paints no occurrences of the word at the caret"
    );
    let start = line.find(query).expect("the line holds the query");
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (0, line[..start].chars().count()),
        "the caret stands at the start of the match"
    );
}
