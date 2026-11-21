//! Rename in place: Shift+F6 changes the name in every place of the open file as the
//! user types, and Enter asks the language for the rename that reaches the other files.
//! The stand-in language answers the preparation, the places and the rename itself.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentEdit, DocumentSpot, LanguageFn, LanguageId, LanguageSupport,
    PrepareRename, Provider, Queries, RenameQuery, SemanticServer, SourceEdit, SourceId,
    SyntaxProvider, TextPoint, WorkspaceChange, offset_in,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
    wait_for_signal,
};
use crate::window_tests_copy_class::shown;
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("rename-in-place");

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

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn word_at(text: &str, point: TextPoint) -> Option<(usize, usize)> {
    let offset = offset_in(text, point);
    let mut start = offset;
    while start > 0 {
        let character = text[..start].chars().next_back()?;
        if !is_word(character) {
            break;
        }
        start -= character.len_utf8();
    }
    let mut end = offset;
    while end < text.len() {
        let character = text[end..].chars().next()?;
        if !is_word(character) {
            break;
        }
        end += character.len_utf8();
    }
    (start < end).then_some((start, end))
}

/// Every whole-word `word` replaced by `new_word`, in the coordinates of the text before
/// the edits.
fn replace_word(text: &str, word: &str, new_word: &str) -> Vec<DocumentEdit> {
    let mut edits = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(word) {
        let start = from + found;
        let end = start + word.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        if !before.is_some_and(is_word) && !after.is_some_and(is_word) {
            edits.push(DocumentEdit {
                span: span_in(text, start, end),
                text: new_word.to_owned(),
            });
        }
        from = end;
    }
    edits
}

/// The language of these tests: preparation and rename over a `TextPoint`, with the
/// answers the window shows and the queries it sent kept for the assertions.
pub(crate) struct RenameLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    rename_queries: Mutex<Vec<RenameQuery>>,
    closed: Mutex<Option<PathBuf>>,
    refuse: Mutex<bool>,
    /// The preparation questions the language keeps open while it is silent.
    held: Mutex<Vec<lang_api::Reply<Option<PrepareRename>>>>,
    silent: Mutex<bool>,
}

impl RenameLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            rename_queries: Mutex::new(Vec::new()),
            closed: Mutex::new(None),
            refuse: Mutex::new(false),
            held: Mutex::new(Vec::new()),
            silent: Mutex::new(false),
        })
    }

    /// The language no longer prepares a name: the editor must not start the mode.
    fn refuse_rename(&self) {
        *self.refuse.lock().unwrap() = true;
    }

    /// The language does not answer the preparation until `answer_held` gives it an answer.
    pub(crate) fn be_silent(&self) {
        *self.silent.lock().unwrap() = true;
    }

    /// Answers the last preparation question the language kept open.
    pub(crate) fn answer_held(&self, answer: Option<PrepareRename>, confidence: Confidence) {
        if let Some(mut reply) = self.held.lock().unwrap().pop() {
            let _ = reply.send(Answer::new(answer, confidence, SOURCE));
        }
    }

    /// A file the rename changes without it being open in a tab.
    fn set_closed(&self, path: PathBuf) {
        *self.closed.lock().unwrap() = Some(path);
    }

    pub(crate) fn queries(&self) -> Vec<RenameQuery> {
        self.rename_queries.lock().unwrap().clone()
    }

    fn text_of(&self, uri: &str) -> Option<String> {
        if let Some(text) = self.documents.lock().unwrap().get(uri) {
            return Some(text.to_string());
        }
        uri_of_path(uri).and_then(|path| fs::read_to_string(path).ok())
    }
}

fn uri_of_path(uri: &str) -> Option<PathBuf> {
    editor_view::uri_path(uri)
}

impl SemanticServer for RenameLanguage {
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
            .then_some(0)
    }
}

impl Provider<DocumentSpot, Option<PrepareRename>> for RenameLanguage {
    fn start(
        &self,
        spot: DocumentSpot,
        _: base::Cancel,
        mut reply: lang_api::Reply<Option<PrepareRename>>,
    ) {
        if *self.silent.lock().unwrap() {
            self.held.lock().unwrap().push(reply);
            return;
        }
        if *self.refuse.lock().unwrap() {
            let _ = reply.send(Answer::new(None, Confidence::Certain, SOURCE));
            return;
        }
        let Some(text) = self.text_of(&spot.uri) else {
            let _ = reply.send(Answer::new(None, Confidence::Certain, SOURCE));
            return;
        };
        let answer = word_at(&text, spot.point).map(|(start, end)| PrepareRename {
            span: Some(span_in(&text, start, end)),
        });
        let _ = reply.send(Answer::new(answer, Confidence::Certain, SOURCE));
    }
}

