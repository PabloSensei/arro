//! Quick Definition: the header of the file of the definition, the source with the
//! colours and the line numbers of the editor, and the window that scrolls and resizes.

use std::path::Path;

use editor_view::Languages;
use lang_api::{DefinitionPreview, SourceSpan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::px;
use workspace::Workspace;

use crate::window_tests::{
    active_path, bounds_of, draw, editor_of, line_height, open_path, press, wait_for_signal,
};
use crate::window_tests_hierarchy::{HierarchyLanguage, open_window, project};
use crate::window_tests_lang::{span_in, uri_of};

/// The window opens with the formats of the files around the project: the definition of
/// this test is a JSON file, so its source is coloured by the JSON grammar.
fn use_file_syntax(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve: file_syntax::language_for,
            templates: |_| None,
        });
    });
}

/// The preview of a definition in `path`, declared at `at`.
fn preview_of(path: &Path, file: &str, at: usize) -> DefinitionPreview {
    DefinitionPreview {
        text: file[at..].to_owned(),
        target: Some(SourceSpan {
            uri: uri_of(path),
            span: span_in(file, at, at + 1),
        }),
    }
}

fn shown(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> editor_view::DefinitionView {
    let view = editor_of(cx, window, path);
    view.read_with(cx, |view, _| {
        view.definition_view()
            .expect("the definition popup has its rows")
    })
}

#[ui_kit::gpui::test]
fn quick_definition_has_a_header_and_coloured_code(cx: &mut TestAppContext) {
    let file = "{\n  \"name\": \"Person\",\n  \"age\": 42\n}\n";
    let root = project(
        "definition-header",
        &[("src/Use.txt", "use\n"), ("src/Person.json", file)],
    );
    let use_path = root.join("src").join("Use.txt");
    let person = root.join("src").join("Person.json");
    let language = HierarchyLanguage::new();
    *language.preview.lock().unwrap() = Some(preview_of(&person, file, 0));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    use_file_syntax(cx);

    open_path(cx, window, &use_path);
    press(cx, window, "ctrl-shift-i");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "quick-definition-header").size.height > px(0.),
        "the popup has a header"
    );
    assert!(
        bounds_of(cx, window, "quick-definition-edit").size.height > px(0.),
        "the header has the Edit Source button"
    );

    let view = editor_of(cx, window, &use_path);
    wait_for_signal(cx, "the colours of the definition", |cx| {
        view.read_with(cx, |view, _| {
            let Some(shown) = view.definition_view() else {
                return false;
            };
            let colors: Vec<String> = shown
                .rows
                .iter()
                .flat_map(|row| row.colors.iter().map(|color| format!("{color:?}")))
                .collect();
            let distinct = {
                let mut colors = colors.clone();
                colors.sort();
                colors.dedup();
                colors.len()
            };
            distinct > 1
        })
    });

    let shown = shown(cx, window, &use_path);
    assert_eq!(shown.name, "Person.json", "the header names the file");
    assert!(
        shown.folder.ends_with("/src"),
        "the header shows the folder: {}",
        shown.folder
    );
}

#[ui_kit::gpui::test]
fn f4_opens_the_definition_at_its_line(cx: &mut TestAppContext) {
    let file = "{\n  \"name\": \"Person\",\n  \"age\": 42,\n  \"city\": \"Paris\"\n}\n";
    let root = project(
        "definition-f4",
        &[("src/Use.txt", "use\n"), ("src/Person.json", file)],
    );
    let use_path = root.join("src").join("Use.txt");
    let person = root.join("src").join("Person.json");
    let at = file.find("\"name\"").unwrap();
    let language = HierarchyLanguage::new();
    *language.preview.lock().unwrap() = Some(preview_of(&person, file, at));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    use_file_syntax(cx);

    open_path(cx, window, &use_path);
    press(cx, window, "ctrl-shift-i");
    draw(cx, window);
    press(cx, window, "f4");
    draw(cx, window);
    wait_for_signal(cx, "the opened definition", |cx| {
        active_path(cx, window).as_deref() == Some(person.as_path())
    });
    assert_eq!(
        active_path(cx, window).as_deref(),
        Some(person.as_path()),
        "F4 opens the file of the definition"
    );
    let view = editor_of(cx, window, &person);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()).0,
        1,
        "the caret stands on the line of the declaration"
    );
}

#[ui_kit::gpui::test]
fn a_long_definition_scrolls_after_twenty_lines(cx: &mut TestAppContext) {
    let target = "{\n}\n";
    let mut file = String::from("{\n");
    for index in 0..38 {
        file.push_str(&format!("  \"field{index}\": {index},\n"));
    }
    file.push_str("}\n");
    let root = project(
        "definition-scroll",
        &[("src/Use.txt", "use\n"), ("src/Person.json", target)],
    );
    let use_path = root.join("src").join("Use.txt");
    let person = root.join("src").join("Person.json");
    let language = HierarchyLanguage::new();
    *language.preview.lock().unwrap() = Some(DefinitionPreview {
        text: file,
        target: Some(SourceSpan {
            uri: uri_of(&person),
            span: span_in(target, 0, 1),
        }),
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    use_file_syntax(cx);

    open_path(cx, window, &use_path);
    press(cx, window, "ctrl-shift-i");
    draw(cx, window);
    let rows = bounds_of(cx, window, "quick-definition-rows");
    let area = bounds_of(cx, window, "quick-definition-scroll");
    assert!(
        rows.size.height > area.size.height,
        "the definition is taller than the window: {rows:?} against {area:?}"
    );
    assert!(
        f32::from(area.size.height) <= 20. * line_height(cx) + 1.,
        "the window shows at most twenty lines: {area:?}"
    );
}
