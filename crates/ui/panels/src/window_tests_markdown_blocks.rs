//! The blocks of a markdown file in the preview beside its text: a table wider than the
//! preview wraps its text inside the preview, a table is as wide as its columns need, a
//! `<details>` block is closed until its summary is clicked, and a reference link opens the
//! file of its definition.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::px;
use workspace::Workspace;

use crate::window_tests::{
    active_path, bounds_of, click, draw, editor_of, line_height, open_path, open_sample_window,
    type_text, wait_for_signal,
};

/// Whether an element with a debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Waits until the preview draws words of the text.
fn wait_for_preview(
    cx: &mut TestAppContext,
    view: &ui_kit::gpui::Entity<editor_view::EditorView>,
    words: &str,
) {
    wait_for_signal(cx, "the markdown preview", |cx| {
        view.read_with(cx, |view, _| {
            view.preview_text().is_some_and(|text| text.contains(words))
        })
    });
}

/// A file of the project with the text of the test.
fn markdown_file(root: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, text).unwrap();
    path
}

#[ui_kit::gpui::test]
fn a_table_wider_than_the_preview_wraps_inside_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-wrap");
    let long = "one two three four five six seven eight nine ten eleven twelve thirteen fourteen \
                fifteen sixteen seventeen eighteen nineteen twenty";
    let path = markdown_file(
        &root,
        "wide.md",
        &format!(
            "| Key | Long cell | Other long cell |\n| --- | --- | --- |\n| short | {long} | {long} |\n"
        ),
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "fifteen");
    // The second frame lays the table out once its blocks are parsed.
    draw(cx, window);
    draw(cx, window);

    let pane = bounds_of(cx, window, "markdown-preview-pane");
    let table = bounds_of(cx, window, "markdown-preview-table-0");
    assert!(
        table.right() <= pane.right() + px(1.),
        "the table does not go past the edge of the preview: {table:?} in {pane:?}"
    );
    let header = bounds_of(cx, window, "markdown-preview-table-0-cell-0-0");
    let wrapped = bounds_of(cx, window, "markdown-preview-table-0-cell-1-1");
    assert!(
        wrapped.size.height > header.size.height + px(line_height(cx) * 0.5),
        "the long cell takes lines of its own: {wrapped:?} against the header {header:?}"
    );
}

#[ui_kit::gpui::test]
fn a_small_table_is_as_wide_as_its_columns_need(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-columns");
    let path = markdown_file(
        &root,
        "small.md",
        "| A | Wide column |\n| - | ----------- |\n| x | some words |\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "some words");
    draw(cx, window);
    draw(cx, window);

    let narrow = bounds_of(cx, window, "markdown-preview-table-0-cell-1-0");
    let wide = bounds_of(cx, window, "markdown-preview-table-0-cell-1-1");
    assert!(
        narrow.size.width < wide.size.width,
        "each column is as wide as its text: {narrow:?} and {wide:?}"
    );
    let pane = bounds_of(cx, window, "markdown-preview-pane");
    let table = bounds_of(cx, window, "markdown-preview-table-0");
    assert!(
        table.size.width < pane.size.width * 0.8,
        "a small table is narrower than the preview: {table:?} in {pane:?}"
    );
}

#[ui_kit::gpui::test]
fn a_right_aligned_cell_sits_at_the_right_edge_of_its_column(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-align");
    let path = markdown_file(
        &root,
        "align.md",
        "| Left | Centered | Right-aligned column header |\n| :--- | :---: | ---: |\n| a | b | [site](https://example.com) |\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "site");
    draw(cx, window);
    draw(cx, window);

    let cell = bounds_of(cx, window, "markdown-preview-table-0-cell-1-2");
    let word = bounds_of(cx, window, "markdown-preview-link-0-0");
    assert!(
        cell.right() - word.right() < px(20.),
        "the right-aligned word is at the right of its cell: {word:?} in {cell:?}"
    );
    assert!(
        word.left() - cell.left() > px(100.),
        "the right-aligned word is not at the left of its cell: {word:?} in {cell:?}"
    );
}

#[ui_kit::gpui::test]
fn a_details_block_opens_and_closes_at_a_click_on_its_summary(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-details");
    let path = markdown_file(
        &root,
        "details.md",
        "<details>\n<summary>More</summary>\n\nHidden words\n\n</details>\n\nAfter\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "After");
    draw(cx, window);

    assert!(shown(cx, window, "markdown-preview-details-summary-0"));
    assert!(
        !shown(cx, window, "markdown-preview-details-body-0"),
        "the block is closed at first"
    );

    click(cx, window, "markdown-preview-details-summary-0");
    draw(cx, window);
    assert!(
        shown(cx, window, "markdown-preview-details-body-0"),
        "a click on the summary opens the block"
    );

    click(cx, window, "markdown-preview-details-summary-0");
    draw(cx, window);
    assert!(
        !shown(cx, window, "markdown-preview-details-body-0"),
        "a second click closes it again"
    );
}

#[ui_kit::gpui::test]
fn an_open_details_block_stays_open_when_the_text_is_parsed_again(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-remember");
    let path = markdown_file(
        &root,
        "remember.md",
        "Intro\n\n<details>\n<summary>More</summary>\n\nHidden words\n\n</details>\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Intro");
    draw(cx, window);

    click(cx, window, "markdown-preview-details-summary-0");
    draw(cx, window);
    assert!(shown(cx, window, "markdown-preview-details-body-0"));

    // The text changes at its start, so the whole document is parsed again.
    type_text(cx, window, "x");
    wait_for_preview(cx, &view, "xIntro");
    draw(cx, window);
    draw(cx, window);
    assert!(
        shown(cx, window, "markdown-preview-details-body-0"),
        "the block the reader opened stays open"
    );
}

#[ui_kit::gpui::test]
fn a_reference_link_opens_the_file_of_its_definition(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-blocks-reference");
    let other = markdown_file(&root, "other.md", "# Other\n");
    let path = markdown_file(
        &root,
        "guide.md",
        "See [the other page][page] for more.\n\n[page]: other.md\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "the other page");
    draw(cx, window);

    click(cx, window, "markdown-preview-link-0-0");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        active_path(cx, window),
        Some(other),
        "a reference link opens the file its definition names"
    );
}
