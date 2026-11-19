//! Completion and navigation as the user feels them: the list filters itself while the
//! prefix grows, the order puts the likely entry first, the import of an accepted entry
//! comes with it, Tab replaces the rest of the word, a faster key cancels the request
//! that went stale and Ctrl+B opens one place at once or a list of several.
//!
//! The last test measures the real server on a copy of PetClinic. It needs `IDE_CORPUS`
//! and a filled package cache:
//!
//! `cargo test -p panels window_tests_speed -- --ignored --test-threads=1`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use editor_view::{EditorView, Languages, Spot};
use lang_api::{
    Answer, CompletionEntry, CompletionKind, CompletionList, CompletionProvider,
    CompletionResolveProvider, Confidence, DocumentEdit, DocumentSpot, LanguageFn, LanguageId,
    LanguageSupport, NavigationProvider, NavigationQuery, Provider, Queries, Reply, SourceId,
    SourceSpan, SyntaxProvider, TextPoint, TextSpan,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("speed-server");

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

fn entry(label: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        insert_text: Some(label.to_owned()),
        ..CompletionEntry::default()
    }
}

/// The language of these tests: the entries, the wait, the places and the readiness come
/// from the test, and no process runs.
pub(crate) struct SpeedLanguage {
    syntax: NoSyntax,
    entries: Mutex<Vec<CompletionEntry>>,
    delay: Mutex<Duration>,
    navigation: Mutex<Vec<SourceSpan>>,
    ready: AtomicBool,
    complete: AtomicBool,
    completions: AtomicUsize,
    cancelled: Arc<AtomicUsize>,
    resolves: AtomicUsize,
    navigations: AtomicUsize,
}

impl SpeedLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            entries: Mutex::new(Vec::new()),
            delay: Mutex::new(Duration::ZERO),
            navigation: Mutex::new(Vec::new()),
            ready: AtomicBool::new(true),
            complete: AtomicBool::new(true),
            completions: AtomicUsize::new(0),
            cancelled: Arc::new(AtomicUsize::new(0)),
            resolves: AtomicUsize::new(0),
            navigations: AtomicUsize::new(0),
        })
    }

    pub(crate) fn set_entries(&self, entries: Vec<CompletionEntry>) {
        *self.entries.lock().unwrap() = entries;
    }

    fn set_delay(&self, delay: Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    pub(crate) fn set_navigation(&self, spans: Vec<SourceSpan>) {
        *self.navigation.lock().unwrap() = spans;
    }

    pub(crate) fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Relaxed);
    }

    /// Whether the answer holds every entry that can match a longer prefix.
    fn set_complete(&self, complete: bool) {
        self.complete.store(complete, Ordering::Relaxed);
    }

    pub(crate) fn completions(&self) -> usize {
        self.completions.load(Ordering::Relaxed)
    }

    fn cancelled(&self) -> usize {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub(crate) fn resolves(&self) -> usize {
        self.resolves.load(Ordering::Relaxed)
    }

    fn navigations(&self) -> usize {
        self.navigations.load(Ordering::Relaxed)
    }
}

