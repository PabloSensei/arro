//! The project tree with Compact Middle Packages, and Ctrl+drag that copies. The files of
//! these tests are text, and the source roots are given to the tree as data, as the run
//! panel gives them when a project model is there.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point};
use workspace::Workspace;

use crate::project::{ActiveProjectPanel, ProjectPanel};
use crate::window_tests::{
    bounds_of, click, draw, open_path, open_window, sample_project, wait_for_signal,
};
use crate::window_tests_menus::{reveal, show_project};

/// The rows of the tree on screen, as their depth and name.
fn row_names(cx: &mut TestAppContext, panel: &gpui::Entity<ProjectPanel>) -> Vec<(usize, String)> {
    panel.read_with(cx, |panel, _| {
        panel
            .tree()
            .rows()
            .iter()
            .map(|row| (row.depth, row.name.clone()))
            .collect()
    })
}

fn project_panel(cx: &mut TestAppContext) -> gpui::Entity<ProjectPanel> {
    cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

/// Gives the tree the source roots of the project, as a project model would.
fn set_source_roots(
    cx: &mut TestAppContext,
    panel: &gpui::Entity<ProjectPanel>,
    roots: Vec<PathBuf>,
) {
    panel.update(cx, |panel, cx| panel.set_source_roots_for_test(roots, cx));
    cx.run_until_parked();
}

/// Switches Compact Middle Packages in the settings, as its menu row does.
pub(crate) fn set_compact(cx: &mut TestAppContext, window: WindowHandle<Workspace>, on: bool) {
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.project_compact_middle_packages = on, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn ctrl() -> Modifiers {
    Modifiers {
        control: true,
        ..Modifiers::default()
    }
}

/// Drags the row `from` onto the row `to` with `modifiers` held, as a pointer does.
fn drag_with(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: &'static str,
    to: &'static str,
    modifiers: Modifiers,
) {
    let from = bounds_of(cx, window, from).center();
    let to = bounds_of(cx, window, to).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, modifiers);
    visual.simulate_mouse_down(from, MouseButton::Left, modifiers);
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            modifiers,
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, modifiers);
    cx.run_until_parked();
    draw(cx, window);
}

/// A project with `src/code/com/example/demo/App.txt`, where `code` is a source root, and
/// `docs/guide/en/notes.txt`, an ordinary folder chain.
fn packages_project(name: &str) -> (PathBuf, PathBuf) {
    let root = sample_project(name);
    let app = root
        .join("src")
        .join("code")
        .join("com")
        .join("example")
        .join("demo")
        .join("App.txt");
    write(&app, "class\n");
    write(
        &root.join("docs").join("guide").join("en").join("notes.txt"),
        "notes\n",
    );
    (root, app)
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[ui_kit::gpui::test]
fn a_chain_of_folders_under_a_source_root_is_one_row_named_with_dots(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-dots");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    reveal(cx, window, &app);
    reveal(cx, window, &root.join("docs").join("guide").join("en"));

    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com.example.demo".to_owned())),
        "the package is one row: {rows:?}"
    );
    assert!(
        rows.contains(&(1, "docs/guide/en".to_owned())),
        "an ordinary chain is one row with slashes: {rows:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|(_, name)| name == "com" || name == "example"),
        "the middle packages have no row of their own: {rows:?}"
    );
    // The row is on screen under the name the tree gives it.
    let _ = bounds_of(cx, window, "tree-row-com.example.demo");
}

#[ui_kit::gpui::test]
fn the_switch_shows_one_folder_per_row_and_joins_them_again(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-switch");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    reveal(cx, window, &app);

    set_compact(cx, window, false);
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com".to_owned())) && rows.contains(&(4, "example".to_owned())),
        "every folder has its row when the switch is off: {rows:?}"
    );
    set_compact(cx, window, true);
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com.example.demo".to_owned())),
        "the chain is one row again: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn the_view_options_menu_turns_the_switch_off_and_on(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-menu");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    reveal(cx, window, &app);

    invoke_switch_row(cx, window);
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com".to_owned())),
        "the menu row turned the switch off: {rows:?}"
    );
    invoke_switch_row(cx, window);
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com.example.demo".to_owned())),
        "the menu row turned the switch on again: {rows:?}"
    );
}

/// Runs the Compact Middle Packages row of View Options, under Tree Appearance, as a click
/// does. The menu is opened first when it is not open.
fn invoke_switch_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let open = window
        .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
        .unwrap();
    if !open {
        click(cx, window, "project-options");
        cx.run_until_parked();
        draw(cx, window);
    }
    let action = window
        .read_with(cx, |workspace, _| {
            let entries = workspace.menu_entries()?;
            let appearance = entries
                .iter()
                .find(|entry| entry.label.as_ref() == "Tree Appearance")?;
            let switch = appearance
                .submenu
                .iter()
                .find(|entry| entry.label.as_ref().ends_with("Compact Middle Packages"))?;
            switch.on_click.clone()
        })
        .unwrap()
        .expect("Tree Appearance has the Compact Middle Packages switch");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| action(window, cx));
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn select_opened_file_shows_the_file_in_its_compacted_row(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-select");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    open_path(cx, window, &app);

    click(cx, window, "project-collapse-all");
    click(cx, window, "project-select-opened");
    wait_for_signal(cx, "the opened file in the tree", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.selected_path() == Some(app.as_path()) && panel.tree().index_of(&app).is_some()
        })
    });
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com.example.demo".to_owned()))
            && rows.contains(&(4, "App.txt".to_owned())),
        "the compacted row opens to the file: {rows:?}"
    );
    let _ = bounds_of(cx, window, "tree-row-App.txt");
}

