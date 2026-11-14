//! The live tab of the Run window without a real application: a small HTTP server gives
//! the JSON of the sections and a script prints the lines of the server and of the start,
//! so the readiness dot, the tables, the jumps, the 404 hint and the stopped state are
//! checked the way the window shows them.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{
    LiveApplication, LiveEndpoint, LiveNote, LiveRequest, LiveRow, LiveSection, LiveTable,
    LiveTarget, LiveTone, RunConfig, RunConfigProvider, RunPlan,
};
use serde_json::Value;
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use workspace::Workspace;

use crate::index::ActiveIndex;
use crate::live_panel::LivePanel;
use crate::run::RunPanel;
use crate::window_tests::{active_path, bounds_of, click, draw, editor_of};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

const HEALTH: &str =
    r#"{"status":"UP","components":{"db":{"status":"UP"},"diskSpace":{"status":"DOWN"}}}"#;
const BEANS: &str = r#"{"contexts":{"application":{"beans":{"ownerController":{"scope":"singleton","type":"demo.OwnerController","dependencies":["ownerRepository"]},"ownerRepository":{"scope":"singleton","type":"demo.OwnerRepository","dependencies":[]}}}}}"#;
const MAPPINGS: &str = r#"{"contexts":{"application":{"mappings":{"dispatcherServlets":{"dispatcherServlet":[{"predicate":"{GET [/owners]}","handler":"demo.OwnerController#findAll()"}]},"dispatcherHandlers":{"dispatcherServlet":[{"predicate":"{POST [/owners]}","handler":"demo.OwnerController#create()"}]}}}}}"#;
const ENV: &str = r#"{"propertySources":[{"name":"application.properties","properties":{"server.port":{"value":"8080"},"spring.datasource.password":{"value":"******"}}}]}"#;

/// A server of the current test on a loopback port: it answers the four sections with
/// the fixtures above, or 404 for all of them when the test asks for the hint.
struct Server {
    port: u16,
    stop: Arc<AtomicBool>,
}

impl Server {
    fn start(not_exposed: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("the test server binds");
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else {
                    continue;
                };
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or("/");
                let (status, body) = if not_exposed {
                    ("404 Not Found", "{}")
                } else {
                    let body = match path {
                        "/actuator/health" => HEALTH,
                        "/actuator/beans" => BEANS,
                        "/actuator/mappings" => MAPPINGS,
                        "/actuator/env" => ENV,
                        _ => "{}",
                    };
                    if path.starts_with("/actuator/") {
                        ("200 OK", body)
                    } else {
                        ("404 Not Found", "{}")
                    }
                };
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(answer.as_bytes());
            }
        });
        Self { port, stop }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
    }
}

#[derive(Default)]
struct Seen {
    port: Option<u16>,
    started: bool,
}

struct FakeLive {
    port: u16,
    seen: std::sync::Mutex<Seen>,
    settings: PathBuf,
}

impl FakeLive {
    fn observe_text(&self, line: &str) -> bool {
        let mut seen = self
            .seen
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let before = (seen.port, seen.started);
        if let Some((_, rest)) = line.split_once("Tomcat started on port ") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            seen.port = digits.parse().ok();
        }
        if line.contains("Started App in ") && line.contains(" seconds") {
            seen.started = true;
        }
        before != (seen.port, seen.started)
    }

    fn started(&self) -> bool {
        self.seen
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .started
    }
}

impl LiveApplication for FakeLive {
    fn title(&self) -> String {
        "Actuator".to_owned()
    }

    fn observe(&self, line: &str) -> bool {
        self.observe_text(line)
    }

    fn started(&self) -> bool {
        FakeLive::started(self)
    }

    fn endpoint(&self) -> Option<LiveEndpoint> {
        Some(LiveEndpoint {
            host: "127.0.0.1".to_owned(),
            port: self.port,
        })
    }

    fn section_path(&self, section: LiveSection) -> Option<String> {
        Some(
            match section {
                LiveSection::Health => "/actuator/health",
                LiveSection::Beans => "/actuator/beans",
                LiveSection::Mappings => "/actuator/mappings",
                LiveSection::Environment => "/actuator/env",
            }
            .to_owned(),
        )
    }

    fn app_url(&self, path: &str) -> Option<String> {
        Some(format!("http://127.0.0.1:{}{path}", self.port))
    }

    fn not_exposed(&self) -> LiveNote {
        LiveNote {
            text: "The endpoint is not exposed. Add it to management.endpoints.web.exposure.include in application.properties."
                .to_owned(),
            file: Some(self.settings.clone()),
            label: Some("Open application.properties".to_owned()),
        }
    }

