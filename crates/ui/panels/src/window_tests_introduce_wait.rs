//! Introduce Variable while the language answers late. The language holds each answer until
//! the test releases it, so the test decides whether a key or a move comes before the answer
//! or after it: Escape, another window, a move of the caret and an edit take the wait back,
//! and the answer that comes later opens nothing. A wait that lasts says that it is looking
//! for expressions, and Escape closes that hint.
//!
//! cargo test -p panels window_tests_introduce_wait

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, RefactorSelectionsProvider,
    RenamePosition, RenameProvider, RenameQuery, Reply, SelectionEntry, SelectionQuery,
    SemanticServer, SourceId, SyntaxProvider, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, Theme};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, scratch, type_text};
use crate::window_tests_edit::text_point;
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-introduce-wait");

/// The hint of a wait that lasts, as the editor shows it under the caret.
const HINT: &str = "Looking for expressions…";

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

/// One line of code with an expression under the caret.
const ONE_LINE: &str =
    "class A {\n    String last(Owner owner) {\n        return owner.getLastName();\n    }\n}\n";
/// The same line after the extract, with the name the server chose.
const ONE_LINE_EXTRACTED: &str = "class A {\n    String last(Owner owner) {\n        String value = owner.getLastName();\n        return value;\n    }\n}\n";

/// The number of the line that holds the expression and the column of the owner in it.
fn expression_place(text: &str) -> (usize, usize) {
    let line = text
        .lines()
        .position(|line| line.contains("return owner."))
        .expect("the text has the expression");
    let column = text
        .lines()
        .nth(line)
        .and_then(|line| line.find("owner"))
        .expect("the line starts the expression with the owner");
    (line, column)
}

/// The first byte of the expression the return statement uses.
fn expression_at(text: &str) -> usize {
    text.find("return ").expect("the text has the return") + "return ".len()
}

/// The selection answers held until the test sends them, each with the answer it will send.
type HeldSelections = Mutex<Vec<(Reply<Vec<SelectionEntry>>, Vec<SelectionEntry>)>>;

/// The action answers held until the test sends them, each with the answer it will send.
type HeldActions = Mutex<Vec<(Reply<Vec<ActionEntry>>, Vec<ActionEntry>)>>;

/// The language of the test: its answers are set, and each kind of answer can be held
/// until the test sends it.
struct WaitLanguage {
    syntax: NoSyntax,
    actions: Mutex<Vec<ActionEntry>>,
    selections: Mutex<Vec<SelectionEntry>>,
    hold_selections: AtomicBool,
    hold_actions: AtomicBool,
    held_selections: HeldSelections,
    held_actions: HeldActions,
    selections_asked: AtomicUsize,
    actions_asked: AtomicUsize,
}

