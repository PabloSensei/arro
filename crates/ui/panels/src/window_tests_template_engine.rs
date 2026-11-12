//! The variables and the Expand with key of live templates without a real window: an
//! Enter or Space template expands by its own key, a plain space keeps its meaning, the
//! names of the language fill the variables and Skip if defined walks past a field.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use editor_view::{Languages, Template, TemplateKind, TemplateLanguage};
use templates::FileTemplateDefinition;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Workspace;

use crate::run::ProjectSetup;
use crate::settings_templates::TemplatesSetup;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window_with, press, sample_project,
    type_text,
};
use crate::window_tests::{project_setup, scratch};

static TEMPLATES: &[Template] = &[
    Template {
        key: "sout",
        description: "Prints a value",
        kind: TemplateKind::Abbreviation,
        body: "print($END$);",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
    Template {
        key: "log",
        description: "Records a value",
        kind: TemplateKind::Abbreviation,
        body: "record($END$);",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
    Template {
        key: "spacey",
        description: "Emits a value",
        kind: TemplateKind::Abbreviation,
        body: "emit($END$);",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
    Template {
        key: "clazz",
        description: "The name of the type",
        kind: TemplateKind::Abbreviation,
        body: "$CLASS$",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
    Template {
        key: "skip",
        description: "A defined and an empty variable",
        kind: TemplateKind::Abbreviation,
        body: "$A$ $B$;$END$",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
];

/// The templates of the `.txt` files of these tests.
fn test_language() -> &'static TemplateLanguage {
    static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
    LANGUAGE.get_or_init(|| TemplateLanguage {
        templates: TEMPLATES,
        contexts: "",
        grammar: || dockerfile_syntax::config().grammar,
        indent_unit: "    ",
        receiver_prefixes: &["new"],
        query: OnceLock::new(),
        declarations: Some(|_, _, _, _| templates::Declarations {
            class_name: "Widget".to_owned(),
            method_name: "run".to_owned(),
            package_name: "demo".to_owned(),
        }),
    })
}

fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    let extension = path.extension()?.to_str()?;
    extension.eq_ignore_ascii_case("txt").then(test_language)
}

fn resolve(path: &Path) -> Option<editor_view::LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    extension
        .eq_ignore_ascii_case("txt")
        .then(dockerfile_syntax::config)
}

fn no_package(_: &Path, _: &Path) -> String {
    String::new()
}

fn file_definitions() -> Vec<FileTemplateDefinition> {
    vec![FileTemplateDefinition::new(
        "class",
        "Class",
        "made ${NAME}",
        no_package,
    )]
}

fn setup() -> ProjectSetup {
    project_setup(scratch("template-engine-cache"))
}

/// A window whose strings are `.txt` files of a grammar without a server, with the
/// templates and the file templates of the test.
fn open_templates_window(
    cx: &mut TestAppContext,
    root: PathBuf,
) -> ui_kit::gpui::WindowHandle<Workspace> {
    let window = open_window_with(cx, root, setup());
    cx.update(|cx| {
        cx.set_global(Languages {
            resolve,
            templates: templates_for,
        });
        cx.set_global(
            TemplatesSetup {
                language: Some(test_language()),
                files: file_definitions(),
            }
            .initial(),
        );
    });
    cx.run_until_parked();
    draw(cx, window);
    window
}

fn open_page(cx: &mut TestAppContext, window: ui_kit::gpui::WindowHandle<Workspace>) {
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-live-templates");
    draw(cx, window);
}

fn dismiss(cx: &mut TestAppContext, window: ui_kit::gpui::WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| workspace.dismiss(window, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn replace_field(
    cx: &mut TestAppContext,
    window: ui_kit::gpui::WindowHandle<Workspace>,
    field: &'static str,
    text: &str,
) {
    click(cx, window, field);
    draw(cx, window);
    press(cx, window, "ctrl-a");
    type_text(cx, window, text);
    draw(cx, window);
}

fn open_editor(
    cx: &mut TestAppContext,
    window: ui_kit::gpui::WindowHandle<Workspace>,
    root: &Path,
) -> ui_kit::gpui::Entity<editor_view::EditorView> {
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    editor_of(cx, window, &path)
}

fn expand_with(
    cx: &mut TestAppContext,
    window: ui_kit::gpui::WindowHandle<Workspace>,
    selector: &'static str,
) {
    open_page(cx, window);
    click(cx, window, "settings-template-any-log");
    draw(cx, window);
    click(cx, window, selector);
    draw(cx, window);
    dismiss(cx, window);
}

#[gpui::test]
fn an_enter_template_expands_by_enter(cx: &mut TestAppContext) {
    let root = sample_project("templates-enter");
    let window = open_templates_window(cx, root.clone());
    expand_with(cx, window, "settings-template-expand-enter");

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " log");
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(text.contains("record("), "{text}");
    assert!(!text.contains("log"), "{text}");
}

#[gpui::test]
fn an_enter_template_does_not_expand_by_tab(cx: &mut TestAppContext) {
    let root = sample_project("templates-enter-tab");
    let window = open_templates_window(cx, root.clone());
    expand_with(cx, window, "settings-template-expand-enter");

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " log");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("log"), "{text}");
    assert!(!text.contains("record("), "{text}");
}

#[gpui::test]
fn the_default_key_of_the_page_expands_a_template(cx: &mut TestAppContext) {
    let root = sample_project("templates-default-key");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window);
    click(cx, window, "template-default-expand-enter");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " log");
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(text.contains("record("), "{text}");
}

