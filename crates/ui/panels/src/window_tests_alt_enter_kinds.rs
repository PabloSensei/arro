//! Alt+Enter at a place lists the quick fixes and the refactorings of the place, each with the
//! bulb of its kind. The source actions of the whole file are not listed there: Alt+Insert
//! lists them after its own rows. A list that the filter leaves empty says that no intention
//! is available.
//!
//! cargo test -p panels window_tests_alt_enter_kinds

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Languages};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, Answer, Confidence, GenerateChoice, GenerateKind,
    GenerateProvider, GenerateQuery, Generation, LanguageFn, LanguageId, LanguageSupport, Provider,
    Queries, SourceEdit, SourceId, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_editor_popups::{PopupLanguage, open_window_with};
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("alt-enter-kinds");
const TEXT: &str = "problem\n";
/// The bulbs of the rows in the Arro set: the file of the light look and the one of the dark.
const QUICK_FIX_BULB: [&str; 2] = [
    "icons/codeInsight/quickFix.svg",
    "icons/codeInsight/quickFix_dark.svg",
];
const WARNING_BULB: [&str; 2] = [
    "icons/codeInsight/intention.svg",
    "icons/codeInsight/intention_dark.svg",
];
const PLAIN_BULB: [&str; 2] = [
    "icons/codeInsight/intentionBulb.svg",
    "icons/codeInsight/intentionBulb_dark.svg",
];

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

/// A language whose server answers the Source Generate questions of a class that has a
/// field, and whose source actions come from `actions`.
struct Generating {
    syntax: NoSyntax,
    actions: Arc<PopupLanguage>,
}

impl Provider<GenerateQuery, Generation> for Generating {
    fn start(&self, query: GenerateQuery, _: base::Cancel, mut reply: lang_api::Reply<Generation>) {
        // Only equals() and hashCode() have a field to write; the others have nothing to offer.
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

impl LanguageSupport for Generating {
    fn id(&self) -> LanguageId {
        LanguageId("alt-enter-kinds-generate")
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

    fn generate(&self) -> Option<&GenerateProvider> {
        Some(self)
    }
}

/// The window of the project `root`, whose server is `language`.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<Generating>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("alt-enter-kinds-state");
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
    let setup = project_setup(scratch("alt-enter-kinds-cache"));
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

/// An action of the server at the place of the tests, with the edit it would make, so the
/// list shows it without asking for more.
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

/// The server's answer for a file: an action of the file, a source action, a refactoring and
/// a quick fix.
fn mixed_answer(path: &Path) -> Vec<ActionEntry> {
    vec![
        action(
            path,
            "Organize imports",
            ActionKind::OrganizeImports,
            "source.organizeImports",
        ),
        action(
            path,
            "Generate toString()",
            ActionKind::Source,
            "source.generate.toString",
        ),
        action(
            path,
            "Inline Method",
            ActionKind::Refactor,
            "refactor.inline",
        ),
        action(path, "Add import", ActionKind::QuickFix, "quickfix.import"),
    ]
}

/// Whether the element with the debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The file of a bulb in the look the window draws with: the theme of the window says which.
fn bulb(cx: &mut TestAppContext, files: [&'static str; 2]) -> &'static str {
    let dark = cx.read(|cx| cx.global::<Theme>().dark);
    files[usize::from(dark)]
}

/// The rows of the open list, as whether each is a quick fix and its label. `None` when no
/// list is open.
fn rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<Vec<(bool, String)>> {
    view.read_with(cx, |view, _| view.actions_visible())
        .map(|rows| rows.rows)
}

/// The labels of the open list, in order. Empty when no list is open.
fn labels(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Vec<String> {
    rows(cx, view)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

#[ui_kit::gpui::test]
fn alt_enter_lists_the_quick_fix_and_the_refactoring_with_their_bulbs(cx: &mut TestAppContext) {
    let root = project("alt-enter-kinds-rows", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    language.set_actions(mixed_answer(&path));
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert_eq!(
        rows(cx, &view),
        Some(vec![
            (true, "Add import".to_owned()),
            (false, "Inline Method".to_owned()),
        ]),
        "the quick fix comes first, then the refactoring; the source actions of the file are left out"
    );
    assert!(
        shown(cx, window, "actions-popup"),
        "Alt+Enter draws the list"
    );
    let quick_fix = bulb(cx, QUICK_FIX_BULB);
    assert!(
        shown(cx, window, quick_fix),
        "a quick fix of an error has the red bulb: {quick_fix}"
    );
    let plain = bulb(cx, PLAIN_BULB);
    assert!(shown(cx, window, plain), "a refactoring has the plain bulb");
    let warning = bulb(cx, WARNING_BULB);
    assert!(
        !shown(cx, window, warning),
        "no quick fix of a warning is at the caret, so no yellow bulb"
    );
}

#[ui_kit::gpui::test]
fn alt_insert_lists_the_source_actions_of_the_file(cx: &mut TestAppContext) {
    let root = project("alt-enter-kinds-insert", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    language.set_actions(mixed_answer(&path));
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-insert");
    wait_for(cx, "the Generate list", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| view.actions_visible())
            .is_some()
    });
    assert_eq!(
        labels(cx, &view),
        ["Generate toString()"],
        "Alt+Insert gives the source action of the file, and the quick fix and the refactoring stay out"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Generate")
    );
}

#[ui_kit::gpui::test]
fn alt_enter_with_only_source_actions_says_no_intentions_are_available(cx: &mut TestAppContext) {
    let root = project("alt-enter-kinds-empty", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    language.set_actions(vec![
        action(
            &path,
            "Generate toString()",
            ActionKind::Source,
            "source.generate.toString",
        ),
        action(
            &path,
            "Organize imports",
            ActionKind::OrganizeImports,
            "source.organizeImports",
        ),
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        rows(cx, &view).is_none(),
        "an empty list does not open at the caret"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some("No intentions available"),
        "the caret says that no intention is available"
    );
}

#[ui_kit::gpui::test]
fn alt_insert_of_a_class_lists_the_source_actions_after_the_rows_of_source_generate(
    cx: &mut TestAppContext,
) {
    let text = "class A {\n}\n";
    let root = project("alt-enter-kinds-generate", &[("src/A.txt", text)]);
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
            "Organize imports",
            ActionKind::OrganizeImports,
            "source.organizeImports",
        ),
        action(
            &path,
            "Change modifiers to final where possible",
            ActionKind::Source,
            "source.generate.finalModifiers",
        ),
    ]);
    let language = Arc::new(Generating {
        syntax: NoSyntax(Queries::default()),
        actions,
    });
    let window = open_window(cx, root.clone(), language);
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
    assert_eq!(
        labels(cx, &view),
        [
            "Constructor",
            "Getter",
            "Setter",
            "Getter and Setter",
            "equals() and hashCode()",
            "toString()",
            "Override Methods",
            "Implement Methods",
            "Sort members",
            "Change modifiers to final where possible",
        ],
        "the rows of Source Generate come first; the server's toString is not listed twice, \
         and Organize Imports stays out"
    );
}
