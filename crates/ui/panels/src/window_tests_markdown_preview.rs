//! The preview of a markdown file beside its editor: the three modes of the tab, the
//! rendered document that follows the text while typing, the links inside it and the
//! scroll that keeps the editor and the preview on the same block.

use std::fs;

use editor_view::{DocBlock, DocInline, PreviewMode, parse_documentation};
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
fn markdown_file(root: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::write(&path, text).unwrap();
    path
}

#[ui_kit::gpui::test]
fn a_markdown_tab_shows_the_rendered_document_beside_the_text(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-open");
    let path = markdown_file(&root, "guide.md", "# Guide\n\nSome **bold** words.\n");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Guide");
    draw(cx, window);

    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::EditorAndPreview),
        "a markdown tab opens with the editor and the preview"
    );
    let editor = bounds_of(cx, window, "editor-text");
    let preview = bounds_of(cx, window, "markdown-preview-pane");
    assert!(
        preview.left() >= editor.right() - px(1.),
        "the preview stands to the right of the text"
    );
    assert!(
        view.read_with(cx, |view, _| view
            .preview_text()
            .is_some_and(|text| text.contains("bold"))),
        "the preview draws the words of the document"
    );
}

#[ui_kit::gpui::test]
fn a_plain_file_has_no_preview(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-plain");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    draw(cx, window);

    let view = editor_of(cx, window, &path);
    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        None,
        "only a markdown file has the modes of a preview"
    );
    assert!(!shown(cx, window, "markdown-preview-pane"));
    assert!(!shown(cx, window, "markdown-mode-editor"));
}

#[ui_kit::gpui::test]
fn typing_parses_the_preview_again(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-typing");
    let path = markdown_file(&root, "notes.md", "# Notes\n\nold words\n");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "old words");
    assert!(
        view.read_with(cx, |view, _| view
            .preview_text()
            .is_some_and(|text| !text.contains("new"))),
        "the preview starts from the text of the file"
    );

    type_text(cx, window, "new ");
    wait_for_preview(cx, &view, "new");
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view
            .preview_text()
            .is_some_and(|text| text.contains("new") && text.contains("old words"))),
        "the preview holds both the typed words and the old ones"
    );
}

#[ui_kit::gpui::test]
fn the_mode_buttons_hide_and_show_the_panels(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-modes");
    let path = markdown_file(&root, "guide.md", "# Guide\n\nWords.\n");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Guide");
    draw(cx, window);

    click(cx, window, "markdown-mode-editor");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::EditorOnly)
    );
    assert!(shown(cx, window, "editor-text"));
    assert!(!shown(cx, window, "markdown-preview-pane"));

    click(cx, window, "markdown-mode-preview");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::PreviewOnly)
    );
    assert!(!shown(cx, window, "editor-text"));
    assert!(shown(cx, window, "markdown-preview-pane"));

    click(cx, window, "markdown-mode-split");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::EditorAndPreview)
    );
    assert!(shown(cx, window, "editor-text"));
    assert!(shown(cx, window, "markdown-preview-pane"));
}

#[ui_kit::gpui::test]
fn a_relative_link_of_the_preview_opens_the_file(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-link");
    let other = markdown_file(&root, "other.md", "# Other\n");
    let path = markdown_file(
        &root,
        "guide.md",
        "See [the other page](other.md) for more.\n",
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
        "a link to a file of the project opens it in the window"
    );
}

#[ui_kit::gpui::test]
fn the_preview_follows_the_editor_to_the_block_at_the_top(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-scroll");
    let mut text = String::from("# First\n\n");
    for index in 0..40 {
        text.push_str(&format!("Line {index} of the first part.\n\n"));
    }
    text.push_str("## Second section\n\n");
    for index in 0..60 {
        text.push_str(&format!("Line {index} of the second part.\n\n"));
    }
    let path = markdown_file(&root, "guide.md", &text);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Second section");
    draw(cx, window);

    let second = text
        .lines()
        .position(|line| line == "## Second section")
        .expect("the second heading");
    let expected = parse_documentation(&text)
        .iter()
        .position(|block| {
            matches!(
                block,
                DocBlock::Heading { level: 2, text }
                    if text.iter().any(|inline| {
                        matches!(inline, DocInline::Text(words) if words == "Second section")
                    })
            )
        })
        .expect("the preview has the heading as a block");

    let height = line_height(cx);
    view.update(cx, |view, cx| {
        view.scroll_to_top(second as f32 * height, cx);
    });
    draw(cx, window);
    draw(cx, window);

    assert_eq!(
        view.read_with(cx, |view, _| view.preview_block()),
        Some(expected),
        "the preview moved to the block the editor shows"
    );
    assert!(
        view.read_with(cx, |view, _| view.preview_scroll()) > 0.,
        "the panel of the preview is scrolled"
    );
}

