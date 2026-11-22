//! Find in Files draws a row of code as one run of text with the matches of the search
//! inside it, and the preview under the list paints every match of the search, with the
//! one of the selected row drawn apart.
//!
//! `cargo test -p panels window_tests_find_in_files_highlight`

use std::path::Path;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

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

/// The file of the project with one line that holds the match in the middle, and the
/// window of Find in Files over it.
fn window_with_line(cx: &mut TestAppContext, name: &str, line: &str) -> WindowHandle<Workspace> {
    let text = format!("{line}\n");
    open_finder(cx, name, &[("Owner.txt", text.as_str())])
}

/// A row of the list is one run of text: the letters of the match keep the places they
/// have in the line, and no gap of the row stands between them.
#[gpui::test]
fn a_result_row_is_one_run_of_text_with_the_match_inside(cx: &mut TestAppContext) {
    let name = "findPaginated";
    let line = "Page<Owner> ownersResults = findPaginated ForOwnersLastName(page, lastName);";
    let window = window_with_line(cx, "find-hit-spacing", line);
    let finder = finder(cx);
    search(cx, &finder, name);
    draw(cx, window);

    let (drawn, ranges) = finder.read_with(cx, |finder, _| {
        finder
            .hit_text_at(1)
            .expect("the first row under the file row is a hit")
    });
    let start = line.find(name).expect("the line holds the match");
    assert_eq!(drawn, line, "the row keeps the line as one run of text");
    assert_eq!(
        ranges,
        vec![start..start + name.len()],
        "the match keeps the bytes of the line"
    );
    assert!(
        drawn[ranges[0].end..].starts_with(" ForOwnersLastName"),
        "the text after the match starts right at its edge"
    );
}

/// The preview paints every match of the query in the file, and the match of the selected
/// row has a run of its own, drawn with the colour of the current match.
#[gpui::test]
fn the_preview_paints_every_match_and_the_selected_one_apart(cx: &mut TestAppContext) {
    let text = "first needle\nsecond needle at the end needle\n";
    let window = open_finder(cx, "find-preview-matches", &[("a.txt", text)]);
    let finder = finder(cx);
    search(cx, &finder, "needle");
    draw(cx, window);

    let preview = finder.read_with(cx, |finder, _| finder.preview().clone());
    let path = finder.read_with(cx, |finder, _| finder.root().join("a.txt"));
    wait_for_preview(cx, &preview, &path, "needle");
    draw(cx, window);

    let (all, current) = preview.read_with(cx, |preview, _| {
        (preview.match_highlight_texts(), preview.highlight_text())
    });
    assert_eq!(
        all,
        vec!["needle".to_owned(); 3],
        "every match of the query is painted, not only the selected one"
    );
    assert_eq!(
        current.as_deref(),
        Some("needle"),
        "the match of the selected row is painted apart"
    );
}
