//! The first open of a project: the README it shows and the one tab a file keeps however
//! the case of its path is typed.

use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use editor_view::PreviewMode;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::window_tests::{editor_of, open_window, scratch};

/// A project of the test with a file that is not a README and, when asked, a README.
fn project(name: &str, readme: Option<(&str, &str)>) -> std::path::PathBuf {
    let root = scratch(name);
    fs::write(root.join("notes.txt"), "notes\n").unwrap();
    if let Some((file, text)) = readme {
        fs::write(root.join(file), text).unwrap();
    }
    root
}

/// A launch of a window of `root`, for a second window of a test that already has one.
fn launch_of(root: &Path) -> Launch {
    let state_dir = scratch("first-open-second-state");
    Launch {
        root: root.to_path_buf(),
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    }
}

/// The files of the open tabs, pane by pane.
fn open_paths(cx: &TestAppContext, window: gpui::WindowHandle<Workspace>) -> Vec<Vec<String>> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .open_tabs()
                .into_iter()
                .map(|pane| {
                    pane.into_iter()
                        .map(|path| path.display().to_string())
                        .collect()
                })
                .collect()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_first_open_of_a_project_shows_its_readme_as_a_preview(cx: &mut TestAppContext) {
    let root = project(
        "first-open-readme",
        Some(("README.md", "# Read me\n\nWelcome.\n")),
    );
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let readme = root.join("README.md");
    let tabs = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs.len(), 1, "the first open shows one group");
    assert_eq!(tabs[0].len(), 1, "the README is the only tab");
    assert_eq!(
        tabs[0][0].file_name(),
        Some(OsStr::new("README.md")),
        "the tab holds the README of the project"
    );
    let view = editor_of(cx, window, &readme);
    assert_eq!(
        view.read_with(cx, |view, _| view.preview_mode()),
        Some(PreviewMode::PreviewOnly),
        "the README opens as the rendered document"
    );
}

#[ui_kit::gpui::test]
fn a_project_opened_again_is_not_forced_to_show_the_readme(cx: &mut TestAppContext) {
    let root = project(
        "first-open-again",
        Some(("README.md", "# Read me\n\nWelcome.\n")),
    );
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    assert!(
        !open_paths(cx, window)[0].is_empty(),
        "the first open showed the README"
    );

    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the test window has a store");
    // Closing the README and the window writes the layout, so the project has a saved
    // state without tabs now.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_active_tab(window, cx);
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let second = cx.add_window(move |window, cx| {
        Workspace::new_for_test(
            launch_of(&root),
            Layout::default(),
            Some(store),
            Vec::new(),
            window,
            cx,
        )
    });
    second
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert!(
        open_paths(cx, second)
            .into_iter()
            .flatten()
            .next()
            .is_none(),
        "an opened project keeps its saved tabs without the README"
    );
}

#[ui_kit::gpui::test]
fn a_project_without_a_readme_opens_no_tab(cx: &mut TestAppContext) {
    let root = project("first-open-none", None);
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert!(
        open_paths(cx, window)
            .into_iter()
            .flatten()
            .next()
            .is_none(),
        "nothing opens when the project has no README"
    );
}

#[ui_kit::gpui::test]
fn the_readme_stays_closed_when_the_setting_is_off(cx: &mut TestAppContext) {
    let root = project(
        "first-open-off",
        Some(("README.md", "# Read me\n\nWelcome.\n")),
    );
    let window = open_window(cx, root);
    cx.update(|cx| {
        cx.set_global(Settings {
            open_readme_on_first_open: false,
            ..Settings::default()
        })
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert!(
        open_paths(cx, window)
            .into_iter()
            .flatten()
            .next()
            .is_none(),
        "the setting keeps the README closed"
    );
}

#[cfg(any(windows, target_os = "macos"))]
#[ui_kit::gpui::test]
fn one_tab_per_file_whatever_the_case_of_its_path(cx: &mut TestAppContext) {
    let root = project("first-open-case", Some(("README.md", "# Read me\n")));
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(root.join("readme.md"), window, cx);
            workspace.open_file(root.join("README.md"), window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let (tabs, titles) = window
        .read_with(cx, |workspace, _| {
            (workspace.open_tabs(), workspace.tab_titles())
        })
        .unwrap();
    assert_eq!(tabs.len(), 1, "the file has one group");
    assert_eq!(tabs[0].len(), 1, "the second spelling opens no tab");
    assert_eq!(
        tabs[0][0].file_name(),
        Some(OsStr::new("README.md")),
        "the tab keeps the name of the file on disk"
    );
    assert_eq!(
        titles,
        vec![vec!["README.md".to_owned()]],
        "the title is the name the file has on disk"
    );
}

#[ui_kit::gpui::test]
fn a_tab_under_a_linked_project_root_keeps_that_root_in_its_path(cx: &mut TestAppContext) {
    let real = scratch("first-open-link-real");
    fs::write(real.join("notes.txt"), "notes\n").unwrap();
    let root = scratch("first-open-link-root").join("project");
    if let Err(error) = test_support::link_dir(&real, &root) {
        eprintln!("the test is skipped: this system made no directory link: {error}");
        return;
    }
    let window = open_window(cx, root.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file(root.join("notes.txt"), window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let tabs = window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap();
    assert_eq!(tabs[0].len(), 1, "the file opens one tab");
    assert_eq!(
        tabs[0][0],
        root.join("notes.txt"),
        "the tab keeps the root the project was opened from"
    );
    assert!(
        tabs[0][0].starts_with(&root),
        "the tab stays under the opened root: {}",
        tabs[0][0].display()
    );
}
