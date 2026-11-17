//! The look of the Debug tool window: the buttons of the toolbar stand apart with their
//! icons, the frames and the variables have parts of their own, and an empty session is
//! one gray line instead of headings without content.
//!
//! `cargo test -p panels window_tests_debug_layout`
//!
//! The test at the end runs one whole session on the real adapter and needs `IDE_CORPUS`
//! and a filled package cache:
//!
//! `cargo test -p panels window_tests_debug_layout -- --ignored --test-threads=1`

use std::time::Duration;

use dap_client::DebugState;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DEBUG_TOOL, Workspace};

use crate::window_tests::{bounds_of, click, draw, open_sample_window};
use crate::window_tests_debug_actions::{end_session, setup, stop_at_start};

/// The time a real session of the adapter gets to answer.
const LIMIT: Duration = Duration::from_secs(120);

/// A selector that outlives the loop it is built in.
fn leaked(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    // The bottom dock slides in: the click of a test lands on the settled frame.
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    draw(cx, window);
}

fn absent(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_none()
}

/// Every button of the toolbar, in the order of the IDEA bar.
const BUTTONS: &[&str] = &[
    "debug-rerun",
    "debug-continue",
    "debug-pause",
    "debug-stop",
    "debug-over",
    "debug-into",
    "debug-out",
    "debug-run-to-cursor",
    "debug-drop-frame",
    "debug-evaluate",
    "debug-breakpoints",
];

#[ui_kit::gpui::test]
fn the_toolbar_buttons_stand_apart_inside_the_bar(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "debug-layout-toolbar");
    show_debug(cx, window);
    let toolbar = bounds_of(cx, window, "debug-toolbar");
    let mut seen: Vec<(&str, gpui::Bounds<gpui::Pixels>)> = Vec::new();
    for id in BUTTONS {
        let selector = leaked((*id).to_owned());
        let button = bounds_of(cx, window, selector);
        assert!(
            button.size.width > ui_kit::px(0.) && button.size.height > ui_kit::px(0.),
            "{id} is drawn"
        );
        assert!(
            button.left() >= toolbar.left()
                && button.right() <= toolbar.right()
                && button.top() >= toolbar.top()
                && button.bottom() <= toolbar.bottom(),
            "{id} lies inside the toolbar"
        );
        let icon = bounds_of(cx, window, leaked(format!("{id}-icon")));
        assert!(
            icon.size.width > ui_kit::px(0.) && icon.size.height > ui_kit::px(0.),
            "{id} shows an icon"
        );
        assert!(
            icon.left() >= button.left()
                && icon.right() <= button.right()
                && icon.top() >= button.top()
                && icon.bottom() <= button.bottom(),
            "the icon of {id} lies inside its button"
        );
        for (other, bounds) in &seen {
            assert!(
                !button.intersects(bounds),
                "{id} overlaps the button {other}"
            );
        }
        seen.push((id, button));
    }
}

#[ui_kit::gpui::test]
fn frames_and_variables_have_their_parts_and_do_not_overlap(cx: &mut TestAppContext) {
    let fixture = setup(cx, "debug-layout-parts");
    let window = fixture.window;
    let debug = stop_at_start(cx, &fixture);
    show_debug(cx, window);
    draw(cx, window);

    let frames = bounds_of(cx, window, "debug-frames");
    let variables = bounds_of(cx, window, "debug-variables");
    let splitter = bounds_of(cx, window, "debug-splitter");
    assert!(
        frames.size.width > ui_kit::px(0.) && frames.size.height > ui_kit::px(0.),
        "the frames have a part of their own"
    );
    assert!(
        variables.size.width > ui_kit::px(0.) && variables.size.height > ui_kit::px(0.),
        "the variables have a part of their own"
    );
    assert!(
        splitter.size.height > ui_kit::px(0.),
        "the divider between them is drawn"
    );
    assert!(!frames.intersects(&variables), "the parts do not overlap");
    assert!(
        frames.right() <= splitter.left(),
        "frames end at the divider"
    );
    assert!(
        splitter.right() <= variables.left(),
        "the variables start at the divider"
    );
    assert!(
        bounds_of(cx, window, "debug-watch").size.height > ui_kit::px(0.),
        "the watch list names its button"
    );
    end_session(cx, &debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn rerun_starts_the_same_launch_again(cx: &mut TestAppContext) {
    let fixture = setup(cx, "debug-layout-rerun");
    let window = fixture.window;
    let debug = stop_at_start(cx, &fixture);
    show_debug(cx, window);
    debug.update(cx, |panel, cx| panel.step_over(cx));
    crate::window_tests_ui::wait_for(cx, "a stop after the first one", |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().stops) > 1
    });

    click(cx, window, "debug-rerun");
    crate::window_tests_ui::wait_for(cx, "the first stop of the new session", |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. }) && snapshot.stops == 1
    });
    end_session(cx, &debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn an_empty_session_is_one_line_and_no_headers(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "debug-layout-empty");
    show_debug(cx, window);

    assert!(
        bounds_of(cx, window, "debug-empty").size.height > ui_kit::px(0.),
        "the empty Debugger tab says it in one line"
    );
    for selector in [
        "debug-frames",
        "debug-variables",
        "debug-watch-part",
        "debug-threads",
        "debug-empty-frames",
        "debug-empty-variables",
        "debug-empty-watch",
    ] {
        let selector = leaked(selector.to_owned());
        assert!(
            absent(cx, window, selector),
            "{selector} is not drawn before a session"
        );
    }

    click(cx, window, "debug-tab-console");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "debug-empty-console").size.height > ui_kit::px(0.),
        "the empty console says it in one line"
    );
    assert!(absent(cx, window, "debug-empty"));
}

