//! The name an extraction and an Introduce Parameter take from the language: the name field
//! shows the name the language suggests, and Enter writes it into the code through the rename.
//!
//! The language is a stand-in: it answers the actions, the names and the rename from memory,
//! so the test checks what the window does with the answers.

use std::collections::HashMap;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::{Languages, uri_path};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit,
    LanguageFn, LanguageId, LanguageSupport, NameRole, Provider, Queries, RenamePosition,
    RenameProvider, RenameQuery, Reply, SemanticServer, SourceEdit, SourceId, SyntaxProvider,
    TextPoint, TextSpan, WorkspaceChange,
};
use platform::CanonPath;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-naming");

/// The one line of the tests: its value is the expression an extraction takes.
pub(crate) const LINE: &str = "int pageSize = 5;\n";

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

pub(crate) fn edit_in(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: span_in(text, start, end),
        text: new_text.to_owned(),
    }
}

pub(crate) fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

/// The edits that replace every whole word `word` of `text` with `new_word`.
fn replace_words(text: &str, word: &str, new_word: &str) -> Vec<DocumentEdit> {
    let is_word = |character: char| character.is_alphanumeric() || character == '_';
    let mut edits = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let start = from + found;
        let end = start + word.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            edits.push(edit_in(text, start, end, new_word));
        }
        from = end;
    }
    edits
}

/// The language of the test: the actions are set and no process runs. The name it suggests
/// is the one of its role, and it suggests none when `suggests` is off.
pub(crate) struct NamingLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    actions: Mutex<Vec<ActionEntry>>,
    /// The name the server gave the new declaration, which the rename changes.
    introduced: &'static str,
    suggests: bool,
}

impl NamingLanguage {
    pub(crate) fn new(introduced: &'static str, suggests: bool) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            introduced,
            suggests,
        })
    }

    pub(crate) fn set_actions(&self, actions: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = actions;
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

impl SemanticServer for NamingLanguage {
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

impl Provider<ActionQuery, Vec<ActionEntry>> for NamingLanguage {
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

impl Provider<RenameQuery, WorkspaceChange> for NamingLanguage {
    fn start(&self, query: RenameQuery, _: base::Cancel, mut reply: Reply<WorkspaceChange>) {
        let edits = self
            .text_of(&query.spot.uri)
            .map(|text| replace_words(&text, self.introduced, &query.new_name))
            .unwrap_or_default();
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

impl LanguageSupport for NamingLanguage {
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

    fn rename(&self) -> Option<&RenameProvider> {
        Some(self)
    }

    fn suggest_name(
        &self,
        text: &str,
        expression: Range<usize>,
        role: NameRole,
        _: Option<&str>,
    ) -> Option<String> {
        // The stand-in names the value of its line only, so a wrong place finds no name.
        if !self.suggests || text.get(expression) != Some("5") {
            return None;
        }
        let name = match role {
            NameRole::Constant => "PAGE_SIZE",
            NameRole::Parameter => "limit",
            NameRole::Variable | NameRole::Field => "pageSize",
        };
        Some(name.to_owned())
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
    language: Arc<NamingLanguage>,
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

/// Extract Constant of the value: the server declares the constant above the line under the
/// name `_5`, and puts the name in place of the value.
fn constant_action(path: &Path) -> ActionEntry {
    let uri = uri_of(path);
    let declaration = "int _5 = 5;\n";
    let value = LINE.find('5').expect("the value is in the line");
    ActionEntry {
        title: "Extract constant".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.extract.constant".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri: uri.clone(),
            edits: vec![
                edit_in(LINE, 0, 0, declaration),
                edit_in(LINE, value, value + 1, "_5"),
            ],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: Some(RenamePosition {
            uri,
            offset: declaration
                .find("_5")
                .expect("the name is in the declaration") as u32,
            length: 2,
        }),
    }
}

/// Introduce Parameter of the value: the server adds the parameter `i` to a method above the
/// line and puts the parameter in place of the value. The server marks no place for the name.
fn parameter_action(path: &Path) -> ActionEntry {
    let uri = uri_of(path);
    let method = "void run(int i) {}\n";
    let value = LINE.find('5').expect("the value is in the line");
    ActionEntry {
        title: "Introduce parameter".to_owned(),
        kind: Some(ActionKind::Refactor),
        code: Some("refactor.introduce.parameter".to_owned()),
        preferred: false,
        edits: vec![SourceEdit {
            uri,
            edits: vec![
                edit_in(LINE, 0, 0, method),
                edit_in(LINE, value, value + 1, "i"),
            ],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
        rename: None,
    }
}

/// Selects the value of the line: the caret goes to the end of the line, one step back, and
/// the selection takes the character before it.
pub(crate) fn select_value(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "end");
    press(cx, window, "left");
    press(cx, window, "shift-left");
}

#[ui_kit::gpui::test]
fn extract_constant_shows_the_name_of_the_language_and_enter_writes_it(cx: &mut TestAppContext) {
    let root = project("name-constant", &[("src/Form.txt", LINE)]);
    let open = root.join("src").join("Form.txt");
    let language = NamingLanguage::new("_5", true);
    language.set_actions(vec![constant_action(&open)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    select_value(cx, window);
    press(cx, window, "ctrl-alt-c");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.name_field_text())
            .as_deref(),
        Some("PAGE_SIZE"),
        "the name field shows the name the language gives"
    );
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("int PAGE_SIZE = 5;"), "{text}");
    assert!(text.contains("int pageSize = PAGE_SIZE;"), "{text}");
}

#[ui_kit::gpui::test]
fn introduce_parameter_takes_the_name_of_the_language_and_enter_writes_it(cx: &mut TestAppContext) {
    let root = project("name-parameter", &[("src/Form.txt", LINE)]);
    let open = root.join("src").join("Form.txt");
    let language = NamingLanguage::new("i", true);
    language.set_actions(vec![parameter_action(&open)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    select_value(cx, window);
    press(cx, window, "ctrl-alt-p");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.name_field_text())
            .as_deref(),
        Some("limit"),
        "the name field shows the name the language gives"
    );
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(text.contains("void run(int limit) {}"), "{text}");
    assert!(text.contains("int pageSize = limit;"), "{text}");
}

#[ui_kit::gpui::test]
fn introduce_parameter_without_a_suggestion_applies_the_name_of_the_server(
    cx: &mut TestAppContext,
) {
    let root = project("name-parameter-plain", &[("src/Form.txt", LINE)]);
    let open = root.join("src").join("Form.txt");
    let language = NamingLanguage::new("i", false);
    language.set_actions(vec![parameter_action(&open)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    select_value(cx, window);
    press(cx, window, "ctrl-alt-p");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.name_field_text()),
        None,
        "no name field opens when the language suggests no name"
    );

    let text = buffer_of(cx, &view);
    assert!(text.contains("void run(int i) {}"), "{text}");
    assert!(text.contains("int pageSize = i;"), "{text}");
}