    fn read(&self, section: LiveSection, body: &str) -> Result<LiveTable, String> {
        let value: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
        match section {
            LiveSection::Health => {
                let mut table = LiveTable::new(&["Status", "Component"]);
                let status = value["status"].as_str().unwrap_or_default();
                table.push(LiveRow {
                    cells: vec![status.to_owned(), String::new()],
                    tone: if status == "UP" {
                        LiveTone::Good
                    } else {
                        LiveTone::Bad
                    },
                    ..LiveRow::default()
                });
                if let Some(components) = value["components"].as_object() {
                    for (name, component) in components {
                        let status = component["status"].as_str().unwrap_or_default();
                        table.push(LiveRow {
                            cells: vec![status.to_owned(), name.clone()],
                            depth: 1,
                            tone: if status == "UP" {
                                LiveTone::Good
                            } else {
                                LiveTone::Bad
                            },
                            ..LiveRow::default()
                        });
                    }
                }
                Ok(table)
            }
            LiveSection::Beans => {
                let mut table = LiveTable::new(&["Bean", "Type", "Scope", "Dependencies"]);
                if let Some(beans) = value
                    .pointer("/contexts/application/beans")
                    .and_then(Value::as_object)
                {
                    for (name, bean) in beans {
                        let type_name = bean["type"].as_str().unwrap_or_default();
                        table.push(LiveRow {
                            cells: vec![
                                name.clone(),
                                type_name.to_owned(),
                                bean["scope"].as_str().unwrap_or_default().to_owned(),
                                bean["dependencies"]
                                    .as_array()
                                    .map(|items| {
                                        items
                                            .iter()
                                            .filter_map(Value::as_str)
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    })
                                    .unwrap_or_default(),
                            ],
                            target: Some(LiveTarget::Class(type_name.to_owned())),
                            ..LiveRow::default()
                        });
                    }
                }
                Ok(table)
            }
            LiveSection::Mappings => {
                let mut table = LiveTable::new(&["Method", "Path", "Handler"]);
                for (at, key) in ["dispatcherServlets", "dispatcherHandlers"]
                    .iter()
                    .enumerate()
                {
                    let Some(items) = value
                        .pointer(&format!(
                            "/contexts/application/mappings/{key}/dispatcherServlet"
                        ))
                        .and_then(Value::as_array)
                    else {
                        continue;
                    };
                    for item in items {
                        let predicate = item["predicate"].as_str().unwrap_or_default();
                        let method = if at == 0 { "GET" } else { "POST" };
                        let path = predicate
                            .split_once('[')
                            .and_then(|(_, rest)| rest.split_once(']'))
                            .map(|(path, _)| path)
                            .unwrap_or("/");
                        let handler = item["handler"].as_str().unwrap_or_default();
                        let (class, method_name) = handler
                            .split_once('#')
                            .map(|(class, rest)| {
                                (
                                    class.to_owned(),
                                    rest.split(['(', ' ']).next().unwrap_or("").to_owned(),
                                )
                            })
                            .unwrap_or((handler.to_owned(), String::new()));
                        table.push(LiveRow {
                            cells: vec![method.to_owned(), path.to_owned(), handler.to_owned()],
                            target: Some(LiveTarget::Method {
                                class,
                                method: method_name,
                            }),
                            request: Some(LiveRequest {
                                method: method.to_owned(),
                                path: path.to_owned(),
                            }),
                            ..LiveRow::default()
                        });
                    }
                }
                Ok(table)
            }
            LiveSection::Environment => {
                let mut table = LiveTable::new(&["Source", "Property", "Value"]);
                if let Some(sources) = value["propertySources"].as_array() {
                    for source in sources {
                        table.push(LiveRow {
                            cells: vec![
                                source["name"].as_str().unwrap_or_default().to_owned(),
                                String::new(),
                                String::new(),
                            ],
                            group: true,
                            ..LiveRow::default()
                        });
                        if let Some(properties) = source["properties"].as_object() {
                            for (name, property) in properties {
                                table.push(LiveRow {
                                    cells: vec![
                                        String::new(),
                                        name.clone(),
                                        property["value"].as_str().unwrap_or_default().to_owned(),
                                    ],
                                    depth: 1,
                                    ..LiveRow::default()
                                });
                            }
                        }
                    }
                }
                Ok(table)
            }
        }
    }
}

struct FakeProvider {
    root: PathBuf,
    port: u16,
    /// Waits before the first line, so a test can see the tab before it is ready.
    delayed: bool,
}

