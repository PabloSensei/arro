//! The first failure of a fresh server cache is repeated instead of shown: the window
//! asks the server for another import and keeps the notification away. A second such
//! failure is a real error and reaches the user.
//!
//! The language is a stand-in: it answers the project problems from memory, counts the
//! imports the window asks for and sends the status lines the window reads.

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
use ui_kit::gpui::TestAppContext;
use ui_kit::gpui::WindowHandle;
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::problems::ActiveProblems;
use crate::window_tests::{draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("import-retry");

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

/// A language whose server repeats a fresh-cache import failure on request.
struct RetryLanguage {
    syntax: NoSyntax,
    problems: Arc<Mutex<Vec<ProjectProblem>>>,
    /// The list is sent whenever this number moves.
    version: Arc<AtomicU64>,
    status: futures_channel::mpsc::UnboundedSender<String>,
    reimports: AtomicUsize,
}

impl RetryLanguage {
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

impl Provider<ProjectProblemsQuery, Vec<ProjectProblem>> for RetryLanguage {
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

impl SemanticServer for RetryLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}

    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<lang_api::DocumentEdit>) {}

    fn save(&self, _: &str) {}

    fn close(&self, _: &str) {}

    fn reimport(&self) {
        self.reimports.fetch_add(1, Ordering::SeqCst);
        self.status
            .unbounded_send("Fixture: importing".to_owned())
            .ok();
    }

    fn version(&self, _: &str) -> Option<i32> {
        None
    }
}

impl LanguageSupport for RetryLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("import-retry")
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
    language: Arc<RetryLanguage>,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("import-retry-state");
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
            project_setup(scratch("import-retry-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// The failure of a fresh OSGi cache: the server names its initialization script before
/// the cache unpacks it. `retry` marks the first such failure of the server session.
fn fresh_cache(root: &Path, log: PathBuf, retry: bool) -> ProjectProblem {
    ProjectProblem {
        file: Some(root.join("build.gradle")),
        line: 0,
        severity: Severity::Error,
        message: "Could not run build action.\nCaused by: The specified initialization script \
                  'C:\\state\\server\\demo\\config\\org.eclipse.osgi\\57\\0\\.cp\\gradle\\init\\init.gradle' \
                  does not exist."
            .to_owned(),
        summary: "The specified initialization script \
                  'C:\\state\\server\\demo\\config\\org.eclipse.osgi\\57\\0\\.cp\\gradle\\init\\init.gradle' \
                  does not exist"
            .to_owned(),
        log: Some(log),
        tool: Some(1),
        retry,
    }
}

#[ui_kit::gpui::test]
fn the_first_fresh_cache_failure_is_repeated_and_the_second_is_shown(cx: &mut TestAppContext) {
    let root = sample_project("import-retry");
    let log = root.join("server.log");
    std::fs::write(&log, "the server log\n").unwrap();
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let language = RetryLanguage::new(sender.clone());
    let window = window_with_language(cx, root.clone(), language.clone(), statuses);
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());

    // The server names the script of its fresh cache: the window imports again instead
    // of telling the user about a failure that the next import clears.
    language.set_problems(vec![fresh_cache(&root, log.clone(), true)]);
    wait_for_signal(cx, "the repeated import", |_| language.reimports() == 1);
    cx.run_until_parked();
    assert_eq!(
        notification(cx, window),
        None,
        "the first fresh-cache failure is not shown"
    );
    assert_eq!(
        status(cx, window).as_deref(),
        Some("Fixture: importing"),
        "the status bar says the import runs"
    );
    assert!(
        problems.read_with(cx, |hub, _| hub.state().items(&root).is_empty()),
        "the repeated failure is not a row in Problems"
    );

    // The repeat finds the script still missing: this time it is a real error.
    language.set_problems(vec![fresh_cache(&root, log.clone(), false)]);
    sender
        .unbounded_send(
            "Fixture: import failed: The specified initialization script does not exist".to_owned(),
        )
        .unwrap();
    wait_for_signal(cx, "the notification of the second failure", |cx| {
        notification(cx, window).is_some_and(|(_, text)| text.contains("initialization script"))
    });
    assert_eq!(
        language.reimports(),
        1,
        "the window repeats the import once, not in a loop"
    );
    assert!(
        problems.read_with(cx, |hub, _| {
            hub.state()
                .items(&root)
                .iter()
                .any(|item| item.label.contains("initialization script"))
        }),
        "the second failure is a row in Problems"
    );
    draw(cx, window);
}
