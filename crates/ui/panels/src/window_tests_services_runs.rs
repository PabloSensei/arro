//! T-0230: run configurations beside Docker in the Services window. The group of a
//! provider, the state of each configuration, Run and Stop from the tree and the header,
//! the console and the live tab of the same run the Run window shows, and the browser
//! link of a running program.
//!
//! cargo test -p panels window_tests_services_runs

#![cfg(windows)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::ExternalLinks;
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{
    LiveApplication, LiveEndpoint, LiveNote, LiveSection, LiveTable, RunConfig, RunConfigProvider,
    RunPlan, ServiceGroup,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{RUN_TOOL, SERVICES_TOOL, Workspace};

use crate::docker::{ActiveDocker, DockerPanel};
use crate::run::RunPanel;
use crate::script::ActiveRunPanel;
use crate::window_tests::{bounds_of, click, draw, open_window_with, sample_project};
use crate::window_tests_run::{setup, wait_for};
use crate::window_tests_services::{fake_docker, menu_labels, right_click};
use crate::window_tests_ui::ENV;

/// The group of the applications the window exists for.
const BOOT: ServiceGroup = ServiceGroup {
    id: "boot",
    title: "Boot",
    optional: false,
};
/// The group of plain programs: it lists them only when the user asks for that.
const APPLICATION: ServiceGroup = ServiceGroup {
    id: "application",
    title: "Application",
    optional: true,
};
/// The port the stand-in application of the test says it listens on.
const PORT: u16 = 8080;

/// Three configurations: two of the first group and one of the optional group.
fn config(name: &str) -> RunConfig {
    RunConfig {
        name: name.to_owned(),
        provider: "fixture".into(),
        assemble_first: false,
        settings: Default::default(),
        ..RunConfig::default()
    }
}

struct FixtureRuns;

impl RunConfigProvider for FixtureRuns {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![config("PetClinic"), config("Api"), config("Plain")])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            // A line at once, then a program that outlives the test until Stop.
            program: ProcessSpec::new("cmd")
                .args([
                    "/d",
                    "/c",
                    "echo started from services & ping -n 120 127.0.0.1",
                ])
                // No helper in this test binary, so Stop goes straight to force.
                .ctrl_helper("no-such-helper.exe"),
            grace: Duration::from_secs(1),
        })
    }

    fn service_group(&self, config: &RunConfig) -> Option<ServiceGroup> {
        match config.name.as_str() {
            "Plain" => Some(APPLICATION),
            _ => Some(BOOT),
        }
    }

    fn live_application(
        &self,
        config: &RunConfig,
        _: &ProjectModel,
    ) -> Option<Arc<dyn LiveApplication>> {
        (config.name != "Plain").then(|| Arc::new(FixtureLive) as Arc<dyn LiveApplication>)
    }
}

/// The live sections of the stand-in application: it says it started and listens on the
/// port of the test.
struct FixtureLive;

impl LiveApplication for FixtureLive {
    fn title(&self) -> String {
        "Actuator".to_owned()
    }

    fn observe(&self, _: &str) -> bool {
        false
    }

    fn started(&self) -> bool {
        true
    }

    fn endpoint(&self) -> Option<LiveEndpoint> {
        None
    }

    fn section_path(&self, _: LiveSection) -> Option<String> {
        None
    }

    fn app_url(&self, path: &str) -> Option<String> {
        Some(format!("http://127.0.0.1:{PORT}{path}"))
    }

    fn app_port(&self) -> Option<u16> {
        Some(PORT)
    }

    fn not_exposed(&self) -> LiveNote {
        LiveNote {
            text: "The endpoint is not exposed".to_owned(),
            ..LiveNote::default()
        }
    }

    fn read(&self, _: LiveSection, _: &str) -> Result<LiveTable, String> {
        Err("the test reads no section".to_owned())
    }
}

type Opened = Arc<Mutex<Vec<String>>>;

/// Opens the window on the stand-in Docker and the stand-in provider, shows Services and
/// waits for both trees.
fn open(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<DockerPanel>,
    gpui::Entity<RunPanel>,
    Opened,
) {
    let root = sample_project(name);
    let (script, _) = fake_docker(&format!("{name}-cli"));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureRuns)];
    let opened: Opened = Arc::new(Mutex::new(Vec::new()));
    let recorder = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(url.to_owned());
        }))))
    });
    let guard = ENV.lock().unwrap_or_else(|poison| poison.into_inner());
    // SAFETY: the windows read the variables at creation, under `ENV`.
    unsafe {
        std::env::set_var("IDE_DOCKER_CLI", &script);
        std::env::set_var(platform::NO_BROWSER_ENV, "1");
    }
    let window = open_window_with(cx, root, setup);
    // SAFETY: the panel has read the variable already.
    unsafe { std::env::remove_var("IDE_DOCKER_CLI") };
    drop(guard);

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    let docker = cx.update(|cx| cx.global::<ActiveDocker>().0.clone());
    let runs = cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone());
    wait_for(cx, "the configurations of the run window", |cx| {
        runs.read_with(cx, |runs, _| !runs.state().configs.is_empty())
    });
    wait_for(cx, "the containers of the stand-in Docker", |cx| {
        docker.read_with(cx, |panel, _| {
            panel
                .report()
                .iter()
                .any(|line| line.contains("petclinic-web-1"))
        })
    });
    draw(cx, window);
    (window, docker, runs, opened)
}

