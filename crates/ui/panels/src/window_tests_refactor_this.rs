//! Refactor This at the caret, without a real window: the rows of the editor and of a
//! server, with the form that an action of the server needs before it runs.
//!
//! The language is a stand-in: it answers the actions, their form and the change the
//! form turns into, so the test checks what the window does with them.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    ActionApplyProvider, ActionApplyRequest, ActionChoice, ActionEntry, ActionField, ActionKind,
    ActionMember, ActionOutcome, ActionParameters, ActionParametersProvider, ActionParametersQuery,
    ActionProvider, ActionQuery, Answer, Confidence, LanguageFn, LanguageId, LanguageSupport,
    Provider, Queries, SourceEdit, SourceId, SyntaxProvider, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{bounds_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{edit_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-server");

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

/// The language of the test: every answer is set, no process runs.
pub(crate) struct RefactorLanguage {
    syntax: NoSyntax,
    actions: Mutex<Vec<ActionEntry>>,
    form: Mutex<Option<ActionParameters>>,
    change: Mutex<WorkspaceChange>,
    applied: Mutex<Vec<ActionApplyRequest>>,
}

impl RefactorLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            actions: Mutex::new(Vec::new()),
            form: Mutex::new(None),
            change: Mutex::new(WorkspaceChange::default()),
            applied: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
    }

    pub(crate) fn set_form(&self, form: ActionParameters) {
        *self.form.lock().unwrap() = Some(form);
    }

    fn set_change(&self, change: WorkspaceChange) {
        *self.change.lock().unwrap() = change;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for RefactorLanguage {
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

impl Provider<ActionParametersQuery, Option<ActionParameters>> for RefactorLanguage {
    fn start(
        &self,
        _: ActionParametersQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Option<ActionParameters>>,
    ) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(self.answer(form));
    }
}

impl Provider<ActionApplyRequest, ActionOutcome> for RefactorLanguage {
    fn start(
        &self,
        query: ActionApplyRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<ActionOutcome>,
    ) {
        self.applied.lock().unwrap().push(query);
        let change = self.change.lock().unwrap().clone();
        let _ = reply.send(self.answer(ActionOutcome {
            change,
            error: None,
        }));
    }
}

impl LanguageSupport for RefactorLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake")
    }

    fn matches(&self, path: &std::path::Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn action_parameters(&self) -> Option<&ActionParametersProvider> {
        Some(self)
    }

    fn action_apply(&self) -> Option<&ActionApplyProvider> {
        Some(self)
    }
}

pub(crate) fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    root
}

pub(crate) fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<RefactorLanguage>,
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

pub(crate) fn action(title: &str, code: &str, handle: Option<&str>) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some(code.to_owned()),
        preferred: false,
        edits: Vec::new(),
        renames: Vec::new(),
        handle: handle.map(str::to_owned),
        resolve: None,
        rename: None,
    }
}

/// Moves the selection of the open Refactor This list down until the row `label` is
/// selected. The rows of the server sit among the editor's own rows in their groups, so a
/// test names the row it picks instead of counting the Down presses.
pub(crate) fn select_refactoring(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    label: &str,
) {
    let view = editor_of(cx, window, path);
    for _ in 0..40 {
        if view
            .read_with(cx, |view, _| view.actions_selected())
            .as_deref()
            == Some(label)
        {
            return;
        }
        press(cx, window, "down");
    }
    panic!("the list has no row {label} to select");
}

