//! The blame column of the editor: the gutter menu annotates a file, the labels follow
//! the text, Close Annotations takes the column away and a click opens the commit in the
//! log. Every repository lives in a scratch directory, so the machine is left alone.

use std::fs;
use std::path::PathBuf;

use editor_view::{EditorView, Spot};
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, click, draw, editor_of, line_height, open_path, open_window, press,
};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

/// Two commits of two authors: Alice writes every line, Bob changes the third and the
/// fourth and adds a file of his own.
fn repository(name: &str) -> PathBuf {
    let root = crate::window_tests::scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["config", "user.email", "alice@example.invalid"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Alice"]).unwrap();
    fs::write(root.join("main.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "the first change"])
        .unwrap();
    git.run(&root, ["config", "user.email", "bob@example.invalid"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Bob"]).unwrap();
    fs::write(
        root.join("main.txt"),
        "one\ntwo\nthree by bob\nfour by bob\nfive\n",
    )
    .unwrap();
    fs::write(root.join("bob.txt"), "bob\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "the second change"])
        .unwrap();
    root
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

fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(position, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn has_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) -> bool {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .is_some_and(|entries| entries.iter().any(|entry| entry.label.as_ref() == label))
        })
        .unwrap()
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
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
}

fn open_gutter_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let position = gutter_point(cx, window, 0);
    right_click_at(cx, window, position);
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

fn annotate(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    view: &gpui::Entity<EditorView>,
) {
    open_gutter_menu(cx, window);
    wait_for(cx, "the gutter menu", |cx| {
        has_menu_row(cx, window, "Annotate with Git Blame")
    });
    click_menu(cx, window, "Annotate with Git Blame");
    wait_for(cx, "the blame labels", |cx| !labels(cx, view).is_empty());
}

#[ui_kit::gpui::test]
fn the_gutter_menu_annotates_the_lines_with_their_authors(cx: &mut TestAppContext) {
    let root = repository("annotate-labels");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    annotate(cx, window, &view);

    // The labels stand on the first line of a run of one commit: both authors show.
    let found = labels(cx, &view);
    assert_eq!(
        found
            .iter()
            .map(|(line, label)| (*line, label.contains("Alice") || label.contains("Bob")))
            .collect::<Vec<_>>(),
        [(0, true), (2, true), (4, true)]
    );
    assert!(found[0].1.contains("Alice"));
    assert!(found[1].1.contains("Bob"));
    assert!(found[2].1.contains("Alice"));

    // A hint over a label names the commit, its author and its message.
    let at = label_point(cx, window, 0);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(at, None, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        has_selector(cx, window, "editor-annotate-hint"),
        "the hint of the label is on screen"
    );
}

#[ui_kit::gpui::test]
fn a_line_typed_above_keeps_the_labels_with_their_lines(cx: &mut TestAppContext) {
    let root = repository("annotate-edit");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);

    // Enter at the end of the second line: a new empty line takes no label, the labels
    // under it move one line down with their text.
    view.update(cx, |view, cx| {
        view.go_to(Spot::Line { line: 1, column: 3 }, cx)
    });
    press(cx, window, "enter");

    wait_for(cx, "the moved labels", |cx| {
        let lines: Vec<u32> = labels(cx, &view).iter().map(|(line, _)| *line).collect();
        lines == [0, 3, 5]
    });
    let found = labels(cx, &view);
    assert!(found[0].1.contains("Alice"));
    assert!(found[1].1.contains("Bob"));
    assert!(found[2].1.contains("Alice"));

    // A save reaches the status of Git, and the column is read again: the line that only
    // lives in the working tree is not committed yet.
    view.update(cx, |view, cx| view.save_blocking(cx));
    wait_for(cx, "the label of the new line", |cx| {
        labels(cx, &view)
            .iter()
            .any(|(line, label)| *line == 2 && label.contains("Not Committed"))
    });
}

#[ui_kit::gpui::test]
fn close_annotations_takes_the_column_away_and_a_label_opens_its_commit(cx: &mut TestAppContext) {
    let root = repository("annotate-close");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    annotate(cx, window, &view);

    // The same menu row closes the column now.
    open_gutter_menu(cx, window);
    wait_for(cx, "the closing row", |cx| {
        has_menu_row(cx, window, "Close Annotations")
    });
    click_menu(cx, window, "Close Annotations");
    wait_for(cx, "the column to go away", |cx| {
        !view.read_with(cx, |view, _| view.has_annotations())
    });

    // A click on the label of the oldest commit opens the log on it: the first commit
    // changed main.txt alone, the second one main.txt and bob.txt.
    annotate(cx, window, &view);
    let at = label_point(cx, window, 0);
    click_at(cx, window, at);
    wait_for(cx, "the file of the commit", |cx| {
        has_selector(cx, window, "log-file-0")
    });
    assert!(
        !has_selector(cx, window, "log-file-1"),
        "the log shows the files of the oldest commit only"
    );
}
