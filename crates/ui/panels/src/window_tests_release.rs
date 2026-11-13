//! A window gives everything back when it closes: the workspace, its panels and the
//! watchers of the project folder. These tests run a window like the others but close it
//! before the test ends, so the process keeps no entity and no file watcher of it.

use std::path::Path;

use ui_kit::gpui::{self, TestAppContext};
use workspace::Workspace;

use crate::project::ActiveProjectPanel;
use crate::window_tests::{open_sample_window, wait_for_signal};

/// Closes the window the way its close button does.
fn close(cx: &mut TestAppContext, window: gpui::WindowHandle<Workspace>) {
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

/// The folder of the settings file, which the index watches with a watcher of its own.
fn settings_folder(
    cx: &TestAppContext,
    window: gpui::WindowHandle<Workspace>,
) -> std::path::PathBuf {
    window
        .read_with(cx, |workspace, _| workspace.settings_dir().to_path_buf())
        .unwrap()
}

/// Waits until no watcher of the process watches `root` or a folder under it.
fn wait_for_no_watchers(cx: &mut TestAppContext, root: &Path) {
    wait_for_signal(cx, "the watchers of the closed window", |_| {
        fs_watch::FsWatcher::live_count_under(root) == 0
    });
}

#[ui_kit::gpui::test]
fn closing_the_window_releases_the_workspace_and_its_watchers(cx: &mut TestAppContext) {
    let before = fs_watch::FsWatcher::live_count();
    let (root, window) = open_sample_window(cx, "release");
    let settings = settings_folder(cx, window);
    let workspace = window.root(cx).unwrap().downgrade();
    assert_eq!(
        fs_watch::FsWatcher::live_count_under(&root),
        1,
        "the window watches the project folder"
    );
    assert_eq!(
        fs_watch::FsWatcher::live_count_under(&settings),
        1,
        "the window watches the settings folder"
    );
    eprintln!(
        "live watchers: {before} before the window, {} with it",
        fs_watch::FsWatcher::live_count()
    );

    close(cx, window);

    wait_for_no_watchers(cx, &root);
    wait_for_no_watchers(cx, &settings);
    eprintln!(
        "live watchers after the close: {}",
        fs_watch::FsWatcher::live_count()
    );
    assert!(
        workspace.upgrade().is_none(),
        "the workspace of the closed window still lives"
    );
    assert!(
        cx.read(|cx| cx.try_global::<ActiveProjectPanel>().is_none()),
        "the global of the closed window still holds its project panel"
    );
}

#[ui_kit::gpui::test]
fn the_end_of_the_test_closes_the_window_it_left_open(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "release-quit");
    let settings = settings_folder(cx, window);
    let workspace = window.root(cx).unwrap().downgrade();
    assert_eq!(fs_watch::FsWatcher::live_count_under(&root), 1);

    cx.quit();

    wait_for_no_watchers(cx, &root);
    wait_for_no_watchers(cx, &settings);
    assert!(
        workspace.upgrade().is_none(),
        "the window the test left open outlived the test"
    );
}

#[ui_kit::gpui::test]
fn a_second_window_takes_the_globals_of_the_first(cx: &mut TestAppContext) {
    let (first_root, first) = open_sample_window(cx, "release-first");
    let first_settings = settings_folder(cx, first);
    close(cx, first);
    wait_for_no_watchers(cx, &first_root);
    wait_for_no_watchers(cx, &first_settings);
    assert!(
        cx.read(|cx| cx.try_global::<ActiveProjectPanel>().is_none()),
        "the globals of the first window are gone"
    );

    let (second_root, second) = open_sample_window(cx, "release-second");
    let settings = settings_folder(cx, second);
    let workspace = second.root(cx).unwrap().downgrade();
    assert_eq!(
        fs_watch::FsWatcher::live_count_under(&second_root),
        1,
        "the second window watches its own project folder"
    );
    assert_eq!(fs_watch::FsWatcher::live_count_under(&settings), 1);
    assert!(
        cx.read(|cx| cx.try_global::<ActiveProjectPanel>().is_some()),
        "the second window installs its globals"
    );

    close(cx, second);
    wait_for_no_watchers(cx, &second_root);
    wait_for_no_watchers(cx, &settings);
    assert!(
        workspace.upgrade().is_none(),
        "the workspace of the second window still lives"
    );
}
