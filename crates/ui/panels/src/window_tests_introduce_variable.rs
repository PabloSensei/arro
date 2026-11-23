//! Introduce Variable at the caret: the list of the nested expressions under it, the
//! list of the occurrences only when the expression is not the only one, and the name of
//! the variable typed over at once when there is nothing to choose.
//!
//! The language is a stand-in: it answers the expressions and the actions from memory,
//! so the test checks what the window does with the answers.
//!
//! cargo test -p panels window_tests_introduce_variable

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use editor_view::{Languages, uri_path};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, RefactorSelectionsProvider,
    RenamePosition, RenameProvider, RenameQuery, SelectionEntry, SelectionQuery, SemanticServer,
    SourceEdit, SourceId, SyntaxProvider, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, Theme};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_edit::text_point;
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-introduce-variable");

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

/// Two lines that use the same expression.
const TWO_LINES: &str = "class A {\n    String last(Owner owner) {\n        return owner.getLastName();\n    }\n\n    String first(Owner owner) {\n        return owner.getLastName();\n    }\n}\n";

/// The number of the line that holds the expression and the column of its start.
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

/// The language of the test: every answer is set, no process runs.
struct VariableLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    selections: Mutex<Vec<SelectionEntry>>,
    selections_asked: AtomicUsize,
}

impl VariableLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            selections: Mutex::new(Vec::new()),
            selections_asked: AtomicUsize::new(0),
        })
    }

    fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
    }

    fn set_selections(&self, selections: Vec<SelectionEntry>) {
        *self.selections.lock().unwrap() = selections;
    }

    fn selections_asked(&self) -> usize {
        self.selections_asked.load(Ordering::SeqCst)
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for VariableLanguage {
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

impl Provider<ActionQuery, Vec<ActionEntry>> for VariableLanguage {
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

impl Provider<SelectionQuery, Vec<SelectionEntry>> for VariableLanguage {
    fn start(
        &self,
        _: SelectionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SelectionEntry>>,
    ) {
        self.selections_asked.fetch_add(1, Ordering::SeqCst);
        let selections = self.selections.lock().unwrap().clone();
        let _ = reply.send(self.answer(selections));
    }
}

impl Provider<RenameQuery, WorkspaceChange> for VariableLanguage {
    fn start(
        &self,
        query: RenameQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<WorkspaceChange>,
    ) {
        let text = self
            .documents
            .lock()
            .unwrap()
            .get(&query.spot.uri)
            .map(ToString::to_string)
            .or_else(|| uri_path(&query.spot.uri).and_then(|path| fs::read_to_string(path).ok()));
        let Some(text) = text else {
            let _ = reply.send(self.answer(WorkspaceChange::default()));
            return;
        };
        let mut edits = Vec::new();
        let mut rest = 0;
        while let Some(at) = text[rest..].find("value") {
            let start = rest + at;
            edits.push(edit_in(
                &text,
                start,
                start + "value".len(),
                &query.new_name,
            ));
            rest = start + "value".len();
        }
        let change = WorkspaceChange {
            edits: vec![SourceEdit {
                uri: query.spot.uri.clone(),
                edits,
            }],
            renames: Vec::new(),
        };
        let _ = reply.send(self.answer(change));
    }
}

impl LanguageSupport for VariableLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-introduce-variable")
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

/// The expressions under the caret, the outer one named first so that the window has to
/// put the inner one first.
fn nested_selections(text: &str) -> Vec<SelectionEntry> {
    let at = expression_at(text);
    vec![
        selection("owner.getLastName()", at, "owner.getLastName()".len()),
        selection("owner", at, "owner".len()),
    ]
}

/// The two rows of an extract of a value: this place, and every place.
fn variable_actions(
    path: &Path,
    after: &str,
    this_edits: Vec<DocumentEdit>,
    every_edits: Vec<DocumentEdit>,
) -> Vec<ActionEntry> {
    let uri = uri_of(path);
    let rename = RenamePosition {
        uri: uri.clone(),
        offset: after.find("value").unwrap_or(0) as u32,
        length: "value".len() as u32,
    };
    let this = ActionEntry {
        title: "Extract to local variable".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.variable".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri.clone(),
            edits: this_edits,
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(rename.clone()),
    };
    let every = ActionEntry {
        title: "Extract to local variable (replace all occurrences)".to_owned(),
        edits: vec![SourceEdit {
            uri,
            edits: every_edits,
        }],
        ..this.clone()
    };
    vec![this, every]
}

/// The actions of a single occurrence: the whole line becomes the extracted one.
fn one_occurrence_actions(path: &Path, text: &str) -> Vec<ActionEntry> {
    let edit = edit_in(text, 0, text.len(), ONE_LINE_EXTRACTED);
    variable_actions(path, ONE_LINE_EXTRACTED, vec![edit.clone()], vec![edit])
}

/// The actions of two occurrences: the all row replaces both of them.
fn two_occurrence_actions(path: &Path, text: &str) -> Vec<ActionEntry> {
    let first = text
        .find("owner.getLastName()")
        .expect("the first expression");
    let second = text[first + 1..]
        .find("owner.getLastName()")
        .expect("the second expression")
        + first
        + 1;
    let length = "owner.getLastName()".len();
    let mut after = text.to_owned();
    after.replace_range(second..second + length, "value");
    after.replace_range(first..first + length, "value");
    variable_actions(
        path,
        &after,
        vec![edit_in(text, first, first + length, "value")],
        vec![
            edit_in(text, first, first + length, "value"),
            edit_in(text, second, second + length, "value"),
        ],
    )
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<VariableLanguage>,
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

/// The labels of an open pick list.
fn labels(cx: &TestAppContext, view: &gpui::Entity<editor_view::EditorView>) -> Vec<String> {
    view.read_with(cx, |view, _| {
        view.actions_visible()
            .map(|rows| rows.rows.into_iter().map(|(_, label)| label).collect())
            .unwrap_or_default()
    })
}

#[ui_kit::gpui::test]
fn expressions_of_the_caret_are_listed_inner_first(cx: &mut TestAppContext) {
    let root = project("introduce-expressions", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = VariableLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(one_occurrence_actions(&open, ONE_LINE));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    let (line, column) = expression_place(ONE_LINE);
    click_text(cx, window, line, column + 2);

    press(cx, window, "ctrl-alt-v");
    wait_for(cx, "the list of expressions", |cx| {
        view.read_with(cx, |view, _| view.actions_visible().is_some())
    });
    draw(cx, window);

    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Expressions")
    );
    assert_eq!(labels(cx, &view), ["owner", "owner.getLastName()"]);
    let at = expression_at(ONE_LINE);
    assert_eq!(
        view.read_with(cx, |view, _| view.action_highlight()),
        Some(at..at + "owner".len()),
        "the first row marks the inner expression in the text"
    );

    press(cx, window, "down");
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_selected())
            .as_deref(),
        Some("owner.getLastName()")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.action_highlight()),
        Some(at..at + "owner.getLastName()".len()),
        "the second row marks the whole expression"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.actions_visible())
            .is_none(),
        "Escape cancels the whole refactoring"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.action_highlight()),
        None,
        "the mark of the expression goes with the list"
    );
}