impl Provider<RenameQuery, WorkspaceChange> for RenameLanguage {
    fn start(
        &self,
        query: RenameQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<WorkspaceChange>,
    ) {
        self.rename_queries.lock().unwrap().push(query.clone());
        let Some(text) = self.text_of(&query.spot.uri) else {
            let _ = reply.send(Answer::new(
                WorkspaceChange::default(),
                Confidence::Unknown,
                SOURCE,
            ));
            return;
        };
        let Some((start, end)) = word_at(&text, query.spot.point) else {
            let _ = reply.send(Answer::new(
                WorkspaceChange::default(),
                Confidence::Unknown,
                SOURCE,
            ));
            return;
        };
        let word = &text[start..end];
        let mut change = WorkspaceChange::default();
        change.edits.push(SourceEdit {
            uri: query.spot.uri.clone(),
            edits: replace_word(&text, word, &query.new_name),
        });
        if let Some(path) = self.closed.lock().unwrap().clone()
            && let Ok(closed) = fs::read_to_string(&path)
        {
            change.edits.push(SourceEdit {
                uri: uri_of(&path),
                edits: replace_word(&closed, word, &query.new_name),
            });
        }
        let _ = reply.send(Answer::new(change, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for RenameLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("rename-in-place-test")
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

    fn prepare_rename(&self) -> Option<&lang_api::PrepareRenameProvider> {
        Some(self)
    }

    fn rename(&self) -> Option<&lang_api::RenameProvider> {
        Some(self)
    }
}

pub(crate) fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<RenameLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("rename-in-place-state");
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
    let setup = project_setup(scratch("rename-in-place-cache"));
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

#[ui_kit::gpui::test]
fn typing_changes_every_place_and_enter_renames_the_other_file(cx: &mut TestAppContext) {
    let root = project(
        "rename-in-place",
        &[
            ("src/Open.txt", "alpha beta alpha\n"),
            ("src/Closed.txt", "alpha gamma\n"),
        ],
    );
    let open = root.join("src").join("Open.txt");
    let closed = root.join("src").join("Closed.txt");
    let language = RenameLanguage::new();
    language.set_closed(closed.clone());
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "shift-f6");
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "Shift+F6 starts changing the name in the text"
    );

    // The name starts selected, so the first character replaces it in every place.
    type_text(cx, window, "delta");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "delta beta delta\n",
        "every occurrence follows the typing"
    );

    press(cx, window, "enter");
    wait_for_signal(cx, "the rename of the closed file", |_| {
        fs::read_to_string(&closed).is_ok_and(|text| text == "delta gamma\n")
    });
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "delta beta delta\n");
    let queries = language.queries();
    assert_eq!(queries.len(), 1, "the language was asked once");
    assert_eq!(queries[0].new_name, "delta");
    assert_eq!(queries[0].spot.uri, uri_of(&open));
}

#[ui_kit::gpui::test]
fn the_second_shift_f6_opens_the_rename_field_and_puts_the_original_back(cx: &mut TestAppContext) {
    let root = project(
        "rename-in-place-options",
        &[("src/Open.txt", "alpha beta\n")],
    );
    let open = root.join("src").join("Open.txt");
    let language = RenameLanguage::new();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "shift-f6");
    cx.run_until_parked();
    draw(cx, window);
    type_text(cx, window, "de");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "de beta\n");

    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-popup"),
        "the second Shift+F6 shows the window with the options"
    );
    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "the linked editing is over"
    );
    assert_eq!(
        buffer_of(cx, &view),
        "alpha beta\n",
        "the text waits for the window to apply its name"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "alpha beta\n");
}

#[ui_kit::gpui::test]
fn a_name_the_language_refuses_leaves_the_text_alone_and_says_so_in_the_status_bar(
    cx: &mut TestAppContext,
) {
    let root = project(
        "rename-in-place-refused",
        &[("src/Open.txt", "alpha beta\n")],
    );
    let open = root.join("src").join("Open.txt");
    let language = RenameLanguage::new();
    language.refuse_rename();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "shift-f6");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "the mode does not start"
    );
    assert_eq!(buffer_of(cx, &view), "alpha beta\n");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| {
                workspace.language_status().map(ToString::to_string)
            })
            .unwrap()
            .as_deref(),
        Some("This name cannot be renamed"),
        "the status bar says why nothing happened"
    );
}

/// The method the silent tests change: the loop variable and the argument are `pet`.
pub(crate) const METHOD: &str =
    "void visit(List<Pet> pets) {\n    for (Pet pet : pets) {\n        feed(pet);\n    }\n}\n";
/// `METHOD` once the name `pet` became `Zz` in every place.
pub(crate) const RENAMED: &str =
    "void visit(List<Pet> pets) {\n    for (Pet Zz : pets) {\n        feed(Zz);\n    }\n}\n";

