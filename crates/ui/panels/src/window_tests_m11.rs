//! The M11 finish: a new or removed build file reaches the language, Build Project
//! beside a debug test run, Match Brace from the middle of a pair, Shift+F2 and Forward,
//! and the autostash note in the status bar.
//!
//! cargo test -p panels window_tests_m11

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::{BreakpointSpec, DebugService, Debugger, Launch, ReloadOutcome};
use editor_view::Languages;
use lang_api::{
    DiagnosticEntry, FileChange, LanguageFn, LanguageId, LanguageSupport, Queries, SemanticServer,
    Severity, SourceSpan, SyntaxProvider, TextPoint, TextSpan,
};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel, Unit};
use runner::{RunConfig, RunConfigProvider, RunPlan, TestListen, TestProvider, TestRequest};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{
    DockState, FileIcons, Launch as WindowLaunch, Layout, RUN_TOOL, Settings, Workspace,
};

use crate::index::SearchSetup;
use crate::run::ProjectSetup;
use crate::usages::{self, ActiveShowUsages};
use crate::window_tests::{
    draw, editor_of, open_path, open_window, press, project_setup, sample_project, scratch,
};
use crate::window_tests_git::{
    conflicting_repository, git_state, has_selector, wait_for_repository,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in};
#[cfg(windows)]
use crate::window_tests_run::{open_with, run_panel, snapshot, wait_for as wait_run};
use crate::{AgentOptions, LanguageServer, connect, install};

const LIMIT: Duration = Duration::from_secs(15);
const TICK: Duration = Duration::from_millis(50);

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
}

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

/// A language that watches build scripts and keeps the changes it was told about.
struct WatchLanguage {
    syntax: NoSyntax,
    watched: Mutex<Vec<(String, FileChange)>>,
}

impl WatchLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            watched: Mutex::new(Vec::new()),
        })
    }

    fn watched(&self) -> Vec<(String, FileChange)> {
        self.watched.lock().unwrap().clone()
    }
}

impl SemanticServer for WatchLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}
    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<lang_api::DocumentEdit>) {}
    fn save(&self, _: &str) {}
    fn close(&self, _: &str) {}

    fn file_changed(&self, uri: &str, change: FileChange) {
        self.watched.lock().unwrap().push((uri.to_owned(), change));
    }

    fn version(&self, _: &str) -> Option<i32> {
        None
    }
}

impl LanguageSupport for WatchLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("watch")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn watches_file(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cfg"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }
}