// The whole session on the real adapter: a breakpoint stops the program, the thread, the
// frames and the variables are there, Step Over moves to the next line, Resume reaches the
// end and the output of the program is in the console.

use std::time::Instant;

use window_language::{Probe, RealLanguage};

use crate::run::ProjectSetup;
use crate::window_tests::{open_path, press, scratch};
use crate::window_tests_debug_actions::{debug_panel, run_panel};
use crate::window_tests_real::{build_window, close, finish, project, wait_for, wait_ready};

fn flow_setup(language: &RealLanguage, probe: &Probe) -> ProjectSetup {
    ProjectSetup {
        tool: Some(language.probe_build()),
        providers: language.flow_providers(probe),
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("debug-layout-flow-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    }
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_whole_real_session_stops_steps_resumes_and_prints(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-debug-flow");
    let probe = language
        .install_flow_probe()
        .expect("the flow probe compiles");
    let setup = flow_setup(&language, &probe);
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let run = run_panel(cx);
    wait_for(cx, "the probe configuration", LIMIT, |cx| {
        run.read_with(cx, |run, _| {
            run.state().configs.iter().any(|name| name == "DebugProbe")
        })
    });
    assert!(run.update(cx, |run, cx| run.select_named("DebugProbe", cx)));
    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });
    draw(cx, window);
    press(cx, window, "shift-f9");

    wait_for(cx, "the stop at the breakpoint", LIMIT, |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });
    wait_for(cx, "the thread, the frames and the scopes", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        !snapshot.threads.is_empty() && !snapshot.frames.is_empty() && !snapshot.scopes.is_empty()
    });
    let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
    assert!(snapshot.current_thread.is_some(), "a thread is selected");
    assert!(
        snapshot
            .frames
            .first()
            .is_some_and(|frame| frame.line == probe.line),
        "the stack stops on the breakpoint line: {:?}",
        snapshot.frames.first()
    );
    assert!(
        snapshot
            .frames
            .iter()
            .any(|frame| frame.name.contains("main")),
        "the stack reaches main: {:?}",
        snapshot.frames
    );

    let scopes: Vec<i64> = snapshot
        .scopes
        .iter()
        .map(|scope| scope.reference)
        .collect();
    for scope in &scopes {
        debug.update(cx, |panel, cx| panel.expand(*scope, cx));
    }
    wait_for(cx, "the local variables", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        let variables: Vec<&dap_client::Variable> = snapshot
            .scopes
            .iter()
            .flat_map(|scope| {
                snapshot
                    .variables
                    .get(&scope.reference)
                    .into_iter()
                    .flatten()
            })
            .collect();
        !variables.is_empty()
    });
    let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
    let variables: Vec<dap_client::Variable> = snapshot
        .scopes
        .iter()
        .flat_map(|scope| {
            snapshot
                .variables
                .get(&scope.reference)
                .into_iter()
                .flatten()
        })
        .cloned()
        .collect();
    let total = variables
        .iter()
        .find(|variable| variable.name == "total")
        .expect("the local int is in the scope");
    assert_eq!(total.value, "0", "the int has its value");
    assert_eq!(total.reference, 0, "the int has no children");
    let items = variables
        .iter()
        .find(|variable| variable.name == "items")
        .expect("the object is in the scope");
    assert!(
        items.reference != 0 && !items.type_name.is_empty(),
        "the object can be opened: {items:?}"
    );
    let items_reference = items.reference;
    debug.update(cx, |panel, cx| panel.toggle_expanded(items_reference, cx));
    wait_for(cx, "the children of the object", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().clone())
            .variables
            .get(&items_reference)
            .is_some_and(|children| !children.is_empty())
    });

    // Step Over leaves the call of `add` and stops on the next line.
    debug.update(cx, |panel, cx| panel.step_over(cx));
    wait_for(cx, "the step to the next line", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line == probe.line + 1)
    });

    // Resume reaches the end: no other breakpoint is in the way.
    press(cx, window, "f9");
    wait_for(cx, "the end of the program", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    wait_for(cx, "the program output", LIMIT, |cx| {
        let console = debug.read_with(cx, |panel, _| panel.snapshot().console.clone());
        console
            .iter()
            .any(|line| line.text.contains("probe-stdout"))
            && console.iter().any(|line| line.text.contains("items=2"))
    });

    // The output is on the Console tab.
    click(cx, window, "debug-tab-console");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "debug-console").size.height > ui_kit::px(0.),
        "the console shows the output of the program"
    );

    close(cx, window, language, true);
    finish(
        "the_whole_real_session_stops_steps_resumes_and_prints",
        started,
    );
}
