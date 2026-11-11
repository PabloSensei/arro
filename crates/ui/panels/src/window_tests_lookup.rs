//! Choosing an entry of the completion list, the lists of one expected type and of
//! types only, the Parameter Info overloads, the arguments of an inserted call, the
//! templates around a selection, the documentation page and a late resolve.
//!
//! The real server checks at the end need `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_lookup -- --ignored --test-threads=1`

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use editor_view::{EditorView, ExternalLinks, LanguageConfig, Languages, Spot, TemplateLanguage};
use lang_api::{
    CallParameter, CallSignature, CompletionEntry, CompletionKind, CompletionList,
    CompletionProvider, CompletionResolveProvider, Confidence, DocumentEdit, DocumentSpot,
    ExpectedType, ExpectedTypeProvider, ExternalDoc, ExternalDocProvider, LanguageFn, LanguageId,
    LanguageSupport, Provider, Queries, SemanticServer, SignatureHelp, SignatureInfoProvider,
    SignatureQuery, SourceId, SyntaxProvider, offset_in,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch,
};
use crate::window_tests_lang::edit_in;
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("lookup-server");

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

/// The language of these tests: every answer is set by the test, no process runs.
struct LookupLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    entries: Mutex<Vec<CompletionEntry>>,
    resolved: Mutex<Vec<DocumentEdit>>,
    resolve_delay: Mutex<Duration>,
    expected: Mutex<Option<String>>,
    signatures: Mutex<SignatureHelp>,
    documentation: Mutex<Option<String>>,
    completions: AtomicUsize,
    resolves: AtomicUsize,
}

impl LookupLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            entries: Mutex::new(Vec::new()),
            resolved: Mutex::new(Vec::new()),
            resolve_delay: Mutex::new(Duration::from_millis(0)),
            expected: Mutex::new(None),
            signatures: Mutex::new(SignatureHelp::default()),
            documentation: Mutex::new(None),
            completions: AtomicUsize::new(0),
            resolves: AtomicUsize::new(0),
        })
    }

    fn set_entries(&self, entries: Vec<CompletionEntry>) {
        *self.entries.lock().unwrap() = entries;
    }

    fn set_resolve(&self, edits: Vec<DocumentEdit>, delay: Duration) {
        *self.resolved.lock().unwrap() = edits;
        *self.resolve_delay.lock().unwrap() = delay;
    }

    fn set_expected(&self, type_name: Option<&str>) {
        *self.expected.lock().unwrap() = type_name.map(str::to_owned);
    }

    fn set_signatures(&self, signatures: SignatureHelp) {
        *self.signatures.lock().unwrap() = signatures;
    }

    fn set_documentation(&self, url: Option<&str>) {
        *self.documentation.lock().unwrap() = url.map(str::to_owned);
    }

    fn completions(&self) -> usize {
        self.completions.load(Ordering::Relaxed)
    }

    fn resolves(&self) -> usize {
        self.resolves.load(Ordering::Relaxed)
    }

    /// The parameter the caret is in, from the commas between the opening bracket and
    /// the caret.
    fn active_parameter(&self, spot: &DocumentSpot) -> usize {
        let documents = self.documents.lock().unwrap();
        let Some(text) = documents.get(&spot.uri) else {
            return 0;
        };
        let offset = offset_in(text, spot.point);
        let before = &text[..offset];
        let Some(open) = before.rfind('(') else {
            return 0;
        };
        let mut depth = 0i32;
        let mut count = 0;
        for character in before[open + 1..].chars() {
            match character {
                '(' | '<' | '[' => depth += 1,
                ')' | '>' | ']' => depth -= 1,
                ',' if depth == 0 => count += 1,
                _ => {}
            }
        }
        count
    }

    fn answer<T>(&self, value: T) -> lang_api::Answer<T> {
        lang_api::Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for LookupLanguage {
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

impl Provider<DocumentSpot, CompletionList> for LookupLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<CompletionList>) {
        self.completions.fetch_add(1, Ordering::Relaxed);
        let entries = self.entries.lock().unwrap().clone();
        let _ = reply.send(self.answer(CompletionList {
            entries,
            complete: true,
        }));
    }
}

