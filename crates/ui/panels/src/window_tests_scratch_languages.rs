//! The New Scratch File menu of the window without a window: the names of the languages,
//! the icons of the file types, the order of the rows and the search.

use std::fs;
use std::path::PathBuf;

use editor_view::Languages;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_popups::{ScratchFileType, ScratchFileTypes, ScratchLanguagePopup};
use crate::window_tests::{active_path, draw, open_path, open_sample_window, press, type_text};
use crate::window_tests_ui::wait_for;

/// The languages of the window: the real language of the application and the formats
/// around a project, so that every row takes its name from its language.
fn use_languages(cx: &mut TestAppContext) {
    let (extension, label) = window_language::scratch_file_type();
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: |path| {
                window_language::language_for(path).or_else(|| file_syntax::language_for(path))
            },
            templates: |_| None,
        });
        cx.set_global(ScratchFileTypes(vec![ScratchFileType { extension, label }]));
    });
}

fn popup(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Entity<ScratchLanguagePopup> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the scratch menu is open");
    view.downcast::<ScratchLanguagePopup>()
        .map_err(|_| ())
        .expect("the open popup is the scratch menu")
}

fn labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    popup(cx, window).read_with(cx, |popup, _| popup.labels())
}

fn show(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-alt-shift-insert");
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_scratch_menu_names_the_languages_with_a_capital_letter(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "scratch-languages-names");
    use_languages(cx);
    show(cx, window);
    let labels = labels(cx, window);
    let (_, language) = window_language::scratch_file_type();
    for name in [
        language,
        "Kotlin",
        "Groovy",
        "JSON",
        "XML",
        "YAML",
        "Properties",
        "Markdown",
        "SQL",
        "HTTP Request",
        "Shell Script",
        "Plain text",
    ] {
        assert!(
            labels.iter().any(|label| label == name),
            "the menu has {name}: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|label| label == &language.to_lowercase()),
        "the name is the one of the language: {labels:?}"
    );
}

#[ui_kit::gpui::test]
fn the_language_of_the_open_file_comes_first(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "scratch-languages-active");
    use_languages(cx);
    let (extension, language) = window_language::scratch_file_type();
    let source = root.join("src").join(format!("Main.{extension}"));
    fs::write(&source, "class Main {}\n").unwrap();
    open_path(cx, window, &source);
    show(cx, window);
    assert_eq!(
        labels(cx, window).first().map(String::as_str),
        Some(language),
        "the language of the active editor is the first row"
    );
}

#[ui_kit::gpui::test]
fn typing_filters_the_languages(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "scratch-languages-search");
    use_languages(cx);
    show(cx, window);
    type_text(cx, window, "js");
    draw(cx, window);
    assert_eq!(
        labels(cx, window),
        vec!["JSON".to_owned()],
        "the letters keep the languages whose name has them"
    );
}

#[ui_kit::gpui::test]
fn the_last_chosen_language_follows_the_active_one(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "scratch-languages-last");
    use_languages(cx);
    show(cx, window);
    type_text(cx, window, "json");
    draw(cx, window);
    press(cx, window, "enter");
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let scratches = state_dir.join("scratches");
    wait_for(cx, "the scratch file", |cx| {
        active_path(cx, window).is_some_and(|path| {
            path.starts_with(&scratches)
                && path.extension().and_then(|extension| extension.to_str()) == Some("json")
        })
    });
    open_path(cx, window, &root.join("README.txt"));
    show(cx, window);
    let labels = labels(cx, window);
    assert_eq!(labels.first().map(String::as_str), Some("Plain text"));
    assert_eq!(labels.get(1).map(String::as_str), Some("JSON"));
}

#[ui_kit::gpui::test]
fn enter_creates_the_file_of_the_chosen_language(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "scratch-languages-enter");
    use_languages(cx);
    show(cx, window);
    type_text(cx, window, "js");
    draw(cx, window);
    press(cx, window, "enter");
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let scratches = state_dir.join("scratches");
    wait_for(cx, "the scratch file", |cx| {
        active_path(cx, window)
            .is_some_and(|path| path.starts_with(&scratches) && path.extension().is_some())
    });
    let path: PathBuf = active_path(cx, window).unwrap();
    assert_eq!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("json")
    );
    assert!(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.starts_with("scratch")),
        "the file is a scratch file: {path:?}"
    );
    assert!(path.is_file());
}
