//! The Rebase dialog of the Git Log without a window: the menu item that opens it, the
//! list of the commits, the keys of the actions, Alt+Up and Alt+Down, Start Rebasing,
//! Cancel and the first row that has nothing to squash. Every repository lives in a scratch
//! directory and its identity is set there, so the machine is left alone.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use vcs::{GitRunner, RebaseAction};
use workspace::Workspace;

use crate::git_log::{ActiveGitLog, RebaseView};
use crate::window_tests::{bounds_of, click, draw, open_window, press};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// The row of the commit menu of Interactively Rebase from Here, counted with separators.
const INTERACTIVE: &str = "menu-9";

/// A base commit and the commits `one` and `two`, each of them adding a file.
fn repository(name: &str) -> (PathBuf, GitRunner) {
    let (root, _bare, git) = repository_with_origin(name);
    commit_file(&git, &root, "one.txt", "one");
    commit_file(&git, &root, "two.txt", "two");
    (root, git)
}

fn commit_file(git: &GitRunner, root: &Path, name: &str, message: &str) {
    fs::write(root.join(name), format!("{message}\n")).unwrap();
    git.run(root, ["add", name]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

fn open_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-commit-0"));
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
}

fn menu_labels(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
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
        .unwrap_or_default()
}

fn log_subjects(cx: &TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .commits()
            .iter()
            .map(|commit| commit.subject.clone())
            .collect()
    })
}

fn actions(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<RebaseAction> {
    let view = dialog(cx, window);
    cx.read(|cx| view.read(cx).actions())
}

fn plan_subjects(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    let view = dialog(cx, window);
    cx.read(|cx| view.read(cx).subjects())
}

/// The open Rebase dialog of the window.
fn dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<RebaseView> {
    let overlay = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the rebase dialog is open");
    overlay
        .downcast::<RebaseView>()
        .map_err(|_| ())
        .expect("the open popup is the rebase dialog")
}

/// The log is open and the dialog starts at `one`: the list of the Git Log scrolls, and
/// the second row is the oldest commit whose actions the window keeps on screen.
fn open_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, INTERACTIVE);
    wait_for(cx, "the rebase dialog", |cx| {
        has_selector(cx, window, "rebase-row-1")
    });
}

#[ui_kit::gpui::test]
fn the_menu_opens_the_rebase_dialog_with_every_commit(cx: &mut TestAppContext) {
    let (root, _git) = repository("rebase-dialog");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    let labels = menu_labels(cx, window);
    assert!(
        labels
            .iter()
            .any(|label| label == "Interactively Rebase from Here..."),
        "the action is in {labels:?}"
    );

    click(cx, window, INTERACTIVE);
    wait_for(cx, "the rebase dialog", |cx| {
        has_selector(cx, window, "rebase-view") && has_selector(cx, window, "rebase-row-1")
    });
    assert_eq!(plan_subjects(cx, window), ["one", "two"]);
    assert_eq!(
        actions(cx, window),
        [RebaseAction::Pick; 2],
        "every row starts with Pick"
    );
}

#[ui_kit::gpui::test]
fn a_key_drops_the_selected_commit_and_start_removes_it_from_the_log(cx: &mut TestAppContext) {
    let (root, _git) = repository("rebase-drop");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    open_dialog(cx, window);

    press(cx, window, "d");
    assert_eq!(
        actions(cx, window)[0],
        RebaseAction::Drop,
        "D drops the row"
    );

    click(cx, window, "rebase-start");
    wait_for(cx, "the dropped commit", |cx| {
        log_subjects(cx) == ["two", "base"]
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
}

#[ui_kit::gpui::test]
fn alt_down_moves_the_selected_commit_down_and_the_rebase_keeps_the_order(cx: &mut TestAppContext) {
    let (root, _git) = repository("rebase-move");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    open_dialog(cx, window);

    press(cx, window, "alt-down");
    assert_eq!(
        plan_subjects(cx, window),
        ["two", "one"],
        "the first commit moved under the second"
    );

    click(cx, window, "rebase-start");
    wait_for(cx, "the reordered log", |cx| {
        log_subjects(cx) == ["one", "two", "base"]
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
}

#[ui_kit::gpui::test]
fn the_first_row_has_no_squash_and_no_fixup(cx: &mut TestAppContext) {
    let (root, _git) = repository("rebase-first");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    open_dialog(cx, window);

    click(cx, window, "rebase-action-0");
    wait_for(cx, "the list of actions", |cx| {
        has_selector(cx, window, "rebase-menu-0")
    });
    click(cx, window, "rebase-option-0-squash");
    assert_eq!(
        actions(cx, window)[0],
        RebaseAction::Pick,
        "Squash of the first row does nothing"
    );
    click(cx, window, "rebase-option-0-fixup");
    assert_eq!(
        actions(cx, window)[0],
        RebaseAction::Pick,
        "Fixup of the first row does nothing"
    );

    click(cx, window, "rebase-option-0-drop");
    assert_eq!(
        actions(cx, window)[0],
        RebaseAction::Drop,
        "the other rows work"
    );
}

#[ui_kit::gpui::test]
fn cancel_leaves_the_branch_as_it_was(cx: &mut TestAppContext) {
    let (root, git) = repository("rebase-cancel");
    let head = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    open_dialog(cx, window);

    press(cx, window, "d");
    click(cx, window, "rebase-cancel");
    wait_for(cx, "the closed dialog", |cx| {
        !has_selector(cx, window, "rebase-view")
    });

    assert_eq!(log_subjects(cx), ["two", "one", "base"]);
    let now = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    assert_eq!(now, head);
}
