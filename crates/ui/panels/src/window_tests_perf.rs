//! The Performance Monitor without a real window: Find Action opens it, a key typed in
//! the editor reaches its latency table, and Copy as Text puts the table on the clipboard.

use std::time::{Duration, Instant};

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::PERFORMANCE_TOOL;

use crate::performance::ActivePerformance;
use crate::window_tests::{click, draw, open_palette, open_path, open_sample_window, type_text};

/// How long a test waits for the panel timer.
const LIMIT: Duration = Duration::from_secs(20);

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
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

#[ui_kit::gpui::test]
fn find_action_shows_the_performance_panel_and_copy_writes_the_table(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "performance");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    type_text(cx, window, "abc");
    draw(cx, window);

    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, "performance monitor");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "enter");
    cx.run_until_parked();
    draw(cx, window);

    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(PERFORMANCE_TOOL))
        .unwrap();
    assert!(opened, "Show Performance Monitor opens the tool window");

    let panel = cx.update(|cx| cx.global::<ActivePerformance>().0.clone());
    wait_for(cx, "the keystroke row", |cx| {
        panel.read_with(cx, |panel, _| panel.count("editor.keystroke") > 0)
    });
    draw(cx, window);
    let report = panel.read_with(cx, |panel, _| panel.report());
    let line = report
        .lines()
        .find(|line| line.starts_with("editor.keystroke\t"))
        .expect("the table has the editor key row");
    let events: u64 = line
        .split('\t')
        .nth(4)
        .and_then(|count| count.parse().ok())
        .expect("the row has a count");
    assert!(events > 0, "the row counts the key that was typed");

    click(cx, window, "perf-copy");
    let copied = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy as Text writes the clipboard");
    assert!(copied.contains("Performance Monitor"), "{copied}");
    assert!(copied.contains("editor.keystroke"), "{copied}");
    assert!(copied.contains("Memory by process"), "{copied}");
    assert!(copied.contains("Memory by subsystem"), "{copied}");
}
