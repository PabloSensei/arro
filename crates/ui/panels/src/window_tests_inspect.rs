//! Inspect Code, Run Inspection by Name, the Reformat File dialog and Code Cleanup in
//! the window, on a language that answers from memory. The ignored tests at the end run
//! the same paths on a copy of PetClinic with the real server.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base::Cancel;
use editor_view::Languages;
use keymap::workspace as keys;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DiagnosticEntry,
    DiagnosticNote, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit, FileDiagnostics,
    FormatProvider, FormatQuery, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    RangeFormatProvider, RangeFormatQuery, SemanticServer, Severity, SourceEdit, SourceId,
    SyntaxProvider, TextPoint, TextSpan, WorkspaceDiagnosticsProvider, WorkspaceDiagnosticsQuery,
};
use platform::CanonPath;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, INSPECTION_TOOL, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::inspect::{ActiveInspect, Finding, InspectHub, RowKind};
use crate::run::ProjectSetup;
use crate::window_tests::{
    active_path, buffer_of, click, draw, editor_of, git_repository, open_path, press,
    project_setup, scratch, type_text,
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

/// The language of these tests: diagnostics with codes, a formatter of two modes and
/// the actions the test sets.
struct TestLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    notes: Mutex<Vec<(String, DiagnosticNote)>>,
    actions: Mutex<Vec<ActionEntry>>,
    whole: Mutex<String>,
    range: Mutex<String>,
}

impl TestLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            notes: Mutex::new(Vec::new()),
            actions: Mutex::new(Vec::new()),
            whole: Mutex::new("// whole\n".to_owned()),
            range: Mutex::new("// range\n".to_owned()),
        })
    }

    fn set_note(&self, path: &Path, note: DiagnosticNote) {
        self.notes.lock().unwrap().push((uri_of(path), note));
    }

    fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

