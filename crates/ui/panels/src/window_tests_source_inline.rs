//! The source actions and the inline refactorings of the server, with a server stub. Alt+Insert
//! lists only the kinds of Source Generate. Code runs Sort Members and Add final Modifiers.
//! Alt+Enter, Refactor This and Ctrl+Alt+N look at the declaration of the symbol at the caret:
//! a declaration in a library gives no Inline row, and a declaration in the project gives one.
//!
//! cargo test -p panels window_tests_source_inline

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Languages};
use keymap::editor as editor_keys;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, Answer, Confidence, GenerateChoice, GenerateKind,
    GenerateProvider, GenerateQuery, Generation, LanguageFn, LanguageId, LanguageSupport,
    NavigationProvider, NavigationQuery, Provider, Queries, SourceEdit, SourceId, SourceSpan,
    SyntaxProvider, TextSpan,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_editor_popups::PopupLanguage;
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("source-inline");
const TEXT: &str = "problem\n";
/// The declaration of a class of a library, as the server names it.
const LIBRARY: &str = "jdt://contents/lib.jar/pkg/Page.class";

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language whose server offers `actions`, names `declaration` as the place of the symbol at
/// the caret, and answers the parts of a class the way Source Generate asks.
struct Declaring {
    syntax: NoSyntax,
    actions: Arc<PopupLanguage>,
    declaration: Vec<SourceSpan>,
}