/// The labels of the open Refactor This list of the file at `path`, from the top.
fn listed_labels(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Vec<String> {
    let view = editor_of(cx, window, path);
    view.read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open")
        .rows
        .into_iter()
        .map(|(_, label)| label)
        .collect()
}

#[ui_kit::gpui::test]
fn refactor_this_opens_the_form_of_a_server_action_and_applies_its_change(cx: &mut TestAppContext) {
    let root = project(
        "refactor-form",
        &[("src/Shape.txt", "shape\n"), ("src/Other.txt", "other\n")],
    );
    let shape = root.join("src").join("Shape.txt");
    let other = root.join("src").join("Other.txt");
    let made = root.join("src").join("ShapeView.txt");
    let language = RefactorLanguage::new();
    language.set_actions(vec![action(
        "Extract interface...",
        "refactor.extract.interface",
        Some("handle"),
    )]);
    language.set_form(ActionParameters {
        title: "Extract interface".to_owned(),
        members: vec![
            ActionMember {
                name: "area".to_owned(),
                detail: Some("double".to_owned()),
                checked: true,
                value: "area-handle".to_owned(),
            },
            ActionMember {
                name: "name".to_owned(),
                detail: Some("String".to_owned()),
                checked: true,
                value: "name-handle".to_owned(),
            },
        ],
        members_label: "Members".to_owned(),
        fields: vec![ActionField {
            key: "name".to_owned(),
            label: "Interface name".to_owned(),
            value: "ShapeView".to_owned(),
        }],
        choices: vec![ActionChoice {
            name: "probe".to_owned(),
            detail: Some("/src".to_owned()),
            value: "{\"uri\":\"probe\"}".to_owned(),
        }],
        choices_label: "Destination".to_owned(),
    });
    let mut change = WorkspaceChange::default();
    change.edits.push(SourceEdit {
        uri: uri_of(&shape),
        edits: vec![edit_in("shape\n", 5, 5, " implements ShapeView")],
    });
    change.edits.push(SourceEdit {
        uri: uri_of(&other),
        edits: vec![edit_in("other\n", 0, 5, "touched")],
    });
    change.edits.push(SourceEdit {
        uri: uri_of(&made),
        edits: vec![edit_in("", 0, 0, "interface ShapeView\n")],
    });
    language.set_change(change);

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &shape);
    let view = editor_of(cx, window, &shape);

    // The row of the server opens the form, which says what it needs.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Refactor This lists the refactorings"
    );
    // The row of the server is an extraction, after the editor's own extractions, so the
    // list does not select it at once.
    select_refactoring(cx, window, &shape, "Extract Interface...");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "refactor-form").size.height > px(0.),
        "the row opens the form of the action"
    );
    assert!(
        bounds_of(cx, window, "refactor-form-member-0").size.height > px(0.),
        "the form lists the members"
    );
    assert!(
        bounds_of(cx, window, "refactor-form-choice-0").size.height > px(0.),
        "the form lists the destination"
    );

    // Unticking the first member goes to the language as the value of the second.
    press(cx, window, "space");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    let applied = language.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].members, ["name-handle"]);
    assert_eq!(applied[0].fields[0].value, "ShapeView");
    assert_eq!(applied[0].choice.as_deref(), Some("{\"uri\":\"probe\"}"));

    // The change reaches the open file, the closed one and the new one.
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "shape implements ShapeView\n"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "touched\n");
    assert_eq!(fs::read_to_string(&made).unwrap(), "interface ShapeView\n");

    // The open file changes in one undo step, as a rename does.
    press(cx, window, "ctrl-z");
    assert_eq!(crate::window_tests::buffer_of(cx, &view), "shape\n");
}

#[ui_kit::gpui::test]
fn refactor_this_lists_the_refactorings_of_the_editor_with_their_keys(cx: &mut TestAppContext) {
    let root = project("refactor-own", &[("src/Own.txt", "alpha beta\n")]);
    let own = root.join("src").join("Own.txt");
    let language = RefactorLanguage::new();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);

    // Without an action of the server the first row is Rename, which changes the name
    // in the text itself.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "the editor lists its own refactorings"
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "rename-in-place-hint").size.height > px(0.),
        "Rename is the first row of the list"
    );
    press(cx, window, "escape");

    // The next row is Change Signature, which opens its form of the window.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "change-signature").size.height > px(0.),
        "Change Signature is a row of the list"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn refactor_this_has_its_heading_and_selects_the_first_row_of_the_names(cx: &mut TestAppContext) {
    let root = project("refactor-this-heading", &[("src/Own.txt", "alpha beta\n")]);
    let own = root.join("src").join("Own.txt");
    let window = open_window(cx, root.clone(), RefactorLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);
    let view = editor_of(cx, window, &own);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_title()),
        Some("Refactor This"),
        "the list is headed Refactor This"
    );
    let labels = listed_labels(cx, window, &own);
    let names: Vec<&str> = labels.iter().take(5).map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "Rename...",
            "Change Signature...",
            "Move...",
            "Copy...",
            "Safe Delete..."
        ],
        "the names of the element come first, in IDEA's order"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_selected()),
        Some("Rename...".to_owned()),
        "the first row of the first group is selected when the list opens"
    );
}

