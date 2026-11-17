//! Breakpoints that do not stop the program, the message they print and the expression
//! they evaluate, in the properties of a point and in the gutter of the window without a
//! window. The last part of the file runs the same on the real adapter.
//!
//! `cargo test -p panels window_tests_logpoints`
//!
//! The ignored tests at the end need `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_logpoints -- --ignored --test-threads=1`

use std::sync::Arc;
use std::time::{Duration, Instant};

use dap_client::DebugState;
use runner::RunConfigProvider;
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
    WeakEntity, WindowHandle,
};
use ui_kit::{point, px};
use workspace::Workspace;

use crate::debug::DebugPanel;
use crate::run::ProjectSetup;
use crate::window_tests::{
    bounds_of, click, click_gutter, draw, editor_of, line_height, open_path, press, scratch,
    type_text,
};
use crate::window_tests_debug_actions::{
    debug_panel, end_session, setup, setup_with_log_points, shown,
};
use crate::window_tests_ui::wait_for;

/// How long a real session of the adapter gets to answer.
const LIMIT: Duration = Duration::from_secs(120);

/// Hold Shift and click the gutter of the editor `pane` at a buffer line counted from zero.
fn shift_click_gutter(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    pane: usize,
    line: usize,
) {
    let height = line_height(cx);
    let selector = if pane == 0 {
        "editor-body-0"
    } else {
        "editor-body-1"
    };
    let bounds = bounds_of(cx, window, selector);
    let x = bounds.left() + px(30.);
    let y = bounds.top() + px(height * line as f32 + height / 2.);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(
        point(x, y),
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    cx.run_until_parked();
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(workspace::DEBUG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    // The bottom dock slides in: the click of a test lands on the settled frame.
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    draw(cx, window);
}

/// Turns the wheel over the middle of `selector`.
fn wheel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    rows: f32,
) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position: bounds.center(),
        delta: ScrollDelta::Lines(point(0., -rows)),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Brings a row of the debug tree that stands below the visible part into view.
fn reveal_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    for _ in 0..30 {
        let row = bounds_of(cx, window, selector);
        let viewport = window
            .update(cx, |_, window, _| window.viewport_size())
            .unwrap();
        if row.top() >= px(0.) && row.bottom() <= viewport.height {
            return;
        }
        wheel(cx, window, "scroll-area-debug-tree", 4.);
    }
    panic!("the row of {selector} did not come into view");
}

#[ui_kit::gpui::test]
fn the_properties_of_a_breakpoint_show_suspend_the_message_and_the_expression(
    cx: &mut TestAppContext,
) {
    let fixture = setup(cx, "logpoint-properties");
    let path = fixture.root.join("src").join("main.txt");
    open_path(cx, fixture.window, &path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| panel.toggle_breakpoint(path.clone(), 1, cx));
    editor_of(cx, fixture.window, &path).update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, fixture.window);

    press(cx, fixture.window, "ctrl-shift-f8");
    draw(cx, fixture.window);
    assert!(shown(cx, fixture.window, "breakpoint-popup"));
    for selector in [
        "breakpoint-popup-suspend",
        "breakpoint-popup-hit-message",
        "breakpoint-popup-log-expression",
    ] {
        assert!(shown(cx, fixture.window, selector), "{selector} is there");
    }
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_suspends(&path, 1)),
        Some(true),
        "Suspend is on by default"
    );
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_hit_message(&path, 1)),
        Some(false),
        "the hit message is off by default"
    );

    click(cx, fixture.window, "breakpoint-popup-log-expression");
    draw(cx, fixture.window);
    type_text(cx, fixture.window, "\"i=\" + i");
    press(cx, fixture.window, "enter");
    wait_for(cx, "the logged expression", |cx| {
        debug.read_with(cx, |panel, _| {
            panel.breakpoint_log_expression(&path, 1).map(str::to_owned)
        }) == Some("\"i=\" + i".to_owned())
    });

    // The prompt is an overlay of its own: the popup is opened again for the whole list.
    press(cx, fixture.window, "ctrl-shift-f8");
    draw(cx, fixture.window);
    assert!(shown(cx, fixture.window, "breakpoint-popup"));
    click(cx, fixture.window, "breakpoint-popup-more");
    draw(cx, fixture.window);
    assert!(shown(cx, fixture.window, "breakpoints-dialog"));
    for selector in [
        "breakpoint-dialog-suspend",
        "breakpoint-dialog-hit-message",
        "breakpoint-dialog-log-expression",
    ] {
        assert!(shown(cx, fixture.window, selector), "{selector} is there");
    }
    press(cx, fixture.window, "escape");
}