impl Declaring {
    fn new(actions: Arc<PopupLanguage>, declaration: Vec<SourceSpan>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            actions,
            declaration,
        })
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for Declaring {
    fn start(
        &self,
        _: NavigationQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SourceSpan>>,
    ) {
        let _ = reply.send(Answer::new(
            self.declaration.clone(),
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl Provider<GenerateQuery, Generation> for Declaring {
    fn start(&self, query: GenerateQuery, _: base::Cancel, mut reply: lang_api::Reply<Generation>) {
        // Only equals() and hashCode() have a field to write; the other parts offer nothing.
        let generation = if query.kind == GenerateKind::EqualsAndHashCode {
            Generation {
                choices: vec![GenerateChoice {
                    label: "name".to_owned(),
                    detail: Some("String".to_owned()),
                    checked: true,
                    handle: "name".to_owned(),
                }],
                ..Generation::default()
            }
        } else {
            Generation::default()
        };
        let _ = reply.send(Answer::new(generation, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for Declaring {
    fn id(&self) -> LanguageId {
        LanguageId("source-inline")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self.actions.as_ref())
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn generate(&self) -> Option<&GenerateProvider> {
        Some(self)
    }
}

/// The window of the project `root`, whose server is `language`.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<Declaring>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("source-inline-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("source-inline-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// An action of the server at the place of the tests. Its edit changes the first word of the
/// text to "changed", so a run of the action shows in the buffer.
fn action(path: &Path, title: &str, kind: ActionKind, code: &str) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(kind),
        code: Some(code.to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri_of(path),
            edits: vec![edit_in(TEXT, 0, 7, "changed")],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: None,
    }
}

/// The declaration of the place in the project: the file `path`.
fn in_project(path: &Path) -> Vec<SourceSpan> {
    vec![SourceSpan {
        uri: uri_of(path),
        span: TextSpan::default(),
    }]
}

/// The declaration of the place in a library.
fn in_library() -> Vec<SourceSpan> {
    vec![SourceSpan {
        uri: LIBRARY.to_owned(),
        span: TextSpan::default(),
    }]
}

/// The labels of the open list. Empty when no list is open.
fn labels(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Vec<String> {
    view.read_with(cx, |view, _| view.actions_visible())
        .map(|rows| rows.rows)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

/// The rows of the Code submenu of the main menu, with the command each row runs.
fn code_rows(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Vec<(String, Option<String>)> {
    let sections = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let code = sections
        .iter()
        .find(|entry| entry.label == "Code")
        .expect("the menu bar has a Code section");
    code.submenu
        .iter()
        .map(|row| {
            (
                row.label.to_string(),
                row.id.as_ref().map(|id| id.to_string()),
            )
        })
        .collect()
}

/// Runs a command of the window the way its menu row does.
fn run_command(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(id, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn generate_lists_the_generate_kinds_and_not_sort_members_or_the_final_modifiers(
    cx: &mut TestAppContext,
) {
    let text = "class A {\n}\n";
    let root = project("source-inline-generate", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![
        action(
            &path,
            "Generate toString()",
            ActionKind::Source,
            "source.generate.toString",
        ),
        action(
            &path,
            "Sort members",
            ActionKind::Source,
            "source.sortMembers",
        ),
        action(
            &path,
            "Change modifiers to final where possible",
            ActionKind::Source,
            "source.generate.finalModifiers",
        ),
    ]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, Vec::new()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 9, cx));

    press(cx, window, "alt-insert");
    wait_for(cx, "the Generate list", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.actions_visible())
            .is_some()
    });
    let rows = labels(cx, &view);
    assert!(
        rows.iter().any(|row| row == "toString()"),
        "the kind of Source Generate is listed: {rows:?}"
    );
    assert!(
        rows.iter()
            .all(|row| row != "Sort members" && !row.starts_with("Change modifiers")),
        "Sort Members and the final modifiers are Code rows, not Generate rows: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn code_menu_lists_sort_members_and_runs_the_server_action(cx: &mut TestAppContext) {
    let root = project("source-inline-sort", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Sort members",
        ActionKind::Source,
        "source.sortMembers",
    )]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, Vec::new()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    let rows = code_rows(cx, window);
    assert!(
        rows.iter().any(|(label, id)| {
            label == "Sort Members" && id.as_deref() == Some(editor_keys::SORT_MEMBERS)
        }),
        "the Code menu has Sort Members: {rows:?}"
    );
    run_command(cx, window, editor_keys::SORT_MEMBERS);
    wait_for(cx, "the server's sort of the members", |cx| {
        buffer_of(cx, &view) == "changed\n"
    });
}

#[ui_kit::gpui::test]
fn code_menu_lists_add_final_modifiers_and_runs_the_server_action(cx: &mut TestAppContext) {
    let root = project("source-inline-final", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Change modifiers to final where possible",
        ActionKind::Source,
        "source.generate.finalModifiers",
    )]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, Vec::new()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    let rows = code_rows(cx, window);
    assert!(
        rows.iter().any(|(label, id)| {
            label == "Add final Modifiers"
                && id.as_deref() == Some(editor_keys::ADD_FINAL_MODIFIERS)
        }),
        "the Code menu has Add final Modifiers: {rows:?}"
    );
    run_command(cx, window, editor_keys::ADD_FINAL_MODIFIERS);
    wait_for(cx, "the server's final modifiers", |cx| {
        buffer_of(cx, &view) == "changed\n"
    });
}

#[ui_kit::gpui::test]
fn alt_enter_leaves_out_the_inline_method_of_a_library(cx: &mut TestAppContext) {
    let root = project("source-inline-library", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Inline Method",
        ActionKind::Refactor,
        "refactor.inline",
    )]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, in_library()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        labels(cx, &view).is_empty(),
        "the library's Inline Method is not offered: {:?}",
        labels(cx, &view)
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some("No intentions available"),
        "with nothing else to offer, the caret says so"
    );
}

#[ui_kit::gpui::test]
fn alt_enter_adds_the_inline_method_once_its_declaration_is_in_the_project(
    cx: &mut TestAppContext,
) {
    let root = project(
        "source-inline-project",
        &[("src/Main.txt", TEXT), ("src/Other.txt", "other\n")],
    );
    let path = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let actions = PopupLanguage::new();
    // A row the list shows at once, so the Inline row joins a list that is already open.
    actions.set_actions(vec![
        action(
            &path,
            "Extract Variable",
            ActionKind::Refactor,
            "refactor.extract.variable",
        ),
        action(
            &path,
            "Inline Method",
            ActionKind::Refactor,
            "refactor.inline",
        ),
    ]);
    let window = open_window(
        cx,
        root.clone(),
        Declaring::new(actions, in_project(&other)),
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    wait_for(cx, "the Inline Method row", |cx| {
        draw(cx, window);
        labels(cx, &view).iter().any(|row| row == "Inline Method")
    });
    let rows = labels(cx, &view);
    assert!(
        rows.iter().any(|row| row == "Extract Variable"),
        "the row that was there stays: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn ctrl_alt_n_on_a_method_of_a_library_says_so_at_the_caret(cx: &mut TestAppContext) {
    let root = project("source-inline-keys", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Inline Method",
        ActionKind::Refactor,
        "refactor.inline",
    )]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, in_library()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-n");
    wait_for(cx, "the note at the caret", |cx| {
        view.read_with(cx, |view, _| view.note_text()).is_some()
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some("Cannot inline a method of a library"),
    );
    assert_eq!(buffer_of(cx, &view), TEXT, "nothing is inlined");
}

#[ui_kit::gpui::test]
fn refactor_this_offers_the_inline_method_of_a_declaration_in_the_project(cx: &mut TestAppContext) {
    let root = project(
        "source-inline-refactor",
        &[("src/Main.txt", TEXT), ("src/Other.txt", "other\n")],
    );
    let path = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Inline Method",
        ActionKind::Refactor,
        "refactor.inline",
    )]);
    let window = open_window(
        cx,
        root.clone(),
        Declaring::new(actions, in_project(&other)),
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-shift-t");
    wait_for(cx, "the Inline Method row of Refactor This", |cx| {
        draw(cx, window);
        labels(cx, &view).iter().any(|row| row == "Inline Method")
    });
    assert!(
        labels(cx, &view).iter().all(|row| row != "Inline..."),
        "the server named the inline row, so the editor's own row is not added: {:?}",
        labels(cx, &view)
    );
}

#[ui_kit::gpui::test]
fn refactor_this_leaves_out_the_inline_method_of_a_library(cx: &mut TestAppContext) {
    let root = project("source-inline-refactor-library", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let actions = PopupLanguage::new();
    actions.set_actions(vec![action(
        &path,
        "Inline Method",
        ActionKind::Refactor,
        "refactor.inline",
    )]);
    let window = open_window(cx, root.clone(), Declaring::new(actions, in_library()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-shift-t");
    cx.run_until_parked();
    draw(cx, window);
    let rows = labels(cx, &view);
    assert!(
        rows.iter()
            .all(|row| row != "Inline Method" && row != "Inline..."),
        "no Inline row for a library's declaration: {rows:?}"
    );
    assert!(!rows.is_empty(), "the other refactorings are still listed");
}