impl WaitLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            actions: Mutex::new(Vec::new()),
            selections: Mutex::new(Vec::new()),
            hold_selections: AtomicBool::new(false),
            hold_actions: AtomicBool::new(false),
            held_selections: Mutex::new(Vec::new()),
            held_actions: Mutex::new(Vec::new()),
            selections_asked: AtomicUsize::new(0),
            actions_asked: AtomicUsize::new(0),
        })
    }

    fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
    }

    fn set_selections(&self, selections: Vec<SelectionEntry>) {
        *self.selections.lock().unwrap() = selections;
    }

    fn hold_selections(&self, hold: bool) {
        self.hold_selections.store(hold, Ordering::SeqCst);
    }

    fn hold_actions(&self, hold: bool) {
        self.hold_actions.store(hold, Ordering::SeqCst);
    }

    fn selections_asked(&self) -> usize {
        self.selections_asked.load(Ordering::SeqCst)
    }

    fn actions_asked(&self) -> usize {
        self.actions_asked.load(Ordering::SeqCst)
    }

    /// Sends the held answers about the expressions, as the server would at this moment.
    fn release_selections(&self) {
        let held: Vec<_> = self.held_selections.lock().unwrap().drain(..).collect();
        for (mut reply, entries) in held {
            let _ = reply.send(self.answer(entries));
        }
    }

    /// Sends the held answers about the actions.
    fn release_actions(&self) {
        let held: Vec<_> = self.held_actions.lock().unwrap().drain(..).collect();
        for (mut reply, entries) in held {
            let _ = reply.send(self.answer(entries));
        }
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for WaitLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}

    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<DocumentEdit>) {}

    fn save(&self, _: &str) {}

    fn close(&self, _: &str) {}

    fn version(&self, _: &str) -> Option<i32> {
        Some(1)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for WaitLanguage {
    fn start(
        &self,
        query: ActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<ActionEntry>>,
    ) {
        self.actions_asked.fetch_add(1, Ordering::SeqCst);
        let entries: Vec<ActionEntry> = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        if self.hold_actions.load(Ordering::SeqCst) {
            self.held_actions.lock().unwrap().push((reply, entries));
        } else {
            let _ = reply.send(self.answer(entries));
        }
    }
}

impl Provider<SelectionQuery, Vec<SelectionEntry>> for WaitLanguage {
    fn start(
        &self,
        _: SelectionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SelectionEntry>>,
    ) {
        self.selections_asked.fetch_add(1, Ordering::SeqCst);
        let selections = self.selections.lock().unwrap().clone();
        if self.hold_selections.load(Ordering::SeqCst) {
            self.held_selections
                .lock()
                .unwrap()
                .push((reply, selections));
        } else {
            let _ = reply.send(self.answer(selections));
        }
    }
}

impl Provider<RenameQuery, WorkspaceChange> for WaitLanguage {
    fn start(&self, _: RenameQuery, _: base::Cancel, mut reply: lang_api::Reply<WorkspaceChange>) {
        let _ = reply.send(self.answer(WorkspaceChange::default()));
    }
}

impl LanguageSupport for WaitLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-introduce-wait")
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

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self)
    }
}

/// A place in the text the server names, as the extract answers name it.
fn selection(name: &str, offset: usize, length: usize) -> SelectionEntry {
    SelectionEntry {
        name: name.to_owned(),
        offset: offset as u32,
        length: length as u32,
    }
}

/// The expressions under the caret, the outer one named first.
fn nested_selections(text: &str) -> Vec<SelectionEntry> {
    let at = expression_at(text);
    vec![
        selection("owner.getLastName()", at, "owner.getLastName()".len()),
        selection("owner", at, "owner".len()),
    ]
}

