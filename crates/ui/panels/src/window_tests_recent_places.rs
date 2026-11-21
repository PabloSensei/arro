//! Recent Locations: Ctrl+Shift+E opens its window with the place of the caret of the open
//! file, an empty log opens the same window with its empty state, the places come back with
//! the saved project, and a place of a file that is gone is dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{
    Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext,
    WindowHandle,
};
use workspace::{
    FileIcons, Launch, Layout, RECENT_PLACES_POPUP, Settings, Workspace, bind_keys, load_layout,
};

use crate::window_tests::{
    active_path, bounds_of, click, draw, editor_of, open_path, open_sample_window, open_window,
    press, project_setup, scratch, type_text, wait_for_project_tree, wait_for_signal,
};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn tag(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<&'static str> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_tag())
        .unwrap()
}

fn places(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<workspace::Location> {
    window
        .read_with(cx, |workspace, _| workspace.recent_places().to_vec())
        .unwrap()
}

/// Waits until the text of the file reached its editor.
fn wait_loaded(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    let view = editor_of(cx, window, path);
    wait_for_signal(cx, "the text of the file", |cx| {
        view.read_with(cx, |view, _| view.is_loaded())
    });
}

/// A double click on the element with a debug selector, the way a row of the project tree
/// opens a file.
fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

/// A launch of a window of `root`, for a second window of the same project.
fn launch_of(root: &Path, state_dir: &Path) -> Launch {
    Launch {
        root: root.to_path_buf(),
        open: None,
        state_dir: state_dir.to_path_buf(),
        settings_dir: state_dir.to_path_buf(),
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    }
}

/// A window built the way the application builds it, from a layout and a store that came
/// from an earlier run.
fn open_saved_window(
    cx: &mut TestAppContext,
    root: &Path,
    state_dir: &Path,
    layout: Layout,
    store: Option<Arc<store::Store>>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        bind_keys(cx);
    });
    let launch = launch_of(root, state_dir);
    let setup = project_setup(scratch("recent-places-cache"));
    let agents = crate::AgentOptions {
        state_dir: state_dir.to_path_buf(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = crate::LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace = Workspace::new_for_test(launch, layout, store, Vec::new(), window, cx);
        crate::install(
            &mut workspace,
            crate::index::SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        crate::connect(&mut workspace, server, cx);
        workspace
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.restore_tabs(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
}

/// An empty project without a README, so the first open shows no file.
fn empty_project(name: &str) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("main.txt"), "alpha\nbeta\n").unwrap();
    root
}

#[ui_kit::gpui::test]
fn ctrl_shift_e_opens_the_window_with_the_place_of_the_caret(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "recent-places-open");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    wait_loaded(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-shift-e");
    draw(cx, window);

    assert_eq!(
        tag(cx, window),
        Some(RECENT_PLACES_POPUP),
        "Ctrl+Shift+E opens the Recent Locations window"
    );
    assert!(
        has(cx, window, "places-popup"),
        "the window is on screen with a file open"
    );
    assert!(
        has(cx, window, "places-row-0"),
        "the list has the row of the place of the caret"
    );
    let places = places(cx, window);
    assert!(
        matches!(places.first(), Some(place) if place.path == main && place.line == 1),
        "the place of the caret is the first one: {places:?}"
    );
    assert!(
        !has(cx, window, "notification"),
        "the window replaces the notification"
    );
}

#[ui_kit::gpui::test]
fn an_empty_log_opens_the_window_itself(cx: &mut TestAppContext) {
    let root = empty_project("recent-places-empty");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        active_path(cx, window),
        None,
        "the project opens without a file"
    );

    press(cx, window, "ctrl-shift-e");
    draw(cx, window);

    assert_eq!(
        tag(cx, window),
        Some(RECENT_PLACES_POPUP),
        "Ctrl+Shift+E opens the window even with nothing in the log"
    );
    assert!(
        has(cx, window, "places-popup"),
        "the window is on screen without open files"
    );
    assert!(
        has(cx, window, "places-empty"),
        "the window shows its empty state"
    );
    assert!(
        !has(cx, window, "notification"),
        "no notification takes the place of the window"
    );
}

#[ui_kit::gpui::test]
fn places_come_back_with_the_project_and_a_gone_file_is_dropped(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "recent-places-saved");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    wait_loaded(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    type_text(cx, window, "z");
    // Saving takes the edit back out of the tab, so it closes without a question. The
    // place of the edit stays in the log.
    view.update(cx, |view, cx| view.save(cx));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |view, _| view.is_dirty()),
        "the saved tab has no unsaved text"
    );
    open_path(cx, window, &notes);
    draw(cx, window);
    wait_loaded(cx, window, &notes);
    let before = places(cx, window);
    assert!(
        before.iter().any(|place| place.path == notes),
        "opening a file records its place: {before:?}"
    );

    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the test window has a store");
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let layout = load_layout(&store, &root);
    let saved: Vec<(PathBuf, usize)> = layout
        .recent_places
        .places()
        .iter()
        .map(|place| (place.path.clone(), place.line))
        .collect();
    assert!(
        saved.contains(&(main.clone(), 1)),
        "the place of the edit is saved with the project: {saved:?}"
    );
    assert!(
        saved.contains(&(notes.clone(), 0)),
        "the place of the opened file is saved too: {saved:?}"
    );

    fs::remove_file(&main).unwrap();
    let second = open_saved_window(cx, &root, &state_dir, layout, Some(store));
    draw(cx, second);
    assert_eq!(
        active_path(cx, second),
        Some(notes.clone()),
        "only the file that is still there comes back"
    );
    press(cx, second, "ctrl-shift-e");
    draw(cx, second);

    assert!(
        has(cx, second, "places-popup"),
        "the saved places open in their window"
    );
    let places = places(cx, second);
    assert!(
        places.iter().all(|place| place.path != main),
        "the place of the removed file is dropped: {places:?}"
    );
    assert!(
        places.iter().any(|place| place.path == notes),
        "the place of the file that is still there is back: {places:?}"
    );
}

#[ui_kit::gpui::test]
fn opening_a_file_from_the_tree_records_its_place(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "recent-places-tree");
    wait_for_project_tree(cx);
    draw(cx, window);
    click(cx, window, "tree-row-src");
    draw(cx, window);
    double_click(cx, window, "tree-row-main.txt");

    let main = root.join("src").join("main.txt");
    wait_loaded(cx, window, &main);
    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "the click on the row opens the file"
    );
    let places = places(cx, window);
    assert!(
        matches!(places.first(), Some(place) if place.path == main),
        "the opened file is the latest place: {places:?}"
    );
    assert!(
        matches!(places.first(), Some(place) if place.line == 0),
        "the caret of a file opened from the tree stands on its first line"
    );
}
