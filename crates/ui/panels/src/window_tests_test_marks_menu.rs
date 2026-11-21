//! The mark of a test opens a menu with Run and Debug, as the mark of a program does. The
//! Run row runs the test as it always did, and Alt+click starts it under the debugger.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{RunPlan, TestDebug, TestListen, TestProvider, TestRequest, TestSite};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{draw, editor_of, open_path, sample_project};
use crate::window_tests_m11::ReloadProbe;
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The test requests a provider received, from a plain run or from a debug run.
type Requests = Arc<Mutex<Vec<TestRequest>>>;

/// A class with one test on line 1, from zero.
const SOURCE: &str = "class Probe {\n    void works() {\n        check();\n    }\n}\n";

/// Runs the one passing test of the class and keeps every request, plain and debugged.
struct PassingTests {
    plain: Requests,
    debugged: Requests,
}

impl TestProvider for PassingTests {
    fn plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
    ) -> base::Result<RunPlan> {
        self.plain
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        announce_passing(listen.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn debug_plan(
        &self,
        _: &ProjectModel,
        request: &TestRequest,
        listen: &TestListen,
        _: &TestDebug,
    ) -> base::Result<RunPlan> {
        self.debugged
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request.clone());
        announce_passing(listen.clone());
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

/// Plays the listener of a test run: one class with one passing test.
fn announce_passing(listen: TestListen) {
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

/// The test on the line of `void works`, one-based as the sites are.
fn find_tests(text: &str, _: &Path) -> Vec<TestSite> {
    text.lines()
        .position(|line| line.contains("void works"))
        .map(|index| {
            vec![TestSite {
                suite: "demo.Probe".into(),
                case: Some("works".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

/// The project of a probe test: its root, its requests and its window.
pub(crate) struct Probe {
    pub(crate) root: PathBuf,
    pub(crate) plain: Requests,
    pub(crate) debugged: Requests,
    pub(crate) window: WindowHandle<Workspace>,
    pub(crate) panel: gpui::Entity<RunPanel>,
}

/// A project with the class of the test open in a window whose test provider records
/// the requests. The source is written before the window opens, so the project lists it.
pub(crate) fn open_probe(cx: &mut TestAppContext, name: &str, with_debugger: bool) -> Probe {
    let root = sample_project(name);
    std::fs::write(root.join("src").join("Probe.code"), SOURCE).unwrap();
    let plain = Arc::new(Mutex::new(Vec::new()));
    let debugged = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(PassingTests {
        plain: plain.clone(),
        debugged: debugged.clone(),
    }));
    setup.test_sites = find_tests;
    if with_debugger {
        setup.debug = Some(ReloadProbe::new());
    }
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    Probe {
        root,
        plain,
        debugged,
        window,
        panel,
    }
}

fn menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.menu_entries().is_some_and(|entries| {
                    entries.iter().any(|entry| entry.label.as_ref() == label)
                })
            })
            .unwrap()
    });
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    click_selector(cx, window, &format!("menu-{index}"));
}

/// The editor with the class open and its mark on the line of the test.
fn open_mark(cx: &mut TestAppContext, probe: &Probe) -> gpui::Entity<editor_view::EditorView> {
    let source = probe.root.join("src").join("Probe.code");
    open_path(cx, probe.window, &source);
    let view = editor_of(cx, probe.window, &source);
    wait_for(cx, "the mark of the test", |cx| {
        view.read_with(cx, |view, _| !view.run_marks().is_empty())
    });
    view
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_test_mark_opens_a_menu_with_run_and_debug_and_runs_nothing(cx: &mut TestAppContext) {
    let probe = open_probe(cx, "test-marks-menu", false);
    let view = open_mark(cx, &probe);

    probe
        .window
        .update(cx, |workspace, window, cx| {
            crate::marks::run_mark_clicked(&view, 1, workspace, window, cx);
        })
        .unwrap();
    draw(cx, probe.window);

    assert_eq!(
        menu_labels(cx, probe.window),
        [
            "Run 'works()'",
            "Debug 'works()'",
            "Modify Run Configuration..."
        ],
        "the mark of a test offers Run and Debug, as a program does"
    );
    let shortcuts: Vec<Option<String>> = probe
        .window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.shortcut.as_ref().map(|text| text.to_string()))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap();
    let run_key = keymap::shortcut_label("ctrl-shift-f10", keymap::platform());
    assert_eq!(
        shortcuts[0].as_deref(),
        Some(run_key.as_str()),
        "the Run row shows the key of Run a test, Ctrl+Shift+F10"
    );
    assert!(
        probe.plain.lock().unwrap().is_empty() && probe.debugged.lock().unwrap().is_empty(),
        "a click on the mark starts nothing"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_run_row_runs_the_test_as_it_always_did(cx: &mut TestAppContext) {
    let probe = open_probe(cx, "test-marks-run", false);
    let view = open_mark(cx, &probe);
    probe
        .window
        .update(cx, |workspace, window, cx| {
            crate::marks::run_mark_clicked(&view, 1, workspace, window, cx);
        })
        .unwrap();
    draw(cx, probe.window);

    click_menu(cx, probe.window, "Run 'works()'");
    let panel = probe.panel.clone();
    wait_for(cx, "the run of the test", |cx| {
        probe.plain.lock().unwrap().len() == 1 && panel.read_with(cx, |panel, _| panel.idle())
    });
    let first = probe.plain.lock().unwrap()[0].clone();
    assert_eq!(first.suite.as_deref(), Some("demo.Probe"));
    assert_eq!(first.case.as_deref(), Some("works"));
    assert!(
        probe.debugged.lock().unwrap().is_empty(),
        "Run does not start the debugger"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn alt_click_on_the_test_mark_runs_it_under_the_debugger(cx: &mut TestAppContext) {
    let probe = open_probe(cx, "test-marks-debug", true);
    let view = open_mark(cx, &probe);
    // The debug start waits for its port on the executor, so the action is called directly
    // with the same click the mark makes for Alt.
    probe
        .window
        .update(cx, |workspace, window, cx| {
            crate::marks::debug_mark_clicked(&view, 1, workspace, window, cx);
        })
        .unwrap();

    let panel = probe.panel.clone();
    wait_for(cx, "the debug run of the test", |cx| {
        probe.debugged.lock().unwrap().len() == 1
            && panel.read_with(cx, |panel, _| {
                panel
                    .test_report()
                    .iter()
                    .any(|line| line.contains("works"))
            })
    });
    assert!(
        probe.plain.lock().unwrap().is_empty(),
        "Alt+click plans the debug run, not a plain one"
    );
    let debugged = probe.debugged.lock().unwrap()[0].clone();
    assert_eq!(debugged.case.as_deref(), Some("works"));
}
