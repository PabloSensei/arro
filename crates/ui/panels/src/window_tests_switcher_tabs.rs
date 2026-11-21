//! The Switcher lists every open tab: the file on screen first, the files by last use, then
//! the tabs that no editor has shown in this run. Recent Files survives a restart through
//! the state folder, and a file that is gone after the last run leaves both lists.
//!
//! A restart is a second window in the same state folder, with the list of recent files
//! of the first one reset, as a new process starts with it empty.
//!
//! `cargo test -p panels window_tests_switcher_tabs`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::Languages;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Settings, Workspace, load_layout};

use crate::index::SearchSetup;
use crate::navigation::Navigator;
use crate::palette::{Palette, switcher_order};
use crate::window_tests::{draw, open_path, press, project_setup, scratch};
use crate::{AgentOptions, LanguageServer, connect, install};

/// The files of the test project, in the order the tests open them.
const FILES: [&str; 6] = [
    "alpha.txt",
    "beta.txt",
    "gamma.txt",
    "delta.txt",
    "epsilon.txt",
    "zeta.txt",
];

/// A project of plain text files and no README, so that the first open shows no file of
/// its own.
fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(&root).unwrap();
    for file in FILES {
        fs::write(root.join(file), format!("{file}\n")).unwrap();
    }
    root
}

/// The state folder of a test, empty at the start: a folder left by an earlier run would
/// restore its layout.
fn fresh_state(name: &str) -> PathBuf {
    let state = scratch(&format!("{name}-state"));
    let _ = fs::remove_dir_all(&state);
    state
}

/// A window of `root` in the state folder of the caller. The layout the folder saved is
/// read first and restored after the panels are installed, as the start of the program does.
fn open_project(
    cx: &mut TestAppContext,
    root: PathBuf,
    state_dir: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let layout = store
        .as_deref()
        .map(|store| load_layout(store, &root))
        .unwrap_or_default();
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace.restore_tabs(window, cx);
        workspace
    });
    cx.update(|cx| {
        cx.on_app_quit(|cx| {
            for handle in cx.windows() {
                let Some(handle) = handle.downcast::<Workspace>() else {
                    continue;
                };
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
            async {}
        })
        .detach();
    });
    window
}

/// Closes the window the way its close button does, which saves the layout of the project.
fn close(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

/// Closes the window, runs `between` with the files of the project as they are then, and
/// starts the project again in the same state folder. The list of recent files of the
/// process is empty again: only the saved layout brings the files back.
fn restart(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    state_dir: &Path,
    between: impl FnOnce(),
) -> WindowHandle<Workspace> {
    close(cx, window);
    between();
    cx.update(|cx| cx.set_global(Navigator::default()));
    let window = open_project(cx, root.to_path_buf(), state_dir.to_path_buf());
    cx.run_until_parked();
    draw(cx, window);
    window
}

/// The palette on screen, when its action has opened it.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<Palette> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open")
        .downcast::<Palette>()
        .expect("the popup is the palette")
}

/// The names of the test files among the rows, in the order of the list. The tool windows
/// that the Switcher adds have other names.
fn file_titles(rows: &[(String, String)]) -> Vec<&str> {
    rows.iter()
        .map(|(title, _)| title.as_str())
        .filter(|title| FILES.contains(title))
        .collect()
}

/// The rows and the selected row of the palette on screen.
fn rows_and_selected(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> (Vec<(String, String)>, usize) {
    palette(cx, window).read_with(cx, |palette, _| {
        (palette.rows_text(), palette.selected_row())
    })
}

#[ui_kit::gpui::test]
fn the_switcher_lists_the_restored_tabs_by_use_and_selects_the_previous_file(
    cx: &mut TestAppContext,
) {
    let root = project("switcher-restored");
    let state = fresh_state("switcher-restored");
    let window = open_project(cx, root.clone(), state.clone());
    cx.run_until_parked();
    draw(cx, window);
    for file in FILES {
        open_path(cx, window, &root.join(file));
    }
    let window = restart(cx, window, &root, &state, || {});

    let tabs = window
        .read_with(cx, |workspace, _| {
            workspace.open_tabs().into_iter().flatten().count()
        })
        .unwrap();
    assert_eq!(tabs, FILES.len(), "every tab comes back");

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, selected) = rows_and_selected(cx, window);
    assert_eq!(
        file_titles(&rows),
        [
            "zeta.txt",
            "epsilon.txt",
            "delta.txt",
            "gamma.txt",
            "beta.txt",
            "alpha.txt"
        ],
        "the file on screen first, then the files by last use: {rows:?}"
    );
    assert_eq!(selected, 1, "the previous file is selected");
}

