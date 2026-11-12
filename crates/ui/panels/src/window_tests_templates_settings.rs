//! Settings -> Editor -> Live Templates, Postfix Completion and File and Code
//! Templates without a real window: a template added or changed on a page expands in the
//! editor, a disabled one does not, and `templates.json` of another program is read.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use editor_view::{Languages, Template, TemplateKind, TemplateLanguage, TemplateSettings};
use templates::FileTemplateDefinition;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::{PROJECT_TOOL, Workspace};

use crate::project::ActiveProjectPanel;
use crate::run::ProjectSetup;
use crate::settings_templates::TemplatesSetup;
use crate::tree_ops::{Creates, FileTemplate};
use crate::window_tests::{
    active_path, buffer_of, click, draw, editor_of, open_path, open_window_with, press, scratch,
    type_text, wait_for_signal, with_window,
};
use crate::window_tests::{project_setup, sample_project};

const LABEL: &str = "Class";

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
        key: "psvm",
        description: "A main declaration",
        kind: TemplateKind::Abbreviation,
        body: "void main() {\n\t$END$\n}",
        fields: &[],
        contexts: &[editor_view::TEMPLATE_ANY],
    },
    Template {
        key: "nn",
        description: "Not null",
        kind: TemplateKind::Postfix,
        body: "if ($EXPR$ != null) {\n\t$END$\n}",
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
        declarations: None,
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
        LABEL,
        "made ${NAME}",
        no_package,
    )]
}

fn class_template() -> FileTemplate {
    FileTemplate {
        label: LABEL,
        icon: "nodes/class",
        extension: "txt",
        creates: Creates::File,
        content: |_, _, name| format!("class {name} {{}}\n"),
    }
}

fn setup() -> ProjectSetup {
    let mut setup = project_setup(scratch("templates-cache"));
    setup.templates = vec![class_template()];
    setup
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

fn open_page(
    cx: &mut TestAppContext,
    window: ui_kit::gpui::WindowHandle<Workspace>,
    page: &'static str,
) {
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, page);
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

#[gpui::test]
fn a_template_added_on_the_page_expands_by_tab(cx: &mut TestAppContext) {
    let root = sample_project("templates-add");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window, "settings-page-live-templates");
    click(cx, window, "template-add");
    draw(cx, window);
    replace_field(cx, window, "template-abbreviation", "mine");
    replace_field(cx, window, "template-text", "run");
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " mine");
    press(cx, window, "tab");
    assert!(
        buffer_of(cx, &view).contains("run"),
        "{}",
        buffer_of(cx, &view)
    );
}

#[gpui::test]
fn a_disabled_sout_does_not_expand(cx: &mut TestAppContext) {
    let root = sample_project("templates-disabled");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window, "settings-page-live-templates");
    click(cx, window, "settings-template-enabled-any-sout");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " sout");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("sout"), "{text}");
    assert!(!text.contains("print("), "{text}");
}

#[gpui::test]
fn a_changed_psvm_expands_its_new_text(cx: &mut TestAppContext) {
    let root = sample_project("templates-changed");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window, "settings-page-live-templates");
    click(cx, window, "settings-template-any-psvm");
    draw(cx, window);
    replace_field(cx, window, "template-text", "void disk");
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " psvm");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("void disk"), "{text}");
    assert!(!text.contains("void main()"), "{text}");
}

#[gpui::test]
fn a_disabled_postfix_nn_is_not_offered(cx: &mut TestAppContext) {
    let root = sample_project("templates-postfix");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window, "settings-page-postfix-completion");
    click(cx, window, "settings-postfix-enabled-any-nn");
    draw(cx, window);
    dismiss(cx, window);

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " value.nn");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("value.nn"), "{text}");
    assert!(!text.contains("!= null"), "{text}");
}

#[gpui::test]
fn the_class_template_of_the_page_makes_a_new_file(cx: &mut TestAppContext) {
    let root = sample_project("templates-file");
    let window = open_templates_window(cx, root.clone());
    open_page(cx, window, "settings-page-file-and-code-templates");
    click(cx, window, "settings-file-template-class");
    draw(cx, window);
    replace_field(cx, window, "file-template-text", "made thing");
    dismiss(cx, window);

    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    let src = root.join("src");
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&src, cx))
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "alt-insert");
    draw(cx, window);
    click(cx, window, "menu-2");
    draw(cx, window);
    type_text(cx, window, "Thing");
    press(cx, window, "enter");
    draw(cx, window);

    let made = root.join("src").join("Thing.txt");
    assert_eq!(fs::read_to_string(&made).unwrap(), "made thing");
    assert_eq!(active_path(cx, window), Some(made));
}

#[gpui::test]
fn a_templates_file_written_on_disk_reaches_the_editor(cx: &mut TestAppContext) {
    let root = sample_project("templates-watch");
    let window = open_templates_window(cx, root.clone());
    let dir = window
        .read_with(cx, |workspace, _| workspace.settings_dir().to_path_buf())
        .unwrap();
    let before = cx.read(|cx| {
        cx.try_global::<TemplateSettings>()
            .map_or(0, |settings| settings.generation)
    });
    fs::write(
        dir.join("templates.json"),
        r#"{"version": 1, "live": {"any/psvm": {"body": "void disk() {} $END$"}}}"#,
    )
    .unwrap();
    wait_for_signal(cx, "the templates file of the settings folder", |cx| {
        cx.read(|cx| {
            cx.try_global::<TemplateSettings>()
                .is_some_and(|settings| settings.generation > before)
        })
    });

    let view = open_editor(cx, window, &root);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, " psvm");
    press(cx, window, "tab");
    let text = buffer_of(cx, &view);
    assert!(text.contains("void disk"), "{text}");
}
