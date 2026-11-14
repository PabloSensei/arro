//! Method breakpoints, field watchpoints and Drop Frame in the window without a window:
//! the gutter of a declaration, Add Field Watchpoint in the Variables tree and the
//! restart of a frame on the mock adapter.
//!
//! `cargo test -p panels window_tests_breakpoint_kinds`
//!
//! The adapter of these tests lists `setFunctionBreakpoints`, `setDataBreakpoints`,
//! `dataBreakpointInfo` and `restartFrame` among its capabilities.

use editor_view::{LineDeclaration, OutlineKind};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, click_gutter, draw, editor_of, open_path, press};
use crate::window_tests_debug_actions::{
    debug_panel, end_session, setup, setup_with_breakpoint_kinds, shown, stop_at_start,
};

const TEXT: &str = "class Probe {\n    int value;\n    void work() {\n    }\n}\n";

fn declarations() -> Vec<LineDeclaration> {
    vec![
        LineDeclaration {
            line: 0,
            name: "Probe".into(),
            owner: None,
            kind: OutlineKind::Class,
        },
        LineDeclaration {
            line: 1,
            name: "value".into(),
            owner: Some("demo.Probe".into()),
            kind: OutlineKind::Field,
        },
        LineDeclaration {
            line: 2,
            name: "work".into(),
            owner: Some("demo.Probe".into()),
            kind: OutlineKind::Method,
        },
    ]
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
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
    visual.simulate_event(ui_kit::gpui::ScrollEvent {
        position: bounds.center(),
        delta: ui_kit::gpui::ScrollDelta::Lines(ui_kit::point(0., -rows)),
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
        if row.top() >= ui_kit::px(0.) && row.bottom() <= viewport.height {
            return;
        }
        wheel(cx, window, "scroll-area-debug-tree", 4.);
    }
    panic!("the row of {selector} did not come into view");
}

#[ui_kit::gpui::test]
fn a_gutter_click_on_a_method_puts_a_method_breakpoint(cx: &mut TestAppContext) {
    let setup = setup_with_breakpoint_kinds(cx, "method-breakpoint");
    let path = setup.root.join("src").join("main.txt");
    std::fs::write(&path, TEXT).unwrap();
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| {
        view.set_line_declarations(declarations(), cx)
    });
    draw(cx, setup.window);

    click_gutter(cx, setup.window, 0, 2);

    let debug = debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.function_breakpoint_names()),
        vec!["demo.Probe#work".to_owned()]
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.function_breakpoints().to_vec()),
        vec![2],
        "the gutter paints the method breakpoint on its line"
    );

    press(cx, setup.window, "shift-f9");
    let started = std::time::Instant::now();
    loop {
        let requests = setup.service.requests();
        if requests.iter().any(|request| {
            request["command"] == "setFunctionBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|item| item["name"] == "demo.Probe#work"))
        }) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the method breakpoint did not reach the adapter"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    stop_at_start(cx, &setup);
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn a_gutter_click_on_a_field_puts_a_field_watchpoint(cx: &mut TestAppContext) {
    let setup = setup_with_breakpoint_kinds(cx, "field-watchpoint");
    let path = setup.root.join("src").join("main.txt");
    std::fs::write(&path, TEXT).unwrap();
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| {
        view.set_line_declarations(declarations(), cx)
    });
    draw(cx, setup.window);

    click_gutter(cx, setup.window, 0, 1);

    let debug = debug_panel(cx);
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.data_breakpoint_ids()),
        vec!["demo.Probe#value".to_owned()]
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.field_watchpoints().to_vec()),
        vec![1],
        "the gutter paints the eye on the field line"
    );

    press(cx, setup.window, "shift-f9");
    let started = std::time::Instant::now();
    loop {
        let requests = setup.service.requests();
        if requests.iter().any(|request| {
            request["command"] == "setDataBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| {
                        list.iter().any(|item| {
                            item["dataId"] == "demo.Probe#value" && item["accessType"] == "write"
                        })
                    })
        }) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the field watchpoint did not reach the adapter"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    stop_at_start(cx, &setup);
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn add_field_watchpoint_from_the_variables_tree_asks_the_adapter(cx: &mut TestAppContext) {
    let setup = setup_with_breakpoint_kinds(cx, "field-from-variables");
    let debug = stop_at_start(cx, &setup);
    debug.update(cx, |panel, cx| panel.expand(100, cx));
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "var-text"));
    reveal_row(cx, setup.window, "var-text");

    right_click(cx, setup.window, "var-text");
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "menu-popup"), "the menu is open");
    press(cx, setup.window, "down");
    press(cx, setup.window, "enter");

    let started = std::time::Instant::now();
    loop {
        let requests = setup.service.requests();
        let asked = requests
            .iter()
            .any(|request| request["command"] == "dataBreakpointInfo");
        let sent = requests.iter().any(|request| {
            request["command"] == "setDataBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| list.iter().any(|item| item["dataId"] == "demo.Probe#text"))
        });
        if asked && sent {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the field watchpoint did not reach the adapter: {requests:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    assert!(
        debug
            .read_with(cx, |panel, _| panel.data_breakpoint_ids())
            .contains(&"demo.Probe#text".to_owned())
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn drop_frame_restarts_the_selected_frame_when_the_adapter_allows_it(cx: &mut TestAppContext) {
    let setup = setup_with_breakpoint_kinds(cx, "drop-frame");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);

    // The outermost frame has no caller to return to.
    debug.update(cx, |panel, cx| panel.select_frame(12, cx));
    let started = std::time::Instant::now();
    loop {
        let selected = debug.read_with(cx, |panel, _| panel.snapshot().selected_frame);
        if selected == Some(12) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the last frame was not selected"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    assert!(
        !debug.read_with(cx, |panel, _| panel.restart_frame_enabled()),
        "the outermost frame cannot be dropped"
    );

    debug.update(cx, |panel, cx| panel.select_frame(11, cx));
    let started = std::time::Instant::now();
    loop {
        let selected = debug.read_with(cx, |panel, _| panel.snapshot().selected_frame);
        if selected == Some(11) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the first frame was not selected"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    assert!(debug.read_with(cx, |panel, _| panel.restart_frame_enabled()));

    reveal_row(cx, setup.window, "frame-11");
    right_click(cx, setup.window, "frame-11");
    draw(cx, setup.window);
    assert!(
        shown(cx, setup.window, "menu-popup") && shown(cx, setup.window, "menu-0"),
        "the frame menu offers Drop Frame"
    );
    press(cx, setup.window, "escape");

    click(cx, setup.window, "debug-drop-frame");
    let started = std::time::Instant::now();
    loop {
        if setup.service.requests().iter().any(|request| {
            request["command"] == "restartFrame" && request["arguments"]["frameId"] == 11
        }) {
            break;
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "the frame restart did not reach the adapter"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(50));
        cx.run_until_parked();
    }
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn drop_frame_is_unavailable_when_the_adapter_does_not_support_it(cx: &mut TestAppContext) {
    let setup = setup(cx, "drop-frame-unsupported");
    let debug = stop_at_start(cx, &setup);
    show_debug(cx, setup.window);

    assert!(
        !debug.read_with(cx, |panel, _| panel.restart_frame_enabled()),
        "the adapter did not offer the request"
    );
    click(cx, setup.window, "debug-drop-frame");
    cx.run_until_parked();
    assert!(
        !setup
            .service
            .requests()
            .iter()
            .any(|request| request["command"] == "restartFrame"),
        "a disabled button sends nothing"
    );
    end_session(cx, &debug, &setup.service);
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