impl SemanticServer for TestLanguage {
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

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for TestLanguage {
    fn start(
        &self,
        query: DiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self
            .notes
            .lock()
            .unwrap()
            .iter()
            .filter(|(uri, _)| *uri == query.0)
            .map(|(_, note)| DiagnosticEntry {
                span: note.span,
                severity: note.severity,
                message: note.message.clone(),
                unnecessary: false,
            })
            .collect();
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
        let mut grouped: HashMap<String, Vec<DiagnosticNote>> = HashMap::new();
        for (uri, note) in self.notes.lock().unwrap().iter() {
            grouped.entry(uri.clone()).or_default().push(note.clone());
        }
        let files = grouped
            .into_iter()
            .map(|(uri, diagnostics)| FileDiagnostics { uri, diagnostics })
            .collect();
        let _ = reply.send(self.answer(files));
    }
}

impl Provider<FormatQuery, Vec<DocumentEdit>> for TestLanguage {
    fn start(&self, _: FormatQuery, _: Cancel, mut reply: lang_api::Reply<Vec<DocumentEdit>>) {
        let text = self.whole.lock().unwrap().clone();
        let edit = DocumentEdit {
            span: TextSpan {
                start: TextPoint::default(),
                end: TextPoint {
                    line: 1_000_000,
                    character: 0,
                },
            },
            text,
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl Provider<RangeFormatQuery, Vec<DocumentEdit>> for TestLanguage {
    fn start(
        &self,
        query: RangeFormatQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DocumentEdit>>,
    ) {
        let edit = DocumentEdit {
            span: query.range,
            text: self.range.lock().unwrap().clone(),
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for TestLanguage {
    fn start(&self, query: ActionQuery, _: Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
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

    fn range_format(&self) -> Option<&RangeFormatProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
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

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<TestLanguage>,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("inspect-state");
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

fn rows(cx: &TestAppContext) -> Vec<RowKind> {
    let hub = hub(cx);
    hub.read_with(cx, |hub, cx| {
        hub.panel()
            .read(cx)
            .rows()
            .iter()
            .map(|row| row.kind)
            .collect()
    })
}

fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn inspect_code_lists_the_problems_of_the_current_file_and_opens_one(cx: &mut TestAppContext) {
    let root = project("inspect-current", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
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
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);

    press(cx, window, "alt-shift-i");
    draw(cx, window);
    assert!(
        visible(cx, window, "inspect-scope-popup"),
        "Alt+Shift+I asks for the scope"
    );
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(INSPECTION_TOOL))
            .unwrap(),
        "the results open in their window"
    );
    assert_eq!(findings(cx).len(), 1);
    assert_eq!(findings(cx)[0].code.as_deref(), Some("unused"));
    let rows = rows(cx);
    assert!(rows.contains(&RowKind::Severity), "{rows:?}");
    assert!(rows.contains(&RowKind::Kind), "{rows:?}");
    assert!(rows.contains(&RowKind::File), "{rows:?}");

    press(cx, window, "down down down");
    draw(cx, window);
    press(cx, window, "f4");
    assert_eq!(active_path(cx, window), Some(path.clone()));
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 0));
}

#[ui_kit::gpui::test]
fn quick_fixes_of_a_node_apply_to_the_selection(cx: &mut TestAppContext) {
    let root = project("inspect-fixes", &[("src/Main.txt", "value\n")]);
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
    press(cx, window, "alt-shift-i");
    draw(cx, window);
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "down down down");
    draw(cx, window);
    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(visible(cx, window, "menu-0"), "Alt+Enter lists the fixes");
    click(cx, window, "menu-0");
    cx.run_until_parked();
    assert_eq!(buffer_of(cx, &view), "fixed\n");
}

#[ui_kit::gpui::test]
fn run_inspection_by_name_lists_the_known_codes_and_runs_one(cx: &mut TestAppContext) {
    let root = project(
        "inspect-by-name",
        &[("src/Main.txt", "value\n"), ("src/Other.txt", "other\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = TestLanguage::new();
    language.set_note(
        &main,
        note(
            0,
            0,
            5,
            Severity::Error,
            "The value is never used",
            "unused",
        ),
    );
    language.set_note(
        &other,
        note(
            0,
            0,
            5,
            Severity::Warning,
            "The name is never used",
            "unused",
        ),
    );
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    press(cx, window, "alt-shift-i");
    draw(cx, window);
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(findings(cx).len(), 1, "the current file has one problem");

    press(cx, window, "ctrl-alt-shift-i");
    draw(cx, window);
    assert!(
        visible(cx, window, "run-inspection"),
        "the list of inspections opens"
    );
    type_text(cx, window, "unused");
    draw(cx, window);
    assert!(visible(cx, window, "run-inspection-row-0"));
    press(cx, window, "enter");
    draw(cx, window);
    assert!(visible(cx, window, "inspect-scope-popup"));
    click(cx, window, "inspect-scope-0");
    cx.run_until_parked();
    draw(cx, window);
    let all = findings(cx);
    assert_eq!(all.len(), 2, "the whole project has both problems");
    assert!(
        all.iter()
            .all(|finding| finding.code.as_deref() == Some("unused"))
    );
}

#[ui_kit::gpui::test]
fn the_reformat_dialog_remembers_its_flags_and_formats_only_changed_lines(cx: &mut TestAppContext) {
    let root = git_repository("inspect-reformat");
    let path = root.join("main.txt");
    let language = TestLanguage::new();
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.replace_text(6..10, "BETA", cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    assert!(visible(cx, window, "reformat-dialog"), "the dialog opens");
    click(cx, window, "reformat-only-changed");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    assert_eq!(buffer_of(cx, &view), "alpha\n// range\n");
    assert!(
        cx.read_global::<Settings, _>(|settings, _| settings.reformat_only_changed),
        "the flag is remembered"
    );

    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "alpha\nBETA\n");

    // Ctrl+Alt+L uses the remembered flags without the dialog.
    press(cx, window, "ctrl-alt-l");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "alpha\n// range\n",
        "Ctrl+Alt+L formats only the changed line"
    );
    press(cx, window, "ctrl-z");

    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    click(cx, window, "reformat-only-changed");
    click(cx, window, "reformat-whole-file");
    draw(cx, window);
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &view),
        "// whole\n",
        "the whole file is formatted"
    );
}

#[ui_kit::gpui::test]
fn code_cleanup_applies_the_safe_fixes_in_one_undo_step(cx: &mut TestAppContext) {
    let root = project("inspect-cleanup", &[("src/Main.txt", "bad\n")]);
    let path = root.join("src").join("Main.txt");
    let language = TestLanguage::new();
    let uri = uri_of(&path);
    language.set_actions(vec![action(
        "Fix the file",
        ActionKind::FixAll,
        "source.fixAll",
        &uri,
        0,
        3,
        "good",
    )]);
    let window = open_window_with(cx, root.clone(), language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keys::CODE_CLEANUP, window, cx)
        })
        .unwrap();
    assert!(ran, "Code Cleanup is a command");
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(buffer_of(cx, &view), "good\n");

    click(cx, window, "editor-body-0");
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "bad\n",
        "one step takes the whole cleanup back"
    );
}

// The tests below need `IDE_CORPUS` and a filled package cache:
//
// `cargo test -p panels window_tests_inspect -- --ignored --test-threads=1`

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
        cache_root: scratch("inspect-real-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn inspect_code_by_module_finds_an_error_in_a_closed_file(cx: &mut TestAppContext) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("inspect-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    let broken = window_language::write_broken_sample(&root);
    let language = window_language::RealLanguage::open(root.clone(), scratch("inspect-real-state"))
        .expect("the language server starts");
    let setup = module_setup(&root);
    let window = crate::window_tests_real::build_window(cx, root.clone(), &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    crate::window_tests_real::wait_for(
        cx,
        "the module of the project",
        Duration::from_secs(60),
        |cx| {
            let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
            run.read_with(cx, |run, _| !run.modules().is_empty())
        },
    );

    press(cx, window, "alt-shift-i");
    draw(cx, window);
    click(cx, window, "inspect-scope-1");
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_for(
        cx,
        "the error of the closed file",
        Duration::from_secs(300),
        |cx| {
            findings(cx)
                .iter()
                .any(|finding| finding.path == broken && finding.severity == Severity::Error)
        },
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(INSPECTION_TOOL))
            .unwrap(),
        "the results window is open"
    );

    crate::window_tests_real::close(cx, window, language, false);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn reformat_only_changes_the_lines_of_the_last_commit(cx: &mut TestAppContext) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("reformat-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    let sample = window_language::write_vcs_format_sample(&root);
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    let language =
        window_language::RealLanguage::open(root.clone(), scratch("reformat-real-state"))
            .expect("the language server starts");
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    let before = buffer_of(cx, &view);
    let changed = before.replacen("return  1;", "return   1;", 1);
    view.update(cx, |view, cx| {
        let all = 0..before.len();
        view.replace_text(all, &changed, cx);
    });
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    click(cx, window, "reformat-only-changed");
    draw(cx, window);
    press(cx, window, "enter");
    crate::window_tests_real::wait_for(cx, "the formatting", Duration::from_secs(120), |cx| {
        buffer_of(cx, &view) != changed
    });
    let after = buffer_of(cx, &view);
    assert!(
        !after.contains("return   1;"),
        "the changed line is formatted: {after}"
    );
    assert!(
        after.contains("public int second(){return  2;}"),
        "the untouched method keeps its formatting: {after}"
    );

    crate::window_tests_real::close(cx, window, language, false);
}