#[ui_kit::gpui::test]
fn one_occurrence_goes_straight_to_the_name(cx: &mut TestAppContext) {
    let root = project("introduce-one", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = VariableLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(one_occurrence_actions(&open, ONE_LINE));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    let (line, column) = expression_place(ONE_LINE);
    click_text(cx, window, line, column + 2);

    press(cx, window, "ctrl-alt-v");
    wait_for(cx, "the list of expressions", |cx| {
        view.read_with(cx, |view, _| view.actions_visible().is_some())
    });
    press(cx, window, "down");
    press(cx, window, "enter");
    wait_for(cx, "the field of the name", |cx| {
        draw(cx, window);
        shown(cx, window, "extract-name-popup")
    });

    assert!(
        !shown(cx, window, "actions-popup"),
        "one occurrence needs no list of occurrences"
    );
    assert_eq!(
        buffer_of(cx, &view),
        ONE_LINE,
        "the text changes only when the name is confirmed"
    );

    type_text(cx, window, "renamed");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    let text = buffer_of(cx, &view);
    assert!(
        text.contains("String renamed = owner.getLastName();"),
        "{text}"
    );
    assert!(text.contains("return renamed;"), "{text}");
}

#[ui_kit::gpui::test]
fn the_list_of_occurrences_names_the_count(cx: &mut TestAppContext) {
    let root = project("introduce-two", &[("src/Main.txt", TWO_LINES)]);
    let open = root.join("src").join("Main.txt");
    let language = VariableLanguage::new();
    language.set_selections(nested_selections(TWO_LINES));
    language.set_actions(two_occurrence_actions(&open, TWO_LINES));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    let (line, column) = expression_place(TWO_LINES);
    click_text(cx, window, line, column + 2);

    press(cx, window, "ctrl-alt-v");
    wait_for(cx, "the list of expressions", |cx| {
        view.read_with(cx, |view, _| view.actions_visible().is_some())
    });
    press(cx, window, "down");
    press(cx, window, "enter");
    wait_for(cx, "the list of occurrences", |cx| {
        view.read_with(cx, |view, _| view.actions_visible().is_some())
    });

    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Multiple occurrences found")
    );
    let rows = labels(cx, &view);
    assert!(
        rows.contains(&"Replace all 2 occurrences".to_owned()),
        "the count of the places is named: {rows:?}"
    );
    assert!(
        rows.contains(&"Replace this occurrence only".to_owned()),
        "the row of this place is named: {rows:?}"
    );
    assert_eq!(rows.len(), 2, "no other choice is offered: {rows:?}");

    let first = TWO_LINES
        .find("owner.getLastName()")
        .expect("the first place");
    let second = TWO_LINES[first + 1..]
        .find("owner.getLastName()")
        .expect("the second place")
        + first
        + 1;
    let length = "owner.getLastName()".len();
    press(cx, window, "down");
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_selected())
            .as_deref(),
        Some("Replace all 2 occurrences")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.action_highlights()),
        Some(vec![first..first + length, second..second + length]),
        "the row of every place marks both of them in the text"
    );
}

#[ui_kit::gpui::test]
fn a_selection_skips_the_list_of_expressions(cx: &mut TestAppContext) {
    let root = project("introduce-selection", &[("src/Main.txt", ONE_LINE)]);
    let open = root.join("src").join("Main.txt");
    let language = VariableLanguage::new();
    language.set_selections(nested_selections(ONE_LINE));
    language.set_actions(one_occurrence_actions(&open, ONE_LINE));
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    let (line, column) = expression_place(ONE_LINE);
    click_text(cx, window, line, column);
    press(cx, window, "ctrl-shift-right");
    draw(cx, window);

    press(cx, window, "ctrl-alt-v");
    wait_for(cx, "the field of the name", |cx| {
        draw(cx, window);
        shown(cx, window, "extract-name-popup")
    });

    assert_eq!(
        language.selections_asked(),
        0,
        "a selection is the expression itself"
    );
    assert!(
        !shown(cx, window, "actions-popup"),
        "neither the expressions nor the occurrences are listed"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_text())
            .as_deref(),
        Some("owner"),
        "the selection stays the expression the extract takes"
    );
}
