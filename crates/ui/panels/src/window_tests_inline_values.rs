//! The values of the stopped frame after the lines of the method, as IDEA shows them,
//! on the mock adapter and on the real debugger at the end.
//!
//! `cargo test -p panels window_tests_inline_values`
//!
//! The ignored tests need `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_inline_values -- --ignored --test-threads=1`

use std::time::{Duration, Instant};

use dap_client::DebugState;
use runner::{AttachPlan, RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::window_tests::{draw, editor_of, open_path, press, scratch, type_text};
use crate::window_tests_debug_actions::{MockService, debug_panel, end_session, run_panel};
use crate::window_tests_lang::FakeLanguage;

/// How long a test waits for the panel or the adapter.
const LIMIT: Duration = Duration::from_secs(20);
/// One turn of the simulated clock: the panels poll every 50 ms.
const TICK: Duration = Duration::from_millis(50);

/// The buffer line the mock adapter stops at, from zero: its frame names line 42.
const STOP_LINE: usize = 41;

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

/// A method long enough that the line the mock adapter stops at names two locals.
fn source() -> String {
    let mut text = String::from("void main() {\n");
    for _ in 0..40 {
        text.push_str("    // a line of the method\n");
    }
    text.push_str("    text items\n}\n");
    text
}

struct Fixture {
    window: WindowHandle<Workspace>,
    view: gpui::Entity<editor_view::EditorView>,
    debug: gpui::Entity<crate::debug::DebugPanel>,
    service: std::sync::Arc<MockService>,
}

/// Opens the window with a language behind it, starts the mock session and waits for the
/// values of its first stop.
fn stopped(cx: &mut TestAppContext, name: &str) -> Fixture {
    let root = crate::window_tests_lang::project(name, &[("src/main.txt", &source())]);
    let window = crate::window_tests_lang::open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let service = MockService::new(path);
    let debug = debug_panel(cx);
    debug
        .update(cx, |panel, cx| {
            panel.start_attach(
                service.clone(),
                "127.0.0.1".into(),
                1,
                "fixture".into(),
                None,
                cx,
            )
        })
        .expect("the mock adapter attaches");
    wait_for(cx, "the first stop with its variables", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. }) && !snapshot.variables.is_empty()
    });
    wait_for(cx, "the inline values", |cx| {
        !view
            .read_with(cx, |view, _| view.debug_values().to_vec())
            .is_empty()
    });
    draw(cx, window);
    Fixture {
        window,
        view,
        debug,
        service,
    }
}

fn tails(cx: &TestAppContext, fixture: &Fixture) -> Vec<editor_view::InlineValue> {
    fixture
        .view
        .read_with(cx, |view, _| view.debug_values().to_vec())
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn focus_panel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    debug: &gpui::Entity<crate::debug::DebugPanel>,
) {
    let focus = debug.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn the_values_of_the_stopped_frame_stand_after_the_lines_that_name_them(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "inline-values-stop");

    let shown = tails(cx, &fixture);
    let line = shown
        .iter()
        .find(|value| value.line == STOP_LINE)
        .unwrap_or_else(|| panic!("no values on the stop line: {shown:?}"));
    assert_eq!(line.text, "text = \"hello\", items = ArrayList (2)");
    assert!(!line.changed, "the first stop has no changed value");
    assert_eq!(shown.len(), 1, "only the stop line names the variables");

    end_session(cx, &fixture.debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn a_long_value_is_cut_and_a_changed_one_is_marked(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "inline-values-changed");
    show_debug(cx, fixture.window);
    fixture.debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, fixture.window);
    focus_panel(cx, fixture.window, &fixture.debug);

    fixture
        .debug
        .update(cx, |panel, _| panel.select_variable_for_test(100, "text"));
    press(cx, fixture.window, "f2");
    draw(cx, fixture.window);
    press(cx, fixture.window, "ctrl-a");
    let long = "x".repeat(80);
    type_text(cx, fixture.window, &long);
    press(cx, fixture.window, "enter");

    // A step takes the new value into a new stop, which marks it as changed.
    press(cx, fixture.window, "f8");
    wait_for(cx, "the changed value of the next stop", |cx| {
        tails(cx, &fixture)
            .iter()
            .any(|value| value.line == STOP_LINE && value.changed && value.text.contains('…'))
    });

    let shown = tails(cx, &fixture);
    let line = shown
        .iter()
        .find(|value| value.line == STOP_LINE)
        .expect("the stop line keeps its values");
    assert!(
        line.text.contains("text = x"),
        "the long value is shown: {}",
        line.text
    );
    let value = line
        .text
        .split("text = ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("the value of text");
    assert_eq!(
        value
            .trim_end_matches('…')
            .chars()
            .filter(|c| *c == 'x')
            .count(),
        60,
        "the value keeps sixty characters: {}",
        line.text
    );
    assert!(line.text.contains("items = ArrayList (2)"));

    end_session(cx, &fixture.debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn resume_takes_the_values_away(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "inline-values-resume");

    // The mock adapter stops again at once, so the values have to go with the command
    // itself, before the snapshot of the next stop can bring them back.
    fixture.debug.update(cx, |panel, cx| panel.resume(cx));
    assert!(
        tails(cx, &fixture).is_empty(),
        "the values stayed after Resume"
    );

    end_session(cx, &fixture.debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn the_setting_hides_the_values(cx: &mut TestAppContext) {
    let fixture = stopped(cx, "inline-values-setting");
    assert!(
        !tails(cx, &fixture).is_empty(),
        "the values are on by default"
    );

    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_show_values_inline = false;
        cx.set_global(settings);
    });
    cx.run_until_parked();
    assert!(
        tails(cx, &fixture).is_empty(),
        "the values go when the setting is off"
    );

    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.debug_show_values_inline = true;
        cx.set_global(settings);
    });
    cx.run_until_parked();
    assert!(
        !tails(cx, &fixture).is_empty(),
        "the values come back when the setting is on"
    );

    end_session(cx, &fixture.debug, &fixture.service);
}

