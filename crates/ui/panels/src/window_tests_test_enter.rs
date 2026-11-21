//! Enter in the window of tests with no row selected selects the first test and opens its
//! source. Nothing is said about a missing selection.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use runner::TestRequest;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::RUN_TOOL;

use crate::window_tests::{active_path, draw, press, wait_for_file_index};
use crate::window_tests_run::wait_for;
use crate::window_tests_test_marks_menu::open_probe;

#[cfg(windows)]
#[ui_kit::gpui::test]
fn enter_in_the_test_window_opens_the_first_test_when_no_row_is_selected(cx: &mut TestAppContext) {
    let probe = open_probe(cx, "test-enter-first", false);
    wait_for_file_index(cx);
    let panel = probe.panel.clone();
    panel.update(cx, |panel, cx| {
        panel.run_tests(
            TestRequest {
                suite: Some("demo.Probe".into()),
                ..TestRequest::default()
            },
            cx,
        )
    });
    wait_for(cx, "the run of the tests", |cx| {
        probe.plain.lock().unwrap().len() == 1 && panel.read_with(cx, |panel, _| panel.idle())
    });
    // Every test passed, so the tree has selected no row when Enter comes.
    probe
        .window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, probe.window);

    press(cx, probe.window, "enter");
    let source = probe.root.join("src").join("Probe.code");
    wait_for(cx, "the source of the first test", |cx| {
        active_path(cx, probe.window).as_deref() == Some(source.as_path())
    });
    let notice = probe
        .window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(title, text)| format!("{title}: {text}"))
        .unwrap_or_default();
    assert!(
        !notice.contains("Cannot open the test"),
        "Enter without a selection says nothing: {notice}"
    );
}