/// The window of the fake language of this file. `window_tests_lang` opens one for its
/// own type only.
fn open_watch_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<WatchLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("m11-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(ui_kit::Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = WindowLaunch {
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
    let setup = project_setup(scratch("m11-cache"));
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

#[ui_kit::gpui::test]
fn a_new_and_a_removed_build_file_reach_the_language(cx: &mut TestAppContext) {
    let root = scratch("m11-watch");
    let old = root.join("old.cfg");
    let new = root.join("module.cfg");
    fs::write(&old, "old\n").unwrap();
    fs::write(root.join("Main.txt"), "main\n").unwrap();
    let language = WatchLanguage::new();
    let _window = open_watch_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    // The index starts its watcher before its scan; an event made right after the window
    // opens still arrives.
    std::thread::sleep(Duration::from_millis(300));

    fs::write(&new, "module\n").unwrap();
    wait_for(cx, "the created build file", |_| {
        language
            .watched()
            .iter()
            .any(|(uri, change)| *change == FileChange::Created && uri.ends_with("module.cfg"))
    });

    fs::remove_file(&old).unwrap();
    wait_for(cx, "the removed build file", |_| {
        language
            .watched()
            .iter()
            .any(|(uri, change)| *change == FileChange::Removed && uri.ends_with("old.cfg"))
    });
}

/// A build tool that records whether Build Project asked for the test classes too.
struct BuildProbe {
    model: ProjectModel,
    builds: Arc<Mutex<Vec<bool>>>,
}

impl BuildSystem for BuildProbe {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn build(&self, _: &Path, tests: bool) -> base::Result<ProcessSpec> {
        self.builds.lock().unwrap().push(tests);
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// Tests whose JVM announces a debug port and then keeps running.
struct ProbeTests;

impl TestProvider for ProbeTests {
    fn plan(
        &self,
        _: &ProjectModel,
        _: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        announce_tests(listen.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(
        &self,
        _: &ProjectModel,
        _: &TestRequest,
        listen: &TestListen,
        _: &runner::TestDebug,
    ) -> base::Result<RunPlan> {
        announce_tests(listen.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args([
                "/d",
                "/c",
                "echo Listening for transport dt_socket at address: 5005 & ping -n 120 127.0.0.1",
            ]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_ready(&self) -> Option<&'static str> {
        Some("Listening for transport dt_socket at address:")
    }
}

fn announce_tests(listen: TestListen) {
    std::thread::spawn(move || {
        use std::io::Write;
        let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", listen.port)) else {
            return;
        };
        let _ = writeln!(stream, "{}", listen.token);
        for line in [
            r#"{"event":"plan"}"#,
            r#"{"event":"started","id":"p","name":"Probe","kind":"suite","suite":"demo.Probe"}"#,
            r#"{"event":"started","id":"p/ok","parent":"p","name":"works()","kind":"case","suite":"demo.Probe","case":"works"}"#,
            r#"{"event":"finished","id":"p/ok","outcome":"passed","durationMs":1}"#,
            r#"{"event":"planFinished"}"#,
        ] {
            if writeln!(stream, "{line}").is_err() {
                return;
            }
        }
    });
}

/// A debug service on the mock adapter that answers `redefineClasses`.
struct ReloadProbe {
    runtime: tokio::runtime::Runtime,
    server: Mutex<Option<lang_fixture::MockDap>>,
    reloads: AtomicUsize,
}

impl ReloadProbe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("the test runtime starts"),
            server: Mutex::new(None),
            reloads: AtomicUsize::new(0),
        })
    }

    fn session(&self, name: String) -> Result<Debugger, String> {
        let server = lang_fixture::MockDap::start().map_err(|error| error.to_string())?;
        let address = server.address().to_owned();
        *self
            .server
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(server);
        let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (snapshots, incoming) = std::sync::mpsc::channel();
        let launch = Launch {
            address,
            adapter_id: "fixture".into(),
            name,
            ..Launch::default()
        };
        self.runtime.spawn(async move {
            let _ = dap_client::run(launch, receiver, snapshots).await;
        });
        Ok(Debugger::new(commands, incoming))
    }
}

impl DebugService for ReloadProbe {
    fn start(
        &self,
        _: ProcessSpec,
        name: String,
        _: Option<PathBuf>,
        _: Vec<BreakpointSpec>,
        _: Vec<String>,
        _: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name)
    }

    fn attach(
        &self,
        _: String,
        _: u16,
        name: String,
        _: Option<String>,
        _: Vec<BreakpointSpec>,
        _: Vec<String>,
        _: Vec<String>,
    ) -> Result<Debugger, String> {
        self.session(name)
    }

    fn reload_classes(
        &self,
        debugger: Debugger,
        _: Vec<PathBuf>,
        done: std::sync::mpsc::Sender<ReloadOutcome>,
    ) -> bool {
        self.reloads.fetch_add(1, Ordering::SeqCst);
        self.runtime.spawn(async move {
            let outcome = match debugger
                .request("redefineClasses", serde_json::json!({}))
                .await
            {
                Ok(body) => ReloadOutcome {
                    reloaded: body
                        .get("changedClasses")
                        .and_then(serde_json::Value::as_array)
                        .map(|list| {
                            list.iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    error: None,
                },
                Err(error) => ReloadOutcome {
                    reloaded: Vec::new(),
                    error: Some(error),
                },
            };
            let _ = done.send(outcome);
        });
        true
    }
}

fn model(root: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        modules: vec![Module {
            id: "demo".into(),
            name: "demo".into(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".into(),
                output_dirs: vec![root.join("classes")],
                ..Unit::default()
            }],
        }],
        ..ProjectModel::default()
    }
}

fn probe_setup(root: &Path, builds: Arc<Mutex<Vec<bool>>>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(BuildProbe {
            model: model(root),
            builds,
        })),
        providers: Vec::new(),
        tests: Some(Arc::new(ProbeTests)),
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("m11-probe-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: Some(ReloadProbe::new()),
    }
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn ctrl_f9_builds_both_class_sets_during_a_debug_test_run(cx: &mut TestAppContext) {
    let root = sample_project("m11-debug-test-build");
    let builds = Arc::new(Mutex::new(Vec::new()));
    let window = open_with(cx, &root, probe_setup(&root, builds.clone()));
    let panel = run_panel(cx);
    wait_run(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| {
                panel.debug_tests(
                    TestRequest {
                        suite: Some("demo.Probe".into()),
                        ..TestRequest::default()
                    },
                    window,
                    cx,
                )
            })
        })
        .unwrap();

    wait_run(cx, "the debug test run", |cx| {
        !snapshot(cx).state.eq(&dap_client::DebugState::Inactive)
            && panel.read_with(cx, |panel, _| {
                panel
                    .test_report()
                    .iter()
                    .any(|line| line.contains("works"))
            })
    });

    let class = root.join("classes/demo/Probe.class");
    fs::create_dir_all(class.parent().unwrap()).unwrap();
    fs::write(&class, b"changed").unwrap();

    press(cx, window, "ctrl-f9");
    wait_run(cx, "the build during the test run", |cx| {
        !builds.lock().unwrap().is_empty() && panel.read_with(cx, |panel, _| panel.state().running)
    });
    assert_eq!(
        builds.lock().unwrap().as_slice(),
        [true],
        "Build Project asked for the test classes too"
    );
    assert!(
        panel
            .read_with(cx, |panel, _| panel.test_report())
            .iter()
            .any(|line| line.contains("works") && line.contains("passed")),
        "the test tree survived the build console"
    );

    wait_run(cx, "the hot swap answer", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap()
            > 0
    });
    let text = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(_, text)| text)
        .unwrap_or_default();
    assert!(text.contains("Reloaded"), "{text}");

    press(cx, window, "ctrl-f2");
    wait_run(cx, "the end of the test session", |cx| {
        snapshot(cx).state == dap_client::DebugState::Terminated
    });
}

