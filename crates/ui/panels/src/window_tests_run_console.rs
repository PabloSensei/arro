//! The console of Run and Debug as IDEA has it: a frame of a stack trace opens its line, a
//! compiler message opens its file, Ctrl+F searches the output, the funnel hides lines and
//! the test tree runs one test. Mute Breakpoints keeps the points and sends the adapter an
//! empty list.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DEBUG_TOOL, RUN_TOOL, Workspace};

use crate::window_tests::{
    active_path, bounds_of, click, draw, editor_of, open_path, open_sample_window, open_window,
    press, sample_project, type_text, wait_for_file_index, wait_for_signal,
};
use crate::window_tests_run::{
    EchoRun, FixtureTests, MockService, debug_panel, long_image, open_with, run_panel, setup,
    snapshot, test_source, wait_for,
};

fn show_run(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
}

fn show_debug(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DEBUG_TOOL, window, cx)
        })
        .unwrap();
}

/// Puts lines into the console of the Run tool window the way a program writes them.
fn seed(cx: &mut TestAppContext, window: WindowHandle<Workspace>, lines: &[&str]) {
    let panel = run_panel(cx);
    window
        .update(cx, |_, _, cx| {
            panel.update(cx, |panel, cx| {
                panel.seed_console_lines(lines);
                cx.notify();
            });
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_frame_of_the_run_console_opens_its_line(cx: &mut TestAppContext) {
    let root = sample_project("console-frame");
    let folder = root.join("src").join("demo");
    fs::create_dir_all(&folder).unwrap();
    let source = folder.join("Vitest.code");
    fs::write(&source, "class Vitest {\n    void fails() {}\n}\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    show_run(cx, window);
    wait_for_file_index(cx);
    seed(
        cx,
        window,
        &[
            "demo.IllegalStateException: boom",
            "\tat demo.Vitest.fails(Vitest.code:2)",
            "Process finished with exit code 1",
        ],
    );

    click(cx, window, "run-console-line-1");
    wait_for_signal(cx, "the file of the frame", |cx| {
        active_path(cx, window).as_deref() == Some(source.as_path())
    });
    let view = editor_of(cx, window, &source);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(1),
        "the caret is on the line of the frame"
    );
}

#[ui_kit::gpui::test]
fn a_compiler_message_of_the_run_console_opens_its_file(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "console-compiler");
    let folder = root.join("src").join("demo");
    fs::create_dir_all(&folder).unwrap();
    let source = folder.join("Vitest.code");
    fs::write(&source, "class Vitest {\n    void fails() {}\n}\n").unwrap();
    show_run(cx, window);
    seed(
        cx,
        window,
        &["src/demo/Vitest.code:3: error: cannot find the name"],
    );

    click(cx, window, "run-console-line-0");
    wait_for_signal(cx, "the file of the message", |cx| {
        active_path(cx, window).as_deref() == Some(source.as_path())
    });
    let view = editor_of(cx, window, &source);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(2),
        "the caret is on the line of the message"
    );
}

#[ui_kit::gpui::test]
fn ctrl_f_finds_the_text_of_the_run_console_and_counts_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "console-search");
    show_run(cx, window);
    seed(cx, window, &["alpha", "boom one", "beta", "boom two"]);

    press(cx, window, "ctrl-f");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "run-console-search").size.height > ui_kit::px(0.),
        "Ctrl+F shows the search bar"
    );
    type_text(cx, window, "boom");
    let panel = run_panel(cx);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.console_search_status()),
        "1/2",
        "the counter names the match and the count"
    );

    press(cx, window, "f3");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.console_search_status()),
        "2/2",
        "F3 goes to the next match"
    );
    press(cx, window, "shift-f3");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.console_search_status()),
        "1/2",
        "Shift+F3 goes back"
    );
}

#[ui_kit::gpui::test]
fn the_filter_of_the_run_console_hides_the_lines_without_its_text(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "console-filter");
    show_run(cx, window);
    seed(cx, window, &["alpha", "boom", "ALPHA two"]);
    let panel = run_panel(cx);

    click(cx, window, "run-console-filter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "run-console-filter-row").size.height > ui_kit::px(0.),
        "the funnel shows the field"
    );
    type_text(cx, window, "alpha");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.visible_console_lines()),
        ["alpha", "ALPHA two"],
        "only the lines with the text stay"
    );
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("run-console-line-1").is_none(),
        "the line the filter hides is not drawn"
    );
    assert!(
        visual.debug_bounds("run-console-line-2").is_some(),
        "the line the filter keeps is drawn"
    );
}

