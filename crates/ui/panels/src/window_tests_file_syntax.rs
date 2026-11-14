//! The formats around a project reach the editor through `resolve`: a JSON and an XML
//! file are parsed and coloured by the grammar of their own format.

use std::fs;
use std::path::PathBuf;

use editor_view::{HighlightKind, HighlightSpan, Languages};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{
    buffer_of, editor_of, open_path, open_window, press, scratch, wait_for_parse_signal,
};

fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src").join("data.json"),
        "{\n  \"name\": \"app\",\n  \"port\": 8080\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("src").join("layout.xml"),
        "<root id=\"7\">\n  <!-- a note -->\n  <child/>\n</root>\n",
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
fn a_json_and_an_xml_file_come_with_highlighted_spans(cx: &mut TestAppContext) {
    let root = project("file-syntax");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    use_file_syntax(cx);

    let json = root.join("src").join("data.json");
    open_path(cx, window, &json);
    wait_for_parse_signal(cx, window, &json);
    let view = editor_of(cx, window, &json);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the JSON file is coloured");
    let fields = coloured(&spans, &text, HighlightKind::Field);
    assert!(
        fields.iter().any(|field| field.contains("name")),
        "a JSON key is a field: {fields:?}"
    );
    let strings = coloured(&spans, &text, HighlightKind::String);
    assert!(
        strings.iter().any(|string| string.contains("app")),
        "a JSON string keeps its colour: {strings:?}"
    );
    let numbers = coloured(&spans, &text, HighlightKind::Number);
    assert!(
        numbers.iter().any(|number| number == "8080"),
        "a JSON number keeps its colour: {numbers:?}"
    );

    let xml = root.join("src").join("layout.xml");
    open_path(cx, window, &xml);
    wait_for_parse_signal(cx, window, &xml);
    let view = editor_of(cx, window, &xml);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the XML file is coloured");
    let tags = coloured(&spans, &text, HighlightKind::Keyword);
    assert!(
        tags.iter().any(|tag| tag == "root") && tags.iter().any(|tag| tag == "child"),
        "the names of XML elements are keywords: {tags:?}"
    );
    let attributes = coloured(&spans, &text, HighlightKind::Field);
    assert!(
        attributes.iter().any(|attribute| attribute == "id"),
        "an XML attribute is a field: {attributes:?}"
    );
    let comments = coloured(&spans, &text, HighlightKind::Comment);
    assert!(
        comments.iter().any(|comment| comment.contains("a note")),
        "an XML comment keeps its colour: {comments:?}"
    );

    // A format without a line comment takes the block comment on Ctrl+/.
    press(cx, window, "ctrl-home");
    press(cx, window, "ctrl-/");
    assert!(
        buffer_of(cx, &view).starts_with("<!--<root id=\"7\">-->"),
        "Ctrl+/ wraps an XML line in a block comment"
    );
    press(cx, window, "ctrl-/");
    assert!(
        buffer_of(cx, &view).starts_with("<root id=\"7\">"),
        "Ctrl+/ takes the block comment away again"
    );
}

#[gpui::test]
fn a_gradle_script_and_a_markdown_file_come_with_highlighted_spans(cx: &mut TestAppContext) {
    let root = scratch("file-syntax-gradle");
    fs::write(
        root.join("build.gradle"),
        "// build script\ndef name = \"app\"\n\nif (name) {\n    println(\"built\")\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        "# Build\n\n```groovy\ndef build() {}\n```\n",
    )
    .unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    use_file_syntax(cx);

    let gradle = root.join("build.gradle");
    open_path(cx, window, &gradle);
    wait_for_parse_signal(cx, window, &gradle);
    let view = editor_of(cx, window, &gradle);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the Gradle script is coloured");
    let comments = coloured(&spans, &text, HighlightKind::Comment);
    assert!(
        comments
            .iter()
            .any(|comment| comment.contains("build script")),
        "a Groovy comment keeps its colour: {comments:?}"
    );
    let keywords = coloured(&spans, &text, HighlightKind::Keyword);
    assert!(
        keywords.iter().any(|keyword| keyword == "def"),
        "a Groovy keyword keeps its colour: {keywords:?}"
    );
    let strings = coloured(&spans, &text, HighlightKind::String);
    assert!(
        strings.iter().any(|string| string.contains("app")),
        "a Groovy string keeps its colour: {strings:?}"
    );

    let markdown = root.join("README.md");
    open_path(cx, window, &markdown);
    wait_for_parse_signal(cx, window, &markdown);
    let view = editor_of(cx, window, &markdown);
    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(!spans.is_empty(), "the Markdown file is coloured");
    let headings = coloured(&spans, &text, HighlightKind::Keyword);
    assert!(
        headings.iter().any(|heading| heading == "Build"),
        "a Markdown heading keeps its colour: {headings:?}"
    );
    let languages = coloured(&spans, &text, HighlightKind::Annotation);
    assert!(
        languages.iter().any(|language| language == "groovy"),
        "the language of a fenced block keeps its colour: {languages:?}"
    );
}
