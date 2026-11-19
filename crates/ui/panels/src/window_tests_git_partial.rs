//! Partial commits of the Commit window and the Push window, in the window without a
//! window: the ticks of the hunks of a file, the patch of the index, and the commits of
//! the branch on their way to a remote in a scratch folder.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::{COMMIT_TOOL, Workspace};

use crate::script::ActiveCommitPanel;
use crate::window_tests::{click, draw, open_window, press, scratch, type_text};
use crate::window_tests_git::{
    git, git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// Puts the focus on the Commit window, as Alt+0 and the tool button do.
fn focus_commit(cx: &mut TestAppContext, window: gpui::WindowHandle<Workspace>) {
    let panel = cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone());
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
}

fn repository(name: &str) -> (PathBuf, vcs::GitRunner) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\ngamma\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    (root, git)
}

/// A repository with a bare remote and no upstream on the branch yet.
fn repository_with_remote(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let git = git();
    let bare = scratch(&format!("{name}-origin"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    let root = scratch(name);
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(
        &root,
        [
            "remote",
            "add",
            "origin",
            bare.to_str().expect("a UTF-8 scratch path"),
        ],
    )
    .unwrap();
    (root, bare, git)
}

fn subject(git: &vcs::GitRunner, root: &Path) -> String {
    let output = git.run(root, ["log", "-1", "--format=%s"]).unwrap();
    String::from_utf8_lossy(&output).trim().to_string()
}

fn shown(git: &vcs::GitRunner, root: &Path, spec: &str) -> String {
    let output = git.run(root, ["show", spec]).unwrap();
    String::from_utf8_lossy(&output).replace("\r\n", "\n")
}

fn bare_subject(git: &vcs::GitRunner, bare: &Path) -> String {
    let output = git
        .run(bare, ["log", "--all", "-1", "--format=%s"])
        .unwrap();
    String::from_utf8_lossy(&output).trim().to_string()
}

fn commit_all(git: &vcs::GitRunner, root: &Path, message: &str) {
    git.run(root, ["add", "-A"]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

/// Opens the Commit window and the diff of its only file, with the hunk ticks on screen.
fn open_commit_diff(
    cx: &mut TestAppContext,
    window: gpui::WindowHandle<Workspace>,
    state: &gpui::Entity<crate::git::GitState>,
) {
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    focus_commit(cx, window);
    draw(cx, window);
    click(cx, window, "commit-row-0");
    press(cx, window, "ctrl-d");
    wait_for(cx, "the diff of the file", |cx| {
        has_selector(cx, window, "diff-side-by-side")
    });
}

/// Clicks the markup the diff of the Commit window draws for its first hunk.
fn take_off_first_hunk(cx: &mut TestAppContext, window: gpui::WindowHandle<Workspace>) {
    click(cx, window, "commit-hunk-0");
    press(cx, window, "escape");
    // Escape gives the editor the focus back; the Commit window and its keys come back.
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    focus_commit(cx, window);
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_hunk_taken_off_stays_in_the_working_tree(cx: &mut TestAppContext) {
    let (root, git) = repository("git-partial");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\nGAMMA\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    open_commit_diff(cx, window, &state);
    take_off_first_hunk(cx, window);
    wait_for(cx, "the partial tick of the file", |cx| {
        has_selector(cx, window, "commit-partial-0")
    });

    // The keys go to the list after the click on the row; Tab moves them to the message.
    press(cx, window, "tab");
    type_text(cx, window, "only the second hunk");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.last_error().is_none()
        })
    });

    assert_eq!(subject(&git, &root), "only the second hunk");
    assert_eq!(
        shown(&git, &root, "HEAD:main.txt"),
        "alpha\nbeta\nGAMMA\n",
        "the commit holds the second hunk alone"
    );
    assert_eq!(
        fs::read_to_string(root.join("main.txt")).unwrap(),
        "ALPHA\nbeta\nGAMMA\n",
        "both hunks stay in the working tree"
    );
    let status = state.read_with(cx, |state, _| state.status().clone());
    assert_eq!(status.files.len(), 1);
}

#[ui_kit::gpui::test]
fn a_selection_that_no_longer_matches_does_not_commit(cx: &mut TestAppContext) {
    let (root, git) = repository("git-partial-stale");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\nGAMMA\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    open_commit_diff(cx, window, &state);
    take_off_first_hunk(cx, window);

    // Another edit moves the hunks the user picked: the patch of the selection would not
    // match the file any more.
    fs::write(root.join("main.txt"), "ALPHA\nBETA\ngamma\nGAMMA\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    // The keys go to the list after the click on the row; Tab moves them to the message.
    press(cx, window, "tab");
    type_text(cx, window, "must not land");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the failed commit", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none()
                && state
                    .last_error()
                    .is_some_and(|error| error.contains("changed since"))
        })
    });

    assert_eq!(subject(&git, &root), "base", "no commit was made");
    assert_eq!(
        fs::read_to_string(root.join("main.txt")).unwrap(),
        "ALPHA\nBETA\ngamma\nGAMMA\n",
        "the working tree keeps the text of the user"
    );
}