impl Provider<CompletionEntry, CompletionEntry> for LookupLanguage {
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

impl Provider<SignatureQuery, SignatureHelp> for LookupLanguage {
    fn start(
        &self,
        query: SignatureQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<SignatureHelp>,
    ) {
        let mut help = self.signatures.lock().unwrap().clone();
        help.active_parameter = self.active_parameter(&query.0);
        let _ = reply.send(self.answer(help));
    }
}

impl Provider<DocumentSpot, ExpectedType> for LookupLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<ExpectedType>) {
        let type_name = self.expected.lock().unwrap().clone();
        let _ = reply.send(self.answer(ExpectedType { type_name }));
    }
}

impl Provider<DocumentSpot, ExternalDoc> for LookupLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<ExternalDoc>) {
        let url = self.documentation.lock().unwrap().clone();
        let _ = reply.send(self.answer(ExternalDoc { url }));
    }
}

impl LanguageSupport for LookupLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("lookup")
    }

    fn matches(&self, _: &Path) -> bool {
        true
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        Some(self)
    }

    fn signature_info(&self) -> Option<&SignatureInfoProvider> {
        Some(self)
    }

    fn expected_type(&self) -> Option<&ExpectedTypeProvider> {
        Some(self)
    }

    fn external_doc(&self) -> Option<&ExternalDocProvider> {
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

/// Opens the window with the stand-in language and the editor syntax the caller gives.
fn open_window_with_syntax(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<LookupLanguage>,
    resolve: fn(&Path) -> Option<LanguageConfig>,
    templates: fn(&Path) -> Option<&'static TemplateLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages { resolve, templates });
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

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<LookupLanguage>,
) -> WindowHandle<Workspace> {
    open_window_with_syntax(cx, root, language, |_| None, |_| None)
}

fn open_file(
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

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn entry(label: &str, kind: CompletionKind, insert: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        insert_text: Some(insert.to_owned()),
        kind,
        ..CompletionEntry::default()
    }
}

fn call(label: &str, parameters: &[&str]) -> CallSignature {
    CallSignature {
        label: label.to_owned(),
        parameters: parameters
            .iter()
            .map(|parameter| CallParameter {
                label: (*parameter).to_owned(),
            })
            .collect(),
    }
}

fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let start = before.rfind('\n').map_or(0, |at| at + 1);
    (line, text[start..offset].chars().count())
}

#[ui_kit::gpui::test]
fn ctrl_dot_inserts_the_entry_a_dot_and_opens_the_list_again(cx: &mut TestAppContext) {
    let root = project("lookup-dot", &[("src/A.txt", "foBar\n")]);
    let language = LookupLanguage::new();
    language.set_entries(vec![entry("foo", CompletionKind::Method, "foo")]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "foBar\n", 0, 2);
    let before = language.completions();

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "ctrl-.");
    assert_eq!(
        buffer_of(cx, &view),
        "foo.\n",
        "Ctrl+. replaces the rest of the word and leaves a dot"
    );
    assert!(
        language.completions() > before,
        "the list is asked for again after the dot"
    );
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "the new list is open"
    );
}

#[ui_kit::gpui::test]
fn smart_type_shows_only_the_entries_of_the_expected_type(cx: &mut TestAppContext) {
    let root = project("lookup-smart", &[("src/A.txt", "\n")]);
    let language = LookupLanguage::new();
    language.set_expected(Some("String"));
    language.set_entries(vec![
        entry("count : int", CompletionKind::Variable, "count"),
        entry("name : String", CompletionKind::Variable, "name"),
        entry("title : String", CompletionKind::Field, "title"),
    ]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    press(cx, window, "ctrl-shift-space");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "Ctrl+Shift+Space shows the list"
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "name\n",
        "the entry of the other type is not in the list"
    );
}

