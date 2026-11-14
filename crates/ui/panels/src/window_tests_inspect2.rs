//! The second batch of Inspection Results tests: problem kinds, Run Inspection by Name
//! with known kinds, fixed findings, Rerun, the keys of the quick fix menu and the
//! batches of closed files. The ignored tests at the end run the real language server
//! on PetClinic.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DiagnosticEntry,
    DiagnosticNote, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit, FileDiagnostics,
    FormatProvider, FormatQuery, LanguageFn, LanguageId, LanguageSupport, ProblemKind, Provider,
    Queries, SemanticServer, Severity, SourceEdit, SourceId, SyntaxProvider, TextPoint, TextSpan,
    WorkspaceDiagnosticsProvider, WorkspaceDiagnosticsQuery,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{MenuEntry, Theme, point, px};
use workspace::{FileIcons, INSPECTION_TOOL, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::inspect::{ActiveInspect, Finding, InspectHub, Mode, RowKind, Scope};
use crate::run::ProjectSetup;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("memory-server");

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

fn note(
    line: u32,
    start: u32,
    end: u32,
    severity: Severity,
    message: &str,
    code: &str,
) -> DiagnosticNote {
    DiagnosticNote {
        span: TextSpan {
            start: TextPoint {
                line,
                character: start,
            },
            end: TextPoint {
                line,
                character: end,
            },
        },
        severity,
        message: message.to_owned(),
        code: Some(code.to_owned()),
    }
}

fn action(
    title: &str,
    kind: ActionKind,
    code: &str,
    uri: &str,
    start: u32,
    end: u32,
    text: &str,
) -> ActionEntry {
    ActionEntry {
        title: title.to_owned(),
        kind: Some(kind),
        code: Some(code.to_owned()),
        preferred: true,
        edits: vec![SourceEdit {
            uri: uri.to_owned(),
            edits: vec![DocumentEdit {
                span: TextSpan {
                    start: TextPoint {
                        line: 0,
                        character: start,
                    },
                    end: TextPoint {
                        line: 0,
                        character: end,
                    },
                },
                text: text.to_owned(),
            }],
        }],
        renames: Vec::new(),
        handle: None,
    }
}

/// The language of these tests. It publishes notes on demand, offers the actions the
/// test sets, and records the documents the inspection opens and closes.
struct TestLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, (i32, Arc<str>)>>,
    published: Mutex<HashMap<String, Vec<DiagnosticNote>>>,
    actions: Mutex<Vec<ActionEntry>>,
    kinds: Mutex<Vec<ProblemKind>>,
    names: Mutex<HashMap<String, String>>,
    open_now: Mutex<HashSet<String>>,
    max_open: AtomicUsize,
    opened: Mutex<Vec<(String, i32)>>,
    closed: Mutex<Vec<String>>,
    /// A server that publishes an empty list for every document it opens, as a real one
    /// does: the inspection uses this to tell that a batch answered.
    publish_on_open: Mutex<bool>,
}

