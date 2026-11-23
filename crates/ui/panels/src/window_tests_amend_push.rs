//! Commit and Push after an Amend. A commit the upstream has is rewritten only after the user
//! agreed to the force push, and the agreed push uses a lease. Cancel writes nothing, a commit
//! the remote does not have goes without a question, and a lease the remote refuses shows an
//! error with the text of git. Every remote is a bare folder in a scratch directory.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::NotificationLevel;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use vcs::GitRunner;
use workspace::{COMMIT_TOOL, Workspace};

use crate::commit::{CommitPanel, force_push_question};
use crate::git::GitState;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{click, draw, open_window, press, scratch};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// What `what` names in the repository, as git writes it.
fn rev(git: &GitRunner, directory: &Path, what: &str) -> String {
    let output = git.run(directory, ["rev-parse", what]).unwrap();
    String::from_utf8(output).unwrap().trim().to_owned()
}

/// The branch HEAD of the repository is on.
fn branch_of(git: &GitRunner, root: &Path) -> String {
    let output = git
        .run(root, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    String::from_utf8(output).unwrap().trim().to_owned()
}

/// The commit the branch has on the remote.
fn remote_head(git: &GitRunner, bare: &Path, branch: &str) -> String {
    rev(git, bare, &format!("refs/heads/{branch}"))
}

fn commit_all(git: &GitRunner, root: &Path, message: &str) {
    git.run(root, ["add", "-A"]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

fn commit_panel(cx: &TestAppContext) -> Entity<CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

/// Shows the Commit window with the focus in it and draws it once.
fn show_commit(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let focus = commit_panel(cx).read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// A repository whose `main` has the commit `first` as its last one, pushed to the remote.
/// The remote is returned with the repository, and the git runner that works on both.
fn pushed_repository(name: &str) -> (PathBuf, PathBuf, GitRunner) {
    let (root, bare, git) = repository_with_origin(name);
    fs::write(root.join("main.txt"), "alpha\nbeta\nfirst\n").unwrap();
    commit_all(&git, &root, "first");
    git.run(&root, ["push", "-q"]).unwrap();
    (root, bare, git)
}

/// The Commit window over the repository with Amend ticked: the field holds the message of
/// the last commit, `last`.
fn open_amend(
    cx: &mut TestAppContext,
    root: &Path,
    last: &'static str,
) -> (WindowHandle<Workspace>, Entity<GitState>) {
    let window = open_window(cx, root.to_path_buf());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    show_commit(cx, window);
    press(cx, window, "alt-m");
    wait_for(cx, "the message of the last commit", |cx| {
        commit_panel(cx).read_with(cx, |panel, _| panel.amending() && panel.message() == last)
    });
    draw(cx, window);
    (window, state)
}

/// Writes `message` into the field and draws the window, so its buttons are the ones shown.
fn write_message(cx: &mut TestAppContext, window: WindowHandle<Workspace>, message: &str) {
    commit_panel(cx).update(cx, |panel, cx| panel.set_message(message, cx));
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn commit_and_push_asks_before_an_amend_force_pushes_the_pushed_commit(cx: &mut TestAppContext) {
    let (root, bare, git) = pushed_repository("amend-push-ask");
    let (window, state) = open_amend(cx, &root, "first");
    write_message(cx, window, "first, amended");
    let branch = branch_of(&git, &root);
    let before = rev(&git, &root, "HEAD");
    assert_eq!(
        remote_head(&git, &bare, &branch),
        before,
        "the commit to amend is on the remote"
    );

    click(cx, window, "commit-push");
    wait_for(cx, "the force push question", |cx| {
        has_selector(cx, window, "prompt")
    });
    assert_eq!(
        rev(&git, &root, "HEAD"),
        before,
        "nothing is written before the answer"
    );
    assert_eq!(remote_head(&git, &bare, &branch), before);

    click(cx, window, "prompt-confirm");
    wait_for(cx, "the amended commit on the remote", |cx| {
        remote_head(&git, &bare, &branch) != before
            && state.read_with(cx, |state, _| state.busy().is_none())
    });
    assert_eq!(
        rev(&git, &root, "HEAD"),
        remote_head(&git, &bare, &branch),
        "the remote takes the amended commit"
    );
    let target = format!("refs/heads/{branch}");
    let count = String::from_utf8(
        git.run(&bare, ["rev-list", "--count", target.as_str()])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        count.trim(),
        "2",
        "the amended commit replaces the pushed one, none is added"
    );
    let subject = String::from_utf8(
        git.run(&bare, ["log", "-1", "--format=%s", target.as_str()])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(subject.trim(), "first, amended");
    assert!(
        !has_selector(cx, window, "prompt"),
        "the answer closes the question"
    );
}

#[ui_kit::gpui::test]
fn cancel_leaves_the_commit_the_remote_and_the_message_as_they_were(cx: &mut TestAppContext) {
    let (root, bare, git) = pushed_repository("amend-push-cancel");
    let (window, _state) = open_amend(cx, &root, "first");
    write_message(cx, window, "first, amended");
    let branch = branch_of(&git, &root);
    let before = rev(&git, &root, "HEAD");

    click(cx, window, "commit-push");
    wait_for(cx, "the force push question", |cx| {
        has_selector(cx, window, "prompt")
    });
    click(cx, window, "prompt-cancel");
    wait_for(cx, "the question to close", |cx| {
        !has_selector(cx, window, "prompt")
    });

    assert_eq!(rev(&git, &root, "HEAD"), before, "Cancel writes nothing");
    assert_eq!(
        remote_head(&git, &bare, &branch),
        before,
        "the remote keeps its commit"
    );
    let (amending, message) = commit_panel(cx).read_with(cx, |panel, _| {
        (panel.amending(), panel.message().to_string())
    });
    assert!(
        amending && message == "first, amended",
        "the message and Amend stay for another try"
    );
}

#[ui_kit::gpui::test]
fn commit_without_push_rewrites_a_pushed_commit_without_a_question(cx: &mut TestAppContext) {
    let (root, bare, git) = pushed_repository("amend-commit-only");
    let (window, state) = open_amend(cx, &root, "first");
    write_message(cx, window, "first, amended");
    let branch = branch_of(&git, &root);
    let before = rev(&git, &root, "HEAD");

    click(cx, window, "commit-go");
    wait_for(cx, "the amended commit", |cx| {
        rev(&git, &root, "HEAD") != before && state.read_with(cx, |state, _| state.busy().is_none())
    });
    assert!(
        !has_selector(cx, window, "prompt"),
        "a commit that is not pushed asks nothing"
    );
    assert_eq!(
        remote_head(&git, &bare, &branch),
        before,
        "the remote keeps its commit"
    );
}

#[ui_kit::gpui::test]
fn commit_and_push_of_an_amend_of_an_unpushed_commit_asks_nothing(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("amend-push-plain");
    fs::write(root.join("main.txt"), "alpha\nbeta\nlocal\n").unwrap();
    commit_all(&git, &root, "local work");
    let (window, _state) = open_amend(cx, &root, "local work");
    write_message(cx, window, "local work, amended");
    let branch = branch_of(&git, &root);
    let remote_before = remote_head(&git, &bare, &branch);

    click(cx, window, "commit-push");
    wait_for(cx, "the amended commit on the remote", |_| {
        remote_head(&git, &bare, &branch) == rev(&git, &root, "HEAD")
            && remote_head(&git, &bare, &branch) != remote_before
    });
    assert!(
        !has_selector(cx, window, "prompt"),
        "the remote does not have the commit, so the push goes without a question"
    );
}

#[ui_kit::gpui::test]
fn a_lease_the_remote_refuses_shows_an_error_with_the_text_of_git(cx: &mut TestAppContext) {
    let (root, bare, git) = pushed_repository("amend-push-refused");
    let branch = branch_of(&git, &root);
    // Another clone pushes a commit this window has not fetched, so its lease is stale.
    let other = scratch("amend-push-refused-other");
    git.run(
        &other,
        [
            "clone",
            "-q",
            "-b",
            branch.as_str(),
            bare.to_str().unwrap(),
            ".",
        ],
    )
    .unwrap();
    git.run(&other, ["config", "user.email", "other@example.com"])
        .unwrap();
    git.run(&other, ["config", "user.name", "Other"]).unwrap();
    fs::write(other.join("other.txt"), "other\n").unwrap();
    commit_all(&git, &other, "other work");
    git.run(&other, ["push", "-q"]).unwrap();
    let remote = remote_head(&git, &bare, &branch);

    let (window, _state) = open_amend(cx, &root, "first");
    write_message(cx, window, "first, amended");
    click(cx, window, "commit-push");
    wait_for(cx, "the force push question", |cx| {
        has_selector(cx, window, "prompt")
    });
    click(cx, window, "prompt-confirm");
    wait_for(cx, "the error of the refused push", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification_level())
            .unwrap()
            == Some(NotificationLevel::Error)
    });
    let text = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(_, text)| text.to_string())
        .unwrap_or_default();
    assert!(text.contains("rejected"), "the text of git: {text}");
    assert_eq!(
        remote_head(&git, &bare, &branch),
        remote,
        "the remote keeps the commit of the other clone"
    );
}

#[test]
fn the_force_push_question_warns_only_about_a_protected_branch() {
    let plain = "The amended commit was already pushed. Force push with lease?";
    assert_eq!(force_push_question(Some("feature")), plain);
    assert_eq!(force_push_question(None), plain);
    for branch in ["main", "master"] {
        let question = force_push_question(Some(branch));
        assert!(question.starts_with(plain), "{question}");
        assert!(
            question.contains(&format!("Warning: {branch} is a protected branch")),
            "{question}"
        );
    }
}