/// The action of a single occurrence: the whole line becomes the extracted one.
fn single_action(path: &Path, text: &str) -> ActionEntry {
    let uri = uri_of(path);
    let edit = edit_in(text, 0, text.len(), ONE_LINE_EXTRACTED);
    let rename = RenamePosition {
        uri: uri.clone(),
        offset: ONE_LINE_EXTRACTED.find("value").unwrap_or(0) as u32,
        length: "value".len() as u32,
    };
    ActionEntry {
        title: "Extract to local variable".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.variable".to_owned()),
        preferred: false,
        edits: vec![lang_api::SourceEdit {
            uri,
            edits: vec![edit],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(rename),
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<WaitLanguage>,
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
    let setup = crate::window_tests::project_setup(scratch("cache"));
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

/// Clicks into the text of the editor, at a buffer line and a column.
fn click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) {
    let position = text_point(cx, window, line, column);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

/// Whether the popup with the debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Opens the file with the caret on the owner of the expression, and returns the editor.
fn start_at_the_owner(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    open: &Path,
) -> gpui::Entity<editor_view::EditorView> {
    open_path(cx, window, open);
    let view = editor_of(cx, window, open);
    let (line, column) = expression_place(ONE_LINE);
    click_text(cx, window, line, column + 2);
    view
}

/// Whether nothing of the refactoring is on the screen after a frame: no field of the name
/// and no list of expressions or occurrences.
fn nothing_opened(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<editor_view::EditorView>,
) -> bool {
    draw(cx, window);
    !shown(cx, window, "extract-name-popup")
        && view
            .read_with(cx, |view, _| view.actions_visible())
            .is_none()
}

#[ui_kit::gpui::test]
fn escape_before_the_expressions_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-escape", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    assert_eq!(language.selections_asked(), 1, "the expressions were asked");
    press(cx, window, "escape");
    language.release_selections();
    cx.run_until_parked();

    assert!(
        nothing_opened(cx, window, &view),
        "the late answer opened something"
    );
    assert_eq!(language.actions_asked(), 0, "the wait did not go on");
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}

#[ui_kit::gpui::test]
fn escape_before_the_actions_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-actions", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_actions(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    // The selection of the word is a choice of its own: the expressions are not asked, and
    // the actions of the selection wait for the language.
    press(cx, window, "ctrl-shift-right");
    draw(cx, window);
    press(cx, window, "ctrl-alt-v");
    assert_eq!(
        language.selections_asked(),
        0,
        "a selection asks no expressions"
    );
    assert_eq!(
        language.actions_asked(),
        1,
        "the selection went on to its actions"
    );
    press(cx, window, "escape");
    language.release_actions();
    cx.run_until_parked();

    assert!(
        nothing_opened(cx, window, &view),
        "the late actions opened something"
    );
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}

#[ui_kit::gpui::test]
fn another_window_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-window", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    press(cx, window, "alt-9");
    draw(cx, window);
    language.release_selections();
    cx.run_until_parked();

    assert!(
        nothing_opened(cx, window, &view),
        "the Git Log let the refactoring open"
    );
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}

#[ui_kit::gpui::test]
fn a_move_of_the_caret_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-caret", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    let (line, column) = expression_place(ONE_LINE);
    click_text(cx, window, line, column + 6);
    language.release_selections();
    cx.run_until_parked();

    assert!(
        nothing_opened(cx, window, &view),
        "the moved caret let the refactoring open"
    );
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}

#[ui_kit::gpui::test]
fn an_edit_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-edit", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    type_text(cx, window, "x");
    language.release_selections();
    cx.run_until_parked();

    assert!(
        nothing_opened(cx, window, &view),
        "the edit let the refactoring open"
    );
    let text = buffer_of(cx, &view);
    assert!(
        text.contains('x'),
        "the typed letter reached the text: {text}"
    );
    assert!(!text.contains("String value"), "{text}");
}

#[ui_kit::gpui::test]
fn a_long_wait_shows_the_hint_until_the_answer(cx: &mut TestAppContext) {
    let root = project("introduce-wait-long", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some(HINT),
        "a long wait says that it looks for expressions"
    );

    // The hint does not end the wait: it stays, and the answer that comes later opens the list.
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some(HINT),
        "the hint stays while the wait goes on"
    );
    language.release_selections();
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Expressions"),
        "the late answer opened the list of expressions"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()),
        None,
        "the answer took the hint away"
    );
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}

#[ui_kit::gpui::test]
fn escape_closes_the_hint_and_takes_the_variable_back(cx: &mut TestAppContext) {
    let root = project("introduce-wait-hint", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = WaitLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(vec![single_action(&open, ONE_LINE)]);
    language.hold_selections(true);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let view = start_at_the_owner(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()),
        None,
        "a short wait shows no hint"
    );

    cx.executor().advance_clock(Duration::from_millis(600));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some(HINT),
        "a long wait says that it looks for expressions"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()),
        None,
        "Escape closes the hint"
    );
    language.release_selections();
    cx.run_until_parked();
    assert!(
        nothing_opened(cx, window, &view),
        "the late answer opened something"
    );
    assert_eq!(buffer_of(cx, &view), ONE_LINE, "the text did not change");
}
