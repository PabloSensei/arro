//! The Expressions list shows the text of the document as the file spells it, and the
//! name field of an extract opens above a line that sits at the bottom edge of the editor.
//!
//! The language is a stand-in: it answers the expressions and the actions from memory, so
//! the test checks what the window does with the answers.
//!
//! cargo test -p panels window_tests_refactor_popups

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, RefactorSelectionsProvider,
    RenamePosition, SelectionEntry, SelectionQuery, SemanticServer, SourceEdit, SourceId,
    SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{bounds_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-refactor-popups");

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

/// The language of these tests: the expressions, the actions and their edits are set by
/// the test, no process runs.
struct PopupsLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    selections: Mutex<Vec<SelectionEntry>>,
}

impl PopupsLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            selections: Mutex::new(Vec::new()),
        })
    }

    fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
    }

    fn set_selections(&self, selections: Vec<SelectionEntry>) {
        *self.selections.lock().unwrap() = selections;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for PopupsLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .contains_key(uri)
            .then_some(1)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for PopupsLanguage {
    fn start(
        &self,
        query: ActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<ActionEntry>>,
    ) {
        let entries: Vec<ActionEntry> = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<SelectionQuery, Vec<SelectionEntry>> for PopupsLanguage {
    fn start(
        &self,
        _: SelectionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SelectionEntry>>,
    ) {
        let selections = self.selections.lock().unwrap().clone();
        let _ = reply.send(self.answer(selections));
    }
}

impl LanguageSupport for PopupsLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-refactor-popups")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn refactor_selections(&self) -> Option<&RefactorSelectionsProvider> {
        Some(self)
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<PopupsLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
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
    let setup = project_setup(scratch("cache"));
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

/// Whether the popup with the debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Puts the caret on a buffer line and a column, and scrolls the line into sight.
fn caret_at(
    cx: &mut TestAppContext,
    view: &gpui::Entity<editor_view::EditorView>,
    line: usize,
    column: usize,
) {
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();
}

/// The labels of an open pick list.
fn labels(cx: &TestAppContext, view: &gpui::Entity<editor_view::EditorView>) -> Vec<String> {
    view.read_with(cx, |view, _| {
        view.actions_visible()
            .map(|rows| rows.rows.into_iter().map(|(_, label)| label).collect())
            .unwrap_or_default()
    })
}

const CALL_TEXT: &str = "class A {\n    void run() {\n        save(items, count);\n    }\n}\n";

/// The expressions of the call: the server spells the whole call without the space the
/// file has after the comma, as a normalizing server does.
fn call_selections(text: &str) -> Vec<SelectionEntry> {
    let items = text.find("items").expect("the text names the items");
    let call = text.find("save(").expect("the text calls save");
    vec![
        SelectionEntry {
            name: "items".to_owned(),
            offset: items as u32,
            length: "items".len() as u32,
        },
        SelectionEntry {
            name: "save(items,count)".to_owned(),
            offset: call as u32,
            length: "save(items, count)".len() as u32,
        },
    ]
}

#[ui_kit::gpui::test]
fn the_expressions_keep_the_spaces_of_the_document(cx: &mut TestAppContext) {
    let root = project("refactor-popup-expressions", &[("src/Main.txt", CALL_TEXT)]);
    let open = root.join("src").join("Main.txt");
    let language = PopupsLanguage::new();
    language.set_selections(call_selections(CALL_TEXT));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    caret_at(cx, &view, 2, 14);
    draw(cx, window);

    press(cx, window, "ctrl-alt-f");
    wait_for(cx, "the list of expressions", |cx| {
        view.read_with(cx, |view, _| view.actions_visible().is_some())
    });
    draw(cx, window);

    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Expressions")
    );
    assert_eq!(
        labels(cx, &view),
        ["items", "save(items, count)"],
        "the list shows the text of the file, with the space after the comma"
    );
}

/// A file long enough to fill the editor, with the constant on its last line.
fn long_source(lines: usize) -> String {
    let mut text = String::new();
    for index in 0..lines {
        text.push_str(&format!("int value{index} = {index};\n"));
    }
    text.push_str("int total = 42;\n");
    text
}

/// An Extract Constant that adds a declaration at the end of the file.
fn constant_action(path: &Path, text: &str) -> ActionEntry {
    let inserted = "static final int TOTAL = 42;\n";
    let after = format!("{text}{inserted}");
    let name = after
        .find("TOTAL")
        .expect("the declaration names the constant");
    let uri = uri_of(path);
    ActionEntry {
        title: "Extract constant".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.constant".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri.clone(),
            edits: vec![edit_in(text, text.len(), text.len(), inserted)],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(RenamePosition {
            uri,
            offset: name as u32,
            length: "TOTAL".len() as u32,
        }),
    }
}

#[ui_kit::gpui::test]
fn the_name_field_opens_above_a_line_at_the_bottom_edge(cx: &mut TestAppContext) {
    const LINES: usize = 120;
    let text = long_source(LINES);
    let root = project("refactor-popup-name", &[("src/Main.txt", text.as_str())]);
    let open = root.join("src").join("Main.txt");
    let language = PopupsLanguage::new();
    language.set_actions(vec![constant_action(&open, &text)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    caret_at(cx, &view, LINES, 0);
    draw(cx, window);

    press(cx, window, "ctrl-alt-c");
    wait_for(cx, "the field of the name", |cx| {
        draw(cx, window);
        shown(cx, window, "extract-name-popup")
    });
    draw(cx, window);

    let popup = bounds_of(cx, window, "extract-name-popup");
    let editor = view
        .read_with(cx, |view, _| view.editor_bounds())
        .expect("the editor laid out its text");
    let line = view
        .read_with(cx, |view, _| view.caret_line_bounds())
        .expect("the frame drew the row of the caret");
    assert!(
        popup.left() >= editor.left()
            && popup.right() <= editor.right()
            && popup.top() >= editor.top()
            && popup.bottom() <= editor.bottom(),
        "the field {popup:?} stays inside the editor {editor:?}"
    );
    assert!(
        popup.bottom() <= line.top(),
        "the field {popup:?} opens above the line at the bottom {line:?}"
    );
    assert!(
        popup.size.height > px(0.),
        "the field of the name is on screen"
    );
}