#[ui_kit::gpui::test]
fn ctrl_f_finds_the_text_of_the_debug_console(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "debug-console-search");
    show_debug(cx, window);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.push_output("alpha", false, cx);
        panel.push_output("boom here", false, cx);
        panel.push_output("boom again", false, cx);
    });
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "ctrl-f");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "debug-console-search").size.height > ui_kit::px(0.),
        "Ctrl+F shows the search bar of the debug console"
    );
    type_text(cx, window, "boom");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.console_search_status()),
        "1/2"
    );

    click(cx, window, "debug-console-filter");
    draw(cx, window);
    type_text(cx, window, "alpha");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.visible_console_lines()),
        ["alpha"],
        "the funnel of the debug console keeps only the lines with its text"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn mute_breakpoints_sends_an_empty_list_and_brings_the_points_back(cx: &mut TestAppContext) {
    let root = sample_project("console-mute");
    let source = root.join("src").join("main.txt");
    let service = MockService::new();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    setup.debug = Some(service.clone());
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    let debug = debug_panel(cx);

    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-f8");
    wait_for(cx, "the breakpoint", |cx| {
        debug.read_with(&*cx, |panel, _| panel.breakpoint_lines(&source)) == vec![2]
    });

    press(cx, window, "shift-f9");
    wait_for(cx, "the breakpoint stop", |cx| {
        matches!(snapshot(cx).state, dap_client::DebugState::Stopped { .. })
    });
    assert!(
        service
            .breakpoint_requests()
            .last()
            .and_then(|request| request["arguments"]["breakpoints"].as_array())
            .is_some_and(|list| !list.is_empty()),
        "the session started with the breakpoint"
    );

    show_debug(cx, window);
    draw(cx, window);
    click(cx, window, "debug-mute");
    wait_for(cx, "the empty list of breakpoints", |_| {
        service
            .breakpoint_requests()
            .last()
            .and_then(|request| request["arguments"]["breakpoints"].as_array())
            .is_some_and(|list| list.is_empty())
    });
    assert!(debug.read_with(cx, |panel, _| panel.muted()));
    wait_for(cx, "the grey gutter", |cx| {
        view.read_with(cx, |view, _| view.disabled_breakpoints().contains(&1))
    });

    click(cx, window, "debug-mute");
    wait_for(cx, "the breakpoints back", |_| {
        service
            .breakpoint_requests()
            .last()
            .and_then(|request| request["arguments"]["breakpoints"].as_array())
            .is_some_and(|list| !list.is_empty())
    });
    assert!(!debug.read_with(cx, |panel, _| panel.muted()));
    wait_for(cx, "the plain gutter", |cx| {
        view.read_with(cx, |view, _| !view.disabled_breakpoints().contains(&1))
    });

    press(cx, window, "ctrl-f2");
    wait_for(cx, "the end of the session", |cx| {
        snapshot(cx).state == dap_client::DebugState::Terminated
    });
    service.join();
}

/// The test sites of the fixture source: one case in one class.
fn find_tests(text: &str, _: &Path) -> Vec<runner::TestSite> {
    text.lines()
        .position(|line| line.contains("void fails"))
        .map(|index| {
            vec![runner::TestSite {
                suite: "demo.Vitest".into(),
                case: Some("fails".into()),
                line: index as u32 + 1,
            }]
        })
        .unwrap_or_default()
}

#[cfg(windows)]
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[cfg(windows)]
fn click_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
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
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
    draw(cx, window);
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn run_from_the_row_menu_of_the_test_tree_runs_only_that_test(cx: &mut TestAppContext) {
    let root = sample_project("console-test-menu");
    let source = test_source(&root);
    let image = long_image("console-test-menu");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.tests = Some(Arc::new(FixtureTests {
        requests: requests.clone(),
        image: image.clone(),
    }));
    setup.test_sites = find_tests;
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);

    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    press(cx, window, "ctrl-shift-f10");
    wait_for(cx, "the first run", |cx| {
        runs(&requests).len() == 1 && panel.read_with(cx, |panel, _| panel.idle())
    });

    show_run(cx, window);
    draw(cx, window);
    right_click(cx, window, "test-row-e/c/fails");
    click_menu_row(cx, window, "Run 'fails()'");
    wait_for(cx, "the run of the chosen test", |cx| {
        runs(&requests).len() == 2 && panel.read_with(cx, |panel, _| panel.idle())
    });
    let second = runs(&requests)[1].clone();
    assert_eq!(second.suite.as_deref(), Some("demo.Vitest"));
    assert_eq!(second.case.as_deref(), Some("fails"));
    assert!(
        second.cases.is_empty(),
        "the menu ran the one test and not the whole class"
    );
    let _ = fs::remove_file(&image);
}

fn runs(requests: &Arc<Mutex<Vec<runner::TestRequest>>>) -> Vec<runner::TestRequest> {
    requests
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}
