//! Run configurations another IDE stored in the project: the offer to import them, the
//! answer Not Now, and a configuration of tests that runs through the Tests window.
//! The substitute providers stand for the language that reads the files.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{
    ForeignConfigs, RunConfig, RunConfigProvider, RunPlan, TestListen, TestProvider, TestRequest,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::trust::{LanguageStart, ProjectTrust, ServerTrust};
use crate::window_tests::{click, draw, open_window_with_state, press, sample_project, scratch};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// Opens the project with a state folder the caller keeps, so a second window sees what
/// the first one remembered.
fn open_with_state(
    cx: &mut TestAppContext,
    root: &Path,
    plan: crate::run::ProjectSetup,
    state: std::path::PathBuf,
) -> WindowHandle<Workspace> {
    let window = open_window_with_state(cx, root.to_path_buf(), plan, state);
    cx.run_until_parked();
    draw(cx, window);
    window
}

/// A provider that reads two configurations and one unsupported kind from the project.
struct ForeignSource;

impl RunConfigProvider for ForeignSource {
    fn id(&self) -> &'static str {
        "fixture-foreign"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Err("imported configurations do not start from this provider".into())
    }

    fn foreign_configs(&self, _: &ProjectModel) -> Option<ForeignConfigs> {
        Some(ForeignConfigs {
            source: "IntelliJ IDEA",
            configs: vec![imported("Imported App"), imported("Imported Tests")],
            unsupported: vec![("JavaScript Debug".to_owned(), 1)],
            warnings: vec!["Imported App: $OTHER_HOME$ stays as it is".to_owned()],
        })
    }
}

fn imported(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "fixture-foreign".into(),
        assemble_first: false,
        before_launch: Some(Vec::new()),
        ..RunConfig::default()
    }
}

/// A configuration that is a test: its request goes to the test provider.
struct TestConfiguration;

impl RunConfigProvider for TestConfiguration {
    fn id(&self) -> &'static str {
        "fixture-tests"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Demo Test".into(),
            provider: "fixture-tests".into(),
            assemble_first: false,
            before_launch: Some(Vec::new()),
            ..RunConfig::default()
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Err("a test configuration starts through the test provider".into())
    }

    fn test_request(&self, _: &RunConfig) -> Option<TestRequest> {
        Some(TestRequest {
            suite: Some("demo.DemoTest".into()),
            case: Some("passes".into()),
            ..TestRequest::default()
        })
    }
}

/// A test provider that records its requests and ends its run without tests.
struct RecordingTests {
    requests: Arc<Mutex<Vec<TestRequest>>>,
}

impl TestProvider for RecordingTests {
    fn plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        self.requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", listen.port)) {
            let _ = writeln!(stream, "{}", listen.token);
            std::thread::spawn(move || {
                for line in [r#"{"event":"plan"}"#, r#"{"event":"planFinished"}"#] {
                    if writeln!(stream, "{line}").is_err() {
                        return;
                    }
                }
            });
        }
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

/// A language starter for the untrusted project: nothing starts.
struct NoStart;

impl LanguageStart for NoStart {
    fn start(
        &self,
        _trust: ServerTrust,
        _started: futures_channel::mpsc::UnboundedSender<Option<crate::StartedServer>>,
    ) {
    }
}

fn offer(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<(String, Vec<String>)> {
    window
        .read_with(cx, |workspace, _| {
            workspace.last_notification().map(|(title, _)| {
                let buttons = workspace
                    .last_notification_buttons()
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                (title.to_string(), buttons)
            })
        })
        .unwrap()
}

fn with_configurations(root: &Path) {
    let folder = root.join(".idea").join("runConfigurations");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("App.xml"),
        r#"<component name="ProjectRunConfigurationManager">
  <configuration name="Imported App" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.App" />
  </configuration>
</component>"#,
    )
    .unwrap();
}

fn configuration_names(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .state()
            .configs
            .iter()
            .map(ToString::to_string)
            .collect()
    })
}