#[ui_kit::gpui::test]
fn recent_files_come_back_after_a_restart_in_the_state_folder(cx: &mut TestAppContext) {
    let root = project("recent-restart");
    let state = fresh_state("recent-restart");
    let window = open_project(cx, root.clone(), state.clone());
    cx.run_until_parked();
    draw(cx, window);
    for file in FILES {
        open_path(cx, window, &root.join(file));
    }
    let window = restart(cx, window, &root, &state, || {});

    press(cx, window, "ctrl-e");
    draw(cx, window);
    let (rows, selected) = rows_and_selected(cx, window);
    assert_eq!(
        file_titles(&rows),
        [
            "zeta.txt",
            "epsilon.txt",
            "delta.txt",
            "gamma.txt",
            "beta.txt",
            "alpha.txt"
        ],
        "Recent Files keeps the order of use: {rows:?}"
    );
    assert_eq!(selected, 1, "the previous file is selected");
}

#[ui_kit::gpui::test]
fn a_file_deleted_before_the_restart_leaves_the_switcher_and_recent_files(cx: &mut TestAppContext) {
    let root = project("recent-deleted");
    let state = fresh_state("recent-deleted");
    let window = open_project(cx, root.clone(), state.clone());
    cx.run_until_parked();
    draw(cx, window);
    for file in FILES {
        open_path(cx, window, &root.join(file));
    }
    let gone = root.join("delta.txt");
    let window = restart(cx, window, &root, &state, || {
        fs::remove_file(&gone).unwrap();
    });

    let tabs = window
        .read_with(cx, |workspace, _| {
            workspace.open_tabs().into_iter().flatten().count()
        })
        .unwrap();
    assert_eq!(tabs, FILES.len() - 1, "the tab of the file is not restored");

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, _) = rows_and_selected(cx, window);
    assert_eq!(
        file_titles(&rows),
        [
            "zeta.txt",
            "epsilon.txt",
            "gamma.txt",
            "beta.txt",
            "alpha.txt"
        ],
        "the Switcher has no row for the deleted file: {rows:?}"
    );
    press(cx, window, "escape");

    press(cx, window, "ctrl-e");
    draw(cx, window);
    let (rows, _) = rows_and_selected(cx, window);
    assert!(
        !file_titles(&rows).contains(&"delta.txt"),
        "Recent Files has no row for the deleted file: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn a_tab_no_editor_has_shown_follows_the_files_used_and_a_closed_tab_leaves_the_switcher(
    cx: &mut TestAppContext,
) {
    let root = project("switcher-shown");
    let state = fresh_state("switcher-shown");
    let window = open_project(cx, root.clone(), state);
    cx.run_until_parked();
    draw(cx, window);
    for file in ["alpha.txt", "beta.txt", "gamma.txt"] {
        open_path(cx, window, &root.join(file));
    }
    // A tab that opens behind the one on screen never becomes active, so no editor reports it.
    window
        .update(cx, |workspace, window, cx| {
            workspace.open_file_in_background(root.join("delta.txt"), window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, selected) = rows_and_selected(cx, window);
    assert_eq!(
        file_titles(&rows),
        ["gamma.txt", "beta.txt", "alpha.txt", "delta.txt"],
        "the used files first, the tab nobody activated last: {rows:?}"
    );
    assert_eq!(selected, 1, "beta.txt is the previous file");
    press(cx, window, "escape");

    // A closed tab leaves the Switcher, and stays in Recent Files.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_tab_of(&root.join("beta.txt"), window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    press(cx, window, "ctrl-tab");
    draw(cx, window);
    let (rows, _) = rows_and_selected(cx, window);
    assert_eq!(
        file_titles(&rows),
        ["gamma.txt", "alpha.txt", "delta.txt"],
        "the closed tab is not listed: {rows:?}"
    );
    press(cx, window, "escape");

    press(cx, window, "ctrl-e");
    draw(cx, window);
    let (rows, _) = rows_and_selected(cx, window);
    assert!(
        file_titles(&rows).contains(&"beta.txt"),
        "Recent Files keeps the closed tab: {rows:?}"
    );
}

#[test]
fn the_switcher_order_puts_the_screen_first_then_the_use_then_the_tabs() {
    let open = vec![
        PathBuf::from("a.txt"),
        PathBuf::from("b.txt"),
        PathBuf::from("c.txt"),
        PathBuf::from("d.txt"),
    ];
    let recent = vec![
        PathBuf::from("c.txt"),
        PathBuf::from("a.txt"),
        PathBuf::from("gone.txt"),
    ];
    let order = switcher_order(&recent, &open, Some(Path::new("c.txt")));
    assert_eq!(
        order,
        [
            PathBuf::from("c.txt"),
            PathBuf::from("a.txt"),
            PathBuf::from("b.txt"),
            PathBuf::from("d.txt"),
        ]
    );
}
