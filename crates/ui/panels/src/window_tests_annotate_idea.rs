//! The blame column as IDEA draws it: a label on every line, the view options of its
//! menu, and the menu of the line numbers of a right click. Every repository lives in a
//! scratch directory, so the machine is left alone.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use editor_view::EditorView;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px};
use workspace::{FileIcons, Launch, Layout, SETTINGS_FILE, Settings, Workspace};

use crate::debug::ActiveDebug;
use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, line_height, open_path, press, project_setup, scratch,
    wait_for_signal,
};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

use crate::{AgentOptions, LanguageServer, connect, install};

/// Two commits of three lines each: Alice writes the first three, Bob appends the last
/// three, so the column has two runs of one commit side by side.
fn repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "alice@example.invalid"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Alice"]).unwrap();
    fs::write(root.join("main.txt"), "one\ntwo\nthree\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "the first change"])
        .unwrap();
    git.run(&root, ["config", "user.email", "bob@example.invalid"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Bob"]).unwrap();
    fs::write(root.join("main.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "the second change"])
        .unwrap();
    root
}

/// A repository of two committed lines and two lines that only live in the working tree.
fn uncommitted_repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "alice@example.invalid"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Alice"]).unwrap();
    fs::write(root.join("main.txt"), "one\ntwo\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "the first change"])
        .unwrap();
    fs::write(root.join("main.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    root
}

/// The window of a test on a folder of its state that the test keeps: a second window of
/// the test reads the settings the first one wrote.
fn open_window_at(
    cx: &mut TestAppContext,
    root: PathBuf,
    state_dir: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    let file = state_dir.join(SETTINGS_FILE);
    let settings = if file.is_file() {
        Settings::load(&file)
    } else {
        Settings::default()
    };
    cx.update(|cx| {
        cx.set_global(ui_kit::Theme::dark());
        cx.set_global(settings);
        cx.set_global(editor_view::Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
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
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    });
    guard_window(cx);
    window
}

fn guard_window(cx: &mut TestAppContext) {
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
}

fn close(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

/// A point in the gutter of the editor at a buffer line counted from zero.
fn gutter_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(30.),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

/// A point on the blame label of a buffer line counted from zero.
fn label_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(80.),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

/// A point in the text of the editor at a buffer line counted from zero.
fn text_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(420.),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn menu_labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

fn has_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) -> bool {
    menu_labels(cx, window).iter().any(|row| row == label)
}

/// The indexes of the rows of `labels`, each one inside the submenu of the row before.
fn menu_path(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    labels: &[&str],
) -> Vec<usize> {
    window
        .read_with(cx, |workspace, _| {
            let mut level = workspace.menu_entries()?.to_vec();
            let mut path = Vec::new();
            for label in labels {
                let index = level
                    .iter()
                    .position(|entry| entry.label.as_ref() == *label)?;
                path.push(index);
                level = level[index].submenu.clone();
            }
            Some(path)
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu path {labels:?}"))
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: String) {
    let selector: &'static str = Box::leak(selector.into_boxed_str());
    click(cx, window, selector);
}

/// Clicks the rows of `labels` one level after another, opening each submenu.
fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, labels: &[&str]) {
    let path = menu_path(cx, window, labels);
    for depth in 1..=path.len() {
        let parts: Vec<String> = path[..depth].iter().map(usize::to_string).collect();
        click_selector(cx, window, format!("menu-{}", parts.join("-")));
        draw(cx, window);
    }
}

fn annotate(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
) {
    let at = gutter_point(cx, window, 0);
    right_click_at(cx, window, at);
    wait_for(cx, "the gutter menu", |cx| {
        has_menu_row(cx, window, "Annotate with Git Blame")
    });
    click_menu(cx, window, &["Annotate with Git Blame"]);
    wait_for(cx, "the blame labels", |cx| !labels(cx, view).is_empty());
}

/// The painted labels as (line from zero, label).
fn labels(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Vec<(u32, String)> {
    view.read_with(cx, |view, _| {
        view.annotation_lines()
            .into_iter()
            .map(|(line, label, _)| (line, label))
            .collect()
    })
}

#[ui_kit::gpui::test]
fn every_line_of_a_run_carries_the_label_of_its_commit(cx: &mut TestAppContext) {
    let root = repository("annotate-idea-every-line");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    annotate(cx, window, &view);

    let found = labels(cx, &view);
    assert_eq!(
        found.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 5],
        "all six lines are annotated"
    );
    for (index, (_, label)) in found.iter().enumerate() {
        if index < 3 {
            assert!(label.contains("Alice"), "line {index}: {label}");
        } else {
            assert!(label.contains("Bob"), "line {index}: {label}");
        }
    }
    assert!(
        found[0].1.contains("Today"),
        "a commit of today shows Today: {}",
        found[0].1
    );
}

#[ui_kit::gpui::test]
fn a_new_line_says_not_committed_yet_on_each_of_its_lines(cx: &mut TestAppContext) {
    let root = uncommitted_repository("annotate-idea-not-committed");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    annotate(cx, window, &view);

    let found = labels(cx, &view);
    assert_eq!(
        found.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert!(found[0].1.contains("Alice") || found[0].1.contains("Today"));
    assert!(found[1].1.contains("Alice") || found[1].1.contains("Today"));
    assert_eq!(found[2].1, "Not Committed Yet");
    assert_eq!(found[3].1, "Not Committed Yet");
}

#[ui_kit::gpui::test]
fn the_date_of_the_view_is_saved_and_survives_a_new_window(cx: &mut TestAppContext) {
    let root = repository("annotate-idea-date");
    let state_dir = scratch("annotate-idea-date-state");
    let path = root.join("main.txt");

    let window = open_window_at(cx, root.clone(), state_dir.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);
    assert!(labels(cx, &view)[0].1.contains("Today"));

    let at = label_point(cx, window, 0);
    right_click_at(cx, window, at);
    wait_for(cx, "the blame menu", |cx| {
        has_menu_row(cx, window, "Options")
    });
    click_menu(cx, window, &["Options", "View", "✓ Date"]);
    wait_for(cx, "the label without the date", |cx| {
        labels(cx, &view)
            .iter()
            .all(|(_, label)| !label.contains("Today"))
    });
    let settings_file = state_dir.join(SETTINGS_FILE);
    wait_for_signal(cx, "the saved date switch", |_| {
        !Settings::load(&settings_file).annotate_date
    });

    close(cx, window);
    let window = open_window_at(cx, root, state_dir);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);
    let found = labels(cx, &view);
    assert!(
        !found.is_empty() && found.iter().all(|(_, label)| !label.contains("Today")),
        "the new window keeps the choice: {found:?}"
    );
}

#[ui_kit::gpui::test]
fn copy_revision_number_puts_the_hash_of_the_line_on_the_clipboard(cx: &mut TestAppContext) {
    let root = repository("annotate-idea-copy");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);
    let commit = view.read_with(cx, |view, _| view.annotation_lines()[4].2.clone());

    let at = label_point(cx, window, 4);
    right_click_at(cx, window, at);
    wait_for(cx, "the blame menu", |cx| {
        has_menu_row(cx, window, "Copy Revision Number")
    });
    click_menu(cx, window, &["Copy Revision Number"]);

    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(commit.as_str()));
}

#[ui_kit::gpui::test]
fn the_gutter_menu_of_line_five_has_no_editor_rows_and_places_the_breakpoint_there(
    cx: &mut TestAppContext,
) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "annotate-idea-gutter");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    let debug = cx.update(|cx| cx.global::<ActiveDebug>().0.clone());

    let at = gutter_point(cx, window, 4);
    right_click_at(cx, window, at);
    wait_for(cx, "the gutter menu", |cx| {
        has_menu_row(cx, window, "Add Breakpoint")
    });
    for row in [
        "Cut",
        "Copy",
        "Paste",
        "Paste from History...",
        "Go to Declaration",
    ] {
        assert!(
            !has_menu_row(cx, window, row),
            "the gutter menu must not hold the editor row {row}"
        );
    }
    click_menu(cx, window, &["Add Breakpoint"]);
    wait_for(cx, "the breakpoint of line five", |cx| {
        debug
            .read_with(cx, |panel, _| panel.breakpoint_lines(&path))
            .contains(&5)
    });
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![5],
        "the breakpoint lands on the clicked line, not on the caret line"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(0),
        "the click does not move the caret"
    );

    // The same line now offers the rows of a breakpoint that exists.
    let at = gutter_point(cx, window, 4);
    right_click_at(cx, window, at);
    wait_for(cx, "the breakpoint rows", |cx| {
        has_menu_row(cx, window, "Remove Breakpoint")
    });
    assert!(has_menu_row(cx, window, "Edit Breakpoint..."));
    assert!(has_menu_row(cx, window, "Disable Breakpoint"));
    assert!(!has_menu_row(cx, window, "Add Breakpoint"));
}

#[ui_kit::gpui::test]
fn the_annotate_row_follows_the_column_and_the_text_keeps_its_own_menu(cx: &mut TestAppContext) {
    let root = repository("annotate-idea-gutter-check");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    let at = gutter_point(cx, window, 0);
    right_click_at(cx, window, at);
    wait_for(cx, "the row to annotate", |cx| {
        has_menu_row(cx, window, "Annotate with Git Blame")
    });
    assert!(!has_menu_row(cx, window, "✓ Annotate with Git Blame"));
    click_menu(cx, window, &["Annotate with Git Blame"]);
    wait_for(cx, "the blame labels", |cx| !labels(cx, &view).is_empty());

    let at = gutter_point(cx, window, 0);
    right_click_at(cx, window, at);
    wait_for(cx, "the checked row", |cx| {
        has_menu_row(cx, window, "✓ Annotate with Git Blame")
    });

    // A right click in the text still opens the menu of the editor.
    press(cx, window, "escape");
    let at = text_point(cx, window, 0);
    right_click_at(cx, window, at);
    wait_for(cx, "the editor menu", |cx| has_menu_row(cx, window, "Cut"));
    assert!(!has_menu_row(cx, window, "✓ Annotate with Git Blame"));
}

#[ui_kit::gpui::test]
fn a_right_click_on_a_label_opens_the_column_menu_only(cx: &mut TestAppContext) {
    let root = repository("annotate-idea-blame-menu");
    let window = crate::window_tests::open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);

    let at = label_point(cx, window, 4);
    right_click_at(cx, window, at);
    wait_for(cx, "the blame menu", |cx| {
        has_menu_row(cx, window, "Options")
    });
    for row in [
        "Options",
        "Annotate Previous Revision",
        "Show Diff",
        "Copy Revision Number",
        "Show in Git Log",
        "Close Annotations",
    ] {
        assert!(has_menu_row(cx, window, row), "the blame menu holds {row}");
    }
    for row in ["Cut", "Copy", "Paste", "Go to Declaration"] {
        assert!(
            !has_menu_row(cx, window, row),
            "the blame menu must not hold the editor row {row}"
        );
    }
    assert!(has_selector(cx, window, "menu-0"));
}
