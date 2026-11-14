//! The Markdown text in the editor: the inline markup and the language of a fenced
//! block reach the painted rows, not just the parse.

use std::fs;
use std::path::Path;

use editor_view::{HighlightKind, HighlightSpan, LanguageConfig, Languages};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, scratch, wait_for_parse_signal,
};

fn language_by_name(name: &str) -> Option<LanguageConfig> {
    file_syntax::language_by_name(name)
}

/// The formats around a project, with the same names inside a Markdown code fence.
fn resolve(path: &Path) -> Option<LanguageConfig> {
    file_syntax::language_for(path).map(|config| config.with_injection_resolver(language_by_name))
}

fn use_languages(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve,
            templates: |_| None,
        });
    });
}

fn spans_of(spans: &[HighlightSpan], text: &str, kind: HighlightKind) -> Vec<String> {
    spans
        .iter()
        .filter(|span| span.kind == kind)
        .map(|span| text[span.start as usize..span.end as usize].to_owned())
        .collect()
}

#[gpui::test]
fn markdown_inline_and_fenced_code_reach_the_painted_rows(cx: &mut TestAppContext) {
    let root = scratch("markdown-inline");
    let source = "# Build\n\nSome `inline` words.\n\n```kt\nclass Demo { val value = 7 }\n```\n";
    fs::write(root.join("README.md"), source).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    use_languages(cx);

    let path = root.join("README.md");
    open_path(cx, window, &path);
    wait_for_parse_signal(cx, window, &path);
    draw(cx, window);
    click(cx, window, "markdown-mode-editor");
    draw(cx, window);
    let view = editor_of(cx, window, &path);

    let text = buffer_of(cx, &view);
    let spans = view.read_with(cx, |view, _| view.highlight_spans().to_vec());
    assert!(
        spans_of(&spans, &text, HighlightKind::InlineCode).contains(&"inline".to_owned()),
        "a code span keeps its own span: {:?}",
        spans_of(&spans, &text, HighlightKind::InlineCode)
    );
    assert!(
        spans_of(&spans, &text, HighlightKind::Keyword).contains(&"class".to_owned()),
        "the language of the fence colours its keywords: {:?}",
        spans_of(&spans, &text, HighlightKind::Keyword)
    );

    let plain = text.find("Some").expect("plain words");
    let code = text.find("inline").expect("the code span");
    let keyword = text.find("class").expect("the keyword");
    let (plain, code, keyword) = view.read_with(cx, |view, _| {
        (
            view.painted_color_at(plain),
            view.painted_color_at(code),
            view.painted_color_at(keyword),
        )
    });
    let plain = plain.expect("the plain word is painted");
    assert_ne!(code, Some(plain), "inline code is not the plain colour");
    assert_ne!(
        keyword,
        Some(plain),
        "a fenced keyword is not the plain colour"
    );
}