#[ui_kit::gpui::test]
fn smart_type_without_a_known_type_keeps_the_usual_list(cx: &mut TestAppContext) {
    let root = project("lookup-smart-none", &[("src/A.txt", "\n")]);
    let language = LookupLanguage::new();
    language.set_expected(None);
    language.set_entries(vec![
        entry("count : int", CompletionKind::Variable, "count"),
        entry("name : String", CompletionKind::Variable, "name"),
    ]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    press(cx, window, "ctrl-shift-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "count\n");
}

#[ui_kit::gpui::test]
fn class_name_shows_only_types_and_adds_the_import(cx: &mut TestAppContext) {
    let root = project("lookup-class", &[("src/A.txt", "\n")]);
    let language = LookupLanguage::new();
    let mut widget = entry("Widget", CompletionKind::Class, "Widget");
    widget.additional_edits = vec![edit_in("\n", 0, 0, "import demo.Widget;\n")];
    language.set_entries(vec![
        entry("greet", CompletionKind::Method, "greet"),
        entry("value : int", CompletionKind::Variable, "value"),
        widget,
    ]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    press(cx, window, "ctrl-alt-space");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > px(0.),
        "Ctrl+Alt+Space shows the list"
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "import demo.Widget;\nWidget\n",
        "the type goes in with its import, the method and the field do not"
    );
}

#[ui_kit::gpui::test]
fn parameter_info_follows_the_caret_and_switches_the_overload(cx: &mut TestAppContext) {
    let root = project("lookup-parameter", &[("src/A.txt", "call(one, two)\n")]);
    let language = LookupLanguage::new();
    language.set_signatures(SignatureHelp {
        signatures: vec![
            call(
                "call(String one, int two) : void",
                &["String one", "int two"],
            ),
            call("call(String one) : void", &["String one"]),
        ],
        active_signature: 0,
        active_parameter: 0,
    });
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "call(one, two)\n", 0, 5);

    press(cx, window, "ctrl-p");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "signature-popup").size.height > px(0.),
        "Ctrl+P shows the parameters"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.signature_position()),
        Some((0, 0))
    );
    assert!(
        view.read_with(cx, |view, _| view.signature_count()) == Some(2),
        "both overloads arrive"
    );

    press(cx, window, "end");
    draw(cx, window);
    wait_for(cx, "the second parameter", Duration::from_secs(5), |cx| {
        view.read_with(cx, |view, _| view.signature_position()) == Some((0, 1))
    });
    assert!(
        bounds_of(cx, window, "signature-active-parameter")
            .size
            .width
            > px(0.),
        "the active parameter is marked"
    );

    press(cx, window, "ctrl-down");
    assert_eq!(
        view.read_with(cx, |view, _| view.signature_position()),
        Some((1, 0)),
        "Ctrl+Down shows the next overload"
    );
    press(cx, window, "ctrl-up");
    assert_eq!(
        view.read_with(cx, |view, _| view.signature_position()),
        Some((0, 0)),
        "Ctrl+Up goes back"
    );
}

#[ui_kit::gpui::test]
fn tab_walks_the_arguments_of_an_inserted_call(cx: &mut TestAppContext) {
    let root = project("lookup-arguments", &[("src/A.txt", "gr\n")]);
    let language = LookupLanguage::new();
    language.set_entries(vec![entry(
        "greet(String one, String two) : void",
        CompletionKind::Method,
        "greet(one, two)",
    )]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "gr\n", 0, 2);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "greet(one, two)\n");

    press(cx, window, "tab");
    let first = view.read_with(cx, |view, _| view.selection().map(|(range, _)| range));
    assert_eq!(
        first.map(|range| buffer_of(cx, &view)[range].to_owned()),
        Some("one".to_owned())
    );
    press(cx, window, "tab");
    let second = view.read_with(cx, |view, _| view.selection().map(|(range, _)| range));
    assert_eq!(
        second.map(|range| buffer_of(cx, &view)[range].to_owned()),
        Some("two".to_owned())
    );
    press(cx, window, "shift-tab");
    let back = view.read_with(cx, |view, _| view.selection().map(|(range, _)| range));
    assert_eq!(
        back.map(|range| buffer_of(cx, &view)[range].to_owned()),
        Some("one".to_owned())
    );
}

