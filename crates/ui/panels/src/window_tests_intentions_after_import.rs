//! Alt+Enter while the language server imports the project. The list keeps the editor's own
//! rows and a greyed line. Once the server is ready, the server's actions join the open list
//! in place of the greyed line, and the selection stays on the row it was on.
//!
//! cargo test -p panels window_tests_intentions_after_import

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Languages};
use lang_api::{
    ActionEntry, ActionKind, ActionQuery, Answer, Confidence, LanguageId, LanguageSupport,
    Provider, Reply, SourceId, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_generate_list::{GenerateLanguage, project};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("intentions-after-import");
/// The greyed last row of Alt+Enter while the server imports.
const MORE_ACTIONS: &str = "More actions after the project is imported";
/// The row of the editor that is there while the server imports.
const CREATE_TEST: &str = "Create Test";
/// How far the clock moves at each look at a wait, long enough for its timer to fire.
const TICK: Duration = Duration::from_millis(250);

/// A language whose server imports the project. It does not answer the actions until the
/// test makes it ready, and then the questions it got wait for the answer the test sends.
struct Importing {
    language: Arc<GenerateLanguage>,
    ready: AtomicBool,
    can_create_test: bool,
    asked: Mutex<Vec<ActionQuery>>,
    held: Mutex<Vec<Reply<Vec<ActionEntry>>>>,
}

impl Importing {
    fn new(can_create_test: bool) -> Arc<Self> {
        Arc::new(Self {
            language: GenerateLanguage::new(),
            ready: AtomicBool::new(false),
            can_create_test,
            asked: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
        })
    }

    /// The server has imported the project.
    fn set_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Every question about the actions that the server got, in order.
    fn asked(&self) -> Vec<ActionQuery> {
        self.asked.lock().unwrap().clone()
    }

    /// Sends `entries` to the questions the server is holding.
    fn answer(&self, entries: Vec<ActionEntry>, confidence: Confidence) {
        for mut reply in self.held.lock().unwrap().drain(..) {
            let _ = reply.send(Answer::new(entries.clone(), confidence, SOURCE));
        }
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for Importing {
    fn start(&self, query: ActionQuery, _: base::Cancel, reply: Reply<Vec<ActionEntry>>) {
        self.asked.lock().unwrap().push(query);
        self.held.lock().unwrap().push(reply);
    }
}

impl LanguageSupport for Importing {
    fn id(&self) -> LanguageId {
        LanguageId("intentions-after-import")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.language.syntax()
    }

    fn actions(&self) -> Option<&lang_api::ActionProvider> {
        Some(self)
    }

    fn can_create_test(&self, _: &str, _: usize) -> bool {
        self.can_create_test
    }

    fn server_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// An action of the language that the list offers. It has no edit yet: the list shows it,
/// and the server would compute the edit when the row is chosen.
fn action(title: &str, kind: ActionKind) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(kind),
        code: None,
        preferred: false,
        edits: Vec::new(),
        renames: Vec::new(),
        handle: None,
        resolve: Some(format!("resolve {title}")),
        rename: None,
    }
}

/// The window of the project `root`, whose server is `language` and reports that it imports.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<Importing>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("intentions-after-import-state");
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
    let setup = project_setup(scratch("intentions-after-import-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status_lines) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server importing".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status: status_lines,
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

/// Opens the file and puts the caret at `column` of `line`, both from zero.
fn open_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    line: usize,
    column: usize,
) -> gpui::Entity<EditorView> {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);
    view
}

/// The rows of the open Alt+Enter list, as whether each is a quick fix and its label. `None`
/// when no list is open.
fn rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<Vec<(bool, String)>> {
    view.read_with(cx, |view, _| view.actions_visible())
        .map(|rows| rows.rows)
}

/// The labels of the open Alt+Enter list, in order. Empty when no list is open.
fn labels(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Vec<String> {
    rows(cx, view)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

/// The label of the selected row of the open Alt+Enter list.
fn selected(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<String> {
    view.read_with(cx, |view, _| view.actions_selected())
}

/// Whether the greyed line is among the rows of the open list.
fn greyed(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> bool {
    labels(cx, view).iter().any(|label| label == MORE_ACTIONS)
}

/// Opens Alt+Enter at the start of the class while the server imports, and checks that the
/// editor's own row and the greyed line are the list. The server is not asked yet.
fn open_list_while_importing(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    language: &Importing,
) -> gpui::Entity<EditorView> {
    let view = open_at(cx, window, path, 0, 7);
    press(cx, window, "alt-enter");
    cx.run_until_parked();
    assert_eq!(
        labels(cx, &view),
        [CREATE_TEST, MORE_ACTIONS],
        "the editor's row comes first and the greyed line last"
    );
    assert_eq!(selected(cx, &view).as_deref(), Some(CREATE_TEST));
    assert!(
        language.asked().is_empty(),
        "the server is not asked while it imports"
    );
    view
}

/// Makes the server ready and waits until the list has asked it once.
fn ready_and_asked(cx: &mut TestAppContext, language: &Importing) {
    language.set_ready();
    wait_for(cx, "the question about the actions", |cx| {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        !language.asked().is_empty()
    });
}

#[ui_kit::gpui::test]
fn alt_enter_gets_the_actions_of_the_server_once_it_is_ready(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("intentions-after-import-ready", text);
    let language = Importing::new(true);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    let view = open_list_while_importing(cx, window, &path, &language);

    ready_and_asked(cx, &language);
    assert_eq!(language.asked().len(), 1, "the server is asked once");

    language.answer(
        vec![
            action("Add missing import", ActionKind::QuickFix),
            action("Generate getter", ActionKind::Refactor),
        ],
        Confidence::Certain,
    );
    wait_for(cx, "the actions of the server in the open list", |cx| {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        labels(cx, &view)
            .iter()
            .any(|label| label == "Add missing import")
    });

    assert_eq!(
        rows(cx, &view),
        Some(vec![
            (true, "Add missing import".to_owned()),
            (false, "Generate getter".to_owned()),
            (false, CREATE_TEST.to_owned()),
        ]),
        "the quick fix comes first, the greyed line is gone"
    );
    assert_eq!(
        selected(cx, &view).as_deref(),
        Some(CREATE_TEST),
        "the selection stays on the row it was on"
    );

    // A new list at the same place asks the server about the same span.
    press(cx, window, "escape");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.actions_visible())
            .is_none(),
        "Escape closes the list"
    );
    press(cx, window, "alt-enter");
    cx.run_until_parked();
    let asked = language.asked();
    assert_eq!(asked.len(), 2, "the second list asks the ready server");
    assert_eq!(
        asked[0].span, asked[1].span,
        "both lists ask about the same place"
    );
}

#[ui_kit::gpui::test]
fn an_unknown_answer_after_the_wait_still_removes_the_greyed_line(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("intentions-after-import-unknown", text);
    let language = Importing::new(true);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    let view = open_list_while_importing(cx, window, &path, &language);

    ready_and_asked(cx, &language);
    language.answer(Vec::new(), Confidence::Unknown);
    wait_for(cx, "the greyed line to go", |cx| {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        !greyed(cx, &view)
    });

    assert_eq!(
        labels(cx, &view),
        [CREATE_TEST],
        "the editor's row stays and the list says nothing more"
    );
    assert_eq!(selected(cx, &view).as_deref(), Some(CREATE_TEST));
}

#[ui_kit::gpui::test]
fn a_list_closed_while_the_server_imports_never_asks_the_server(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("intentions-after-import-closed", text);
    let language = Importing::new(true);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    let view = open_list_while_importing(cx, window, &path, &language);

    press(cx, window, "escape");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.actions_visible())
            .is_none(),
        "Escape closes the list"
    );

    language.set_ready();
    for _ in 0..4 {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
    assert!(
        language.asked().is_empty(),
        "the wait ended with the list, so nothing is asked"
    );
}