impl RunConfigProvider for FakeProvider {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "PetClinic".to_owned(),
            provider: "fixture".to_owned(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        let script = self.root.join("run.cmd");
        let wait = if self.delayed {
            "ping -n 3 127.0.0.1 >nul\r\n"
        } else {
            ""
        };
        let text = format!(
            "@echo off\r\n{wait}echo 2026-01-01T00:00:00.000  INFO 1 --- [main] Tomcat started on port {} (http)\r\necho Started App in 1.2 seconds\r\nping -n 60 127.0.0.1 >nul\r\n",
            self.port
        );
        std::fs::write(&script, text)?;
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c"]).arg(&script),
            grace: Duration::from_secs(1),
        })
    }

    fn live_application(
        &self,
        _: &RunConfig,
        _: &ProjectModel,
    ) -> Option<Arc<dyn LiveApplication>> {
        let settings = self.root.join("src/main/resources/application.properties");
        std::fs::create_dir_all(settings.parent().unwrap()).ok()?;
        std::fs::write(
            &settings,
            "server.port=8080\nmanagement.endpoints.web.exposure.include=health,beans,mappings,env\n",
        )
        .ok()?;
        Some(Arc::new(FakeLive {
            port: self.port,
            seen: std::sync::Mutex::new(Seen::default()),
            settings,
        }))
    }
}

fn open_live(
    cx: &mut TestAppContext,
    name: &str,
    not_exposed: bool,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<RunPanel>,
    Arc<Server>,
    PathBuf,
) {
    let root = crate::window_tests::sample_project(name);
    let server = Arc::new(Server::start(not_exposed));
    let mut project = setup(&root);
    project.providers = vec![Arc::new(FakeProvider {
        root: root.clone(),
        port: server.port,
        delayed: false,
    })];
    let window = open_with(cx, &root, project);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    (window, panel, server, root)
}

