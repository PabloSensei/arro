//! Reformat Code of a folder selected in the project tree, on a language that answers
//! from memory. The language of this file is shared with the scope tests of Inspect Code.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ::base::Cancel;
use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DiagnosticEntry, DiagnosticNote, DiagnosticsProvider, DiagnosticsQuery,
    DocumentEdit, FileDiagnostics, FormatProvider, FormatQuery, LanguageFn, LanguageId,
    LanguageSupport, Provider, Queries, SemanticServer, Severity, SourceId, SyntaxProvider,
    TextPoint, TextSpan, WorkspaceDiagnosticsProvider, WorkspaceDiagnosticsQuery,
};
use platform::CanonPath;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, INSPECTION_TOOL, Launch, Layout, PROJECT_TOOL, Settings, Workspace};

use crate::index::SearchSetup;
use crate::project::ActiveProjectPanel;
use crate::run::ProjectSetup;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, press, project_setup, scratch, wait_for_signal,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("scope-server");

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

/// The language of the reformat and inspection tests: formatting uppercases the whole
/// text of a document, diagnostics come from the notes the test sets.
pub(crate) struct ScopeLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    notes: Mutex<HashMap<String, Vec<DiagnosticNote>>>,
}

impl ScopeLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            notes: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn set_note(&self, path: &Path, note: DiagnosticNote) {
        self.notes
            .lock()
            .unwrap()
            .entry(uri_of(path))
            .or_default()
            .push(note);
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }

    fn text_of(&self, uri: &str) -> String {
        if let Some(text) = self.documents.lock().unwrap().get(uri) {
            return text.to_string();
        }
        platform::path_from_uri(uri)
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default()
    }
}

pub(crate) fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

impl SemanticServer for ScopeLanguage {
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

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for ScopeLanguage {
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
            .get(&query.0)
            .map(|notes| {
                notes
                    .iter()
                    .map(|note| DiagnosticEntry {
                        span: note.span,
                        severity: note.severity,
                        message: note.message.clone(),
                        unnecessary: false,
                        code: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<WorkspaceDiagnosticsQuery, Vec<FileDiagnostics>> for ScopeLanguage {
    fn start(
        &self,
        _: WorkspaceDiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<FileDiagnostics>>,
    ) {
        let files = self
            .notes
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

impl Provider<FormatQuery, Vec<DocumentEdit>> for ScopeLanguage {
    fn start(&self, query: FormatQuery, _: Cancel, mut reply: lang_api::Reply<Vec<DocumentEdit>>) {
        let edit = DocumentEdit {
            span: TextSpan {
                start: TextPoint::default(),
                end: TextPoint {
                    line: 1_000_000,
                    character: 0,
                },
            },
            text: self.text_of(&query.uri).to_uppercase(),
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl LanguageSupport for ScopeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("scope")
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
}

pub(crate) fn note(line: u32, severity: Severity, message: &str) -> DiagnosticNote {
    DiagnosticNote {
        span: TextSpan {
            start: TextPoint { line, character: 0 },
            end: TextPoint { line, character: 4 },
        },
        severity,
        message: message.to_owned(),
        code: None,
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

pub(crate) fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<ScopeLanguage>,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("scope-state");
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

pub(crate) fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

pub(crate) fn project_panel(cx: &TestAppContext) -> gpui::Entity<crate::project::ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

pub(crate) fn focus_project(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<crate::project::ProjectPanel>,
) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

pub(crate) fn show_inspection(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(INSPECTION_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn reformat_of_a_folder_formats_both_files_and_one_undo_takes_both_back(cx: &mut TestAppContext) {
    let root = project(
        "reformat-folder",
        &[("src/First.txt", "alpha\n"), ("src/Second.txt", "beta\n")],
    );
    let src = root.join("src");
    let first = src.join("First.txt");
    let second = src.join("Second.txt");
    let language = ScopeLanguage::new();
    let window = open_window_with(
        cx,
        root.clone(),
        language,
        project_setup(scratch("reformat-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &first);
    open_path(cx, window, &second);
    let first_view = editor_of(cx, window, &first);
    let second_view = editor_of(cx, window, &second);

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&src, cx));
    focus_project(cx, window, &panel);

    press(cx, window, "ctrl-alt-l");
    draw(cx, window);
    assert!(
        visible(cx, window, "reformat-dialog"),
        "Ctrl+Alt+L in the tree opens the dialog"
    );
    assert!(
        visible(cx, window, "reformat-optimize"),
        "the flags are there"
    );
    assert!(
        visible(cx, window, "reformat-only-changed"),
        "Only changed files is offered"
    );
    assert!(visible(cx, window, "reformat-cancel"));
    click(cx, window, "reformat-run");
    wait_for_signal(cx, "the two formatted files", |cx| {
        buffer_of(cx, &first_view) == "ALPHA\n" && buffer_of(cx, &second_view) == "BETA\n"
    });

    open_path(cx, window, &first);
    press(cx, window, "ctrl-z");
    wait_for_signal(cx, "one undo of both files", |cx| {
        buffer_of(cx, &first_view) == "alpha\n" && buffer_of(cx, &second_view) == "beta\n"
    });
}

#[ui_kit::gpui::test]
fn reformat_without_a_language_server_says_so(cx: &mut TestAppContext) {
    let root = project(
        "reformat-no-server",
        &[("src/First.txt", "alpha\n"), ("src/Second.txt", "beta\n")],
    );
    let src = root.join("src");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&src, cx));
    focus_project(cx, window, &panel);

    press(cx, window, "ctrl-alt-l");
    draw(cx, window);
    assert!(
        visible(cx, window, "reformat-dialog"),
        "the dialog opens without a server"
    );
    click(cx, window, "reformat-run");
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the window says why nothing happened");
    assert!(
        text.to_lowercase().contains("language server"),
        "{title}: {text}"
    );
}