impl TestLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            published: Mutex::new(HashMap::new()),
            actions: Mutex::new(Vec::new()),
            kinds: Mutex::new(Vec::new()),
            names: Mutex::new(HashMap::new()),
            open_now: Mutex::new(HashSet::new()),
            max_open: AtomicUsize::new(0),
            opened: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
            publish_on_open: Mutex::new(false),
        })
    }

    fn set_note(&self, path: &Path, note: DiagnosticNote) {
        self.published
            .lock()
            .unwrap()
            .entry(uri_of(path))
            .or_default()
            .push(note);
    }

    /// A file the server answered for with no problems: it must not be opened again.
    fn declare(&self, path: &Path) {
        self.published
            .lock()
            .unwrap()
            .entry(uri_of(path))
            .or_default();
    }

    fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    /// A kind that Run Inspection by Name offers before any inspection ran.
    fn set_kind(&self, code: &str, name: &str) {
        self.names
            .lock()
            .unwrap()
            .insert(code.to_owned(), name.to_owned());
        self.kinds.lock().unwrap().push(ProblemKind {
            code: code.to_owned(),
            name: name.to_owned(),
        });
    }

    fn publish_on_open(&self) {
        *self.publish_on_open.lock().unwrap() = true;
    }

    fn max_open(&self) -> usize {
        self.max_open.load(Ordering::SeqCst)
    }

    fn opened(&self) -> Vec<(String, i32)> {
        self.opened.lock().unwrap().clone()
    }

    fn closed(&self) -> Vec<String> {
        self.closed.lock().unwrap().clone()
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

impl SemanticServer for TestLanguage {
    fn open(&self, uri: &str, version: i32, text: Arc<str>) {
        self.documents
            .lock()
            .unwrap()
            .insert(uri.to_owned(), (version, text));
        self.opened.lock().unwrap().push((uri.to_owned(), version));
        let mut now = self.open_now.lock().unwrap();
        now.insert(uri.to_owned());
        let count = now.len();
        self.max_open.fetch_max(count, Ordering::SeqCst);
        drop(now);
        if *self.publish_on_open.lock().unwrap() {
            self.published
                .lock()
                .unwrap()
                .entry(uri.to_owned())
                .or_default();
        }
    }

    fn change(&self, uri: &str, version: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        self.documents
            .lock()
            .unwrap()
            .insert(uri.to_owned(), (version, text));
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
        self.open_now.lock().unwrap().remove(uri);
        self.closed.lock().unwrap().push(uri.to_owned());
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .get(uri)
            .map(|(version, _)| *version)
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for TestLanguage {
    fn start(
        &self,
        query: DiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self
            .published
            .lock()
            .unwrap()
            .get(&query.0)
            .map(|notes| {
                notes
                    .iter()
                    .map(|note| DiagnosticEntry {
                        span: note.span,
                        severity: note.severity,
                        message: note.message.clone(),
                        unnecessary: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<WorkspaceDiagnosticsQuery, Vec<FileDiagnostics>> for TestLanguage {
    fn start(
        &self,
        _: WorkspaceDiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<FileDiagnostics>>,
    ) {
        let files = self
            .published
            .lock()
            .unwrap()
            .iter()
            .map(|(uri, notes)| FileDiagnostics {
                uri: uri.clone(),
                diagnostics: notes.clone(),
            })
            .collect();
        let _ = reply.send(self.answer(files));
    }
}

impl Provider<FormatQuery, Vec<DocumentEdit>> for TestLanguage {
    fn start(&self, _: FormatQuery, _: Cancel, mut reply: lang_api::Reply<Vec<DocumentEdit>>) {
        let _ = reply.send(self.answer(Vec::new()));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for TestLanguage {
    fn start(&self, query: ActionQuery, _: Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        // As a server does, answer only about a document open at that very version.
        let known = self
            .documents
            .lock()
            .unwrap()
            .get(&query.uri)
            .map(|(version, _)| *version);
        let entries = if known == Some(query.version) {
            self.actions
                .lock()
                .unwrap()
                .iter()
                .filter(|entry| query.only.is_none() || entry.kind == query.only)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        let _ = reply.send(self.answer(entries));
    }
}

impl LanguageSupport for TestLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("memory")
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

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }

    fn workspace_diagnostics(&self) -> Option<&WorkspaceDiagnosticsProvider> {
        Some(self)
    }

    fn format(&self) -> Option<&FormatProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn problem_kind(&self, code: Option<&str>, message: &str) -> String {
        code.and_then(|code| self.names.lock().unwrap().get(code).cloned())
            .or_else(|| code.map(str::to_owned))
            .unwrap_or_else(|| message.lines().next().unwrap_or("").to_owned())
    }

    fn problem_kinds(&self) -> Vec<ProblemKind> {
        self.kinds.lock().unwrap().clone()
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

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<TestLanguage>,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("inspect2-state");
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

fn hub(cx: &TestAppContext) -> gpui::Entity<InspectHub> {
    cx.read(|cx| cx.global::<ActiveInspect>().0.clone())
}

fn findings(cx: &TestAppContext) -> Vec<Finding> {
    hub(cx).read_with(cx, |hub, _| hub.findings().to_vec())
}

fn rows(cx: &TestAppContext) -> Vec<crate::inspect::Row> {
    let hub = hub(cx);
    hub.read_with(cx, |hub, cx| hub.panel().read(cx).rows().to_vec())
}

fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn focus_panel(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let focus = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

fn start(cx: &mut TestAppContext, window: WindowHandle<Workspace>, mode: Mode, scope: Scope) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(INSPECTION_TOOL, window, cx)
        })
        .unwrap();
    hub(cx).update(cx, |hub, cx| hub.start(mode, scope, cx));
    cx.run_until_parked();
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    limit: Duration,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    crate::window_tests_real::wait_for(cx, what, limit, &mut check);
}

#[ui_kit::gpui::test]
fn findings_of_one_code_form_one_kind_group(cx: &mut TestAppContext) {
    let root = project(
        "inspect2-kind",
        &[("src/First.txt", "first\n"), ("src/Second.txt", "second\n")],
    );
    let language = TestLanguage::new();
    language.set_kind("16777233", "Incompatible types");
    language.set_note(
        &root.join("src/First.txt"),
        note(
            0,
            0,
            5,
            Severity::Error,
            "Type mismatch: cannot convert from String to int",
            "16777233",
        ),
    );
    language.set_note(
        &root.join("src/Second.txt"),
        note(
            0,
            0,
            6,
            Severity::Error,
            "Type mismatch: cannot convert from String to int",
            "16777233",
        ),
    );
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);

    start(cx, window, Mode::Inspect(None), Scope::Project);
    wait_for(cx, "the two findings", Duration::from_secs(10), |cx| {
        findings(cx).len() == 2
    });

    let all = findings(cx);
    assert!(
        all.iter()
            .all(|finding| finding.kind == "Incompatible types")
    );
    let kinds: Vec<_> = rows(cx)
        .into_iter()
        .filter(|row| row.kind == RowKind::Kind)
        .collect();
    assert_eq!(kinds.len(), 1, "{:?}", rows(cx));
    assert_eq!(kinds[0].label, "Incompatible types");
    assert_eq!(kinds[0].hint, "2");
}

#[ui_kit::gpui::test]
fn run_inspection_by_name_offers_a_known_kind_before_any_result(cx: &mut TestAppContext) {
    let root = project("inspect2-known", &[("src/Main.txt", "value\n")]);
    let language = TestLanguage::new();
    language.set_kind("unused", "Unused declaration");
    let window = open_window_with(cx, root, language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-i");
    draw(cx, window);
    assert!(visible(cx, window, "run-inspection"));
    type_text(cx, window, "Unused");
    draw(cx, window);
    assert!(
        visible(cx, window, "run-inspection-row-0"),
        "the known kind is offered"
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        visible(cx, window, "inspect-scope-popup"),
        "the kind opens the scope dialog"
    );
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn a_quick_fix_marks_the_finding_fixed_and_rerun_repeats_the_file(cx: &mut TestAppContext) {
    let root = project("inspect2-fixed", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let uri = uri_of(&path);
    let language = TestLanguage::new();
    language.set_note(
        &path,
        note(
            0,
            0,
            5,
            Severity::Error,
            "The value is never used",
            "unused",
        ),
    );
    language.set_actions(vec![action(
        "Replace the value",
        ActionKind::QuickFix,
        "quickfix.replace",
        &uri,
        0,
        5,
        "fixed",
    )]);
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    start(
        cx,
        window,
        Mode::Inspect(None),
        Scope::CurrentFile(path.clone()),
    );
    wait_for(cx, "the finding", Duration::from_secs(10), |cx| {
        findings(cx).len() == 1
    });
    focus_panel(cx, window);
    press(cx, window, "down down down");
    draw(cx, window);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(visible(cx, window, "menu-0"), "the fixes open");
    press(cx, window, "down");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "fixed\n", "Enter applies the fix");
    wait_for(cx, "the fixed mark", Duration::from_secs(10), |cx| {
        findings(cx).first().is_some_and(|finding| finding.fixed)
    });
    let fixed_row = rows(cx)
        .into_iter()
        .find(|row| matches!(row.kind, RowKind::Finding(_)))
        .expect("the finding row");
    assert!(fixed_row.fixed, "the tree marks the row as fixed");

    click(cx, window, "inspection-rerun");
    wait_for(cx, "the rerun", Duration::from_secs(10), |cx| {
        findings(cx).first().is_some_and(|finding| !finding.fixed)
    });

    // The second fix runs through the keyboard: Ctrl+F5 in the window repeats the run.
    focus_panel(cx, window);
    press(cx, window, "down down down");
    press(cx, window, "alt-enter");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    cx.run_until_parked();
    wait_for(cx, "the fixed mark again", Duration::from_secs(10), |cx| {
        findings(cx).first().is_some_and(|finding| finding.fixed)
    });
    focus_panel(cx, window);
    press(cx, window, "ctrl-f5");
    wait_for(cx, "the keyboard rerun", Duration::from_secs(10), |cx| {
        findings(cx).first().is_some_and(|finding| !finding.fixed)
    });
}

#[ui_kit::gpui::test]
fn the_keys_of_a_menu_shown_with_show_menu_pick_and_close_a_row(cx: &mut TestAppContext) {
    let root = project("inspect2-menu", &[("src/Main.txt", "value\n")]);
    let language = TestLanguage::new();
    let window = open_window_with(cx, root, language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);

    let count = Arc::new(AtomicUsize::new(0));
    let first = count.clone();
    let entries = vec![
        MenuEntry::new("First", move |_, _| {
            first.fetch_add(1, Ordering::SeqCst);
        }),
        MenuEntry::new("Second", |_, _| {}),
    ];
    window
        .update(cx, |workspace, _, cx| {
            workspace.show_menu(point(px(40.), px(40.)), entries, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "Enter runs the highlighted row"
    );

    let entries = vec![MenuEntry::new("Only", |_, _| {})];
    window
        .update(cx, |workspace, _, cx| {
            workspace.show_menu(point(px(40.), px(40.)), entries, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    press(cx, window, "escape");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "Escape closes the menu"
    );
}

#[ui_kit::gpui::test]
fn closed_files_open_in_batches_of_fifty_and_close_again(cx: &mut TestAppContext) {
    let root = scratch("inspect2-batch");
    let mut files = Vec::new();
    for index in 0..120 {
        let path = root.join("src").join(format!("File{index}.txt"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "value\n").unwrap();
        files.push(path);
    }
    let language = TestLanguage::new();
    language.publish_on_open();
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);

    start(cx, window, Mode::Inspect(None), Scope::Module(root.clone()));
    wait_for(cx, "the inspection", Duration::from_secs(60), |cx| {
        hub(cx).read_with(cx, |hub, _| hub.can_rerun())
    });

    assert_eq!(language.opened().len(), files.len());
    assert_eq!(language.closed().len(), files.len());
    assert_eq!(
        language.max_open(),
        50,
        "one batch stays open at a time, and a full batch has fifty files"
    );
}

#[ui_kit::gpui::test]
fn a_file_the_build_answered_for_is_not_opened(cx: &mut TestAppContext) {
    let root = project(
        "inspect2-answered",
        &[
            ("src/Answered.txt", "answered\n"),
            ("src/Asked.txt", "asked\n"),
        ],
    );
    let answered = root.join("src").join("Answered.txt");
    let asked = root.join("src").join("Asked.txt");
    let language = TestLanguage::new();
    language.publish_on_open();
    language.declare(&answered);
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);

    start(cx, window, Mode::Inspect(None), Scope::Module(root.clone()));
    wait_for(cx, "the inspection", Duration::from_secs(10), |cx| {
        hub(cx).read_with(cx, |hub, _| hub.can_rerun())
    });

    let opened: Vec<String> = language.opened().into_iter().map(|(uri, _)| uri).collect();
    assert!(
        !opened.contains(&uri_of(&answered)),
        "a file with an answer is not opened: {opened:?}"
    );
    assert!(
        opened.contains(&uri_of(&asked)),
        "a file without an answer is opened: {opened:?}"
    );
}

#[ui_kit::gpui::test]
fn code_cleanup_opens_a_closed_file_at_its_real_version(cx: &mut TestAppContext) {
    let root = project("inspect2-cleanup", &[("src/Main.txt", "unused")]);
    let path = root.join("src").join("Main.txt");
    let uri = uri_of(&path);
    let language = TestLanguage::new();
    language.set_actions(vec![action(
        "Fix the file",
        ActionKind::FixAll,
        "source.fixAll",
        &uri,
        0,
        6,
        "clean\n",
    )]);
    let window = open_window_with(
        cx,
        root.clone(),
        language.clone(),
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);

    start(cx, window, Mode::Cleanup, Scope::CurrentFile(path.clone()));
    for _ in 0..400 {
        if fs::read_to_string(&path).is_ok_and(|text| text == "clean\n") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "clean\n",
        "opened {:?} closed {:?}",
        language.opened(),
        language.closed()
    );
}

// The tests below need `IDE_CORPUS` and a filled package cache:
//
// `cargo test -p panels window_tests_inspect2 -- --ignored --test-threads=1`

struct StaticTool(project_model::ProjectModel);

impl project_model::BuildSystem for StaticTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<project_model::ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<project_model::ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<proc_sup::ProcessSpec> {
        Err("not used by the test".into())
    }
}

fn module_setup(root: &Path) -> ProjectSetup {
    let model = project_model::ProjectModel {
        root: root.to_path_buf(),
        tool: "test".to_owned(),
        tool_version: None,
        modules: vec![project_model::Module {
            id: "petclinic".to_owned(),
            name: "petclinic".to_owned(),
            dir: root.to_path_buf(),
            units: Vec::new(),
        }],
        resolved: true,
        problems: Vec::new(),
    };
    ProjectSetup {
        tool: Some(Arc::new(StaticTool(model))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("inspect2-real-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

/// Writes a class whose only error is a value of another type.
fn write_type_mismatch(root: &Path, class: &str) -> PathBuf {
    window_language::write_probe(
        root,
        "probe",
        class,
        &format!("package probe;\n\npublic class {class} {{\n    int value = \"text\";\n}}\n"),
    )
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn real_inspection_groups_two_errors_of_one_kind(cx: &mut TestAppContext) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("inspect2-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    let first = write_type_mismatch(&root, "FirstBroken");
    let second = write_type_mismatch(&root, "SecondBroken");
    let language =
        window_language::RealLanguage::open(root.clone(), scratch("inspect2-real-state"))
            .expect("the language server starts");
    let window =
        crate::window_tests_real::build_window(cx, root.clone(), &language, module_setup(&root));
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);

    start(cx, window, Mode::Inspect(None), Scope::Module(root.clone()));
    wait_for(cx, "the two closed files", Duration::from_secs(300), |cx| {
        let all = findings(cx);
        all.iter().any(|finding| finding.path == first)
            && all.iter().any(|finding| finding.path == second)
    });

    let all = findings(cx);
    let kinds: Vec<&str> = all
        .iter()
        .filter(|finding| finding.path == first || finding.path == second)
        .map(|finding| finding.kind.as_str())
        .collect();
    assert!(!kinds.is_empty(), "{all:?}");
    assert!(
        kinds.iter().all(|kind| *kind == kinds[0]),
        "one kind for both files: {kinds:?}"
    );
    assert!(
        !kinds[0].starts_with(|character: char| character.is_ascii_digit()),
        "the kind is a name, not a code: {}",
        kinds[0]
    );
    let group = rows(cx)
        .into_iter()
        .find(|row| row.kind == RowKind::Kind && row.label == kinds[0])
        .expect("the kind group");
    let count: usize = group.hint.parse().expect("the group count");
    assert!(count >= 2, "both files are in one group: {count}");

    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn real_code_cleanup_removes_an_unused_import_from_a_closed_file(cx: &mut TestAppContext) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("inspect2-cleanup-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    let (sample, _) = window_language::write_refactor_samples(&root);
    let language =
        window_language::RealLanguage::open(root.clone(), scratch("inspect2-cleanup-state"))
            .expect("the language server starts");
    let window =
        crate::window_tests_real::build_window(cx, root.clone(), &language, module_setup(&root));
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.view_of(&sample).is_none())
            .unwrap(),
        "the file is closed before the cleanup"
    );

    start(
        cx,
        window,
        Mode::Cleanup,
        Scope::CurrentFile(sample.clone()),
    );
    wait_for(
        cx,
        "the cleanup of the closed file",
        Duration::from_secs(120),
        |cx| {
            hub(cx).read_with(cx, |hub, _| hub.can_rerun())
                && fs::read_to_string(&sample)
                    .is_ok_and(|text| window_language::unused_import_is_gone(&text))
        },
    );

    crate::window_tests_real::close(cx, window, language, false);
}