fn live_of(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> gpui::Entity<LivePanel> {
    panel
        .read_with(cx, |panel, _| panel.live_panel())
        .expect("the live tab of the configuration")
}

fn wait_ready(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_for(cx, "the live tab", |cx| {
        panel.read_with(cx, |panel, _| panel.live_panel().is_some())
    });
    wait_for(cx, "the readiness line", |cx| {
        panel.read_with(cx, |panel, cx| panel.live_started(cx))
    });
}

fn wait_rows(cx: &mut TestAppContext, live: &gpui::Entity<LivePanel>, section: LiveSection) {
    wait_for(cx, "the live rows", |cx| {
        !live.read_with(cx, |live, _| live.rows(section)).is_empty()
    });
}

fn open_live_tab(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<RunPanel>,
) {
    draw(cx, window);
    // The tab of the live sections is beside Console.
    let _ = bounds_of(cx, window, "run-live-tab");
    panel.update(cx, |panel, cx| panel.show_live(cx));
    draw(cx, window);
    wait_for(cx, "the live view to be drawn", |cx| {
        cx.read(|cx| workspace::view_render_count(cx, "actuator")) > 0
    });
    // The section strip is laid out on the frame after the tab switches; the first
    // section is a row of that frame, so the rest of the strip is on screen with it.
    let _ = bounds_of(cx, window, "actuator-section-0");
    draw(cx, window);
}

fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

fn controller(root: &Path) -> PathBuf {
    let folder = root.join("src/main/demo");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("OwnerController.txt");
    std::fs::write(
        &path,
        "package demo;\n\nclass OwnerController {\n    void findAll() {\n    }\n\n    void create() {\n    }\n}\n",
    )
    .unwrap();
    path
}

/// The symbols the index of the window has when the language is not there.
fn index_controller(cx: &TestAppContext, path: &Path) {
    let text = std::fs::read_to_string(path).unwrap();
    let class_at = text.find("OwnerController").unwrap();
    let find_all = text.find("findAll").unwrap();
    let create = text.find("create").unwrap();
    let index = cx.read(|cx| cx.global::<ActiveIndex>().0.clone());
    index.symbols.write().unwrap().update(
        path.to_path_buf(),
        vec![
            lang_api::Symbol {
                name: "OwnerController".to_owned(),
                kind: lang_api::SymbolKind::Class,
                container: None,
                depth: 0,
                range: class_at..text.len(),
                name_range: class_at..class_at + "OwnerController".len(),
            },
            lang_api::Symbol {
                name: "findAll".to_owned(),
                kind: lang_api::SymbolKind::Method,
                container: Some("OwnerController".to_owned()),
                depth: 1,
                range: find_all..find_all + "findAll".len(),
                name_range: find_all..find_all + "findAll".len(),
            },
            lang_api::Symbol {
                name: "create".to_owned(),
                kind: lang_api::SymbolKind::Method,
                container: Some("OwnerController".to_owned()),
                depth: 1,
                range: create..create + "create".len(),
                name_range: create..create + "create".len(),
            },
        ],
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn tomcat_port_and_the_start_line_mark_the_tab_and_open_every_section(cx: &mut TestAppContext) {
    let (window, panel, server, root) = open_live(cx, "actuator-sections", false);
    let source = controller(&root);
    index_controller(cx, &source);
    crate::window_tests::press(cx, window, "shift-f10");
    wait_ready(cx, &panel);
    let live = live_of(cx, &panel);

    draw(cx, window);
    bounds_of(cx, window, "run-ready-dot");
    open_live_tab(cx, window, &panel);
    wait_rows(cx, &live, LiveSection::Health);
    let health = live.read_with(cx, |live, _| live.table(LiveSection::Health).cloned());
    let health = health.expect("the health table");
    assert_eq!(health.rows[0].cells[0], "UP");
    assert_eq!(health.rows[0].tone, LiveTone::Good);
    assert_eq!(health.rows[1].cells[1], "db");
    assert_eq!(health.rows[2].tone, LiveTone::Bad);

    draw(cx, window);
    click(cx, window, "actuator-section-1");
    wait_rows(cx, &live, LiveSection::Beans);
    let beans = live.read_with(cx, |live, _| live.rows(LiveSection::Beans));
    assert!(beans.iter().any(|row| row.cells[0] == "ownerController"));
    live.update(cx, |live, cx| {
        live.set_query(LiveSection::Beans, "demo.OwnerRepository", cx)
    });
    let filtered = live.read_with(cx, |live, _| live.rows(LiveSection::Beans));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].cells[0], "ownerRepository");

    draw(cx, window);
    click(cx, window, "actuator-section-2");
    wait_rows(cx, &live, LiveSection::Mappings);
    draw(cx, window);
    double_click(cx, window, "actuator-row-0");
    assert_eq!(active_path(cx, window).as_deref(), Some(source.as_path()));
    let view = editor_of(cx, window, &source);
    let offset = view.read_with(cx, |view, _| view.head_offset());
    assert_eq!(
        offset,
        Some(
            std::fs::read_to_string(&source)
                .unwrap()
                .find("findAll")
                .unwrap()
        )
    );

    draw(cx, window);
    click(cx, window, "actuator-http-0");
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let file = state.join("scratches/generated-requests.http");
    wait_for(cx, "the generated request", |_| file.is_file());
    let request = std::fs::read_to_string(&file).unwrap();
    assert!(
        request.contains(&format!("GET http://127.0.0.1:{}/owners", server.port)),
        "{request}"
    );

    draw(cx, window);
    click(cx, window, "actuator-section-3");
    wait_rows(cx, &live, LiveSection::Environment);
    let env = live.read_with(cx, |live, _| live.rows(LiveSection::Environment));
    assert!(
        env.iter()
            .any(|row| row.cells[1] == "spring.datasource.password" && row.cells[2] == "******")
    );

    crate::window_tests::press(cx, window, "ctrl-f2");
    wait_for(cx, "the stopped application", |cx| {
        live.read_with(cx, |live, _| live.stopped())
    });
    draw(cx, window);
    bounds_of(cx, window, "actuator-stopped");
    assert!(
        live.read_with(cx, |live, _| live.table(LiveSection::Environment).is_some()),
        "the last data stays after the process ends"
    );
    assert!(live.read_with(cx, |live, _| live.note(LiveSection::Health).is_none()));
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_404_names_the_exposure_setting_and_opens_the_properties_file(cx: &mut TestAppContext) {
    let (window, panel, _server, _root) = open_live(cx, "actuator-404", true);
    crate::window_tests::press(cx, window, "shift-f10");
    wait_ready(cx, &panel);
    let live = live_of(cx, &panel);
    wait_for(cx, "the not exposed hint", |cx| {
        live.read_with(cx, |live, _| live.note(LiveSection::Health).is_some())
    });
    let note = live
        .read_with(cx, |live, _| live.note(LiveSection::Health).cloned())
        .unwrap();
    assert!(
        note.text
            .contains("management.endpoints.web.exposure.include"),
        "{}",
        note.text
    );
    open_live_tab(cx, window, &panel);
    click(cx, window, "actuator-open-settings");
    let file = note.file.clone().unwrap();
    assert_eq!(active_path(cx, window).as_deref(), Some(file.as_path()));
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_waiting_text_is_there_before_the_application_starts(cx: &mut TestAppContext) {
    let root = crate::window_tests::sample_project("actuator-waiting");
    let server = Arc::new(Server::start(false));
    let mut project = setup(&root);
    project.providers = vec![Arc::new(FakeProvider {
        root: root.clone(),
        port: server.port,
        delayed: true,
    })];
    let window = open_with(cx, &root, project);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    crate::window_tests::press(cx, window, "shift-f10");
    wait_for(cx, "the live tab", |cx| {
        panel.read_with(cx, |panel, _| panel.live_panel().is_some())
    });
    let live = live_of(cx, &panel);
    open_live_tab(cx, window, &panel);
    assert!(
        !live.read_with(cx, |live, _| live.started()),
        "the delayed process has not printed the readiness line yet"
    );
    bounds_of(cx, window, "actuator-waiting");
    crate::window_tests::press(cx, window, "ctrl-f2");
}
