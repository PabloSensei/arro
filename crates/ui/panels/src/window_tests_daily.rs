//! The daily round of IDEA outside the text: the two tabs of Problems, Fix all of a
//! check in the file, the Evaluate window and Attach to Process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base::Cancel;
use dap_client::{BreakpointSpec, DebugService, Debugger};
use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DebugProcess,
    DebugProcessProvider, DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit,
    InspectionInfo, InspectionLevel, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    SemanticServer, Severity, SourceEdit, SourceId, SyntaxProvider, TextPoint, TextSpan, offset_in,
};
use proc_sup::ProcessSpec;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::problems::{ActiveProblems, Problem};
use crate::run::ProjectSetup;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_window, press, sample_project, scratch,
    type_text,
};
use crate::window_tests_lang::project;
use crate::window_tests_run::{
    EchoRun, MockService, debug_panel, open_with, run_panel, snapshot, wait_for,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("daily-server");

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

/// The language of these tests: the diagnostics, the fixes and the processes come from
/// the test itself.
struct DailyLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    diagnostics: Mutex<Vec<DiagnosticEntry>>,
    processes: Mutex<Vec<DebugProcess>>,
}

impl DailyLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            diagnostics: Mutex::new(Vec::new()),
            processes: Mutex::new(Vec::new()),
        })
    }

    fn set_diagnostics(&self, entries: Vec<DiagnosticEntry>) {
        *self.diagnostics.lock().unwrap() = entries;
    }

    fn set_processes(&self, processes: Vec<DebugProcess>) {
        *self.processes.lock().unwrap() = processes;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for DailyLanguage {
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

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for DailyLanguage {
    fn start(
        &self,
        _: DiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self.diagnostics.lock().unwrap().clone();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for DailyLanguage {
    fn start(&self, query: ActionQuery, _: Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        if query.only.is_some_and(|kind| kind != ActionKind::QuickFix) {
            let _ = reply.send(self.answer(Vec::new()));
            return;
        }
        let text = self.documents.lock().unwrap().get(&query.uri).cloned();
        let Some(text) = text else {
            let _ = reply.send(self.answer(Vec::new()));
            return;
        };
        let start = offset_in(&text, query.span.start);
        let end = offset_in(&text, query.span.end).max(start);
        // A caret inside the word asks for the fix of the word, a diagnostic asks for
        // the fix of its own range.
        let (start, end) = if start == end {
            let is_word = |character: char| {
                character.is_alphanumeric() || character == '_' || character == '$'
            };
            let mut start = start.min(text.len());
            while start > 0 {
                let previous = text[..start].chars().next_back().unwrap_or(' ');
                if !is_word(previous) {
                    break;
                }
                start -= previous.len_utf8();
            }
            let mut end = end.min(text.len());
            while end < text.len() {
                let next = text[end..].chars().next().unwrap_or(' ');
                if !is_word(next) {
                    break;
                }
                end += next.len_utf8();
            }
            (start, end)
        } else {
            (start, end)
        };
        if text.get(start..end) != Some("bad") {
            let _ = reply.send(self.answer(Vec::new()));
            return;
        }
        let entry = ActionEntry {
            title: "Replace bad".to_owned(),
            kind: Some(ActionKind::QuickFix),
            code: Some("quickfix".to_owned()),
            preferred: true,
            edits: vec![SourceEdit {
                uri: query.uri,
                edits: vec![DocumentEdit {
                    span: query.span,
                    text: "good".to_owned(),
                }],
            }],
            renames: Vec::new(),
            handle: None,
            resolve: None,
        };
        let _ = reply.send(self.answer(vec![entry]));
    }
}

impl Provider<(), Vec<DebugProcess>> for DailyLanguage {
    fn start(&self, _: (), _: Cancel, mut reply: lang_api::Reply<Vec<DebugProcess>>) {
        let processes = self.processes.lock().unwrap().clone();
        let _ = reply.send(self.answer(processes));
    }
}

impl LanguageSupport for DailyLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("daily")
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

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn debug_processes(&self) -> Option<&DebugProcessProvider> {
        Some(self)
    }

    fn inspections(&self) -> Vec<InspectionInfo> {
        vec![InspectionInfo {
            id: "demo.bad".to_owned(),
            title: "Bad value".to_owned(),
            group: "demo".to_owned(),
            default_severity: InspectionLevel::Error,
            configurable: false,
        }]
    }
}

/// Opens a window with the language of the test behind it.
fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<DailyLanguage>,
    setup: ProjectSetup,
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

fn error(line: u32, start: u32, end: u32) -> DiagnosticEntry {
    DiagnosticEntry {
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
        severity: Severity::Error,
        message: "Bad value".to_owned(),
        unnecessary: false,
        code: Some("demo.bad".to_owned()),
    }
}

fn problem(line: usize) -> Problem {
    Problem {
        line,
        column: 0,
        severity: Severity::Error,
        message: "bad value".to_owned(),
    }
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn problems_has_the_tabs_of_the_file_and_the_project_and_follows_the_open_tab(
    cx: &mut TestAppContext,
) {
    let root = sample_project("daily-problems");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    problems.update(cx, |hub, cx| {
        hub.set_file_problems(main.clone(), vec![problem(0)], cx);
        hub.set_file_problems(notes.clone(), vec![problem(0), problem(1)], cx);
    });

    open_path(cx, window, &main);
    press(cx, window, "alt-6");
    draw(cx, window);
    let (tabs, labels) = problems.read_with(cx, |hub, cx| {
        let panel = hub.panel().read(cx);
        let tabs: Vec<String> = panel.tabs().iter().map(|tab| tab.label.clone()).collect();
        let labels: Vec<String> = panel
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        (tabs, labels)
    });
    assert_eq!(
        tabs,
        ["Current File (1)", "Project Errors (3)"],
        "the tabs carry the counts of their lists"
    );
    assert_eq!(
        labels.len(),
        1,
        "the current file tab lists its own problems"
    );

    click(cx, window, "item-tab-project-errors");
    draw(cx, window);
    let (labels, groups) = problems.read_with(cx, |hub, cx| {
        let panel = hub.panel().read(cx);
        let labels: Vec<String> = panel
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        let groups = panel
            .rows()
            .iter()
            .filter(|row| matches!(row, crate::items::PanelRow::Group { .. }))
            .count();
        (labels, groups)
    });
    assert_eq!(labels.len(), 3, "the project tab lists every file");
    assert_eq!(groups, 2, "the project list has a group for each file");

    open_path(cx, window, &notes);
    draw(cx, window);
    click(cx, window, "item-tab-current-file");
    draw(cx, window);
    let (tabs, labels) = problems.read_with(cx, |hub, cx| {
        let panel = hub.panel().read(cx);
        let tabs: Vec<String> = panel.tabs().iter().map(|tab| tab.label.clone()).collect();
        let labels: Vec<String> = panel
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect();
        (tabs, labels)
    });
    assert_eq!(
        tabs[0], "Current File (2)",
        "the tab follows the file that became active"
    );
    assert_eq!(labels.len(), 2, "the list is the one of the new file");
}

#[ui_kit::gpui::test]
fn fix_all_in_file_replaces_three_cases_with_one_undo(cx: &mut TestAppContext) {
    let text = "bad one\nbad two\nbad three\n";
    let root = project("daily-fix-all", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = DailyLanguage::new();
    language.set_diagnostics(vec![error(0, 0, 3), error(1, 0, 3), error(2, 0, 3)]);
    let window = open_language_window(
        cx,
        root.clone(),
        language,
        crate::window_tests::project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for(cx, "the diagnostics of the text", |cx| {
        view.read_with(cx, |view, _| view.diagnostic_marks().len() == 3)
    });
    view.update(cx, |view, cx| view.restore_caret(0, 1, cx));
    draw(cx, window);

    press(cx, window, "alt-enter");
    draw(cx, window);
    let rows = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    let labels: Vec<&str> = rows.rows.iter().map(|(_, label)| label.as_str()).collect();
    assert!(
        labels.contains(&"Fix all 'Bad value' in file"),
        "the quick fix of the check offers the whole file: {labels:?}"
    );

    // The first row is the fix of the place, the second the fix of the whole file.
    press(cx, window, "down enter");
    wait_for(cx, "the three fixes", |cx| {
        crate::window_tests::buffer_of(cx, &view) == "good one\ngood two\ngood three\n"
    });
    press(cx, window, "ctrl-z");
    wait_for(cx, "one undo of the whole file", |cx| {
        crate::window_tests::buffer_of(cx, &view) == text
    });
}

#[ui_kit::gpui::test]
fn the_evaluate_window_shows_the_tree_of_the_value_and_adds_a_watch(cx: &mut TestAppContext) {
    let root = sample_project("daily-evaluate");
    let source = root.join("src").join("main.txt");
    let service = MockService::new();
    let mut setup = crate::window_tests_run::setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    setup.debug = Some(service.clone());
    let window = open_with(cx, &root, setup);
    let debug = debug_panel(cx);

    wait_for(cx, "a run configuration", |cx| {
        !run_panel(cx).read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    press(cx, window, "ctrl-f8");
    wait_for(cx, "the breakpoint", |cx| {
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&source)) == vec![2]
    });
    press(cx, window, "shift-f9");
    wait_for(cx, "the breakpoint stop", |cx| {
        matches!(snapshot(cx).state, dap_client::DebugState::Stopped { .. })
    });

    // The selected text of the editor is the expression the window starts with.
    click(cx, window, "editor-body-0");
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(
        cx,
        window,
        "shift-right shift-right shift-right shift-right shift-right",
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.selected_text())
            .as_deref(),
        Some("alpha"),
        "the test selects a word before Alt+F8"
    );
    press(cx, window, "alt-f8");
    draw(cx, window);
    assert!(
        shown(cx, window, "evaluate-window"),
        "Alt+F8 opens the Evaluate window"
    );
    press(cx, window, "enter");
    wait_for(cx, "the selected expression", |cx| {
        snapshot(cx)
            .evaluation
            .is_some_and(|evaluation| evaluation.expression == "alpha")
    });

    press(cx, window, "ctrl-a");
    type_text(cx, window, "items");
    press(cx, window, "enter");
    wait_for(cx, "the value with fields", |cx| {
        snapshot(cx).evaluation.is_some_and(|evaluation| {
            evaluation.expression == "items" && evaluation.reference == 101
        })
    });
    draw(cx, window);
    assert!(
        shown(cx, window, "evaluate-value"),
        "the value stands in the window"
    );
    wait_for(cx, "the fields of the value", |cx| {
        draw(cx, window);
        shown(cx, window, "evaluate-row-1-0")
    });
    let history = debug.read_with(cx, |panel, _| panel.evaluate_history().to_vec());
    assert_eq!(
        history,
        ["alpha".to_owned(), "items".to_owned()],
        "the window keeps the expressions of the session"
    );

    click(cx, window, "evaluate-add-watch");
    wait_for(cx, "the watch expression", |cx| {
        snapshot(cx)
            .watch
            .iter()
            .any(|watch| watch.expression == "items")
    });

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the session", |cx| {
        snapshot(cx).state == dap_client::DebugState::Terminated
    });
    service.join();
}

/// A debug service that only records the address it was asked to attach to.
struct AttachRecorder {
    attached: Mutex<Vec<(String, u16)>>,
}

impl AttachRecorder {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            attached: Mutex::new(Vec::new()),
        })
    }

    fn endpoints(&self) -> Vec<(String, u16)> {
        self.attached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

impl DebugService for AttachRecorder {
    fn start(
        &self,
        _program: ProcessSpec,
        _name: String,
        _entry: Option<PathBuf>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        Err("this adapter only attaches".to_owned())
    }

    fn attach(
        &self,
        host: String,
        port: u16,
        _name: String,
        _project: Option<String>,
        _breakpoints: Vec<BreakpointSpec>,
        _watch: Vec<String>,
        _exception_filters: Vec<String>,
    ) -> Result<Debugger, String> {
        self.attached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push((host, port));
        Err("the test needs no session".to_owned())
    }
}

#[ui_kit::gpui::test]
fn attach_to_process_lists_the_running_jvms_and_attaches_to_the_chosen_one(
    cx: &mut TestAppContext,
) {
    let root = sample_project("daily-attach");
    let language = DailyLanguage::new();
    language.set_processes(vec![DebugProcess {
        pid: 4242,
        name: "demo.Main".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: 5005,
    }]);
    let recorder = AttachRecorder::new();
    let mut setup = crate::window_tests::project_setup(scratch("cache"));
    setup.debug = Some(recorder.clone());
    let window = open_language_window(cx, root, language, setup);
    cx.run_until_parked();
    draw(cx, window);

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("debug.attach_jvm", window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the list of processes", |cx| {
        draw(cx, window);
        shown(cx, window, "attach-process-0")
    });
    assert!(
        bounds_of(cx, window, "attach-process-manual").size.height > px(0.),
        "a typed host:port stays the last row"
    );

    press(cx, window, "enter");
    wait_for(cx, "the attachment", |_| !recorder.endpoints().is_empty());
    assert_eq!(
        recorder.endpoints(),
        vec![("127.0.0.1".to_owned(), 5005)],
        "the chosen process gives the address of the attachment"
    );
}