// The remote configuration on the real debugger: a program started by the test with the
// agent waiting for the debugger.

use crate::window_tests_real::{build_window, project, wait_for as wait_real, wait_ready};

/// A configuration that attaches to a JVM another process started.
struct RemoteProbe {
    host: String,
    port: u16,
}

impl RunConfigProvider for RemoteProbe {
    fn id(&self) -> &'static str {
        "remote-probe"
    }

    fn discover(&self, _: &project_model::ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Remote JVM Debug".into(),
            provider: "remote-probe".into(),
            assemble_first: false,
            settings: Default::default(),

            ..RunConfig::default()
        }])
    }

    fn plan(&self, _: &RunConfig, _: &project_model::ProjectModel) -> base::Result<RunPlan> {
        Err("the remote configuration attaches, it starts nothing".into())
    }

    fn attach_plan(
        &self,
        _: &RunConfig,
        _: &project_model::ProjectModel,
    ) -> base::Result<Option<AttachPlan>> {
        Ok(Some(AttachPlan {
            host: self.host.clone(),
            port: self.port,
            name: "Remote JVM Debug".into(),
            project: None,
        }))
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free loopback port")
        .port()
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_remote_configuration_attaches_and_leaves_the_jvm_alive(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-remote-debug");
    let probe = language.install_probe().expect("the probe compiles");
    let port = free_port();
    let mut remote = window_language::RealLanguage::start_remote_probe(probe.classes(), port);

    let setup = crate::run::ProjectSetup {
        tool: Some(language.probe_build()),
        providers: vec![std::sync::Arc::new(RemoteProbe {
            host: "127.0.0.1".into(),
            port,
        })],
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("remote-probe-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    };
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = run_panel(cx);
    wait_real(cx, "the remote configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "Remote JVM Debug")
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("Remote JVM Debug", cx)));
    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });

    press(cx, window, "shift-f9");
    wait_real(cx, "the stop of the remote program", LIMIT, |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });

    // Disconnecting leaves the JVM alone: it is not the debuggee this process started.
    debug.update(cx, |panel, cx| panel.stop(cx));
    wait_real(cx, "the end of the session", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    let alive = remote.try_wait().expect("the state of the remote JVM");
    assert!(alive.is_none(), "the remote JVM ended with the session");

    let _ = remote.kill();
    let _ = remote.wait();
    crate::window_tests_real::close(cx, window, language, true);
    crate::window_tests_real::finish(
        "the_remote_configuration_attaches_and_leaves_the_jvm_alive",
        started,
    );
}