/// Waits for `delay`, reporting a cancellation so that a test sees the stale request was
/// dropped instead of answered.
fn wait_or_cancel(cancel: &Cancel, delay: Duration, cancelled: &AtomicUsize) -> bool {
    let step = Duration::from_millis(5);
    let mut left = delay;
    while !left.is_zero() {
        if cancel.is_cancelled() {
            cancelled.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let tick = left.min(step);
        std::thread::sleep(tick);
        left -= tick;
    }
    true
}

impl Provider<DocumentSpot, CompletionList> for SpeedLanguage {
    fn start(&self, _: DocumentSpot, cancel: Cancel, mut reply: Reply<CompletionList>) {
        self.completions.fetch_add(1, Ordering::Relaxed);
        let entries = self.entries.lock().unwrap().clone();
        let complete = self.complete.load(Ordering::Relaxed);
        let delay = *self.delay.lock().unwrap();
        if delay.is_zero() {
            if !cancel.is_cancelled() {
                let _ = reply.send(Answer::new(
                    CompletionList { entries, complete },
                    Confidence::Certain,
                    SOURCE,
                ));
            }
            return;
        }
        let cancelled = self.cancelled.clone();
        std::thread::spawn(move || {
            if wait_or_cancel(&cancel, delay, &cancelled) {
                let _ = reply.send(Answer::new(
                    CompletionList { entries, complete },
                    Confidence::Certain,
                    SOURCE,
                ));
            }
        });
    }
}

impl Provider<CompletionEntry, CompletionEntry> for SpeedLanguage {
    fn start(&self, mut entry: CompletionEntry, _: Cancel, mut reply: Reply<CompletionEntry>) {
        self.resolves.fetch_add(1, Ordering::Relaxed);
        entry.documentation = Some("The documentation of the entry.".to_owned());
        let _ = reply.send(Answer::new(entry, Confidence::Certain, SOURCE));
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for SpeedLanguage {
    fn start(&self, _: NavigationQuery, _: Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        self.navigations.fetch_add(1, Ordering::Relaxed);
        let spans = self.navigation.lock().unwrap().clone();
        let answer = if spans.is_empty() {
            Answer::new(Vec::new(), Confidence::Unknown, SOURCE)
        } else {
            Answer::new(spans, Confidence::Certain, SOURCE)
        };
        let _ = reply.send(answer);
    }
}

impl LanguageSupport for SpeedLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("speed")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    fn preferred_completion_names(&self) -> &'static [&'static str] {
        &["String"]
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        Some(self)
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }
}

/// The language of the editor itself, so a template and a statement have their rules.
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
    language: Arc<SpeedLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("speed-state");
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
            project_setup(scratch("speed-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
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
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    (path, view)
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn span_at(path: &Path, line: u32, character: u32) -> SourceSpan {
    SourceSpan {
        uri: crate::window_tests_lang::uri_of(path),
        span: TextSpan {
            start: TextPoint { line, character },
            end: TextPoint { line, character },
        },
    }
}

/// The labels of the open list, in the order the window shows them.
fn rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<Vec<String>> {
    view.read_with(cx, |view, _| view.completion_rows())
}

#[ui_kit::gpui::test]
fn typing_filters_the_open_list_in_place_and_the_order_follows_idea(cx: &mut TestAppContext) {
    let root = project("speed-filter", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    let mut frequent = entry("String - core");
    frequent.sort_text = Some("0002".to_owned());
    frequent.kind = CompletionKind::Class;
    let mut rare = entry("StrictMath - core");
    rare.sort_text = Some("0001".to_owned());
    rare.kind = CompletionKind::Class;
    let mut elsewhere = entry("Stream - util");
    elsewhere.sort_text = Some("0003".to_owned());
    elsewhere.kind = CompletionKind::Class;
    let mut field = entry("street : String");
    field.sort_text = Some("0000".to_owned());
    field.kind = CompletionKind::Field;
    language.set_entries(vec![rare, elsewhere, field, frequent]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    type_text(cx, window, "Str");
    let shown = rows(cx, &view).expect("the list is open");
    assert_eq!(
        shown,
        [
            "String - core",
            "StrictMath - core",
            "Stream - util",
            "street : String"
        ],
        "a frequent type, then a rare one, then another package, then the field"
    );
    assert_eq!(language.completions(), 1, "the list came from one request");

    type_text(cx, window, "in");
    assert_eq!(
        rows(cx, &view),
        Some(vec!["String - core".to_owned()]),
        "the list filters itself while the prefix grows"
    );
    assert_eq!(
        language.completions(),
        1,
        "a growing prefix asks the server nothing"
    );
}

#[ui_kit::gpui::test]
fn an_incomplete_list_asks_again_when_the_prefix_grows(cx: &mut TestAppContext) {
    let root = project("speed-incomplete", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_complete(false);
    language.set_entries(vec![entry("alpha")]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    type_text(cx, window, "a");
    assert_eq!(language.completions(), 1);
    type_text(cx, window, "l");
    assert_eq!(
        language.completions(),
        2,
        "an incomplete answer cannot know the entries of a longer prefix"
    );
    assert_eq!(rows(cx, &view), Some(vec!["alpha".to_owned()]));
}

#[ui_kit::gpui::test]
fn additional_edits_apply_with_the_entry(cx: &mut TestAppContext) {
    let root = project("speed-import", &[("src/A.txt", "Lis\n")]);
    let language = SpeedLanguage::new();
    let mut import = entry("List");
    import.additional_edits = vec![DocumentEdit {
        span: TextSpan {
            start: TextPoint {
                line: 0,
                character: 0,
            },
            end: TextPoint {
                line: 0,
                character: 0,
            },
        },
        text: "import example.List;\n".to_owned(),
    }];
    language.set_entries(vec![import]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "Lis\n", 0, 3);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "import example.List;\nList\n");
}

#[ui_kit::gpui::test]
fn tab_replaces_the_rest_of_the_word(cx: &mut TestAppContext) {
    let root = project("speed-tab", &[("src/A.txt", "Stri\n")]);
    let language = SpeedLanguage::new();
    language.set_entries(vec![entry("String")]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "Stri\n", 0, 3);

    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "tab");
    assert_eq!(buffer_of(cx, &view), "String\n");
}

#[ui_kit::gpui::test]
fn the_documentation_of_the_selected_entry_arrives_after_the_list(cx: &mut TestAppContext) {
    let root = project("speed-doc", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    let mut first = entry("alpha");
    first.resolve = Some("alpha".to_owned());
    let mut second = entry("beta");
    second.resolve = Some("beta".to_owned());
    language.set_entries(vec![first, second]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, _view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    type_text(cx, window, "a");
    wait_for(cx, "the documentation", Duration::from_secs(5), |_| {
        language.resolves() == 1
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "completion-doc"),
        "the documentation of the selected row is under the list"
    );
    assert_eq!(language.resolves(), 1, "the list asked once for the doc");
}

#[ui_kit::gpui::test]
fn a_faster_keystroke_cancels_the_stale_request(cx: &mut TestAppContext) {
    let root = project("speed-cancel", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_delay(Duration::from_millis(200));
    language.set_entries(vec![entry("String")]);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    type_text(cx, window, "S");
    type_text(cx, window, "t");
    type_text(cx, window, "r");
    wait_for(
        cx,
        "the list of the last key",
        Duration::from_secs(5),
        |cx| rows(cx, &view).is_some(),
    );
    assert_eq!(language.completions(), 3, "each key asked once");
    assert!(
        language.cancelled() >= 2,
        "the first two requests were dropped, not answered: {}",
        language.cancelled()
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_goes_at_once_to_one_target_and_lists_several(cx: &mut TestAppContext) {
    let root = project(
        "speed-nav",
        &[
            ("src/A.txt", "shape\n"),
            ("src/B.txt", "first\n"),
            ("src/C.txt", "second\n"),
        ],
    );
    let language = SpeedLanguage::new();
    let (b, c, a) = (
        root.join("src/B.txt"),
        root.join("src/C.txt"),
        root.join("src/A.txt"),
    );
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "shape\n", 0, 0);

    language.set_navigation(vec![span_at(&b, 0, 0)]);
    press(cx, window, "ctrl-b");
    wait_for(cx, "the one definition", Duration::from_secs(5), |cx| {
        active_path(cx, window) == Some(b.clone())
    });

    open_path(cx, window, &a);
    focus_editor(cx, window);
    // Another place of the same text: its answer is its own, not the cached one.
    view.update(cx, |view, cx| view.go_to(Spot::Offset(1), cx));
    cx.run_until_parked();
    language.set_navigation(vec![span_at(&b, 0, 0), span_at(&c, 0, 0)]);
    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the list of the definitions",
        Duration::from_secs(5),
        |cx| {
            editor_of(cx, window, &a).read_with(cx, |view, _| view.navigation_targets()) == Some(2)
        },
    );
    draw(cx, window);
    assert!(
        shown(cx, window, "navigation-popup"),
        "Ctrl+B shows the places to pick from"
    );
    press(cx, window, "down enter");
    wait_for(cx, "the second definition", Duration::from_secs(5), |cx| {
        active_path(cx, window) == Some(c.clone())
    });
}

#[ui_kit::gpui::test]
fn escape_closes_the_list_and_drops_the_request_behind_it(cx: &mut TestAppContext) {
    let root = project("speed-escape", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_entries(vec![entry("String")]);
    language.set_complete(false);
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "\n", 0, 0);

    type_text(cx, window, "S");
    assert_eq!(language.completions(), 1);
    // The next key asks again: an incomplete answer cannot know the entries of a longer
    // prefix.
    language.set_delay(Duration::from_millis(200));
    type_text(cx, window, "t");
    assert_eq!(language.completions(), 2);
    press(cx, window, "escape");
    wait_for(
        cx,
        "the request to be dropped",
        Duration::from_secs(5),
        |_| language.cancelled() >= 1,
    );
    assert_eq!(
        rows(cx, &view),
        None,
        "the stale answer must not open the list again"
    );
}

#[ui_kit::gpui::test]
fn a_repeated_ctrl_b_at_the_same_place_answers_without_the_server(cx: &mut TestAppContext) {
    let root = project(
        "speed-nav-cache",
        &[("src/A.txt", "shape\n"), ("src/B.txt", "first\n")],
    );
    let language = SpeedLanguage::new();
    let b = root.join("src/B.txt");
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (a, _) = open_file(cx, window, &root, "src/A.txt", "shape\n", 0, 0);

    language.set_navigation(vec![span_at(&b, 0, 0)]);
    press(cx, window, "ctrl-b");
    wait_for(cx, "the one definition", Duration::from_secs(5), |cx| {
        active_path(cx, window) == Some(b.clone())
    });
    assert_eq!(language.navigations(), 1);

    open_path(cx, window, &a);
    focus_editor(cx, window);
    press(cx, window, "ctrl-b");
    wait_for(cx, "the cached definition", Duration::from_secs(5), |cx| {
        active_path(cx, window) == Some(b.clone())
    });
    assert_eq!(
        language.navigations(),
        1,
        "the same press on the same text asks the server nothing"
    );
}

#[ui_kit::gpui::test]
fn the_lists_of_the_editor_close_the_navigation_list(cx: &mut TestAppContext) {
    let root = project(
        "speed-nav-popups",
        &[
            ("src/A.txt", "shape\n"),
            ("src/B.txt", "first\n"),
            ("src/C.txt", "second\n"),
        ],
    );
    let language = SpeedLanguage::new();
    language.set_entries(vec![entry("String")]);
    let (b, c, _) = (
        root.join("src/B.txt"),
        root.join("src/C.txt"),
        root.join("src/A.txt"),
    );
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(cx, window, &root, "src/A.txt", "shape\n", 0, 0);

    language.set_navigation(vec![span_at(&b, 0, 0), span_at(&c, 0, 0)]);
    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the list of the definitions",
        Duration::from_secs(5),
        |cx| view.read_with(cx, |view, _| view.navigation_targets()) == Some(2),
    );
    draw(cx, window);
    press(cx, window, "escape");
    wait_for(
        cx,
        "Escape to close the list",
        Duration::from_secs(5),
        |cx| {
            view.read_with(cx, |view, _| view.navigation_targets())
                .is_none()
        },
    );

    press(cx, window, "ctrl-b");
    wait_for(cx, "the list again", Duration::from_secs(5), |cx| {
        view.read_with(cx, |view, _| view.navigation_targets()) == Some(2)
    });
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the completion list to take the screen",
        Duration::from_secs(5),
        |cx| {
            view.read_with(cx, |view, _| {
                view.navigation_targets().is_none() && view.completion_rows().is_some()
            })
        },
    );
    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the navigation list to take the screen",
        Duration::from_secs(5),
        |cx| {
            view.read_with(cx, |view, _| {
                view.completion_rows().is_none() && view.navigation_targets() == Some(2)
            })
        },
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_falls_back_to_the_project_index_while_the_server_is_not_ready(cx: &mut TestAppContext) {
    let root = project(
        "speed-fallback",
        &[("src/A.txt", "shape\n"), ("src/B.txt", "class Shape {}\n")],
    );
    let language = SpeedLanguage::new();
    language.set_ready(false);
    let b = root.join("src/B.txt");
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let (_, _view) = open_file(cx, window, &root, "src/A.txt", "shape\n", 0, 0);

    let target = span_at(&b, 0, 6);
    cx.update(|cx| {
        cx.set_global(editor_view::SymbolLookup(Some(Arc::new(move |_: &str| {
            Some(target.clone())
        }))));
    });
    press(cx, window, "ctrl-b");
    wait_for(cx, "the place of the index", Duration::from_secs(5), |cx| {
        active_path(cx, window) == Some(b.clone())
    });
}

// ---- The measurement on the real server -------------------------------------------

/// The first answer of a cold server, on this machine.
const COLD: Duration = Duration::from_millis(6000);
/// A repeated answer, when the server is warm.
const WARM: Duration = Duration::from_millis(1500);
/// A place of a library: its source is read out of the JDK or the dependency, not out of
/// the project, and that read can cost more than an answer about the project.
const LIBRARY: Duration = Duration::from_millis(10000);
/// The first read of the source of a library class: the server builds the source of the
/// class file for this one, before the text can be shown.
const LIBRARY_FIRST: Duration = Duration::from_millis(15000);
/// A keystroke that only filters an open list.
const FILTER: Duration = Duration::from_millis(400);

/// The threshold of this machine, scaled by `IDE_PERF_LIMIT_SCALE` for a slower one.
fn limit(base: Duration) -> Duration {
    let scale = std::env::var("IDE_PERF_LIMIT_SCALE")
        .ok()
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|scale| *scale > 0.1)
        .unwrap_or(1.);
    base.mul_f32(scale)
}

/// Runs the keys and waits until `check` holds, returning the time from the first key to
/// the answer. The clock of the test platform advances while the real server works.
fn measure(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    what: &str,
    limit: Duration,
    run: impl FnOnce(&mut TestAppContext, WindowHandle<Workspace>),
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) -> Duration {
    let started = Instant::now();
    run(cx, window);
    loop {
        draw(cx, window);
        if check(cx) {
            break;
        }
        assert!(
            started.elapsed() < limit,
            "{what} took longer than {limit:?}, metrics {metrics:?}",
            metrics = base::perf::snapshot(Duration::from_secs(3600))
        );
        std::thread::sleep(Duration::from_millis(5));
        cx.executor().advance_clock(Duration::from_millis(5));
        cx.run_until_parked();
    }
    started.elapsed()
}

/// How many answers each metric has seen since the process started.
fn request_counts() -> std::collections::HashMap<&'static str, u64> {
    base::perf::snapshot(Duration::from_secs(3600))
        .into_iter()
        .map(|row| (row.name, row.count))
        .collect()
}

/// Waits for the answers of the server about a file it has just opened. The first
/// analysis of a file, and the work the server may run beside it, cost seconds; the
/// numbers below are about the answers of a server that has read this file once.
fn warm_up(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    let before = request_counts();
    open_path(cx, window, path);
    draw(cx, window);
    wait_for(
        cx,
        "the first answers about the file",
        Duration::from_secs(180),
        |_| {
            let counts = request_counts();
            ["lsp.semantic_tokens", "lsp.inlay_hint", "lsp.code_lens"]
                .iter()
                .all(|name| {
                    counts.get(name).copied().unwrap_or(0) > before.get(name).copied().unwrap_or(0)
                })
        },
    );
    // A count of usages is the longest job of the server: it waits for a quiet text.
    for _ in 0..80 {
        if request_counts()
            .get("lsp.code_lens_resolve")
            .copied()
            .unwrap_or(0)
            > before.get("lsp.code_lens_resolve").copied().unwrap_or(0)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    press(cx, window, "escape");
    draw(cx, window);
}

/// The answers of the server and the keys of the editor, as `base::perf` kept them, so a
/// slow number above can be read next to the request that took it.
fn report_metrics() {
    for row in base::perf::snapshot(Duration::from_secs(3600)) {
        if !row.name.starts_with("lsp.") && row.name != "editor.keystroke" {
            continue;
        }
        println!(
            "{}: {} events, p50 {:?}, p95 {:?}, p99 {:?}",
            row.name, row.count, row.p50, row.p95, row.p99
        );
    }
}

fn report(what: &str, first: Duration, again: Duration, prepared: bool) {
    println!(
        "{what}: first {:.0} ms, repeated {:.0} ms{}",
        first.as_secs_f64() * 1000.,
        again.as_secs_f64() * 1000.,
        if prepared { " (prepared)" } else { "" }
    );
}

fn write_probes(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let shape = window_language::main_class(root, "probe", "Shape");
    let circle = window_language::main_class(root, "probe", "Circle");
    let probe = window_language::main_class(root, "probe", "SpeedProbe");
    let main = window_language::main_class(root, "probe", "SpeedMain");
    let files = [
        (
            &shape,
            "package probe;\n\ninterface Shape {\n    double area();\n}\n",
        ),
        (
            &circle,
            "package probe;\n\npublic class Circle implements Shape {\n    @Override\n    public double area() {\n        return 1.0;\n    }\n}\n",
        ),
        (
            &probe,
            "package probe;\n\npublic class SpeedProbe {\n    private String text = \"\";\n\n    void complete() {\n        text\n    }\n\n    void letters() {\n        String a1 = null;\n        String a2 = null;\n        String a3 = null;\n        String b1 = null;\n    }\n\n    double call(Circle circle) {\n        return circle.area();\n    }\n\n    void jdk() {\n        String value = \"\";\n        value.length();\n    }\n}\n",
        ),
        (
            &main,
            "package probe;\n\nclass SpeedMain {\n    void use(Shape shape) {\n        shape.area();\n    }\n}\n",
        ),
    ];
    for (path, text) in files {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    (shape, circle, probe, main)
}

/// Puts the keyboard back into the editor of the active tab: a navigation may have left
/// the focus in the tab it opened.
fn focus_editor(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
}

/// The place of `needle` in the text the editor holds now, after the caret.
fn offset_after(cx: &TestAppContext, view: &gpui::Entity<EditorView>, needle: &str) -> usize {
    let text = buffer_of(cx, view);
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is in the text"))
        + needle.len()
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn completion_and_navigation_latency_on_petclinic(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = crate::window_tests_real::project("speed-real");
    let (shape, circle, probe, main) = write_probes(&root);
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("speed-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    language.open_document(&probe).unwrap();
    language.open_document(&main).unwrap();
    warm_up(cx, window, &probe);

    println!(
        "T-0266 measurements: {}, {} threads, corpus PetClinic",
        std::env::consts::OS,
        std::thread::available_parallelism().map_or(0, |count| count.get())
    );

    // Completion after a dot, then after letters, then a letter of an open list.
    let run_dot = |cx: &mut TestAppContext| {
        open_path(cx, window, &probe);
        focus_editor(cx, window);
        let view = editor_of(cx, window, &probe);
        let at = offset_after(cx, &view, "        text");
        view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
        cx.run_until_parked();
        measure(
            cx,
            window,
            "completion after a dot",
            limit(COLD),
            |cx, window| type_text(cx, window, "."),
            |cx| shown(cx, window, "completion-popup"),
        )
    };
    let dot_first = run_dot(cx);
    press(cx, window, "escape");
    draw(cx, window);
    let dot_again = run_dot(cx);
    report("completion after a dot", dot_first, dot_again, false);
    assert!(dot_first < limit(COLD), "the first dot list is slow");
    assert!(dot_again < limit(WARM), "the repeated dot list is slow");

    // A fresh declaration for every run: the initializer is cleared, so the letters are
    // typed where a type is wanted.
    let letter_run = |cx: &mut TestAppContext, marker: &str, typed: &str| {
        open_path(cx, window, &probe);
        focus_editor(cx, window);
        let view = editor_of(cx, window, &probe);
        let text = buffer_of(cx, &view);
        let declaration = format!("String {marker} = null;");
        let from = text
            .find(&declaration)
            .unwrap_or_else(|| panic!("{declaration:?} is in the probe"));
        let null = from + format!("String {marker} = ").len();
        view.update(cx, |view, cx| {
            view.replace_text(null..null + "null".len(), "", cx);
            view.go_to(Spot::Offset(null), cx);
        });
        cx.run_until_parked();
        draw(cx, window);
        measure(
            cx,
            window,
            &format!("completion after {typed:?}"),
            limit(COLD),
            |cx, window| {
                // One key at a time: after the first letter the list filters the answer
                // instead of asking again.
                for character in typed.chars() {
                    type_text(cx, window, &character.to_string());
                }
            },
            |cx| shown(cx, window, "completion-popup"),
        )
    };
    let one_first = letter_run(cx, "a1", "S");
    press(cx, window, "escape");
    let one_again = letter_run(cx, "a2", "S");
    report("completion after 1 letter", one_first, one_again, false);
    press(cx, window, "escape");
    let two_first = letter_run(cx, "a3", "St");
    report("completion after 2 letters", two_first, two_first, false);
    press(cx, window, "escape");
    let three_first = letter_run(cx, "b1", "Str");
    report(
        "completion after 3 letters",
        three_first,
        three_first,
        false,
    );
    // One more letter of an open list is a filter of the answer already here.
    let filter = measure(
        cx,
        window,
        "a letter of an open list",
        limit(FILTER),
        |cx, window| type_text(cx, window, "i"),
        |cx| shown(cx, window, "completion-popup"),
    );
    println!(
        "a letter of an open list: {:.0} ms",
        filter.as_secs_f64() * 1000.
    );
    assert!(filter < limit(FILTER), "filtering the open list is slow");
    press(cx, window, "escape");

    // Ctrl+B: a class of the project, a method of the project, a class of the JDK.
    let definition = |cx: &mut TestAppContext, path: &Path, needle: &str| {
        open_path(cx, window, path);
        focus_editor(cx, window);
        let view = editor_of(cx, window, path);
        let at = offset_after(cx, &view, needle).saturating_sub(1);

        view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
        cx.run_until_parked();
        press(cx, window, "ctrl-b");
    };
    let class_first = measure(
        cx,
        window,
        "Ctrl+B on a project class",
        limit(COLD),
        |cx, _| definition(cx, &main, "Shape"),
        |cx| active_path(cx, window) == Some(shape.clone()),
    );
    let class_again = measure(
        cx,
        window,
        "Ctrl+B on a project class",
        limit(WARM),
        |cx, _| definition(cx, &main, "Shape"),
        |cx| active_path(cx, window) == Some(shape.clone()),
    );
    report("Ctrl+B on a project class", class_first, class_again, false);

    let method_first = measure(
        cx,
        window,
        "Ctrl+B on a project method",
        limit(COLD),
        |cx, _| definition(cx, &main, "area"),
        |cx| active_path(cx, window) == Some(shape.clone()),
    );
    let method_again = measure(
        cx,
        window,
        "Ctrl+B on a project method",
        limit(WARM),
        |cx, _| definition(cx, &main, "area"),
        |cx| active_path(cx, window) == Some(shape.clone()),
    );
    report(
        "Ctrl+B on a project method",
        method_first,
        method_again,
        false,
    );

    let jdk_first = measure(
        cx,
        window,
        "Ctrl+B on a JDK class",
        limit(LIBRARY_FIRST),
        |cx, _| definition(cx, &probe, "String"),
        |cx| active_path(cx, window).is_some_and(|path| path != probe),
    );
    // The first Ctrl+B opened a large source from the JDK; let the server finish the
    // work that reading it started before the repeated measurement.
    for _ in 0..30 {
        std::thread::sleep(Duration::from_millis(50));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    let jdk_again = measure(
        cx,
        window,
        "Ctrl+B on a JDK class",
        limit(LIBRARY),
        |cx, _| definition(cx, &probe, "String"),
        |cx| active_path(cx, window).is_some_and(|path| path != probe),
    );
    report("Ctrl+B on a JDK class", jdk_first, jdk_again, false);

    // Ctrl+Alt+B: an implementation of the interface.
    let implementation_first = measure(
        cx,
        window,
        "Ctrl+Alt+B",
        limit(COLD),
        |cx, _| {
            open_path(cx, window, &main);
            focus_editor(cx, window);
            let view = editor_of(cx, window, &main);
            let at = offset_after(cx, &view, "Shape") - 1;
            view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
            cx.run_until_parked();
            press(cx, window, "ctrl-alt-b");
        },
        |cx| active_path(cx, window) == Some(circle.clone()),
    );
    let implementation_again = measure(
        cx,
        window,
        "Ctrl+Alt+B",
        limit(WARM),
        |cx, _| {
            open_path(cx, window, &main);
            focus_editor(cx, window);
            let view = editor_of(cx, window, &main);
            let at = offset_after(cx, &view, "Shape") - 1;
            view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
            cx.run_until_parked();
            press(cx, window, "ctrl-alt-b");
        },
        |cx| active_path(cx, window) == Some(circle.clone()),
    );
    report(
        "Ctrl+Alt+B",
        implementation_first,
        implementation_again,
        false,
    );

    // Alt+F7: the usages of the method.
    let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    let usages_ready = |cx: &mut TestAppContext| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && !usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty())
    };
    let usages_first = measure(
        cx,
        window,
        "Find Usages",
        limit(COLD),
        |cx, _| {
            open_path(cx, window, &main);
            focus_editor(cx, window);
            let view = editor_of(cx, window, &main);
            let at = offset_after(cx, &view, "area") - 2;
            view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
            cx.run_until_parked();
            press(cx, window, "alt-f7");
        },
        usages_ready,
    );
    let usages_again = measure(
        cx,
        window,
        "Find Usages",
        limit(WARM),
        |cx, _| {
            open_path(cx, window, &main);
            focus_editor(cx, window);
            let view = editor_of(cx, window, &main);
            let at = offset_after(cx, &view, "area") - 2;
            view.update(cx, |view, cx| view.go_to(Spot::Offset(at), cx));
            cx.run_until_parked();
            press(cx, window, "alt-f7");
        },
        usages_ready,
    );
    report("Find Usages", usages_first, usages_again, false);

    // Go to Class, Go to File and File Structure open the palette.
    let palette = |cx: &mut TestAppContext, key: &str| {
        open_path(cx, window, &probe);
        focus_editor(cx, window);
        press(cx, window, key);
    };
    let class_search_first = measure(
        cx,
        window,
        "Ctrl+N",
        limit(COLD),
        |cx, _| palette(cx, "ctrl-n"),
        |cx| shown(cx, window, "palette"),
    );
    press(cx, window, "escape");
    let class_search_again = measure(
        cx,
        window,
        "Ctrl+N",
        limit(WARM),
        |cx, _| palette(cx, "ctrl-n"),
        |cx| shown(cx, window, "palette"),
    );
    report("Ctrl+N", class_search_first, class_search_again, false);
    press(cx, window, "escape");

    let file_search_first = measure(
        cx,
        window,
        "Ctrl+Shift+N",
        limit(COLD),
        |cx, _| palette(cx, "ctrl-shift-n"),
        |cx| shown(cx, window, "palette"),
    );
    press(cx, window, "escape");
    let file_search_again = measure(
        cx,
        window,
        "Ctrl+Shift+N",
        limit(WARM),
        |cx, _| palette(cx, "ctrl-shift-n"),
        |cx| shown(cx, window, "palette"),
    );
    report("Ctrl+Shift+N", file_search_first, file_search_again, false);
    press(cx, window, "escape");

    let structure_first = measure(
        cx,
        window,
        "Ctrl+F12",
        limit(COLD),
        |cx, _| palette(cx, "ctrl-f12"),
        |cx| shown(cx, window, "palette"),
    );
    press(cx, window, "escape");
    let structure_again = measure(
        cx,
        window,
        "Ctrl+F12",
        limit(WARM),
        |cx, _| palette(cx, "ctrl-f12"),
        |cx| shown(cx, window, "palette"),
    );
    report("Ctrl+F12", structure_first, structure_again, false);
    report_metrics();

    // The prepared list: the answer is in memory and only a key is needed.
    println!(
        "the whole measurement took {:.1} s",
        started.elapsed().as_secs_f64()
    );
    crate::window_tests_real::close(cx, window, language, false);
}