#[ui_kit::gpui::test]
fn refactor_this_puts_the_server_extract_interface_after_the_editor_extractions(
    cx: &mut TestAppContext,
) {
    let root = project(
        "refactor-this-extract-interface",
        &[("src/Own.txt", "alpha beta\n")],
    );
    let own = root.join("src").join("Own.txt");
    let language = RefactorLanguage::new();
    language.set_actions(vec![action(
        "Extract interface...",
        "refactor.extract.interface",
        Some("handle"),
    )]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);
    let view = editor_of(cx, window, &own);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    let labels = listed_labels(cx, window, &own);
    let place = |label: &str| {
        labels
            .iter()
            .position(|row| row == label)
            .unwrap_or_else(|| panic!("no row {label} in {labels:?}"))
    };
    assert!(
        labels.iter().all(|row| row != "Extract interface..."),
        "the label of the server takes the spelling of the menu: {labels:?}"
    );
    assert!(
        place("Extract Method...") < place("Extract Interface...")
            && place("Introduce Parameter...") < place("Extract Interface...")
            && place("Extract Interface...") < place("Pull Members Up..."),
        "the server's extraction comes after the editor's own extractions: {labels:?}"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_selected()),
        Some("Rename...".to_owned()),
        "the first row of the names is selected, not the row of the server"
    );
}

#[ui_kit::gpui::test]
fn refactor_this_separates_its_groups_with_a_line(cx: &mut TestAppContext) {
    let root = project("refactor-this-lines", &[("src/Own.txt", "alpha beta\n")]);
    let own = root.join("src").join("Own.txt");
    let window = open_window(cx, root.clone(), RefactorLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);
    let view = editor_of(cx, window, &own);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    // The names, the extractions, the hierarchy and Inline, each after a line.
    let labels = listed_labels(cx, window, &own);
    assert_eq!(labels[5], "Extract Method...", "{labels:?}");
    assert_eq!(labels[12], "Pull Members Up...", "{labels:?}");
    assert_eq!(labels[14], "Inline...", "{labels:?}");
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_separators()),
        Some(vec![5, 12, 14]),
        "a line goes above each group but the first: {labels:?}"
    );
    assert!(
        bounds_of(cx, window, "actions-separator").size.height > px(0.),
        "the line is drawn"
    );
}

#[ui_kit::gpui::test]
fn refactor_this_draws_no_line_for_a_group_the_server_left_empty(cx: &mut TestAppContext) {
    let root = project("refactor-this-empty", &[("src/Own.txt", "alpha beta\n")]);
    let own = root.join("src").join("Own.txt");
    let language = RefactorLanguage::new();
    // The server answers the hierarchy with no change: its rows are left out, and the
    // editor's own rows of the same kinds are left out with them.
    language.set_actions(vec![
        action("Pull Members Up", "refactor.pull.up", None),
        action("Push Members Down", "refactor.push.down", None),
    ]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &own);
    let view = editor_of(cx, window, &own);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    let labels = listed_labels(cx, window, &own);
    assert_eq!(
        labels.last().map(String::as_str),
        Some("Inline..."),
        "{labels:?}"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.actions_separators()),
        Some(vec![5, 12]),
        "one line above Inline, and none for the empty hierarchy: {labels:?}"
    );
}
