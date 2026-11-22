//! The retry of a failed import is counted per import attempt: Buildship reports the
//! same refusal for both Gradle operations of one import, and neither report reaches the
//! user. The notification of a failure that stays closes itself when the import ends
//! well.
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
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::problems::ActiveProblems;
use crate::window_tests::{draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("import-retry-attempt");

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

/// A language whose server reports the refusals of one import as separate problems.
struct RetryLanguage {
    syntax: NoSyntax,
    problems: Arc<Mutex<Vec<ProjectProblem>>>,
    /// The list is sent whenever this number moves.
    version: Arc<AtomicU64>,
    /// How many lists the provider has answered, so a test waits for the update itself.
    answers: Arc<AtomicUsize>,
    status: futures_channel::mpsc::UnboundedSender<String>,
    reimports: AtomicUsize,
}

impl RetryLanguage {
    fn new(status: futures_channel::mpsc::UnboundedSender<String>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            problems: Arc::default(),
            version: Arc::new(AtomicU64::new(0)),
            answers: Arc::default(),
            status,
            reimports: AtomicUsize::new(0),
        })
    }

    fn set_problems(&self, problems: Vec<ProjectProblem>) {
        *self.problems.lock().unwrap() = problems;
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    fn answers(&self) -> usize {
        self.answers.load(Ordering::SeqCst)
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
        let answers = self.answers.clone();
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
                    answers.fetch_add(1, Ordering::SeqCst);
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
        LanguageId("import-retry-attempt")
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

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The window with the stand-in language and the status stream the test writes.
fn window_with_language(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<RetryLanguage>,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("import-retry-attempt-state");
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
            project_setup(scratch("import-retry-attempt-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// A refusal of a fresh OSGi cache: the server names its initialization script before
/// the cache unpacks it. `retry` marks a refusal of the attempt that is repeated.
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

/// A real failure: the repeat import could not clear it, or the first refusal was of
/// another kind.
fn failure(root: &Path, log: PathBuf) -> ProjectProblem {
    ProjectProblem {
        file: Some(root.join("build.gradle")),
        line: 0,
        severity: Severity::Error,
        message: "Could not run build action.\nCaused by: Task with name 'postRelease' not \
                  found in root project 'demo'."
            .to_owned(),
        summary: "Task with name 'postRelease' not found in root project 'demo'.".to_owned(),
        log: Some(log),
        tool: Some(1),
        retry: false,
    }
}

#[ui_kit::gpui::test]
fn two_refusals_of_one_import_are_repeated_once_and_not_shown(cx: &mut TestAppContext) {
    let root = sample_project("import-retry-attempt-both");
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

    // The first Gradle operation of the import names the script of the fresh cache: the
    // window imports again instead of telling the user about a failure that goes away.
    language.set_problems(vec![fresh_cache(&root, log.clone(), true)]);
    wait_for_signal(cx, "the repeated import", |_| language.reimports() == 1);
    wait_for_signal(cx, "the first refusal", |_| language.answers() >= 2);
    cx.run_until_parked();
    assert_eq!(
        notification(cx, window),
        None,
        "the first refusal is not shown"
    );
    assert_eq!(
        status(cx, window).as_deref(),
        Some("Fixture: importing"),
        "the status bar says the import runs"
    );

    // The second Gradle operation of the same import refuses for the same reason. It is
    // the same repeat, not a real error.
    language.set_problems(vec![fresh_cache(&root, log.clone(), true)]);
    wait_for_signal(cx, "the second refusal", |_| language.answers() >= 3);
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        language.reimports(),
        1,
        "one repeat for one import, not a loop"
    );
    assert_eq!(
        notification(cx, window),
        None,
        "the second refusal is not shown either"
    );
    assert!(
        !has_selector(cx, window, "notification"),
        "no notification is on screen"
    );
    assert!(
        problems.read_with(cx, |hub, _| hub.state().items(&root).is_empty()),
        "the refusals are not rows in Problems"
    );
}

#[ui_kit::gpui::test]
fn the_import_failure_notification_closes_when_the_import_succeeds(cx: &mut TestAppContext) {
    let root = sample_project("import-retry-attempt-closes");
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

    // A failure that the repeat cannot clear reaches the user.
    language.set_problems(vec![failure(&root, log)]);
    sender
        .unbounded_send(
            "Fixture: import failed: Task with name 'postRelease' not found in root project 'demo'."
                .to_owned(),
        )
        .unwrap();
    wait_for_signal(cx, "the failed import notification", |cx| {
        notification(cx, window).is_some_and(|(_, text)| text.contains("postRelease"))
    });
    wait_for_signal(cx, "the failed import row", |cx| {
        problems.read_with(cx, |hub, _| {
            hub.state()
                .items(&root)
                .iter()
                .any(|item| item.label.contains("postRelease"))
        })
    });
    draw(cx, window);
    assert!(
        has_selector(cx, window, "notification"),
        "the failure is on screen"
    );

    // The user reloads the project and the server imports it: the notification and the
    // row leave together with the failure.
    language.set_problems(Vec::new());
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line after the import", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });
    wait_for_signal(cx, "the empty problem list", |_| language.answers() >= 3);
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "notification"),
        "the notification closed itself"
    );
    assert!(
        problems.read_with(cx, |hub, _| hub.state().items(&root).is_empty()),
        "the row left Problems"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.language_action())
            .unwrap()
            .is_none(),
        "a line that is ready again no longer opens the failure"
    );
}