#[ui_kit::gpui::test]
fn shift_click_puts_a_point_that_does_not_stop_and_says_where_it_was_reached(
    cx: &mut TestAppContext,
) {
    let fixture = setup_with_log_points(cx, "logpoint-shift-click");
    let path = fixture.root.join("src").join("main.txt");
    open_path(cx, fixture.window, &path);
    let view = editor_of(cx, fixture.window, &path);
    draw(cx, fixture.window);

    shift_click_gutter(cx, fixture.window, 0, 0);

    let debug = debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_suspends(&path, 1)),
        Some(false),
        "the point does not stop the program"
    );
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_hit_message(&path, 1)),
        Some(true),
        "the point prints where it was reached"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.muted_breakpoints().to_vec()),
        vec![0],
        "the gutter paints the muted point"
    );

    press(cx, fixture.window, "shift-f9");
    wait_for(cx, "the log message at the adapter", |_| {
        fixture.service.requests().iter().any(|request| {
            request["command"] == "setBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| {
                        list.iter().any(|item| {
                            item["logMessage"].as_str().is_some_and(|message| {
                                message.contains("Breakpoint reached at main.txt:1")
                            })
                        })
                    })
        })
    });
    wait_for(cx, "the line in the console", |cx| {
        debug.read_with(cx, |panel, _| {
            panel
                .console_lines()
                .iter()
                .any(|line| line.contains("Breakpoint reached at main.txt:1"))
        })
    });
    assert!(
        !matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        ),
        "the point did not stop the program"
    );
    end_session(cx, &debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn a_point_that_suspends_prints_its_message_and_the_logged_expression(cx: &mut TestAppContext) {
    let fixture = setup(cx, "logpoint-suspending");
    let path = fixture.root.join("src").join("main.txt");
    open_path(cx, fixture.window, &path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(path.clone(), 42, cx);
        panel.set_hit_message(path.clone(), 42, true, cx);
        panel.set_log_expression(path.clone(), 42, "text".into(), cx);
    });

    press(cx, fixture.window, "shift-f9");
    wait_for(cx, "the stop and its console lines", |cx| {
        let lines = debug.read_with(cx, |panel, _| panel.console_lines());
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        ) && lines
            .iter()
            .any(|line| line.contains("Breakpoint reached at start(main.txt:42)"))
            && lines.iter().any(|line| line == "\"hello\"")
    });
    end_session(cx, &debug, &fixture.service);
}