#[ui_kit::gpui::test]
fn an_anchor_link_scrolls_the_preview_to_its_heading(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-anchor");
    let mut text = String::from("[Second](#second-section)\n\n# First\n\n");
    for index in 0..40 {
        text.push_str(&format!("Line {index} of the first part.\n\n"));
    }
    text.push_str("## Second section\n\n");
    for index in 0..60 {
        text.push_str(&format!("Line {index} of the second part.\n\n"));
    }
    let path = markdown_file(&root, "guide.md", &text);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Second section");
    draw(cx, window);
    let before = view.read_with(cx, |view, _| view.preview_scroll());
    assert!(
        before < 20.,
        "the preview starts at its first block, not at {before}"
    );

    click(cx, window, "markdown-preview-link-0-0");
    draw(cx, window);
    let after = view.read_with(cx, |view, _| view.preview_scroll());
    assert!(
        after > before + 100.,
        "an anchor link scrolls the preview to its heading: {before} became {after}"
    );
}

/// The share of the width of the tab a panel takes.
fn width_share(panel: gpui::Bounds<gpui::Pixels>, tab: gpui::Bounds<gpui::Pixels>) -> f32 {
    f32::from(panel.size.width) / f32::from(tab.size.width).max(1.)
}

#[ui_kit::gpui::test]
fn the_panels_of_the_modes_fill_the_tab(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "markdown-preview-sizes");
    let path = markdown_file(
        &root,
        "guide.md",
        "# Guide\n\nThe file has these words.\n\nThe end.\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Guide");
    // The second frame lays the preview out once its blocks are parsed.
    draw(cx, window);
    draw(cx, window);

    let tab = bounds_of(cx, window, "editor-body-0");
    let editor = bounds_of(cx, window, "editor-text");
    let preview = bounds_of(cx, window, "markdown-preview-pane");
    assert!(
        width_share(editor, tab) > 0.33,
        "in the split the text takes a third of the tab at least: {editor:?} in {tab:?}"
    );
    assert!(
        width_share(preview, tab) > 0.33,
        "in the split the preview takes a third of the tab at least: {preview:?} in {tab:?}"
    );
    assert!(
        editor.size.height > tab.size.height / 2.,
        "the text takes half the height of the tab: {editor:?} in {tab:?}"
    );
    assert!(
        preview.size.height > tab.size.height / 2.,
        "the preview takes half the height of the tab: {preview:?} in {tab:?}"
    );

    // The rows the editor painted hold the lines of the file: a text without room to
    // stand in paints none of them, however the elements are there.
    let drawn: Vec<(usize, String)> = view.read_with(cx, |view, _| {
        let mut drawn = Vec::new();
        for row in view.row_paint() {
            if !row.block
                && let Some(text) = view.line_text(row.line)
            {
                drawn.push((row.line, text));
            }
        }
        drawn
    });
    assert!(
        drawn
            .iter()
            .any(|(line, text)| *line == 2 && text.contains("these words")),
        "the painted rows show the text of the file: {drawn:?}"
    );

    click(cx, window, "markdown-mode-editor");
    draw(cx, window);
    let editor = bounds_of(cx, window, "editor-text");
    assert!(
        width_share(editor, tab) > 0.9,
        "the text alone takes the whole tab: {editor:?} in {tab:?}"
    );

    click(cx, window, "markdown-mode-preview");
    draw(cx, window);
    let preview = bounds_of(cx, window, "markdown-preview-pane");
    assert!(
        width_share(preview, tab) > 0.9,
        "the preview alone takes the whole tab: {preview:?} in {tab:?}"
    );
}
