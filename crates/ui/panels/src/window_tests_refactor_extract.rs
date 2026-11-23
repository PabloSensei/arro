//! Extract Variable and Extract Method with the name typed at the caret, and a
//! refactoring of several files that one Ctrl+Z takes back as a whole.
//!
//! The language is a stand-in: it answers the actions and their edits from memory, so
//! the test checks what the window does with the answers.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::{Languages, uri_path};
use lang_api::{
    ActionApplyProvider, ActionApplyRequest, ActionChoice, ActionEntry, ActionField, ActionKind,
    ActionOutcome, ActionParameters, ActionParametersProvider, ActionParametersQuery,
    ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit, LanguageFn, LanguageId,
    LanguageSupport, Provider, Queries, RenamePosition, RenameProvider, RenameQuery, Reply,
    SemanticServer, SourceEdit, SourceId, SyntaxProvider, TextPoint, TextSpan, WorkspaceChange,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_refactor_this::select_refactoring;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-extract");

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

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

fn edit_in(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: span_in(text, start, end),
        text: new_text.to_owned(),
    }
}

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

/// The language of the test: every answer is set, no process runs.
struct ExtractLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    form: Mutex<Option<ActionParameters>>,
    change: Mutex<WorkspaceChange>,
    applied: Mutex<Vec<ActionApplyRequest>>,
}

impl ExtractLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            form: Mutex::new(None),
            change: Mutex::new(WorkspaceChange::default()),
            applied: Mutex::new(Vec::new()),
        })
    }

    fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
    }

    fn set_form(&self, form: ActionParameters) {
        *self.form.lock().unwrap() = Some(form);
    }

    fn set_change(&self, change: WorkspaceChange) {
        *self.change.lock().unwrap() = change;
    }

    fn text_of(&self, uri: &str) -> Option<String> {
        if let Some(text) = self.documents.lock().unwrap().get(uri) {
            return Some(text.to_string());
        }
        uri_path(uri).and_then(|path| fs::read_to_string(path).ok())
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for ExtractLanguage {
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

impl Provider<ActionQuery, Vec<ActionEntry>> for ExtractLanguage {
    fn start(&self, query: ActionQuery, _: base::Cancel, mut reply: Reply<Vec<ActionEntry>>) {
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

impl Provider<ActionParametersQuery, Option<ActionParameters>> for ExtractLanguage {
    fn start(
        &self,
        _: ActionParametersQuery,
        _: base::Cancel,
        mut reply: Reply<Option<ActionParameters>>,
    ) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(self.answer(form));
    }
}

impl Provider<ActionApplyRequest, ActionOutcome> for ExtractLanguage {
    fn start(&self, query: ActionApplyRequest, _: base::Cancel, mut reply: Reply<ActionOutcome>) {
        self.applied.lock().unwrap().push(query);
        let change = self.change.lock().unwrap().clone();
        let _ = reply.send(self.answer(ActionOutcome {
            change,
            error: None,
        }));
    }
}

impl Provider<RenameQuery, WorkspaceChange> for ExtractLanguage {
    fn start(&self, query: RenameQuery, _: base::Cancel, mut reply: Reply<WorkspaceChange>) {
        let Some(text) = self.text_of(&query.spot.uri) else {
            let _ = reply.send(self.answer(WorkspaceChange::default()));
            return;
        };
        let mut edits = Vec::new();
        let mut rest = 0;
        while let Some(at) =
            text[rest..].find(|character: char| character.is_alphanumeric() || character == '_')
        {
            let start = rest + at;
            let end = text[start..]
                .find(|character: char| !(character.is_alphanumeric() || character == '_'))
                .map_or(text.len(), |end| start + end);
            let word = &text[start..end];
            // The name the extract introduced is the one the field shows.
            if word.starts_with("extracted") || word.starts_with("value") {
                edits.push(edit_in(&text, start, end, &query.new_name));
            }
            rest = end;
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

impl LanguageSupport for ExtractLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake")
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

    fn action_parameters(&self) -> Option<&ActionParametersProvider> {
        Some(self)
    }

    fn action_apply(&self) -> Option<&ActionApplyProvider> {
        Some(self)
    }

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self)
    }
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
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

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<ExtractLanguage>,
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

const TWO_FORMS: &str = "int a = x * 2;\nint b = x * 2;\n";

/// The edits of an extract of `x * 2` under one or both occurrences, with the name the
/// server chose at the start of the new declaration.
fn variable_actions(path: &Path) -> Vec<ActionEntry> {
    let text = TWO_FORMS;
    let uri = uri_of(path);
    let declaration = "int value = x * 2;\n";
    let mut this_edits = vec![edit_in(text, 0, 0, declaration)];
    this_edits.push(edit_in(text, 8, 13, "value"));
    let mut every_edits = this_edits.clone();
    every_edits.push(edit_in(text, 23, 28, "value"));
    let name = declaration.find("value").unwrap();
    let rename = RenamePosition {
        uri: uri.clone(),
        offset: name as u32,
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

#[ui_kit::gpui::test]
fn extract_variable_this_occurrence_renames_only_one(cx: &mut TestAppContext) {
    let root = project("extract-one", &[("src/Form.txt", TWO_FORMS)]);
    let open = root.join("src").join("Form.txt");
    let language = ExtractLanguage::new();
    language.set_actions(variable_actions(&open));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "extract-name-popup").size.height > px(0.),
        "the name of the server is ready to be typed over"
    );
    type_text(cx, window, "renamed");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("int renamed = x * 2;"), "{text}");
    assert!(text.contains("int a = renamed;"), "{text}");
    assert!(text.contains("int b = x * 2;"), "{text}");
}

#[ui_kit::gpui::test]
fn extract_variable_all_occurrences_renames_every_place(cx: &mut TestAppContext) {
    let root = project("extract-all", &[("src/Form.txt", TWO_FORMS)]);
    let open = root.join("src").join("Form.txt");
    let language = ExtractLanguage::new();
    language.set_actions(variable_actions(&open));
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "ctrl-alt-v");
    draw(cx, window);
    press(cx, window, "down");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "the list says which places it replaces"
    );
    press(cx, window, "enter");
    draw(cx, window);
    type_text(cx, window, "renamed");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("int renamed = x * 2;"), "{text}");
    assert!(text.contains("int a = renamed;"), "{text}");
    assert!(text.contains("int b = renamed;"), "{text}");
}

