//! Quick Definition: the indent of the source it shows, the gutter of its line numbers,
//! the width of the window and the folder of its header.

use std::path::{Path, PathBuf};

use lang_api::{DefinitionPreview, SourceSpan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::metrics::SPACE_M;
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, editor_of, open_path, press};
use crate::window_tests_hierarchy::{HierarchyLanguage, open_window, project};
use crate::window_tests_lang::{span_in, uri_of};

/// The definition popup of the editor of `path`, as its test reads it.
fn shown(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> editor_view::DefinitionView {
    editor_of(cx, window, path).read_with(cx, |view, _| {
        view.definition_view()
            .expect("the definition popup has its rows")
    })
}

/// Opens a project with a use site and a definition file, and shows the definition.
fn open_definition(
    cx: &mut TestAppContext,
    name: &str,
    file: &str,
    target: &str,
) -> (WindowHandle<Workspace>, PathBuf) {
    let root = project(name, &[("src/Use.txt", "use\n"), (target, file)]);
    let use_path = root.join("src").join("Use.txt");
    let definition = root.join(target);
    let language = HierarchyLanguage::new();
    *language.preview.lock().unwrap() = Some(DefinitionPreview {
        text: file.to_owned(),
        target: Some(SourceSpan {
            uri: uri_of(&definition),
            span: span_in(file, 0, 1),
        }),
    });
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &use_path);
    press(cx, window, "ctrl-shift-i");
    draw(cx, window);
    (window, use_path)
}

const INDENTED: &str = "    public String getLastName() {\n        return this.lastName;\n    }\n";

#[ui_kit::gpui::test]
fn definition_keeps_the_relative_indent_and_a_gap_after_the_numbers(cx: &mut TestAppContext) {
    let (window, use_path) =
        open_definition(cx, "definition-layout-indent", INDENTED, "src/Person.txt");
    let view = shown(cx, window, &use_path);
    assert_eq!(
        view.rows.get(1).map(|row| row.text.as_str()),
        Some("    return this.lastName;"),
        "the indent every line shares is dropped, the rest stays"
    );

    let number = bounds_of(cx, window, "quick-definition-number-1");
    let code = bounds_of(cx, window, "quick-definition-code-1");
    assert!(
        f32::from(code.left() - number.right()) >= SPACE_M,
        "the code starts a gutter gap after the numbers: {number:?} and {code:?}"
    );

    let frame = bounds_of(cx, window, "quick-definition");
    assert!(
        f32::from(frame.size.width) >= 480.,
        "the window is at least 480 points wide: {frame:?}"
    );
}

#[ui_kit::gpui::test]
fn definition_tabs_take_the_width_of_the_editor(cx: &mut TestAppContext) {
    let file = "\tpublic String getLastName() {\n\t\treturn this.lastName;\n\t}\n";
    let (window, use_path) = open_definition(cx, "definition-layout-tabs", file, "src/Person.txt");
    let tab_width = editor_of(cx, window, &use_path).read_with(cx, |view, _| view.tab_width());
    let expected = format!("{}return this.lastName;", " ".repeat(tab_width));
    let view = shown(cx, window, &use_path);
    assert_eq!(
        view.rows.get(1).map(|row| row.text.as_str()),
        Some(expected.as_str()),
        "a tab takes the width it has in the editor"
    );
}

#[ui_kit::gpui::test]
fn definition_header_shows_the_folder_from_the_project_root(cx: &mut TestAppContext) {
    let (window, use_path) = open_definition(
        cx,
        "definition-layout-header",
        INDENTED,
        "src/main/source/org/example/model/Person.txt",
    );
    let view = shown(cx, window, &use_path);
    assert_eq!(view.name, "Person.txt", "the header names the file");
    assert_eq!(
        view.folder, "src/main/source/org/example/model",
        "the folder is the one of the project, not of the disk"
    );
    assert!(
        !view.folder.contains(':'),
        "no drive letter in the header: {}",
        view.folder
    );
    assert!(
        !view.folder.contains("arro-tests"),
        "no folder of the test in the header: {}",
        view.folder
    );
}