#[ui_kit::gpui::test]
fn typing_changes_every_place_of_the_name_while_the_language_is_silent(cx: &mut TestAppContext) {
    let root = project("rename-in-place-silent", &[("src/Owner.txt", METHOD)]);
    let owner = root.join("src").join("Owner.txt");
    let language = RenameLanguage::new();
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "Shift+F6 starts at once, without an answer of the language"
    );

    type_text(cx, window, "Zz");
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        RENAMED,
        "every place of the name follows the typing"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), METHOD, "Escape puts the text back");
    assert!(
        !view.read_with(&*cx, |view, _| view.is_dirty()),
        "the file is not changed"
    );
    assert_eq!(fs::read_to_string(&owner).unwrap(), METHOD);
    assert!(language.queries().is_empty(), "nothing was asked to rename");
}

#[ui_kit::gpui::test]
fn a_late_refusal_puts_the_text_back_and_says_so_in_the_status_bar(cx: &mut TestAppContext) {
    let root = project("rename-in-place-late-refusal", &[("src/Owner.txt", METHOD)]);
    let owner = root.join("src").join("Owner.txt");
    let language = RenameLanguage::new();
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    type_text(cx, window, "Zz");
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), RENAMED);

    language.answer_held(None, Confidence::Certain);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "the refusal ends the mode"
    );
    assert_eq!(buffer_of(cx, &view), METHOD, "the text goes back");
    assert!(
        !view.read_with(&*cx, |view, _| view.is_dirty()),
        "the text is the saved one again"
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| {
                workspace.language_status().map(ToString::to_string)
            })
            .unwrap()
            .as_deref(),
        Some("This name cannot be renamed"),
        "the status bar says why"
    );
}

#[ui_kit::gpui::test]
fn enter_waits_for_the_language_and_renames_across_the_project_once_it_answers(
    cx: &mut TestAppContext,
) {
    let root = project(
        "rename-in-place-waits",
        &[("src/Owner.txt", METHOD), ("src/Other.txt", "feed pet\n")],
    );
    let owner = root.join("src").join("Owner.txt");
    let other = root.join("src").join("Other.txt");
    let language = RenameLanguage::new();
    language.set_closed(other.clone());
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    type_text(cx, window, "Zz");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        language.queries().is_empty(),
        "Enter does not ask the language before it answers"
    );
    assert_eq!(
        buffer_of(cx, &view),
        RENAMED,
        "the name stays as it is typed while Enter waits"
    );
    assert!(
        shown(cx, window, "rename-in-place-hint"),
        "the mode waits for the answer"
    );

    language.answer_held(Some(PrepareRename { span: None }), Confidence::Certain);
    wait_for_signal(cx, "the rename of the other file", |_| {
        fs::read_to_string(&other).is_ok_and(|text| text == "feed Zz\n")
    });
    cx.run_until_parked();
    draw(cx, window);

    let queries = language.queries();
    assert_eq!(queries.len(), 1, "the language was asked once");
    assert_eq!(queries[0].new_name, "Zz");
    assert_eq!(buffer_of(cx, &view), RENAMED);
    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "the rename is done"
    );
}

#[ui_kit::gpui::test]
fn enter_while_the_language_cannot_answer_keeps_the_name_and_says_so(cx: &mut TestAppContext) {
    let root = project("rename-in-place-not-ready", &[("src/Owner.txt", METHOD)]);
    let owner = root.join("src").join("Owner.txt");
    let language = RenameLanguage::new();
    language.be_silent();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    view.update(cx, |view, cx| view.restore_caret(1, 13, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    type_text(cx, window, "Zz");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(language.queries().is_empty());

    language.answer_held(None, Confidence::Unknown);
    cx.run_until_parked();
    draw(cx, window);

    assert!(language.queries().is_empty(), "no rename was asked");
    assert_eq!(buffer_of(cx, &view), RENAMED, "the name stays in this file");
    assert!(!shown(cx, window, "rename-in-place-hint"), "the mode ends");
    assert!(
        shown(cx, window, "lsp-note"),
        "a note says the language is not ready"
    );
}

#[ui_kit::gpui::test]
fn shift_f6_between_two_spaces_says_so_in_the_status_bar(cx: &mut TestAppContext) {
    let root = project(
        "rename-in-place-no-name",
        &[("src/Open.txt", "alpha  beta\n")],
    );
    let open = root.join("src").join("Open.txt");
    let language = RenameLanguage::new();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);
    view.update(cx, |view, cx| view.restore_caret(0, 6, cx));
    draw(cx, window);

    press(cx, window, "shift-f6");
    draw(cx, window);

    assert!(
        !shown(cx, window, "rename-in-place-hint"),
        "no mode starts without a name"
    );
    assert_eq!(buffer_of(cx, &view), "alpha  beta\n");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace
                .status_message()
                .map(ToString::to_string))
            .unwrap()
            .as_deref(),
        Some("Put the caret on a name to rename"),
        "the status bar says why nothing happened"
    );
}
