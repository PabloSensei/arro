//! Editor actions while the language server imports the project. A line at the caret says so
//! for the keys that need the server, Find Usages waits for it, Alt+Enter keeps the rows the
//! editor has, and Ctrl+Space says that its list is not complete and offers the words of the
//! file.
//!
//! cargo test -p panels window_tests_editor_importing

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Languages};
use lang_api::{
    ActionEntry, ActionQuery, Answer, CompletionList, Confidence, DocumentSpot, HoverText,
    LanguageId, LanguageSupport, Provider, Reply, SignatureHelp, SignatureQuery, SourceId,
    SyntaxProvider, UsageEntry, UsageKind,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_generate_list::{GenerateLanguage, project};
use crate::window_tests_ui::wait_for;
use crate::window_tests_usages_tabs::{hub, usage};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("editor-importing-server");
/// The note of a key that needs the server, while the server imports.
const WAITING: &str = "Waiting for the language server to finish importing the project…";
/// The line of the completion list while the server imports.
const IMPORTING: &str = "The project is still importing, results are incomplete";
/// The greyed last row of Alt+Enter while the server imports.
const MORE_ACTIONS: &str = "More actions after the project is imported";
/// The line of the Usages window while the search waits for the server.
const FIND_WAITING: &str = "Waiting for the language server…";
/// How far the clock moves at each look at a wait, long enough for its timer to fire.
const TICK: Duration = Duration::from_millis(250);

/// A language whose server imports the project: it answers nothing until `ready` is set.
/// Its documentation answers wait for a test to send them, and the other questions are held,
/// as a server that is still importing holds them.
struct Importing {
    language: Arc<GenerateLanguage>,
    ready: AtomicBool,
    can_create_test: bool,
    uses: Vec<UsageEntry>,
    hover: Mutex<Vec<Reply<HoverText>>>,
    held: Mutex<Vec<Box<dyn Any + Send>>>,
}

impl Importing {
    fn new(uses: Vec<UsageEntry>, can_create_test: bool) -> Arc<Self> {
        Arc::new(Self {
            language: GenerateLanguage::new(),
            ready: AtomicBool::new(false),
            can_create_test,
            uses,
            hover: Mutex::new(Vec::new()),
            held: Mutex::new(Vec::new()),
        })
    }

    /// The server has imported the project: from now on it answers.
    fn set_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    /// Sends the documentation that the server held back.
    fn answer_hover(&self, text: &str) {
        for mut reply in self.hover.lock().unwrap().drain(..) {
            let _ = reply.send(Answer::new(
                HoverText(text.to_owned()),
                Confidence::Certain,
                SOURCE,
            ));
        }
    }

    fn hold<T: Send + 'static>(&self, reply: Reply<T>) {
        self.held.lock().unwrap().push(Box::new(reply));
    }
}

impl Provider<DocumentSpot, HoverText> for Importing {
    fn start(&self, _: DocumentSpot, _: base::Cancel, reply: Reply<HoverText>) {
        self.hover.lock().unwrap().push(reply);
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for Importing {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: Reply<Vec<UsageEntry>>) {
        let _ = reply.send(Answer::new(self.uses.clone(), Confidence::Certain, SOURCE));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for Importing {
    fn start(&self, _: ActionQuery, _: base::Cancel, reply: Reply<Vec<ActionEntry>>) {
        self.hold(reply);
    }
}

impl Provider<DocumentSpot, CompletionList> for Importing {
    fn start(&self, _: DocumentSpot, _: base::Cancel, reply: Reply<CompletionList>) {
        self.hold(reply);
    }
}

impl Provider<SignatureQuery, SignatureHelp> for Importing {
    fn start(&self, _: SignatureQuery, _: base::Cancel, reply: Reply<SignatureHelp>) {
        self.hold(reply);
    }
}

impl LanguageSupport for Importing {
    fn id(&self) -> LanguageId {
        LanguageId("editor-importing")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.language.syntax()
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        Some(self)
    }

    fn hover(&self) -> Option<&lang_api::HoverProvider> {
        Some(self)
    }

    fn signature_info(&self) -> Option<&lang_api::SignatureInfoProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&lang_api::ActionProvider> {
        Some(self)
    }

    fn completion(&self) -> Option<&lang_api::CompletionProvider> {
        Some(self)
    }

    fn can_create_test(&self, _: &str, _: usize) -> bool {
        self.can_create_test
    }

    fn server_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }
}

/// The window of the project `root`, whose server is `language` and reports that it imports.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<Importing>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("editor-importing-state");
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
    let setup = project_setup(scratch("editor-importing-cache"));
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

/// The note under the caret of the editor, if one is on screen.
fn note(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<String> {
    view.read_with(cx, |view, _| view.note_text())
}

/// Whether the Usages window has a search that waits for the server.
fn waiting_search(cx: &TestAppContext) -> bool {
    hub(cx).read_with(cx, |hub, _| hub.is_waiting())
}

/// The rows of the Usages window, found so far.
fn found_rows(cx: &TestAppContext) -> usize {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().len())
}

/// The line the Usages window shows where its rows would be.
fn usages_line(cx: &TestAppContext) -> String {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).empty_text())
}