fn present(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

fn run_state(cx: &mut TestAppContext, docker: &gpui::Entity<DockerPanel>, name: &str) -> String {
    docker.read_with(cx, |panel, cx| panel.run_state_text(name, cx))
}

#[ui_kit::gpui::test]
fn the_groups_list_their_configurations_and_the_optional_one_waits(cx: &mut TestAppContext) {
    let (window, docker, _, _) = open(cx, "services-runs-groups");

    let entries = docker.read_with(cx, |panel, cx| panel.service_entries(cx));
    assert_eq!(
        entries,
        vec![
            (
                "Boot".to_owned(),
                String::new(),
                "Running: (0/2)".to_owned()
            ),
            (
                "Boot".to_owned(),
                "Api".to_owned(),
                "Not Started".to_owned()
            ),
            (
                "Boot".to_owned(),
                "PetClinic".to_owned(),
                "Not Started".to_owned()
            ),
            (
                "Application".to_owned(),
                String::new(),
                "Running: (0/1)".to_owned()
            ),
        ],
        "the optional group hides its configurations until the user asks"
    );
    bounds_of(cx, window, "services-run-group-boot");
    bounds_of(cx, window, "services-run-PetClinic");
    assert!(!present(cx, window, "services-run-Plain"));

    right_click(cx, window, "services-run-group-application");
    let labels = menu_labels(cx, window);
    assert!(
        labels
            .iter()
            .any(|row| row == "Show all run configurations"),
        "the menu of the optional group offers the configurations: {labels:?}"
    );
    click(cx, window, "menu-0");
    draw(cx, window);
    bounds_of(cx, window, "services-run-Plain");
    let entries = docker.read_with(cx, |panel, cx| panel.service_entries(cx));
    assert!(
        entries
            .iter()
            .any(|(_, name, state)| name == "Plain" && state == "Not Started"),
        "{entries:?}"
    );

    // Docker is still a node of the same tree.
    bounds_of(cx, window, "docker-container-petclinic-web-1");
}

#[ui_kit::gpui::test]
fn run_stop_the_console_and_the_browser_link_of_one_configuration(cx: &mut TestAppContext) {
    let (window, docker, runs, opened) = open(cx, "services-runs-run");

    click(cx, window, "services-run-PetClinic");
    draw(cx, window);
    bounds_of(cx, window, "services-run-PetClinic");

    click(cx, window, "docker-action-run");
    wait_for(cx, "the configuration to run", |cx| {
        run_state(cx, &docker, "PetClinic") == format!("Running :{PORT}")
            && runs.read_with(cx, |runs, _| {
                runs.text()
                    .iter()
                    .any(|line| line.contains("started from services"))
            })
    });
    wait_for(cx, "the configuration to run", |cx| {
        run_state(cx, &docker, "PetClinic") == format!("Running :{PORT}")
            && runs.read_with(cx, |runs, _| {
                runs.text().iter().any(|line| !line.trim().is_empty())
            })
    });
    draw(cx, window);
    bounds_of(cx, window, "services-run-console");
    bounds_of(cx, window, "services-run-tab-actuator");
    let (title, _) = docker.read_with(cx, |panel, _| panel.header_text());
    assert_eq!(title, "PetClinic");
    assert_eq!(
        docker.read_with(cx, |panel, _| panel.right_tab()),
        "run-console",
        "the console is in front of the live tab"
    );

    // The live tab of the same run is the one of the Run window.
    click(cx, window, "services-run-tab-actuator");
    draw(cx, window);
    bounds_of(cx, window, "actuator-section-0");
    assert_eq!(
        docker.read_with(cx, |panel, _| panel.right_tab()),
        "actuator"
    );
    click(cx, window, "services-run-tab-console");
    draw(cx, window);

    click(cx, window, "docker-action-stop-run");
    wait_for(cx, "the configuration to stop", |cx| {
        run_state(cx, &docker, "PetClinic").starts_with("Stopped")
    });
    draw(cx, window);

    // A new run brings the port back, and the browser link opens it.
    click(cx, window, "docker-action-run");
    wait_for(cx, "the configuration to run again", |cx| {
        run_state(cx, &docker, "PetClinic") == format!("Running :{PORT}")
    });
    click(cx, window, "docker-action-browser");
    assert_eq!(
        opened
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_slice(),
        [format!("http://localhost:{PORT}/")],
        "the link of the running application reaches the opener"
    );

    // Open in HTTP Client: a scratch request on the same port.
    click(cx, window, "docker-action-http");
    draw(cx, window);
    let request = crate::window_tests::active_path(cx, window).expect("the request file is open");
    let text = std::fs::read_to_string(&request).expect("the request file is written");
    assert!(
        text.contains(&format!("GET http://localhost:{PORT}/")),
        "the request names the port of the run: {text}"
    );
}

#[ui_kit::gpui::test]
fn a_run_started_in_the_run_window_is_running_in_services(cx: &mut TestAppContext) {
    let (window, docker, runs, _) = open(cx, "services-runs-shared");

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    runs.update(cx, |runs, cx| {
        assert!(
            runs.select_named("Api", cx),
            "the configuration of the window"
        );
        runs.run(cx);
    });
    wait_for(cx, "the run of the Run window", |cx| {
        run_state(cx, &docker, "Api") == format!("Running :{PORT}")
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(SERVICES_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    click(cx, window, "services-run-Api");
    draw(cx, window);
    bounds_of(cx, window, "services-run-console");
    assert_eq!(
        docker.read_with(cx, |panel, cx| panel.run_state_text("Api", cx)),
        "Running :8080",
        "the state of the run is one for both windows"
    );

    runs.update(cx, |runs, cx| runs.stop(cx));
    wait_for(cx, "the stop of the run window", |cx| {
        run_state(cx, &docker, "Api").starts_with("Stopped")
    });
    draw(cx, window);
}
