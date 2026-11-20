//! The Push window closes on its Cancel button, on Escape and once the remote has the
//! commits. Clicks and keys close it, as the user closes it: no test calls `close` itself.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Workspace;

use crate::window_tests::{click, open_window, press};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

fn commit_all(git: &vcs::GitRunner, root: &Path, message: &str) {
    git.run(root, ["add", "-A"]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

fn bare_subject(git: &vcs::GitRunner, bare: &Path) -> String {
    let output = git
        .run(bare, ["log", "--all", "-1", "--format=%s"])
        .unwrap();
    String::from_utf8_lossy(&output).trim().to_string()
}

/// Opens the Push window of a repository whose one local commit waits for its remote.
fn open_push_window(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    gpui::WindowHandle<Workspace>,
    PathBuf,
    PathBuf,
    vcs::GitRunner,
) {
    let (root, bare, git) = repository_with_origin(name);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "alpha\nbeta\nlocal\n").unwrap();
    commit_all(&git, &root, "local work");
    state.update(cx, |state, cx| state.refresh(cx));

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-dialog")
    });
    wait_for(cx, "the commit on its way", |cx| {
        has_selector(cx, window, "push-commit-0")
    });
    (window, root, bare, git)
}

#[ui_kit::gpui::test]
fn the_cancel_button_closes_the_push_window(cx: &mut TestAppContext) {
    let (window, _root, _bare, _git) = open_push_window(cx, "push-cancel-click");
    wait_for(cx, "the Cancel button", |cx| {
        has_selector(cx, window, "push-cancel")
    });
    click(cx, window, "push-cancel");
    assert!(
        !has_selector(cx, window, "push-dialog"),
        "a click on Cancel closes the Push window"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_push_window(cx: &mut TestAppContext) {
    let (window, _root, _bare, _git) = open_push_window(cx, "push-cancel-escape");
    press(cx, window, "escape");
    assert!(
        !has_selector(cx, window, "push-dialog"),
        "Escape closes the Push window"
    );
}

#[ui_kit::gpui::test]
fn the_push_window_closes_once_the_remote_has_the_commits(cx: &mut TestAppContext) {
    let (window, _root, bare, git) = open_push_window(cx, "push-closes-after-push");
    click(cx, window, "push-go");
    wait_for(cx, "the pushed commit", |_| {
        bare_subject(&git, &bare) == "local work"
    });
    wait_for(cx, "the Push window to close", |cx| {
        !has_selector(cx, window, "push-dialog")
    });
}
