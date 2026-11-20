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
struct RenameLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    rename_queries: Mutex<Vec<RenameQuery>>,
    closed: Mutex<Option<PathBuf>>,
    refuse: Mutex<bool>,
}

impl RenameLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            rename_queries: Mutex::new(Vec::new()),
            closed: Mutex::new(None),
            refuse: Mutex::new(false),
        })
    }

    /// The language no longer prepares a name: the editor must not start the mode.
    fn refuse_rename(&self) {
        *self.refuse.lock().unwrap() = true;
    }

    /// A file the rename changes without it being open in a tab.
    fn set_closed(&self, path: PathBuf) {
        *self.closed.lock().unwrap() = Some(path);
    }

    fn queries(&self) -> Vec<RenameQuery> {
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

fn open_window(
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