#[ui_kit::gpui::test]
fn ctrl_q_says_the_server_imports_and_the_documentation_replaces_the_line(cx: &mut TestAppContext) {
    let text = "class A {\n    String name;\n}\n";
    let (root, path) = project("editor-importing-doc", text);
    let language = Importing::new(Vec::new(), false);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    let view = open_at(cx, window, &path, 1, 14);

    press(cx, window, "ctrl-q");
    cx.run_until_parked();
    assert_eq!(
        note(cx, &view).as_deref(),
        Some(WAITING),
        "the key says the server imports, at once"
    );

    language.set_ready();
    language.answer_hover("The name of the field.");
    wait_for(cx, "the documentation", |cx| {
        view.read_with(cx, |view, _| view.quick_doc_text())
            .is_some_and(|text| text.contains("The name of the field."))
    });
    assert_eq!(
        note(cx, &view),
        None,
        "the documentation takes the place of the line"
    );
}

#[ui_kit::gpui::test]
fn ctrl_p_says_the_server_imports_while_it_does(cx: &mut TestAppContext) {
    let text = "class A {\n    void run(int count) {}\n}\n";
    let (root, path) = project("editor-importing-parameters", text);
    let language = Importing::new(Vec::new(), false);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    let view = open_at(cx, window, &path, 1, 18);

    press(cx, window, "ctrl-p");
    cx.run_until_parked();
    assert_eq!(note(cx, &view).as_deref(), Some(WAITING));
}

#[ui_kit::gpui::test]
fn alt_f7_opens_the_usages_window_with_the_waiting_line_and_searches_when_ready(
    cx: &mut TestAppContext,
) {
    let text = "class Owner {\n}\n";
    let (root, path) = project("editor-importing-find", text);
    let uses = vec![usage(&path, text, 6, 11, UsageKind::Read)];
    let language = Importing::new(uses, false);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    open_at(cx, window, &path, 0, 8);

    press(cx, window, "alt-f7");
    cx.run_until_parked();
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "the Usages window opens"
    );
    assert!(waiting_search(cx), "the search waits for the server");
    assert_eq!(usages_line(cx), FIND_WAITING);
    assert_eq!(
        found_rows(cx),
        0,
        "nothing is searched before the server is ready"
    );

    language.set_ready();
    wait_for(cx, "the results of the search", |cx| {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
        found_rows(cx) > 0
    });
    assert!(!waiting_search(cx), "the results end the wait");
}

#[ui_kit::gpui::test]
fn escape_ends_the_wait_of_the_usages_window(cx: &mut TestAppContext) {
    let text = "class Owner {\n}\n";
    let (root, path) = project("editor-importing-find-escape", text);
    let uses = vec![usage(&path, text, 6, 11, UsageKind::Read)];
    let language = Importing::new(uses, false);
    let window = open_window(cx, root, language.clone());
    cx.run_until_parked();
    open_at(cx, window, &path, 0, 8);

    press(cx, window, "alt-f7");
    cx.run_until_parked();
    assert!(waiting_search(cx), "the search waits for the server");
    let focus = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).focus());
    let focused = window
        .update(cx, |_, window, cx| window.focused(cx))
        .unwrap();
    assert!(focused == Some(focus), "the Usages window takes the focus");

    press(cx, window, "escape");
    cx.run_until_parked();
    assert!(!waiting_search(cx), "Escape ends the wait");

    language.set_ready();
    for _ in 0..4 {
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
    assert_eq!(found_rows(cx), 0, "the search does not run after Escape");
}

#[ui_kit::gpui::test]
fn alt_enter_keeps_the_rows_of_the_editor_and_greys_the_line_about_the_rest(
    cx: &mut TestAppContext,
) {
    let text = "class A {\n}\n";
    let (root, path) = project("editor-importing-actions", text);
    let language = Importing::new(Vec::new(), true);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    let view = open_at(cx, window, &path, 0, 7);

    press(cx, window, "alt-enter");
    cx.run_until_parked();
    let rows = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list of Alt+Enter is open");
    let labels: Vec<&str> = rows.rows.iter().map(|(_, label)| label.as_str()).collect();
    assert!(
        labels.contains(&"Create Test"),
        "the editor's own row stays: {labels:?}"
    );
    assert_eq!(
        labels.last(),
        Some(&MORE_ACTIONS),
        "the greyed line comes last: {labels:?}"
    );
}

#[ui_kit::gpui::test]
fn alt_enter_without_rows_of_the_editor_shows_the_waiting_line(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("editor-importing-actions-empty", text);
    let language = Importing::new(Vec::new(), false);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    let view = open_at(cx, window, &path, 0, 7);

    press(cx, window, "alt-enter");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.actions_visible())
            .is_none(),
        "no empty menu opens"
    );
    assert_eq!(note(cx, &view).as_deref(), Some(WAITING));
}

#[ui_kit::gpui::test]
fn ctrl_space_says_the_project_is_importing_and_offers_the_words_of_the_file(
    cx: &mut TestAppContext,
) {
    let text = "alphabet\nalpha\n";
    let (root, path) = project("editor-importing-words", text);
    let language = Importing::new(Vec::new(), false);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    let view = open_at(cx, window, &path, 1, 5);

    press(cx, window, "ctrl-space");
    cx.run_until_parked();
    assert_eq!(note(cx, &view).as_deref(), Some(IMPORTING));
    let rows = view
        .read_with(cx, |view, _| view.completion_rows())
        .expect("the completion list is open");
    assert!(
        rows.iter().any(|row| row == "alphabet"),
        "the words of the file are offered: {rows:?}"
    );
}