#[ui_kit::gpui::test]
fn surround_with_live_template_wraps_the_selection_in_one_step(cx: &mut TestAppContext) {
    let root = project("lookup-surround", &[]);
    let path = window_language::main_class(&root, "probe", "Wrap");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let text = "class Wrap {\n    void run() {\n        first();\n    }\n}\n";
    fs::write(&path, text).unwrap();
    let language = LookupLanguage::new();
    let window = open_window_with_syntax(
        cx,
        root.clone(),
        language,
        window_language::language_for,
        window_language::templates_for,
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    // The templates that fit depend on the syntax tree, which is parsed on its own
    // thread; under load it can come after the key.
    wait_for(cx, "the syntax tree", Duration::from_secs(10), |cx| {
        view.read_with(cx, |view, _| view.has_syntax_tree())
    });
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 2, column: 8 }, cx)
    });
    cx.run_until_parked();
    press(cx, window, "shift-end");
    draw(cx, window);

    press(cx, window, "ctrl-alt-j");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "template-popup").size.height > px(0.),
        "Ctrl+Alt+J shows the templates with the selection"
    );
    press(cx, window, "enter");
    let wrapped = buffer_of(cx, &view);
    assert!(wrapped.contains("if ("), "{wrapped}");
    assert!(wrapped.contains("first();"), "{wrapped}");
    assert_ne!(wrapped, text, "the selected line is wrapped");

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        text,
        "one undo takes the whole template back"
    );
}

#[ui_kit::gpui::test]
fn external_documentation_opens_the_link_through_the_opener(cx: &mut TestAppContext) {
    let root = project("lookup-doc", &[("src/A.txt", "symbol\n")]);
    let language = LookupLanguage::new();
    language.set_documentation(Some("https://docs.example/Symbol"));
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            recorder.lock().unwrap().push(url.to_owned());
        }))));
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, _view) = open_file(cx, window, &root, "src/A.txt", "symbol\n", 0, 3);

    press(cx, window, "shift-f1");
    wait_for(cx, "the documentation link", Duration::from_secs(5), |_| {
        !opened.lock().unwrap().is_empty()
    });
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://docs.example/Symbol"],
        "the test opener gets the link and no browser starts"
    );
}

