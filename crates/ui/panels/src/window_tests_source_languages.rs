//! A Python and a Rust file reach the editor through `resolve`: the languages of the
//! files around a project are parsed and coloured by their own grammar.

use std::fs;
use std::path::PathBuf;

use editor_view::{HighlightKind, HighlightSpan, Languages};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{
    buffer_of, editor_of, open_path, open_window, scratch, wait_for_parse_signal,
};

fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::write(
        root.join("script.py"),
        "# a helper\ndef run(count=42):\n    print(\"app\")\n",
    )
    .unwrap();
    fs::write(
        root.join("main.rs"),
        "// a helper\nfn run() {\n    let name = \"app\";\n}\n",
    )
    .unwrap();
    root
}

/// The window opens with the languages of the sample project; the formats of this test
/// come from the crate under test.
fn use_file_syntax(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: file_syntax::language_for,
            templates: |_| None,
        });
    });
}

fn coloured(spans: &[HighlightSpan], text: &str, kind: HighlightKind) -> Vec<String> {
    spans
        .iter()
        .filter(|span| span.kind == kind)
        .map(|span| text[span.start as usize..span.end as usize].to_owned())
        .collect()
}

#[gpui::test]
fn a_python_and_a_rust_file_come_with_highlighted_spans(cx: &mut TestAppContext) {
    let root = project("source-languages");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    use_file_syntax(cx);

    let python = root.join("script.py");
    open_path(cx, window, &python);
    wait_for_parse_signal(cx, window, &python);
    let view = editor_of(cx, window, &python);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the Python file is coloured");
    let keywords = coloured(&spans, &text, HighlightKind::Keyword);
    assert!(
        keywords.iter().any(|keyword| keyword == "def"),
        "a Python keyword keeps its colour: {keywords:?}"
    );
    let functions = coloured(&spans, &text, HighlightKind::Function);
    assert!(
        functions.iter().any(|function| function == "print"),
        "a Python call keeps its colour: {functions:?}"
    );
    let strings = coloured(&spans, &text, HighlightKind::String);
    assert!(
        strings.iter().any(|string| string.contains("app")),
        "a Python string keeps its colour: {strings:?}"
    );
    let numbers = coloured(&spans, &text, HighlightKind::Number);
    assert!(
        numbers.iter().any(|number| number == "42"),
        "a Python number keeps its colour: {numbers:?}"
    );

    let rust = root.join("main.rs");
    open_path(cx, window, &rust);
    wait_for_parse_signal(cx, window, &rust);
    let view = editor_of(cx, window, &rust);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the Rust file is coloured");
    let comments = coloured(&spans, &text, HighlightKind::Comment);
    assert!(
        comments.iter().any(|comment| comment.contains("a helper")),
        "a Rust comment keeps its colour: {comments:?}"
    );
    let keywords = coloured(&spans, &text, HighlightKind::Keyword);
    assert!(
        keywords.iter().any(|keyword| keyword == "fn"),
        "a Rust keyword keeps its colour: {keywords:?}"
    );
    let functions = coloured(&spans, &text, HighlightKind::Function);
    assert!(
        functions.iter().any(|function| function == "run"),
        "a Rust function keeps its colour: {functions:?}"
    );
    let strings = coloured(&spans, &text, HighlightKind::String);
    assert!(
        strings.iter().any(|string| string.contains("app")),
        "a Rust string keeps its colour: {strings:?}"
    );
}