#[ui_kit::gpui::test]
fn match_brace_goes_to_the_bracket_around_the_caret(cx: &mut TestAppContext) {
    let root = sample_project("m11-brace");
    let path = root.join("src/main.txt");
    fs::write(&path, "if (a) {\n    call();\n}\n").unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(1, 4, cx));
    press(cx, window, "ctrl-shift-m");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 7),
        "the caret goes to the opening bracket that surrounds it"
    );
    press(cx, window, "ctrl-shift-m");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (2, 1),
        "the second press goes to the partner"
    );
}

fn diagnostic(line: u32, character: u32, message: &str) -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint { line, character },
            end: TextPoint {
                line,
                character: character + 1,
            },
        },
        severity: Severity::Error,
        message: message.to_owned(),
        unnecessary: false,
    }
}

#[ui_kit::gpui::test]
fn shift_f2_goes_to_the_previous_problem(cx: &mut TestAppContext) {
    let root = project("m11-previous", &[("src/Main.txt", "one\ntwo\nthree\n")]);
    let main = root.join("src/Main.txt");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![
        diagnostic(0, 0, "first"),
        diagnostic(1, 0, "second"),
        diagnostic(2, 0, "third"),
    ]);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    press(cx, window, "f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (1, 0),
        "F2 goes to the next problem"
    );
    press(cx, window, "shift-f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (0, 0),
        "Shift+F2 goes to the problem before the caret"
    );
    press(cx, window, "shift-f2");
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (2, 0),
        "Shift+F2 goes round before the first problem"
    );
}

