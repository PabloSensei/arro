//! Completion entries with the parts a server sends lazily: additional edits, commands
//! and the resolve step. Also the two keys of the list and hippie completion. The real
//! server check needs `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_complete -- --ignored --test-threads=1`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use futures_util::StreamExt;
use lang_api::{
    CommandQuery, CompletionCommand, CompletionEntry, CompletionList, CompletionProvider,
    CompletionResolveProvider, Confidence, DocumentEdit, DocumentSpot, ExecuteCommandProvider,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SignatureProvider, SignatureQuery,
    SignatureText, SourceId, SyntaxProvider, TextPoint, ask,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("complete-server");

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

fn edit(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: lang_api::TextSpan {
            start: point_in(text, start),
            end: point_in(text, end),
        },
        text: new_text.to_owned(),
    }
}

fn entry(label: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        insert_text: Some(label.to_owned()),
        ..CompletionEntry::default()
    }
}

/// The language of these tests: the entries, the resolve answer and the commands come
/// from the test, no process runs.
pub(crate) struct CompleteLanguage {
    syntax: NoSyntax,
    entries: Mutex<Vec<CompletionEntry>>,
    resolved: Mutex<Vec<DocumentEdit>>,
    resolve_delay: Mutex<Duration>,
    commands: Mutex<Vec<CommandQuery>>,
    completions: AtomicUsize,
    resolves: AtomicUsize,
    signatures: AtomicUsize,
}

impl CompleteLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            entries: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
            resolve_delay: Mutex::new(Duration::from_millis(0)),
            commands: Mutex::new(Vec::new()),
            completions: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
            signatures: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set_entries(&self, entries: Vec<CompletionEntry>) {
        *self.entries.lock().unwrap() = entries;
    }

    fn set_resolve(&self, edits: Vec<DocumentEdit>, delay: Duration) {
        *self.resolved.lock().unwrap() = edits;
        *self.resolve_delay.lock().unwrap() = delay;
    }

    fn commands(&self) -> Vec<CommandQuery> {
        self.commands.lock().unwrap().clone()
    }

    fn completions(&self) -> usize {
        self.completions.load(Ordering::Relaxed)
    }

    fn resolves(&self) -> usize {
        self.resolves.load(Ordering::Relaxed)
    }

    fn signatures(&self) -> usize {
        self.signatures.load(Ordering::Relaxed)
    }
}

