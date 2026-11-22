//! The Commit window with Amend and no ticked file: Amend with a new message rewrites the
//! last commit and keeps its tree, the message of the last commit leaves the Commit buttons
//! inactive, and a commit without Amend and without files stays inactive as before.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{COMMIT_TOOL, Workspace};

use crate::commit::CommitPanel;
use crate::git::GitState;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{click, draw, press, scratch};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};
use crate::window_tests_git_log_icons::open_window_with_icons;

/// A repository with one commit of `main.txt` and nothing changed after it.
fn repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "one\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    root
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

/// Opens a window over the repository and shows the Commit window.
fn open(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, Entity<GitState>) {
    let root = repository(name);
    let window = open_window_with_icons(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    show_commit(cx, window);
    (root, window, state)
}

/// What `what` names in the repository, as git writes it.
fn rev(root: &Path, what: &str) -> String {
    let output = git().run(root, ["rev-parse", what]).unwrap();
    String::from_utf8(output).unwrap().trim().to_owned()
}

fn summary(cx: &TestAppContext) -> String {
    commit_panel(cx).read_with(cx, |panel, cx| panel.summary_text(cx))
}

/// Turns Amend on with Alt+M and waits until the field holds the message of the last
/// commit, which is also when the message of the last commit is known.
fn amend_with_last_message(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-m");
    wait_for(cx, "the message of the last commit", |cx| {
        commit_panel(cx).read_with(cx, |panel, _| panel.amending() && panel.message() == "base")
    });
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn amend_with_a_new_message_rewrites_the_last_commit_without_files(cx: &mut TestAppContext) {
    let (root, window, state) = open(cx, "commit-amend-message");
    amend_with_last_message(cx, window);
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("amend the note", cx));
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "commit-go-disabled"),
        "a new message makes the Commit button active without a ticked file"
    );
    assert_eq!(summary(cx), "Amend · message only");

    let before = rev(&root, "HEAD");
    let tree = rev(&root, "HEAD^{tree}");
    click(cx, window, "commit-go");
    wait_for(cx, "the amended commit", |cx| {
        rev(&root, "HEAD") != before && state.read_with(cx, |state, _| state.busy().is_none())
    });

    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    let log = repo.log(10, 0).unwrap();
    assert_eq!(log.len(), 1, "the last commit is rewritten, none is added");
    assert_eq!(log[0].subject, "amend the note");
    assert_eq!(
        rev(&root, "HEAD^{tree}"),
        tree,
        "the files of the commit stay"
    );
    assert_eq!(fs::read_to_string(root.join("main.txt")).unwrap(), "one\n");
    assert!(
        state.read_with(cx, |state, _| state.status().files.is_empty()),
        "the amend leaves no change behind"
    );
}

#[ui_kit::gpui::test]
fn amend_with_the_message_of_the_last_commit_leaves_commit_inactive(cx: &mut TestAppContext) {
    let (root, window, _state) = open(cx, "commit-amend-same");
    amend_with_last_message(cx, window);
    assert!(
        has_selector(cx, window, "commit-go-disabled"),
        "the message of the last commit changes nothing, so Commit stays inactive"
    );
    assert_eq!(summary(cx), "Amend · message only");

    // The same panel is active with a different message, so the inactive state above is
    // the message and not a view that never drew the buttons.
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("base and more", cx));
    draw(cx, window);
    assert!(!has_selector(cx, window, "commit-go-disabled"));
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("base", cx));
    draw(cx, window);
    assert!(has_selector(cx, window, "commit-go-disabled"));

    let before = rev(&root, "HEAD");
    click(cx, window, "commit-go");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the refusal to amend", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("Change the message or tick a file"))
    });
    assert_eq!(
        rev(&root, "HEAD"),
        before,
        "neither the inactive button nor Ctrl+Enter amends the commit"
    );
}

#[ui_kit::gpui::test]
fn a_commit_without_amend_or_files_stays_inactive(cx: &mut TestAppContext) {
    let (root, window, _state) = open(cx, "commit-amend-off");
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("a note", cx));
    draw(cx, window);
    assert!(
        has_selector(cx, window, "commit-go-disabled"),
        "without Amend and without a ticked file Commit stays inactive"
    );
    assert!(summary(cx).starts_with("0 changed"), "{}", summary(cx));

    let before = rev(&root, "HEAD");
    click(cx, window, "commit-go");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the refusal to commit", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_some_and(|(_, text)| text.contains("Tick at least one file"))
    });
    assert_eq!(rev(&root, "HEAD"), before, "nothing went to git");
}

/// The change of a file the user unticked stays in the working tree: the Amend without
/// files rewrites the message on the tree of the last commit and stages nothing.
#[ui_kit::gpui::test]
fn amend_without_files_leaves_an_unticked_change_out_of_the_commit(cx: &mut TestAppContext) {
    let (root, window, state) = open(cx, "commit-amend-unticked");
    fs::write(root.join("main.txt"), "two\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });
    commit_panel(cx).update(cx, |panel, cx| panel.set_checked("main.txt", false, cx));
    amend_with_last_message(cx, window);
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("amend the note", cx));
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "commit-go-disabled"),
        "a new message makes Amend run although the change is unticked"
    );

    let before = rev(&root, "HEAD");
    let tree = rev(&root, "HEAD^{tree}");
    click(cx, window, "commit-go");
    wait_for(cx, "the amended commit", |cx| {
        rev(&root, "HEAD") != before && state.read_with(cx, |state, _| state.busy().is_none())
    });

    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    assert_eq!(repo.log(10, 0).unwrap()[0].subject, "amend the note");
    assert_eq!(
        rev(&root, "HEAD^{tree}"),
        tree,
        "the unticked change stays out of the commit"
    );
    assert_eq!(fs::read_to_string(root.join("main.txt")).unwrap(), "two\n");
    let status = String::from_utf8(git().run(&root, ["status", "--porcelain"]).unwrap()).unwrap();
    assert!(
        status.contains(" M main.txt"),
        "the change is still in the working tree, unstaged: {status}"
    );
}