#[ui_kit::gpui::test]
fn the_settings_of_the_point_survive_a_new_window(cx: &mut TestAppContext) {
    let folder = test_support::TempDir::new("logpoint-state");
    let store =
        Arc::new(store::Store::open(&folder.join("state.sqlite")).expect("the state file opens"));
    let root = folder.join("project");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("Main.txt");
    std::fs::write(&path, "class Main {}\n").unwrap();

    let first = cx.update(|cx| {
        DebugPanel::create(
            root.clone(),
            Some(store.clone()),
            WeakEntity::new_invalid(),
            cx,
        )
    });
    first.update(cx, |panel, cx| {
        panel.toggle_non_suspending_breakpoint(path.clone(), 7, cx);
        panel.set_log_expression(path.clone(), 7, "\"i=\" + i".into(), cx);
    });
    assert_eq!(
        first.read_with(cx, |panel, _| panel.breakpoint_suspends(&path, 7)),
        Some(false)
    );

    // A new window opens the same store: the point comes back with all its settings.
    let second = cx.update(|cx| {
        DebugPanel::create(
            root.clone(),
            Some(store.clone()),
            WeakEntity::new_invalid(),
            cx,
        )
    });
    assert_eq!(
        second.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![7]
    );
    assert_eq!(
        second.read_with(cx, |panel, _| panel.breakpoint_suspends(&path, 7)),
        Some(false),
        "the point still does not stop the program"
    );
    assert_eq!(
        second.read_with(cx, |panel, _| panel.breakpoint_hit_message(&path, 7)),
        Some(true),
        "the message setting stays"
    );
    assert_eq!(
        second.read_with(cx, |panel, _| {
            panel.breakpoint_log_expression(&path, 7).map(str::to_owned)
        }),
        Some("\"i=\" + i".to_owned()),
        "the logged expression stays"
    );
}

// The same on the real debugger of the `window-language` crate.

use window_language::RealLanguage;

use crate::window_tests::wait_for_outline_signal;
use crate::window_tests_real::{
    build_window, close, finish, project, wait_for as wait_real, wait_ready,
};

fn real_setup(
    language: &RealLanguage,
    providers: Vec<Arc<dyn RunConfigProvider>>,
    cache: &str,
) -> ProjectSetup {
    ProjectSetup {
        tool: Some(language.probe_build()),
        providers,
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch(cache),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    }
}

fn wait_for_debug_probe(cx: &mut TestAppContext) {
    let panel = crate::window_tests_debug_actions::run_panel(cx);
    wait_real(cx, "the probe configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "DebugProbe")
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("DebugProbe", cx)));
}

