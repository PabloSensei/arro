//! A failed import of the project reaches the window: the notification names the
//! reason and offers the Settings page, another import and the server log, and the
//! status line says `import failed` instead of `ready`.
//!
//! The language is a stand-in: it answers the project problems from memory and sends
//! the status lines the window reads.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base::Cancel;
use editor_view::Languages;
use lang_api::{
    Answer, Confidence, LanguageFn, LanguageId, LanguageSupport, ProjectProblem,
    ProjectProblemsProvider, ProjectProblemsQuery, Provider, Queries, Reply, SemanticServer,
    Severity, SourceId, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{click, draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("import-failure");

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

/// A language whose server fails to import the project until the test reloads it.
struct ImportLanguage {
    syntax: NoSyntax,
    problems: Arc<Mutex<Vec<ProjectProblem>>>,
    /// The list is sent whenever this number moves.
    version: Arc<AtomicU64>,
    status: futures_channel::mpsc::UnboundedSender<String>,
    reimports: AtomicUsize,
}

impl ImportLanguage {
    fn new(status: futures_channel::mpsc::UnboundedSender<String>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            problems: Arc::default(),
            version: Arc::new(AtomicU64::new(0)),
            status,
            reimports: AtomicUsize::new(0),
        })
    }

    fn set_problems(&self, problems: Vec<ProjectProblem>) {
        *self.problems.lock().unwrap() = problems;
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    fn reimports(&self) -> usize {
        self.reimports.load(Ordering::SeqCst)
    }
}

impl Provider<ProjectProblemsQuery, Vec<ProjectProblem>> for ImportLanguage {
    fn start(
        &self,
        _: ProjectProblemsQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<ProjectProblem>>,
    ) {
        let mut seen = u64::MAX;
        let problems = self.problems.clone();
        let version = self.version.clone();
        thread::spawn(move || {
            loop {
                if cancel.is_cancelled() {
                    return;
                }
                let current = version.load(Ordering::SeqCst);
                if current != seen {
                    seen = current;
                    let list = problems.lock().unwrap().clone();
                    let answer = Answer::new(list, Confidence::Certain, SOURCE);
                    if reply.send(answer).is_err() && reply.is_cancelled() {
                        return;
                    }
                }
                thread::sleep(Duration::from_millis(5));
            }
        });
    }
}

impl SemanticServer for ImportLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}

    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<lang_api::DocumentEdit>) {}

    fn save(&self, _: &str) {}

    fn close(&self, _: &str) {}

    fn reimport(&self) {
        self.reimports.fetch_add(1, Ordering::SeqCst);
        self.set_problems(Vec::new());
        self.status.unbounded_send("Fixture: ready".to_owned()).ok();
    }

    fn version(&self, _: &str) -> Option<i32> {
        None
    }
}

impl LanguageSupport for ImportLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("import")
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

    fn project_problems(&self) -> Option<&ProjectProblemsProvider> {
        Some(self as &ProjectProblemsProvider)
    }
}

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

fn notification(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<(String, String)> {
    window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(title, text)| (title.to_string(), text.to_string()))
}

/// The window with the stand-in language and the status stream the test writes.
fn window_with_language(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<ImportLanguage>,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("import-failure-state");
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
            project_setup(scratch("import-failure-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn failure(root: &Path, log: PathBuf) -> ProjectProblem {
    ProjectProblem {
        file: Some(root.join("build.gradle")),
        line: 0,
        severity: Severity::Error,
        message:
            "Synchronize project demo failed.\nCaused by: Task with name 'postRelease' not found."
                .to_owned(),
        summary: "Task with name 'postRelease' not found.".to_owned(),
        log: Some(log),
        tool: Some(1),
        retry: false,
    }
}

#[ui_kit::gpui::test]
fn a_failed_import_names_the_reason_and_reload_says_ready_again(cx: &mut TestAppContext) {
    let root = sample_project("import-failure");
    let log = root.join("server.log");
    std::fs::write(&log, "the server log\n").unwrap();
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let language = ImportLanguage::new(sender.clone());
    let window = window_with_language(cx, root.clone(), language.clone(), statuses);
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    // The server reports that the import of the project failed.
    language.set_problems(vec![failure(&root, log.clone())]);
    sender
        .unbounded_send(
            "Fixture: import failed: Task with name 'postRelease' not found.".to_owned(),
        )
        .unwrap();
    wait_for_signal(cx, "the failed import notification", |cx| {
        notification(cx, window).is_some_and(|(_, text)| text.contains("postRelease"))
    });

    let (title, text) = notification(cx, window).expect("the import failure is a notification");
    assert_eq!(title, "Language Server");
    assert!(text.starts_with("import failed: "), "{text}");
    assert_eq!(
        status(cx, window).as_deref(),
        Some("Fixture: import failed: Task with name 'postRelease' not found.")
    );
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(buttons, ["Open Settings", "Reload Project", "Show Log"]);
    let (action, path) = window
        .read_with(cx, |workspace, _| workspace.last_notification_action())
        .unwrap()
        .expect("the notification opens the server log");
    assert_eq!(action, "Show Log");
    assert_eq!(path, log);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.language_action())
            .unwrap()
            == Some("language.show_import_failure")
    );

    // Another notification may replace it. A click on the status line brings the
    // failure back.
    window
        .update(cx, |workspace, _, cx| {
            workspace.notify_user("Something Else", "nothing to do with the import", cx)
        })
        .unwrap();
    draw(cx, window);
    click(cx, window, "st-language");
    wait_for_signal(
        cx,
        "the failure again after a click on the status line",
        |cx| notification(cx, window).is_some_and(|(_, text)| text.contains("postRelease")),
    );

    // Reload Project asks the server for another import, which ends well.
    draw(cx, window);
    click(cx, window, "notification-action-reload");
    wait_for_signal(cx, "the server to import again", |_| {
        language.reimports() == 1
    });
    wait_for_signal(cx, "the ready line after the import", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.language_action())
            .unwrap()
            .is_none(),
        "a line that is ready again no longer opens the failure"
    );
}