#[ui_kit::gpui::test]
fn the_offer_imports_the_configurations_once(cx: &mut TestAppContext) {
    let root = sample_project("run-idea-import");
    with_configurations(&root);
    let mut plan = setup(&root);
    plan.providers = vec![Arc::new(ForeignSource)];
    let window = open_with(cx, &root, plan);
    let panel = run_panel(cx);

    wait_for(cx, "the offer of the configurations", |cx| {
        offer(cx, window).is_some_and(|(title, _)| title.ends_with("run configurations found: 2"))
    });
    let (_, buttons) = offer(cx, window).expect("the offer is on screen");
    assert_eq!(buttons, ["Import", "Not Now"]);

    click(cx, window, "notification-action");

    wait_for(cx, "the imported configurations in the list", |cx| {
        let names = configuration_names(cx, &panel);
        names.iter().any(|name| name == "Imported App")
            && names.iter().any(|name| name == "Imported Tests")
    });
    wait_for(cx, "the summary of the import", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.last_notification().is_some_and(|(_, text)| {
                    text.contains("Imported 2")
                        && text.contains("Skipped, exists 0")
                        && text.contains("JavaScript Debug (1)")
                })
            })
            .unwrap()
    });
    let file = runner::configs_file(&root);
    wait_for(cx, "the configurations in the file", |_cx| {
        runner::load_configs(&file).map_or(0, |configs| configs.len()) == 2
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("run.import_idea_configs", window, cx)
        })
        .unwrap();
    wait_for(cx, "the summary of the second import", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.last_notification().is_some_and(|(_, text)| {
                    text.contains("Imported 0") && text.contains("Skipped, exists 2")
                })
            })
            .unwrap()
    });
    assert_eq!(
        runner::load_configs(&file).unwrap().len(),
        2,
        "the second import keeps one entry per configuration"
    );
}

#[ui_kit::gpui::test]
fn not_now_keeps_the_offer_away_on_the_next_open(cx: &mut TestAppContext) {
    let root = sample_project("run-idea-not-now");
    with_configurations(&root);
    let state = scratch("run-idea-not-now-state");
    let mut plan = setup(&root);
    plan.providers = vec![Arc::new(ForeignSource)];
    let window = open_with_state(cx, &root, plan, state.clone());
    wait_for(cx, "the offer of the configurations", |cx| {
        offer(cx, window).is_some_and(|(title, _)| title.contains("run configurations found"))
    });

    click(cx, window, "notification-action-not-now");

    let mut again = setup(&root);
    again.providers = vec![Arc::new(ForeignSource)];
    let window = open_with_state(cx, &root, again, state);
    let panel = run_panel(cx);
    wait_for(cx, "the project of the second window", |cx| {
        panel.read_with(cx, |panel, _| {
            !panel.is_loading() && panel.model().is_some()
        })
    });
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }

    assert!(
        offer(cx, window).is_none_or(|(title, _)| !title.contains("run configurations found")),
        "Not Now is remembered for this project"
    );
    let names = configuration_names(cx, &panel);
    assert!(
        !names.iter().any(|name| name == "Imported App"),
        "Not Now imports nothing: {names:?}"
    );
}

#[ui_kit::gpui::test]
fn a_project_in_safe_mode_is_not_offered_an_import(cx: &mut TestAppContext) {
    let root = sample_project("run-idea-safe-mode");
    with_configurations(&root);
    cx.update(|cx| cx.set_global(ProjectTrust::untrusted(std::rc::Rc::new(NoStart))));
    let mut plan = setup(&root);
    plan.providers = vec![Arc::new(ForeignSource)];
    let window = open_with(cx, &root, plan);
    let panel = run_panel(cx);
    for _ in 0..10 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }

    assert!(
        offer(cx, window).is_none_or(|(title, _)| !title.contains("run configurations found")),
        "Safe Mode asks nothing about an import"
    );
    assert!(!panel.read_with(cx, |panel, _| panel.trusted()));
    assert!(configuration_names(cx, &panel).is_empty());
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_test_configuration_runs_through_the_tests_window(cx: &mut TestAppContext) {
    let root = sample_project("run-idea-test-config");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut plan = setup(&root);
    plan.providers = vec![Arc::new(TestConfiguration)];
    plan.tests = Some(Arc::new(RecordingTests {
        requests: requests.clone(),
    }));
    let window = open_with(cx, &root, plan);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration of tests", |cx| {
        !configuration_names(cx, &panel).is_empty()
    });

    press(cx, window, "shift-f10");

    wait_for(cx, "the test request and its end", |cx| {
        let started = requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .len();
        started == 1 && panel.read_with(cx, |panel, _| panel.idle())
    });
    let request = requests.lock().unwrap_or_else(|error| error.into_inner())[0].clone();
    assert_eq!(request.suite.as_deref(), Some("demo.DemoTest"));
    assert_eq!(request.case.as_deref(), Some("passes"));
}