#[ui_kit::gpui::test]
fn a_patch_git_refuses_leaves_no_commit(cx: &mut TestAppContext) {
    let root = scratch("git-partial-crlf");
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    git.run(&root, ["config", "core.autocrlf", "false"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\r\nbeta\r\ngamma\r\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();

    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\r\nbeta\r\nGAMMA\r\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    open_commit_diff(cx, window, &state);
    take_off_first_hunk(cx, window);
    press(cx, window, "tab");
    type_text(cx, window, "must not land");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the refused patch", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none()
                && state
                    .last_error()
                    .is_some_and(|error| error.contains("do not apply"))
        })
    });

    assert_eq!(subject(&git, &root), "base", "no commit was made");
    assert_eq!(
        fs::read(root.join("main.txt")).unwrap(),
        b"ALPHA\r\nbeta\r\nGAMMA\r\n",
        "the working tree keeps the text of the user"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_k_shows_the_commits_and_push_sends_them(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-push-window");
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
    wait_for(cx, "the files of the commit", |cx| {
        has_selector(cx, window, "push-file-0")
    });
    click(cx, window, "push-go");
    wait_for(cx, "the pushed commit", |cx| {
        let _ = cx;
        bare_subject(&git, &bare) == "local work"
    });
}

#[ui_kit::gpui::test]
fn force_push_replaces_the_remote_after_the_warning(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("git-force-push");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "alpha\nbeta\nfirst\n").unwrap();
    commit_all(&git, &root, "first");
    git.run(&root, ["push"]).unwrap();
    fs::write(root.join("main.txt"), "alpha\nbeta\nrewritten\n").unwrap();
    commit_all(&git, &root, "rewritten");
    git.run(&root, ["commit", "--amend", "-m", "rewritten"])
        .unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-dialog")
    });
    click(cx, window, "push-force");
    wait_for(cx, "the Force Push item", |cx| {
        has_selector(cx, window, "push-force-item")
    });
    click(cx, window, "push-force-item");
    wait_for(cx, "the warning of Force Push", |cx| {
        has_selector(cx, window, "push-force-confirm")
    });
    click(cx, window, "push-force-confirm");
    wait_for(cx, "the replaced history", |cx| {
        let _ = cx;
        bare_subject(&git, &bare) == "rewritten"
    });
}

#[ui_kit::gpui::test]
fn a_branch_without_upstream_gets_one_from_the_push_window(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_remote("git-push-upstream");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the commits of the branch", |cx| {
        state.read_with(cx, |state, _| state.status().head.is_some())
    });

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-dialog")
    });
    wait_for(cx, "the commit on its way", |cx| {
        has_selector(cx, window, "push-commit-0")
    });
    click(cx, window, "push-go");
    wait_for(cx, "the upstream of the branch", |cx| {
        let _ = cx;
        git.run(&root, ["rev-parse", "--abbrev-ref", "main@{upstream}"])
            .is_ok_and(|output| String::from_utf8_lossy(&output).trim() == "origin/main")
    });
    assert_eq!(bare_subject(&git, &bare), "base");
}