fn stop_at(cx: &mut TestAppContext, debug: &gpui::Entity<DebugPanel>, line: u32) {
    wait_real(cx, "the stop on the line", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line == line)
    });
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_point_without_a_stop_logs_the_loop_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-logpoint");
    let probe = language.install_probe().expect("the probe compiles");
    let setup = real_setup(
        &language,
        language.probe_providers(&probe),
        "logpoint-probe-cache",
    );
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    wait_for_debug_probe(cx);

    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    // The body of the loop of three steps, with the expression of the point.
    debug.update(cx, |panel, cx| {
        panel.toggle_non_suspending_breakpoint(probe.path.clone(), 10, cx);
        panel.set_hit_message(probe.path.clone(), 10, false, cx);
        panel.set_log_expression(probe.path.clone(), 10, "\"i=\" + i".into(), cx);
    });
    press(cx, window, "shift-f9");

    wait_real(cx, "three logged lines", LIMIT, |cx| {
        let lines = debug.read_with(cx, |panel, _| panel.console_lines());
        lines.iter().filter(|line| line.starts_with("i=")).count() >= 3
    });
    wait_real(cx, "the end of the program", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    let lines = debug.read_with(cx, |panel, _| panel.console_lines());
    assert!(
        !lines.iter().any(|line| line.starts_with("Stopped:")),
        "the point did not stop the program: {lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|line| line.starts_with("i=")).count(),
        3,
        "one line per step of the loop: {lines:?}"
    );

    close(cx, window, language, true);
    finish(
        "a_point_without_a_stop_logs_the_loop_on_the_real_adapter",
        started,
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_gutter_watchpoint_stops_on_a_write_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-watch-gutter");
    let probe = language
        .install_watch_probe()
        .expect("the watch probe compiles");
    let setup = real_setup(
        &language,
        language.watch_providers(&probe),
        "watch-gutter-cache",
    );
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    wait_for_debug_probe(cx);

    open_path(cx, window, &probe.path);
    wait_for_outline_signal(cx, window, &probe.path);
    wait_real(cx, "the field declaration in the outline", LIMIT, |cx| {
        editor_of(cx, window, &probe.path).read_with(cx, |view, _| {
            view.declaration_on_line((probe.field_line - 1) as usize)
                .is_some()
        })
    });
    let view = editor_of(cx, window, &probe.path);
    draw(cx, window);
    click_gutter(cx, window, 0, (probe.field_line - 1) as usize);

    let debug = debug_panel(cx);
    assert!(
        debug
            .read_with(cx, |panel, _| panel.data_breakpoint_ids())
            .iter()
            .any(|id| id.contains("counter")),
        "the gutter puts a watchpoint on the field"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.field_watchpoints().to_vec()),
        vec![probe.field_line - 1]
    );

    press(cx, window, "shift-f9");
    stop_at(cx, &debug, probe.write_line);

    close(cx, window, language, true);
    finish(
        "a_gutter_watchpoint_stops_on_a_write_on_the_real_adapter",
        started,
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn add_field_watchpoint_from_the_variables_stops_on_a_write_on_the_real_adapter(
    cx: &mut TestAppContext,
) {
    let started = Instant::now();
    let (root, language) = project("real-watch-variables");
    let probe = language
        .install_watch_probe()
        .expect("the watch probe compiles");
    let setup = real_setup(
        &language,
        language.watch_providers(&probe),
        "watch-variables-cache",
    );
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    wait_for_debug_probe(cx);

    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.break_line, cx)
    });
    press(cx, window, "shift-f9");
    stop_at(cx, &debug, probe.break_line);

    show_debug(cx, window);
    // A program that reads its input brings the console to the front: the test ends the
    // input and looks at the trees, as a user does.
    debug.update(cx, |panel, cx| {
        panel.send_eof(cx);
        panel.show_debugger_front(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let scopes: Vec<i64> = debug.read_with(cx, |panel, _| {
        panel
            .snapshot()
            .scopes
            .iter()
            .map(|scope| scope.reference)
            .collect()
    });
    for scope in &scopes {
        debug.update(cx, |panel, cx| panel.expand(*scope, cx));
    }
    wait_real(cx, "the variables of the stop", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        snapshot
            .scopes
            .iter()
            .any(|scope| snapshot.variables.contains_key(&scope.reference))
    });
    let holder = debug
        .read_with(cx, |panel, _| {
            let snapshot = panel.snapshot();
            snapshot
                .scopes
                .iter()
                .flat_map(|scope| {
                    snapshot
                        .variables
                        .get(&scope.reference)
                        .into_iter()
                        .flatten()
                })
                .find(|variable| variable.name == "holder")
                .cloned()
        })
        .expect("the object of the probe is in the tree");
    assert!(holder.reference != 0, "the object can be opened");
    debug.update(cx, |panel, cx| panel.toggle_expanded(holder.reference, cx));
    wait_real(cx, "the field of the object", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| panel.snapshot().clone())
            .variables
            .get(&holder.reference)
            .is_some_and(|children| children.iter().any(|child| child.name == "value"))
    });
    draw(cx, window);
    let row: &'static str = Box::leak("var-value".to_owned().into_boxed_str());
    reveal_row(cx, window, row);
    let bounds = bounds_of(cx, window, row);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        shown(cx, window, "menu-popup"),
        "the menu of the field is open"
    );
    press(cx, window, "down");
    press(cx, window, "enter");

    // The watchpoint is set while the program is stopped, and the next write stops it.
    wait_real(cx, "the field watchpoint of the object", LIMIT, |cx| {
        debug
            .read_with(cx, |panel, _| panel.data_breakpoint_ids())
            .iter()
            .any(|id| id.contains("value"))
    });
    let stops = debug.read_with(cx, |panel, _| panel.snapshot().stops);
    press(cx, window, "f9");
    wait_real(cx, "the stop on the write of the field", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot.stops > stops
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line == probe.object_write_line)
    });

    close(cx, window, language, true);
    finish(
        "add_field_watchpoint_from_the_variables_stops_on_a_write_on_the_real_adapter",
        started,
    );
}
