//! Alt+F1, Select In: the popup of the open file lists the places the file can be shown in,
//! a digit picks a row, and only the rows that apply to the file are listed.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::git::ActiveGit;
use crate::project::ActiveProjectPanel;
use crate::select_in::reveal_row_name;
use crate::window_tests::{
    click, draw, editor_of, git_repository, open_path, open_sample_window, open_window, press,
    scratch, wait_for_project_tree, wait_for_signal,
};

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The labels of the rows of the Select In popup in order, or `None` when no popup is open.
fn select_in_rows(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<Vec<String>> {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().map(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.label.to_string())
                    .collect()
            })
        })
        .ok()
        .flatten()
}

/// A project with a folder chain down to one file. Returns the root and the file.
fn deep_file(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let deep = root.join("deep").join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("Deep.txt"), "deep\n").unwrap();
    fs::write(root.join("top.txt"), "top\n").unwrap();
    (root, deep.join("Deep.txt"))
}

/// The window of a project, drawn once with its tree read.
fn project_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_project_tree(cx);
    window
}

/// Alt+F1 on the open file, drawn, and the rows the popup shows.
fn open_select_in(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    press(cx, window, "alt-f1");
    draw(cx, window);
    select_in_rows(cx, window).expect("the Select In popup reports its rows")
}

#[ui_kit::gpui::test]
fn alt_f1_opens_select_in_with_project_view_first(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "select-in-popup");
    open_path(cx, window, &root.join("src").join("main.txt"));

    let rows = open_select_in(cx, window);
    assert!(
        has_selector(cx, window, "select-in-popup"),
        "Alt+F1 opens the Select In popup"
    );
    assert_eq!(
        rows.first().map(String::as_str),
        Some("Project View"),
        "Project View is the first row: {rows:?}"
    );
    assert_eq!(
        rows,
        [
            "Project View",
            "Structure",
            reveal_row_name(keymap::platform()),
            "Terminal"
        ],
        "a file outside a Git repository has neither Commit nor Git Log"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "select-in-popup"),
        "Escape closes the popup"
    );
}

#[ui_kit::gpui::test]
fn digit_one_selects_the_open_file_in_the_project_tree(cx: &mut TestAppContext) {
    let (root, deep) = deep_file("select-in-digit");
    let window = project_window(cx, root);
    let panel = cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    open_path(cx, window, &deep);
    click(cx, window, "project-collapse-all");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.tree().index_of(&deep)),
        None,
        "the file is hidden before Alt+F1"
    );

    open_select_in(cx, window);
    press(cx, window, "1");
    wait_for_signal(cx, "the open file selected by the digit 1", |cx| {
        panel.read_with(cx, |panel, _| {
            panel.selected_path() == Some(deep.as_path()) && panel.tree().index_of(&deep).is_some()
        })
    });
    assert_eq!(
        select_in_rows(cx, window),
        None,
        "the popup closes once its row runs"
    );
}

#[ui_kit::gpui::test]
fn an_unchanged_file_has_no_commit_row(cx: &mut TestAppContext) {
    let root = git_repository("select-in-unchanged");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let git = cx.update(|cx| cx.global::<ActiveGit>().0.clone());
    assert!(
        git.read_with(cx, |git, _| git.repo().is_some()),
        "the window found the repository"
    );
    open_path(cx, window, &root.join("main.txt"));

    let rows = open_select_in(cx, window);
    assert!(
        !rows.iter().any(|row| row == "Commit"),
        "an unchanged file has no Commit row: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row == "Git Log"),
        "a project under Git lists Git Log: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn a_changed_file_has_a_commit_row(cx: &mut TestAppContext) {
    let root = git_repository("select-in-changed");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let git = cx.update(|cx| cx.global::<ActiveGit>().0.clone());
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
    });
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();

    let rows = open_select_in(cx, window);
    assert!(
        rows.iter().any(|row| row == "Commit"),
        "a changed file has a Commit row: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn alt_f1_without_an_open_file_opens_nothing(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "select-in-empty");
    press(cx, window, "alt-f1");
    draw(cx, window);
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "with no open file there is nothing to select"
    );
    assert!(!has_selector(cx, window, "select-in-popup"));
}

#[ui_kit::gpui::test]
fn down_and_enter_run_the_highlighted_row(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "select-in-enter");
    open_path(cx, window, &root.join("src").join("main.txt"));

    open_select_in(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    let structure_open = window
        .read_with(cx, |workspace, _| {
            workspace.tool_open(workspace::STRUCTURE_TOOL)
        })
        .unwrap();
    assert!(
        structure_open,
        "Down moves the highlight to Structure and Enter opens it"
    );
    assert_eq!(
        select_in_rows(cx, window),
        None,
        "the popup closes once its row runs"
    );
}

#[ui_kit::gpui::test]
fn digit_three_reveals_the_file_in_the_file_manager(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "select-in-reveal");
    let main = root.join("src").join("main.txt");
    open_path(cx, window, &main);
    ui_kit::clear_revealed_paths();

    open_select_in(cx, window);
    press(cx, window, "3");
    let revealed = ui_kit::revealed_paths();
    assert!(
        revealed.iter().any(|path| path == &main),
        "the digit 3 shows the file in the file manager: {revealed:?}"
    );
    assert_eq!(
        select_in_rows(cx, window),
        None,
        "the popup closes once its row runs"
    );
}