#[ui_kit::gpui::test]
fn ctrl_drag_copies_a_text_file_and_a_plain_drag_moves_it(cx: &mut TestAppContext) {
    let root = sample_project("drag-copy");
    let notes = root.join("notes");
    let archive = root.join("archive");
    write(&notes.join("copied.txt"), "copy me\n");
    write(&notes.join("moved.txt"), "move me\n");
    fs::create_dir_all(&archive).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    show_project(cx, window);
    reveal(cx, window, &notes.join("copied.txt"));
    reveal(cx, window, &notes.join("moved.txt"));
    reveal(cx, window, &archive);

    drag_with(
        cx,
        window,
        "tree-row-copied.txt",
        "tree-row-archive",
        ctrl(),
    );
    wait_for_signal(cx, "the copy on disk", |_| {
        archive.join("copied.txt").is_file()
    });
    assert!(notes.join("copied.txt").is_file(), "Ctrl keeps the source");
    assert_eq!(
        fs::read_to_string(archive.join("copied.txt")).unwrap(),
        "copy me\n"
    );

    drag_with(
        cx,
        window,
        "tree-row-moved.txt",
        "tree-row-archive",
        Modifiers::default(),
    );
    wait_for_signal(cx, "the move on disk", |_| {
        archive.join("moved.txt").is_file() && !notes.join("moved.txt").exists()
    });
}

#[ui_kit::gpui::test]
fn ctrl_drop_on_its_own_folder_names_the_copy_with_a_number(cx: &mut TestAppContext) {
    let root = sample_project("drag-copy-number");
    let notes = root.join("notes");
    write(&notes.join("copied.txt"), "copy me\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    show_project(cx, window);
    reveal(cx, window, &notes.join("copied.txt"));

    drag_with(cx, window, "tree-row-copied.txt", "tree-row-notes", ctrl());
    wait_for_signal(cx, "the numbered copy on disk", |_| {
        notes.join("copied (1).txt").is_file()
    });
    assert_eq!(
        fs::read_to_string(notes.join("copied.txt")).unwrap(),
        "copy me\n"
    );
}

#[ui_kit::gpui::test]
fn a_click_on_the_compacted_row_opens_its_deepest_folder(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-click");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    // Revealing a middle folder opens the folders above it and leaves the chain closed.
    reveal(cx, window, &root.join("src").join("code").join("com"));
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(3, "com.example.demo".to_owned()))
            && !rows.iter().any(|(_, n)| n == "App.txt"),
        "the compacted row starts closed: {rows:?}"
    );

    click(cx, window, "tree-row-com.example.demo");
    wait_for_signal(cx, "the file under the compacted row", |cx| {
        panel.read_with(cx, |panel, _| panel.tree().index_of(&app).is_some())
    });
    let rows = row_names(cx, &panel);
    assert!(
        rows.contains(&(4, "App.txt".to_owned())),
        "the click opens the deepest folder of the row: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn dragging_the_compacted_row_moves_its_deepest_folder(cx: &mut TestAppContext) {
    let (root, app) = packages_project("compact-drag");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = project_panel(cx);
    set_source_roots(cx, &panel, vec![root.join("src").join("code")]);
    show_project(cx, window);
    reveal(cx, window, &app);
    let guide = root.join("docs").join("guide").join("en");
    reveal(cx, window, &guide);

    let demo = app.parent().unwrap().to_path_buf();
    drag_with(
        cx,
        window,
        "tree-row-com.example.demo",
        "tree-row-docs/guide/en",
        Modifiers::default(),
    );
    let moved = guide.join("demo");
    wait_for_signal(cx, "the folder on disk", |_| {
        moved.join("App.txt").is_file() && !demo.exists()
    });
}

#[ui_kit::gpui::test]
fn ctrl_drop_onto_the_same_row_copies_nothing(cx: &mut TestAppContext) {
    let root = sample_project("drag-copy-itself");
    let notes = root.join("notes");
    write(&notes.join("copied.txt"), "copy me\n");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    show_project(cx, window);
    reveal(cx, window, &notes.join("copied.txt"));

    drag_with(
        cx,
        window,
        "tree-row-copied.txt",
        "tree-row-copied.txt",
        ctrl(),
    );
    std::thread::sleep(Duration::from_millis(200));
    cx.run_until_parked();
    assert!(
        !notes.join("copied (1).txt").exists(),
        "a drop onto the row it started from is no copy"
    );
    assert_eq!(
        fs::read_to_string(notes.join("copied.txt")).unwrap(),
        "copy me\n"
    );
}
