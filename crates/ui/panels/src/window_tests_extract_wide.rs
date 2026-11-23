//! The name an extraction takes from the language when the server answers with one wide edit
//! that replaces the whole line, not with a replacement of the value alone. The name of the
//! language is shown anyway: the window takes the expression from the selection and from the
//! text, not from the edits of the server.
//!
//! The language is the stand-in of `window_tests_extract_name`.

use std::path::Path;

use lang_api::{ActionEntry, ActionKind, RenamePosition, SourceEdit};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{buffer_of, draw, editor_of, open_path, press};
use crate::window_tests_extract_name::{
    LINE, NamingLanguage, edit_in, open_window, project, select_value, uri_of,
};

/// Extract Constant of the value as one edit: the whole line becomes the declaration and the
/// line that uses it, and the name of the declaration is marked.
fn wide_constant_action(path: &Path) -> ActionEntry {
    let uri = uri_of(path);
    let replaced = "int _5 = 5;\nint pageSize = _5;\n";
    ActionEntry {
        title: "Extract constant".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.constant".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri.clone(),
            edits: vec![edit_in(LINE, 0, LINE.len(), replaced)],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(RenamePosition {
            uri,
            offset: 4,
            length: 2,
        }),
    }
}

/// Introduce Parameter of the value as one edit: the whole line becomes the method with the
/// parameter and the line that uses it. The server marks no place for the name.
fn wide_parameter_action(path: &Path) -> ActionEntry {
    let uri = uri_of(path);
    let replaced = "void run(int i) {}\nint pageSize = i;\n";
    ActionEntry {
        title: "Introduce parameter".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.introduce.parameter".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri,
            edits: vec![edit_in(LINE, 0, LINE.len(), replaced)],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: None,
    }
}

#[ui_kit::gpui::test]
fn extract_constant_with_one_wide_edit_shows_the_name_of_the_language(cx: &mut TestAppContext) {
    let root = project("name-constant-wide", &[("src/Form.txt", LINE)]);
    let open = root.join("src").join("Form.txt");
    let language = NamingLanguage::new("_5", true);
    language.set_actions(vec![wide_constant_action(&open)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    select_value(cx, window);
    press(cx, window, "ctrl-alt-c");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.name_field_text())
            .as_deref(),
        Some("PAGE_SIZE"),
        "the name field shows the name the language gives"
    );
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("int PAGE_SIZE = 5;"), "{text}");
    assert!(text.contains("int pageSize = PAGE_SIZE;"), "{text}");
}

#[ui_kit::gpui::test]
fn introduce_parameter_with_one_wide_edit_shows_the_name_of_the_language(cx: &mut TestAppContext) {
    let root = project("name-parameter-wide", &[("src/Form.txt", LINE)]);
    let open = root.join("src").join("Form.txt");
    let language = NamingLanguage::new("i", true);
    language.set_actions(vec![wide_parameter_action(&open)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    select_value(cx, window);
    press(cx, window, "ctrl-alt-p");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.name_field_text())
            .as_deref(),
        Some("limit"),
        "the name field shows the name the language gives"
    );
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("void run(int limit) {}"), "{text}");
    assert!(text.contains("int pageSize = limit;"), "{text}");
}