const METHOD_BEFORE: &str = "class A {\n  void run() {\n    call();\n  }\n}\n";
const METHOD_AFTER: &str = "class A {\n  void run() {\n    extracted();\n  }\n\n  private void extracted() {\n    call();\n  }\n}\n";

fn method_action(path: &Path) -> ActionEntry {
    // The server marks the declaration of the new method, not the call it wrote.
    let name = METHOD_AFTER.rfind("extracted").unwrap();
    ActionEntry {
        title: "Extract to method".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.function".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri_of(path),
            edits: vec![edit_in(METHOD_BEFORE, 0, METHOD_BEFORE.len(), METHOD_AFTER)],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(RenamePosition {
            uri: uri_of(path),
            offset: name as u32,
            length: "extracted".len() as u32,
        }),
    }
}

#[ui_kit::gpui::test]
fn extract_method_takes_the_name_and_visibility_of_its_field(cx: &mut TestAppContext) {
    let root = project("extract-method", &[("src/Method.txt", METHOD_BEFORE)]);
    let open = root.join("src").join("Method.txt");
    let language = ExtractLanguage::new();
    language.set_actions(vec![method_action(&open)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "ctrl-alt-m");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "extract-name-popup").size.height > px(0.),
        "the form shows the name and the signature"
    );
    assert!(
        bounds_of(cx, window, "extract-name-visibility").size.height > px(0.),
        "Extract Method shows its visibility"
    );
    // Private, protected, package-private, public.
    press(cx, window, "down");
    press(cx, window, "down");
    press(cx, window, "down");
    draw(cx, window);
    type_text(cx, window, "twice");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("public void twice()"), "{text}");
    assert!(text.contains("twice();"), "{text}");
}

fn action(title: &str, code: &str, handle: Option<&str>) -> ActionEntry {
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

#[ui_kit::gpui::test]
fn one_undo_takes_back_a_refactoring_of_three_files_and_redo_brings_it_back(
    cx: &mut TestAppContext,
) {
    let root = project(
        "extract-undo",
        &[("src/Shape.txt", "shape\n"), ("src/Other.txt", "other\n")],
    );
    let shape = root.join("src").join("Shape.txt");
    let other = root.join("src").join("Other.txt");
    let made = root.join("src").join("ShapeView.txt");
    let language = ExtractLanguage::new();
    language.set_actions(vec![action(
        "Extract interface...",
        "refactor.extract.interface",
        Some("handle"),
    )]);
    language.set_form(ActionParameters {
        title: "Extract interface".to_owned(),
        members: Vec::new(),
        members_label: String::new(),
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
    let change = WorkspaceChange {
        edits: vec![
            SourceEdit {
                uri: uri_of(&shape),
                edits: vec![edit_in("shape\n", 5, 5, " implements ShapeView")],
            },
            SourceEdit {
                uri: uri_of(&other),
                edits: vec![edit_in("other\n", 0, 5, "touched")],
            },
            SourceEdit {
                uri: uri_of(&made),
                edits: vec![edit_in("", 0, 0, "interface ShapeView\n")],
            },
        ],
        renames: Vec::new(),
    };
    language.set_change(change);

    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &shape);
    let view = editor_of(cx, window, &shape);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    select_refactoring(cx, window, &shape, "Extract Interface...");
    press(cx, window, "enter");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "shape implements ShapeView\n",
        "the open file is edited"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "touched\n");
    assert_eq!(fs::read_to_string(&made).unwrap(), "interface ShapeView\n");

    // One Ctrl+Z in the file the refactoring started in takes every file back.
    press(cx, window, "ctrl-z");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "shape\n", "the open file is back");
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        "other\n",
        "the closed file is back"
    );
    assert!(!made.exists(), "the file the change created is gone");

    // Ctrl+Shift+Z applies the whole change again.
    press(cx, window, "ctrl-shift-z");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "shape implements ShapeView\n",
        "the open file is edited again"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "touched\n");
    assert_eq!(fs::read_to_string(&made).unwrap(), "interface ShapeView\n");
}