#[gpui::test]
fn a_space_template_expands_by_space(cx: &mut TestAppContext) {
    let root = sample_project("templates-space");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window);
    click(cx, window, "settings-template-any-spacey");
    draw(cx, window);
    click(cx, window, "settings-template-expand-space");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " spacey");
    type_text(cx, window, " ");
    let text = buffer_of(cx, &view);
    assert!(text.contains("emit("), "{text}");
    assert!(!text.contains("spacey"), "{text}");
}

#[gpui::test]
fn a_plain_space_without_an_expanding_abbreviation_stays_a_space(cx: &mut TestAppContext) {
    let root = sample_project("templates-space-plain");
    let window = open_templates_window(cx, root.clone());

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " ");
    assert_eq!(buffer_of(cx, &view), "alpha \nbeta\n");

    view.update(cx, |view, cx| view.restore_caret(1, 4, cx));
    type_text(cx, window, " sout");
    type_text(cx, window, " ");
    let text = buffer_of(cx, &view);
    assert!(text.contains("sout "), "{text}");
    assert!(!text.contains("print("), "{text}");
}

#[gpui::test]
fn the_class_name_of_the_language_fills_a_variable(cx: &mut TestAppContext) {
    let root = sample_project("templates-class-name");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window);
    click(cx, window, "settings-template-any-clazz");
    draw(cx, window);
    click(cx, window, "template-variable-class-expression");
    draw(cx, window);
    click(cx, window, "template-expression-classname");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " clazz");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("Widget"), "{text}");
    assert!(!text.contains("clazz"), "{text}");
}

#[gpui::test]
fn a_variable_that_is_defined_takes_no_tab_stop(cx: &mut TestAppContext) {
    let root = sample_project("templates-skip");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window);
    click(cx, window, "settings-template-any-skip");
    draw(cx, window);
    replace_field(cx, window, "template-variable-a-expression", "fileName()");
    click(cx, window, "template-variable-a-skip");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " skip");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("main.txt"), "{text}");

    // The caret is on the second, empty variable: typing goes there, not over the name
    // of the file.
    type_text(cx, window, "X");
    let text = buffer_of(cx, &view);
    assert!(text.contains("main.txt X;"), "{text}");
}
