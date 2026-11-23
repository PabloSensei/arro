//! Recent Changes (Alt+Shift+C) names a version of the previous day "Yesterday", not
//! "1 days ago".

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use local_history::{Limits, LocalHistory};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::history::History;
use crate::window_tests::{draw, open_window, press, sample_project, scratch, wait_for_signal};

/// A history that holds one saved version of `path` taken a day ago.
fn history_with_yesterday(path: &Path) -> Arc<LocalHistory> {
    let dir = scratch("recent-changes-yesterday").join("history");
    fs::create_dir_all(&dir).unwrap();
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .saturating_sub(Duration::from_secs(86_400))
        .as_millis();
    fs::write(
        dir.join("index.log"),
        format!("1\t{milliseconds}\tsaved\t-\t{}\t\n", path.display()),
    )
    .unwrap();
    Arc::new(LocalHistory::open(&dir, Limits::default()).unwrap())
}

#[gpui::test]
fn window_tests_recent_changes_calls_a_day_old_version_yesterday(cx: &mut TestAppContext) {
    let root = sample_project("recent-changes-yesterday");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    // The window opens its own history in the background; the seeded one takes its place
    // after that is done, so no later open replaces it.
    wait_for_signal(cx, "the local history of the window", |cx| {
        cx.update(|cx| crate::history::get(cx).is_some())
    });
    let history = history_with_yesterday(&path);
    cx.update(|cx| cx.set_global(History(Some(history))));

    press(cx, window, "alt-shift-c");
    draw(cx, window);

    let ages = cx.update(|cx| crate::recent_changes::report_ages(cx));
    assert_eq!(
        ages,
        ["Yesterday"],
        "a version from the day before says Yesterday"
    );
}