#[ui_kit::gpui::test]
fn ctrl_alt_right_returns_where_back_came_from(cx: &mut TestAppContext) {
    let root = project(
        "m11-forward",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "other\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = FakeLanguage::new();
    language.set_definition(&other, 0, 2);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    press(cx, window, "ctrl-b");
    draw(cx, window);
    assert_eq!(
        crate::window_tests::active_path(cx, window),
        Some(other.clone())
    );
    press(cx, window, "ctrl-alt-left");
    draw(cx, window);
    assert_eq!(
        crate::window_tests::active_path(cx, window),
        Some(main.clone())
    );
    press(cx, window, "ctrl-alt-right");
    draw(cx, window);
    assert_eq!(
        crate::window_tests::active_path(cx, window),
        Some(other.clone()),
        "Ctrl+Alt+Right returns to the place Back left"
    );

    // Alt+Right keeps walking the tabs of the group.
    open_path(cx, window, &main);
    press(cx, window, "alt-right");
    assert_eq!(
        crate::window_tests::active_path(cx, window),
        Some(other),
        "Alt+Right is Next Tab"
    );
}

#[ui_kit::gpui::test]
fn the_status_bar_says_the_local_changes_will_come_back(cx: &mut TestAppContext) {
    let (root, _bare, _git) = conflicting_repository("m11-autostash");
    // A tracked change that the merge does not touch goes to the autostash.
    fs::write(root.join("main.txt"), "local uncommitted\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    press(cx, window, "ctrl-t");
    wait_for(cx, "the update choice", |cx| {
        has_selector(cx, window, "menu-0")
    });
    crate::window_tests::click(cx, window, "menu-0");

    wait_for(cx, "the autostash note", |cx| {
        has_selector(cx, window, "conflict-autostash")
    });
    let label = window
        .read_with(cx, |workspace, _| {
            workspace.branch_text().map(str::to_owned)
        })
        .unwrap()
        .unwrap_or_default();
    assert!(
        label.contains("local changes will be restored when the operation ends"),
        "the status bar names the autostash: {label}"
    );
}

#[ui_kit::gpui::test]
fn show_usages_reads_many_files_in_the_background(cx: &mut TestAppContext) {
    let root = scratch("m11-usages");
    let mut spans = Vec::new();
    for file in 0..30 {
        let path = root.join(format!("src/File{file}.txt"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = String::new();
        for line in 0..20 {
            let start = text.len();
            text.push_str(&format!("greeting {file} {line}\n"));
            spans.push(SourceSpan {
                uri: crate::window_tests_lang::uri_of(&path),
                span: span_in(&text, start, start + 8),
            });
        }
        fs::write(&path, &text).unwrap();
    }
    let main = root.join("src/Main.txt");
    fs::write(&main, "greet\n").unwrap();
    let language = FakeLanguage::new();
    language.set_references(spans);

    usages::reset_background_reads();

    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-f7");
    let popup = cx
        .update(|cx| cx.global::<ActiveShowUsages>().0.clone())
        .expect("the popup is on screen");
    assert_eq!(popup.read_with(cx, |popup, _| popup.row_count()), 600);
    wait_for(cx, "the lines of every file", |cx| {
        popup.read_with(cx, |popup, _| popup.loaded_rows()) == 600
    });
    assert!(
        usages::background_reads() >= 30,
        "the reads of the popup happen away from the window"
    );
}

/// A program that leaves a child with its output pipe and exits at once.
struct LateRun;

impl RunConfigProvider for LateRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Late".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("powershell.exe").args([
                "-NoProfile",
                "-Command",
                "Write-Output 'started-run'; Start-Process -FilePath cmd.exe -ArgumentList \
                 '/d','/c','ping -n 6 127.0.0.1 >nul & echo late-after-exit' -NoNewWindow",
            ]),
            grace: Duration::from_secs(1),
        })
    }
}

fn run_setup(root: &Path, provider: Arc<dyn RunConfigProvider>) -> ProjectSetup {
    let mut setup = crate::window_tests_run::setup(root);
    setup.providers = vec![provider];
    setup
}

fn notification(cx: &TestAppContext, window: WindowHandle<Workspace>) -> String {
    window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(_, text)| text.to_string())
        .unwrap_or_default()
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_finished_run_frees_the_panel_and_keeps_late_output_in_its_own_console(
    cx: &mut TestAppContext,
) {
    let root = sample_project("m11-late-run");
    let window = open_with(cx, &root, run_setup(&root, Arc::new(LateRun)));
    let panel = run_panel(cx);
    wait_run(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });

    press(cx, window, "shift-f10");
    wait_run(cx, "the first process to finish", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("started-run"))
            && panel
                .read_with(cx, |panel, _| panel.text())
                .iter()
                .any(|line| line.contains("Process finished with exit code"))
    });
    assert!(
        !panel.read_with(cx, |panel, _| panel.state().running),
        "the panel is free as soon as the main process exits"
    );

    let serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    press(cx, window, "shift-f10");
    wait_run(cx, "the second run", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("started-run"))
    });
    assert_ne!(
        notification(cx, window),
        "Stop what is running first",
        "the next run starts without Busy"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap()
            == serial,
        "the next run shows no Busy notification"
    );
    wait_run(cx, "the late output of the first run", |cx| {
        panel
            .read_with(cx, |panel, _| panel.draining_text())
            .iter()
            .any(|line| line.contains("late-after-exit"))
    });

    press(cx, window, "ctrl-f2");
    wait_run(cx, "the stopped second run", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
    });
}

fn open_with_layout(
    cx: &mut TestAppContext,
    root: PathBuf,
    layout: Layout,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("m11-layout-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(ui_kit::Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = WindowLaunch {
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
    let setup = project_setup(scratch("m11-layout-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (_sender, status) = futures_channel::mpsc::unbounded();
    let server = LanguageServer {
        support: None,
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
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

#[ui_kit::gpui::test]
fn a_project_without_runs_does_not_restore_the_run_window(cx: &mut TestAppContext) {
    let root = sample_project("m11-empty-run");
    let layout = Layout {
        bottom: DockState {
            open: true,
            size: 260.,
        },
        bottom_tool: Some(RUN_TOOL.to_owned()),
        ..Layout::default()
    };
    let window = open_with_layout(cx, root, layout);
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap(),
        "the empty Run window stays hidden at the start"
    );
}