#[ui_kit::gpui::test]
fn a_late_resolve_keeps_the_snippet_session_and_joins_the_undo_step(cx: &mut TestAppContext) {
    let root = project("lookup-late", &[("src/A.txt", "gr\n")]);
    let language = LookupLanguage::new();
    language.set_resolve(
        vec![edit_in("gr\n", 0, 0, "import demo.Greeting;\n")],
        Duration::from_millis(1200),
    );
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "gr\n", 0, 2);
    let mut snippet = entry(
        "greet(String name) : void",
        CompletionKind::Method,
        "greet(${1:name})$0",
    );
    snippet.snippet = true;
    snippet.resolve = Some("greet".to_owned());
    language.set_entries(vec![snippet]);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    wait_for(cx, "the late import", Duration::from_secs(6), |cx| {
        buffer_of(cx, &view) == "import demo.Greeting;\ngreet(name)\n"
    });

    press(cx, window, "tab");
    assert_eq!(
        buffer_of(cx, &view),
        "import demo.Greeting;\ngreet(name)\n",
        "Tab still moves inside the fields of the snippet"
    );
    assert_eq!(language.resolves(), 1);

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "gr\n",
        "one undo takes the insertion and the late import back"
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn smart_type_offers_the_parameter_of_the_expected_type(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = crate::window_tests_real::project("lookup-real-smart");
    let source = r#"package probe;

public class SmartLookup {

    void take(String text) {
    }

    void call() {
        String value = "a";
        take();
    }
}
"#;
    let path = window_language::write_probe(&root, "probe", "SmartLookup", source);
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let (line, column) = line_column(source, source.find("take()").unwrap() + "take(".len());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-shift-space");
    wait_for(cx, "the smart type list", Duration::from_secs(120), |cx| {
        shown(cx, window, "completion-popup")
    });
    press(cx, window, "enter");
    wait_for(cx, "the expected value", Duration::from_secs(30), |cx| {
        buffer_of(cx, &view).contains("take(value)")
    });

    crate::window_tests_real::finish("smart_type", started);
    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn class_name_offers_a_type_and_adds_its_import(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = crate::window_tests_real::project("lookup-real-class");
    let source = r#"package probe;

public class UseWidget {

    void show() {
        JBu
    }
}
"#;
    let path = window_language::write_probe(&root, "probe", "UseWidget", source);
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let (line, column) = line_column(source, source.find("JBu").unwrap() + "JBu".len());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-space");
    draw(cx, window);
    // The first question may reach the server before it has the new source.
    for _ in 0..4 {
        if shown(cx, window, "completion-popup") {
            break;
        }
        std::thread::sleep(Duration::from_secs(2));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        press(cx, window, "ctrl-alt-space");
        draw(cx, window);
    }
    wait_for(cx, "the type list", Duration::from_secs(60), |cx| {
        shown(cx, window, "completion-popup")
    });
    press(cx, window, "enter");
    wait_for(cx, "the type", Duration::from_secs(30), |cx| {
        buffer_of(cx, &view).contains("JButton")
    });
    wait_for(
        cx,
        "the import of the type",
        Duration::from_secs(60),
        |cx| {
            let text = buffer_of(cx, &view);
            text.contains("import javax.swing.JButton;")
        },
    );

    crate::window_tests_real::finish("class_name", started);
    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn external_documentation_of_a_library_type_reaches_the_opener(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = crate::window_tests_real::project("lookup-real-doc");
    let source = r#"package probe;

public class DocLookup {

    void show() {
        String value = "a";
    }
}
"#;
    let path = window_language::write_probe(&root, "probe", "DocLookup", source);
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            recorder.lock().unwrap().push(url.to_owned());
        }))));
    });
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let (line, column) = line_column(source, source.find("String value").unwrap() + 2);
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();

    let mut tries = 0;
    while opened.lock().unwrap().is_empty() && tries < 6 {
        press(cx, window, "shift-f1");
        let started = Instant::now();
        while opened.lock().unwrap().is_empty() && started.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(50));
            cx.executor().advance_clock(Duration::from_millis(50));
            cx.run_until_parked();
        }
        tries += 1;
    }
    assert!(
        !opened.lock().unwrap().is_empty(),
        "the documentation link arrived"
    );
    let url = opened.lock().unwrap()[0].clone();
    assert!(
        url.contains("docs.oracle.com"),
        "the link of a platform type: {url}"
    );

    crate::window_tests_real::finish("external_documentation", started);
    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn parameter_info_switches_the_overloads_of_the_real_server(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = crate::window_tests_real::project("lookup-real-overloads");
    let source = r#"package probe;

public class Overloads {

    void greet(String name) {
    }

    void greet(String name, int count) {
    }

    void call() {
        greet();
    }
}
"#;
    let path = window_language::write_probe(&root, "probe", "Overloads", source);
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let (line, column) = line_column(source, source.find("greet()").unwrap() + "greet(".len());
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-p");
    wait_for(cx, "the two overloads", Duration::from_secs(120), |cx| {
        view.read_with(cx, |view, _| view.signature_count())
            .is_some_and(|count| count >= 2)
    });
    let first = view.read_with(cx, |view, _| view.signature_position());
    press(cx, window, "ctrl-down");
    let second = view.read_with(cx, |view, _| view.signature_position());
    assert_ne!(first, second, "Ctrl+Down changes the shown overload");

    crate::window_tests_real::finish("parameter_info", started);
    crate::window_tests_real::close(cx, window, language, false);
}