impl Provider<DocumentSpot, CompletionList> for CompleteLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<CompletionList>) {
        self.completions.fetch_add(1, Ordering::Relaxed);
        let entries = self.entries.lock().unwrap().clone();
        let _ = reply.send(lang_api::Answer::new(
            CompletionList {
                entries,
                complete: true,
            },
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl Provider<CompletionEntry, CompletionEntry> for CompleteLanguage {
    fn start(
        &self,
        entry: CompletionEntry,
        _: base::Cancel,
        mut reply: lang_api::Reply<CompletionEntry>,
    ) {
        self.resolves.fetch_add(1, Ordering::Relaxed);
        let edits = self.resolved.lock().unwrap().clone();
        let delay = *self.resolve_delay.lock().unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let mut resolved = entry;
            resolved.additional_edits = edits;
            resolved.resolve = None;
            let _ = reply.send(lang_api::Answer::new(resolved, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<SignatureQuery, SignatureText> for CompleteLanguage {
    fn start(&self, _: SignatureQuery, _: base::Cancel, mut reply: lang_api::Reply<SignatureText>) {
        self.signatures.fetch_add(1, Ordering::Relaxed);
        let _ = reply.send(lang_api::Answer::new(
            SignatureText("greet(String)".to_owned()),
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl Provider<CommandQuery, ()> for CompleteLanguage {
    fn start(&self, query: CommandQuery, _: base::Cancel, mut reply: lang_api::Reply<()>) {
        self.commands.lock().unwrap().push(query);
        let _ = reply.send(lang_api::Answer::new((), Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for CompleteLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("complete")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        Some(self)
    }

    fn execute_command(&self) -> Option<&ExecuteCommandProvider> {
        Some(self)
    }

    fn signatures(&self) -> Option<&SignatureProvider> {
        Some(self)
    }
}

/// A language for the editor itself, so Complete Statement has its rules.
fn plain_language(path: &Path) -> Option<editor_view::LanguageConfig> {
    let is_text = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"));
    if !is_text {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Plain";
    config.statements = Some(editor_view::StatementRules {
        terminator: ";",
        block_words: &[],
    });
    Some(config)
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

pub(crate) fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<CompleteLanguage>,
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
            resolve: plain_language,
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
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

pub(crate) fn open_file(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    name: &str,
    text: &str,
    line: usize,
    column: usize,
) -> (PathBuf, gpui::Entity<EditorView>) {
    let path = root.join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, text).unwrap();
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    (path, view)
}

#[ui_kit::gpui::test]
fn additional_edits_apply_with_the_entry_in_one_step_of_undo(cx: &mut TestAppContext) {
    let root = project("complete-import", &[("src/A.txt", "Lis\n")]);
    let language = CompleteLanguage::new();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "Lis\n", 0, 3);
    language.set_entries({
        let mut import = entry("List");
        import.additional_edits = vec![edit("Lis\n", 0, 0, "import example.List;\n")];
        vec![import]
    });

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "import example.List;\nList\n");

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "Lis\n",
        "one undo takes the entry and its import back"
    );
}

#[ui_kit::gpui::test]
fn a_late_resolve_adds_the_import_after_the_main_edit(cx: &mut TestAppContext) {
    let root = project("complete-late", &[("src/A.txt", "Lis\n")]);
    let language = CompleteLanguage::new();
    language.set_resolve(
        vec![edit("Lis\n", 0, 0, "import example.List;\n")],
        Duration::from_millis(1500),
    );
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "Lis\n", 0, 3);
    let entry = CompletionEntry {
        resolve: Some("List".to_owned()),
        ..entry("List")
    };
    language.set_entries(vec![entry]);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    wait_for(cx, "the entry", Duration::from_secs(5), |cx| {
        buffer_of(cx, &view) == "List\n"
    });
    assert_eq!(
        buffer_of(cx, &view),
        "List\n",
        "the entry goes in before the server answers"
    );

    wait_for(cx, "the late import", Duration::from_secs(6), |cx| {
        buffer_of(cx, &view).starts_with("import example.List;\n")
    });
    assert_eq!(buffer_of(cx, &view), "import example.List;\nList\n");
    assert_eq!(language.resolves(), 1);
}

#[ui_kit::gpui::test]
fn the_commands_of_an_entry_run_on_the_server_or_here(cx: &mut TestAppContext) {
    let root = project(
        "complete-commands",
        &[
            ("src/Server.txt", "gr\n"),
            ("src/Suggest.txt", "gr\n"),
            ("src/Hints.txt", "gr\n"),
        ],
    );
    let language = CompleteLanguage::new();
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);

    let mut server = entry("greet");
    server.command = Some(CompletionCommand {
        title: "Selected".to_owned(),
        command: "service.completion.onDidSelect".to_owned(),
        arguments: "[1,2]".to_owned(),
    });
    language.set_entries(vec![server]);
    let (_, server_view) = open_file(cx, window, &root, "src/Server.txt", "gr\n", 0, 2);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &server_view), "greet\n");
    let commands = language.commands();
    assert_eq!(commands.len(), 1, "the server command is sent");
    assert_eq!(commands[0].command, "service.completion.onDidSelect");
    assert_eq!(commands[0].arguments, "[1,2]");

    let mut suggest = entry("greet");
    suggest.command = Some(CompletionCommand {
        title: "Suggest again".to_owned(),
        command: "editor.action.triggerSuggest".to_owned(),
        arguments: "[]".to_owned(),
    });
    language.set_entries(vec![suggest]);
    let (_, suggest_view) = open_file(cx, window, &root, "src/Suggest.txt", "gr\n", 0, 2);
    let before = language.completions();
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &suggest_view), "greet\n");
    assert!(
        language.completions() > before + 1,
        "triggerSuggest asks for the list again"
    );

    let mut hints = entry("greet");
    hints.command = Some(CompletionCommand {
        title: "Parameter info".to_owned(),
        command: "editor.action.triggerParameterHints".to_owned(),
        arguments: "[]".to_owned(),
    });
    language.set_entries(vec![hints]);
    let (_, hints_view) = open_file(cx, window, &root, "src/Hints.txt", "gr\n", 0, 2);
    let before = language.signatures();
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &hints_view), "greet\n");
    assert!(
        language.signatures() > before,
        "triggerParameterHints asks for the signature"
    );
}

#[ui_kit::gpui::test]
fn enter_keeps_the_rest_of_the_word_and_tab_replaces_it(cx: &mut TestAppContext) {
    let root = project(
        "complete-keys",
        &[("src/Enter.txt", "Stri\n"), ("src/Tab.txt", "Stri\n")],
    );
    let language = CompleteLanguage::new();
    language.set_entries(vec![entry("String")]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);

    let (_, enter_view) = open_file(cx, window, &root, "src/Enter.txt", "Stri\n", 0, 3);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &enter_view),
        "Stringi\n",
        "Enter inserts the entry before the rest of the word"
    );

    let (_, tab_view) = open_file(cx, window, &root, "src/Tab.txt", "Stri\n", 0, 3);
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "tab");
    assert_eq!(
        buffer_of(cx, &tab_view),
        "String\n",
        "Tab replaces the rest of the word"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_enter_inserts_the_entry_and_finishes_the_statement(cx: &mut TestAppContext) {
    let root = project("complete-statement", &[("src/A.txt", "gr")]);
    let language = CompleteLanguage::new();
    language.set_entries(vec![entry("greet")]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "gr", 0, 2);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "greet;\n",
        "the entry is inserted and the statement finished"
    );
}

#[ui_kit::gpui::test]
fn hippie_completion_walks_up_then_down_then_other_buffers(cx: &mut TestAppContext) {
    let root = project(
        "hippie-forward",
        &[
            ("src/A.txt", "alpha\nbeta\nal\nalmond\n"),
            ("src/B.txt", "algebra\n"),
        ],
    );
    let language = CompleteLanguage::new();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/A.txt",
        "alpha\nbeta\nal\nalmond\n",
        2,
        2,
    );
    let other = root.join("src/B.txt");
    open_path(cx, window, &other);
    let other_view = editor_of(cx, window, &other);
    wait_for(cx, "the second buffer", Duration::from_secs(5), |cx| {
        other_view.read_with(cx, |view, _| view.text().is_some())
    });
    open_path(cx, window, &root.join("src/A.txt"));
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 2, column: 2 }, cx)
    });
    cx.run_until_parked();

    press(cx, window, "alt-/");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\nalpha\nalmond\n",
        "the nearest word above the caret comes first"
    );
    press(cx, window, "alt-/");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\nalmond\nalmond\n",
        "then the words below the caret"
    );
    press(cx, window, "alt-/");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\nbeta\nalgebra\nalmond\n",
        "then the words of the other open buffer"
    );
}

