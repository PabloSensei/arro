//! The README of a project in the preview of its tab: the comment at its top is not drawn,
//! the badges share one row, a paragraph runs on to the width of the tab, and a code block
//! takes the colours of the language its fence names.

use std::fs;
use std::path::{Path, PathBuf};

use editor_view::{EditorView, LanguageConfig, Languages};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext};
use workspace::Settings;

use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, open_sample_window, wait_for_signal,
};

/// The top of a README of a project: a comment with a table and tags in it, three badges on
/// three source lines, a paragraph wrapped over two lines, and two code blocks.
const README: &str = r#"<!--
 +========================================================================+
 | <groupId> | math | 1.2 |
 +========================================================================+
 --->
# Apache Commons Lang

[![CI](https://example.com/ci.svg)](https://example.com/ci)
[![Release](https://example.com/release.svg)](https://example.com/release)
[![Docs](https://example.com/docs.svg)](https://example.com/docs)

Apache Commons Lang is a package of utility classes for the lang packages,
classes that could be considered standard to justify their existence in the core library.

```xml
<dependency>
  <groupId>org.example</groupId>
</dependency>
```

```
plain words without a language
```
"#;

/// The three badges of the README, in the order they are drawn.
const BADGES: [&str; 3] = [
    "markdown-preview-picture-0",
    "markdown-preview-picture-1",
    "markdown-preview-picture-2",
];

/// A file of the project with the text of the test.
fn markdown_file(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// Waits until the preview draws words of the text.
fn wait_for_preview(cx: &mut TestAppContext, view: &Entity<EditorView>, words: &str) {
    wait_for_signal(cx, "the markdown preview", |cx| {
        view.read_with(cx, |view, _| {
            view.preview_text().is_some_and(|text| text.contains(words))
        })
    });
}

/// The language of a file of the test, with the names of its fences read by the same table.
fn resolve(path: &Path) -> Option<LanguageConfig> {
    file_syntax::language_for(path).map(|config| config.with_injection_resolver(language_by_name))
}

fn language_by_name(name: &str) -> Option<LanguageConfig> {
    file_syntax::language_by_name(name)
}

fn use_languages(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve,
            templates: |_| None,
        });
    });
}

#[ui_kit::gpui::test]
fn the_comment_at_the_top_of_a_readme_is_not_drawn(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "readme-comment");
    let path = markdown_file(&root, "README.md", README);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Apache Commons Lang");
    draw(cx, window);
    draw(cx, window);

    let text = view
        .read_with(cx, |view, _| view.preview_text())
        .expect("the text of the preview");
    assert!(
        !text.contains("<!--") && !text.contains("+===") && !text.contains("math"),
        "the comment is not drawn: {text:?}"
    );
    assert!(
        text.contains("lang packages, classes that could"),
        "the lines of a paragraph run on to the width of the tab: {text:?}"
    );
}

#[ui_kit::gpui::test]
fn the_badges_of_a_readme_share_one_row(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "readme-badges");
    // The badges of the internet are not asked for: each one draws its text in a frame.
    cx.update(|cx| {
        cx.set_global(Settings {
            markdown_images_from_internet: false,
            ..Settings::default()
        })
    });
    let path = markdown_file(&root, "README.md", README);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Apache Commons Lang");
    draw(cx, window);
    draw(cx, window);

    let mut frames = Vec::new();
    for selector in BADGES {
        frames.push(bounds_of(cx, window, selector));
    }
    let top = f32::from(frames[0].origin.y);
    for (index, frame) in frames.iter().enumerate() {
        assert!(
            (f32::from(frame.origin.y) - top).abs() < 1.,
            "badge {index} stands in the row of the first one: {frame:?} against {top}"
        );
    }
    for pair in frames.windows(2) {
        assert!(
            pair[1].origin.x > pair[0].origin.x,
            "the badges follow each other from left to right: {frames:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn a_code_block_of_a_readme_takes_the_colours_of_its_language(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "readme-code");
    use_languages(cx);
    let path = markdown_file(&root, "README.md", README);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_preview(cx, &view, "Apache Commons Lang");
    wait_for_signal(cx, "the colours of the xml block", |cx| {
        view.read_with(cx, |view, _| {
            view.preview_code_spans()
                .first()
                .is_some_and(|count| *count > 0)
        })
    });

    let counts = view.read_with(cx, |view, _| view.preview_code_spans());
    assert_eq!(counts.len(), 2, "two code blocks: {counts:?}");
    assert!(counts[0] > 0, "the xml block has colours: {counts:?}");
    assert_eq!(
        counts[1], 0,
        "a block without a language keeps the plain colour: {counts:?}"
    );
}
