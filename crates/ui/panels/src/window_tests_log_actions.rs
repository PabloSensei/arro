//! The actions of the Git Log window without a window: the menu of a commit, Cherry-Pick,
//! Revert, Reset, New Branch, New Tag and the Conflicts window of a cherry-pick. Every
//! repository lives in a scratch directory and its identity is set there, so the machine
//! is left alone.

use std::fs;
use std::path::Path;

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use vcs::{GitRunner, Operation};
use workspace::Workspace;

use crate::git_log::ActiveGitLog;
use crate::window_tests::{bounds_of, click, draw, open_window, press, type_text};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// The rows of the commit menu, in the order IDEA lists them.
const COPY: &str = "menu-0";
const NEW_BRANCH: &str = "menu-2";
const NEW_TAG: &str = "menu-3";
const CHERRY_PICK: &str = "menu-5";
const REVERT: &str = "menu-6";
const RESET: &str = "menu-8";

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// Writes `file` with `text`, commits it and returns the new commit.
fn commit_change(git: &GitRunner, root: &Path, text: &str, message: &str) -> String {
    fs::write(root.join("main.txt"), text).unwrap();
    git.run(root, ["add", "main.txt"]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
    let output = git.run(root, ["rev-parse", "HEAD"]).unwrap();
    String::from_utf8_lossy(&output).trim().to_owned()
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

fn log_subjects(cx: &mut TestAppContext) -> Vec<String> {
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

#[ui_kit::gpui::test]
fn the_menu_of_a_commit_lists_the_actions_of_idea(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-actions-menu");
    let head = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    right_click(cx, window, "log-commit-0");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    let labels = menu_labels(cx, window);
    for label in [
        "Copy Revision Number",
        "Checkout Revision",
        "New Branch...",
        "New Tag...",
        "Cherry-Pick",
        "Revert Commit",
        "Reset Current Branch to Here...",
        "Compare with Local",
    ] {
        assert!(
            labels.iter().any(|known| known == label),
            "{label} is missing from {labels:?}"
        );
    }

    // Copy Revision Number puts the whole id on the clipboard.
    click(cx, window, COPY);
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(head.as_str()));
}

#[ui_kit::gpui::test]
fn cherry_pick_moves_a_commit_onto_the_current_branch(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-cherry-pick");
    commit_change(&git, &root, "alpha\nBETA\n", "change");
    commit_change(&git, &root, "alpha\nbeta\n", "restore");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    assert_eq!(log_subjects(cx), ["restore", "change", "base"]);

    // The second row of the log is the commit that changed the line.
    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, CHERRY_PICK);

    wait_for(cx, "the cherry-picked log", |cx| {
        log_subjects(cx)
            .first()
            .is_some_and(|subject| subject == "change")
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    assert_eq!(read_text(&root.join("main.txt")), "alpha\nBETA\n");
}

#[ui_kit::gpui::test]
fn revert_adds_the_opposite_commit(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-revert");
    commit_change(&git, &root, "alpha\nBETA\n", "change");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    right_click(cx, window, "log-commit-0");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, REVERT);

    wait_for(cx, "the revert commit", |cx| {
        log_subjects(cx)
            .first()
            .is_some_and(|subject| subject.starts_with("Revert"))
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    assert_eq!(read_text(&root.join("main.txt")), "alpha\nbeta\n");
}

#[ui_kit::gpui::test]
fn reset_hard_rolls_the_branch_back(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("log-reset");
    let base = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    commit_change(&git, &root, "alpha\nBETA\n", "change");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    // The last row is the base commit; Hard moves the branch back to it.
    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, RESET);
    wait_for(cx, "the reset dialog", |cx| {
        has_selector(cx, window, "reset-view")
    });
    click(cx, window, "reset-hard");

    wait_for(cx, "the reset branch", |cx| {
        state.read_with(cx, |state, _| {
            state.status().head.as_deref() == Some(base.as_str())
        })
    });
    assert_eq!(read_text(&root.join("main.txt")), "alpha\nbeta\n");
    assert_eq!(log_subjects(cx).first().map(String::as_str), Some("base"));
}

#[ui_kit::gpui::test]
fn new_branch_of_a_commit_is_checked_out(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("log-new-branch");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    right_click(cx, window, "log-commit-0");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, NEW_BRANCH);
    wait_for(cx, "the branch prompt", |cx| {
        has_selector(cx, window, "prompt")
    });
    type_text(cx, window, "feature");
    // The Checkout tick is on, as IDEA starts the dialog.
    press(cx, window, "enter");

    wait_for(cx, "the new branch", |cx| {
        state.read_with(cx, |state, _| {
            state.status().branch.as_deref() == Some("feature")
        })
    });
}

#[ui_kit::gpui::test]
fn new_tag_of_a_commit_appears_in_the_log(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("log-new-tag");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);

    right_click(cx, window, "log-commit-0");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, NEW_TAG);
    wait_for(cx, "the tag prompt", |cx| {
        has_selector(cx, window, "prompt")
    });
    type_text(cx, window, "v1");
    press(cx, window, "enter");

    wait_for(cx, "the tag label", |cx| {
        cx.read(|cx| {
            cx.global::<ActiveGitLog>()
                .0
                .read(cx)
                .labels_of(0)
                .iter()
                .any(|(name, _)| name == "v1")
        })
    });
}

#[ui_kit::gpui::test]
fn a_conflicting_cherry_pick_opens_the_conflicts_and_abort_restores_the_branch(
    cx: &mut TestAppContext,
) {
    let (root, _bare, git) = repository_with_origin("log-cherry-conflict");
    commit_change(&git, &root, "one\ntwo\nthree\n", "base three");
    commit_change(&git, &root, "one\nTWO\nthree\n", "change");
    let head = commit_change(&git, &root, "one\n2\nthree\n", "other");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    assert_eq!(log_subjects(cx), ["other", "change", "base three", "base"]);

    right_click(cx, window, "log-commit-1");
    wait_for(cx, "the commit menu", |cx| {
        has_selector(cx, window, "menu-0")
    });
    click(cx, window, CHERRY_PICK);
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-row-0")
            && state.read_with(cx, |state, _| state.operation() == Operation::CherryPick)
    });

    click(cx, window, "conflict-abort");
    wait_for(cx, "the aborted cherry-pick", |cx| {
        state.read_with(cx, |state, _| state.operation() == Operation::None)
    });
    let now = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    assert_eq!(now, head, "Abort leaves the branch where it was");
    assert_eq!(read_text(&root.join("main.txt")), "one\n2\nthree\n");
}