#[ui_kit::gpui::test]
fn hippie_backward_completion_walks_the_same_words_the_other_way(cx: &mut TestAppContext) {
    let root = project(
        "hippie-backward",
        &[
            ("src/A.txt", "al\napple\nalpha\nalmond\n"),
            ("src/B.txt", "algebra\n"),
        ],
    );
    let language = CompleteLanguage::new();
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/A.txt",
        "al\napple\nalpha\nalmond\n",
        0,
        2,
    );
    let other = root.join("src/B.txt");
    open_path(cx, window, &other);
    let other_view = editor_of(cx, window, &other);
    wait_for(cx, "the second buffer", Duration::from_secs(5), |cx| {
        other_view.read_with(cx, |view, _| view.text().is_some())
    });
    open_path(cx, window, &root.join("src/A.txt"));
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 0, column: 2 }, cx)
    });
    cx.run_until_parked();

    press(cx, window, "alt-shift-/");
    assert_eq!(
        buffer_of(cx, &view),
        "algebra\napple\nalpha\nalmond\n",
        "the backward walk starts at the last candidate"
    );
    press(cx, window, "alt-shift-/");
    assert_eq!(buffer_of(cx, &view), "almond\napple\nalpha\nalmond\n");
    press(cx, window, "alt-/");
    assert_eq!(
        buffer_of(cx, &view),
        "algebra\napple\nalpha\nalmond\n",
        "a forward key goes on in the forward order"
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_real_server_resolves_the_import_of_list(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("completion-import");
    let path = window_language::main_class(&root, "probe", "CompletionImport");
    let source = r#"package probe;

public class CompletionImport {

    void show() {
        List
    }
}
"#;
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    language.open_document(&path).unwrap();

    let offset = source.find("List").unwrap() + "List".len();
    let point = point_in(source, offset);
    let uri = platform::CanonPath::new(&path).unwrap().to_uri();
    let support = language.support();
    let provider = support.completion().unwrap();
    let mut replies = ask(
        provider,
        DocumentSpot {
            uri,
            version: 1,
            point,
        },
    );
    let entries: Arc<Mutex<Option<Vec<CompletionEntry>>>> = Arc::new(Mutex::new(None));
    let slot = entries.clone();
    cx.spawn(async move |_| {
        if let Some(answer) = replies.next().await {
            *slot.lock().unwrap() = Some(answer.value.entries);
        }
    })
    .detach();
    wait_for(
        cx,
        "the completion of the types",
        Duration::from_secs(120),
        |_| entries.lock().unwrap().is_some(),
    );
    let entry = entries
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .iter()
        .find(|entry| entry.label.split(" - ").next() == Some("List"))
        .cloned()
        .expect("the server offers List");

    let resolved = if entry.additional_edits.is_empty() {
        let resolve = support
            .completion_resolve()
            .expect("the server resolves completion entries");
        let mut replies = ask(resolve, entry);
        let answer: Arc<Mutex<Option<CompletionEntry>>> = Arc::new(Mutex::new(None));
        let slot = answer.clone();
        cx.spawn(async move |_| {
            if let Some(answer) = replies.next().await {
                *slot.lock().unwrap() = Some(answer.value);
            }
        })
        .detach();
        wait_for(cx, "the resolved entry", Duration::from_secs(30), |_| {
            answer.lock().unwrap().is_some()
        });
        answer.lock().unwrap().clone().unwrap()
    } else {
        entry
    };
    assert!(
        resolved
            .additional_edits
            .iter()
            .any(|edit| edit.text.contains("import ") && edit.text.contains("List")),
        "the import of List comes with the entry: {:?}",
        resolved.additional_edits
    );

    crate::window_tests_real::close(cx, window, language, false);
}
