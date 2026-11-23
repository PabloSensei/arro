//! Amend in the Commit window: the field takes the whole message of the last commit, with
//! its subject, its blank line and its body, and an untick brings back the text the field
//! had before, unless the user changed the text Amend put there.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{COMMIT_TOOL, Workspace};

use crate::commit::CommitPanel;
use crate::git::GitState;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{draw, open_window, press, scratch};
use crate::window_tests_git::{git, git_state, wait_for, wait_for_repository};

const MESSAGE: &str = "Fix the parser\n\nKeep the old rules.\nAdd a test for them.";

/// A repository whose one commit has `message`, with nothing changed after it. The message
/// goes in through a file, so its blank line and its body stay as they are.
fn repository(name: &str, message: &str) -> PathBuf {
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
    let message_file = scratch(&format!("{name}-message")).join("message.txt");
    fs::write(&message_file, message).unwrap();
    git.run(&root, ["commit", "-F", message_file.to_str().unwrap()])
        .unwrap();
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

/// A window over the repository with the Commit window shown, and the state of Git in it.
fn open(cx: &mut TestAppContext, root: &Path) -> (WindowHandle<Workspace>, Entity<GitState>) {
    let window = open_window(cx, root.to_path_buf());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    show_commit(cx, window);
    (window, state)
}

/// Turns Amend on with Alt+M and waits until the field holds the message of the last commit,
/// which is when Amend has filled the field.
fn tick_amend(cx: &mut TestAppContext, window: WindowHandle<Workspace>, message: &'static str) {
    press(cx, window, "alt-m");
    wait_for(cx, "the message of the last commit", |cx| {
        commit_panel(cx).read_with(cx, |panel, _| {
            panel.amending() && panel.message() == message
        })
    });
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn amend_puts_the_whole_message_of_the_last_commit_in_the_field(cx: &mut TestAppContext) {
    let root = repository("amend-whole-message", MESSAGE);
    let (window, _state) = open(cx, &root);

    tick_amend(cx, window, MESSAGE);
    assert_eq!(
        commit_panel(cx).read_with(cx, |panel, _| panel.message().to_string()),
        MESSAGE,
        "the body and the blank line between it and the subject are in the field"
    );
}

#[ui_kit::gpui::test]
fn unticking_amend_brings_back_the_text_the_field_had(cx: &mut TestAppContext) {
    let root = repository("amend-untick-back", MESSAGE);
    let (window, _state) = open(cx, &root);
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("a note", cx));
    draw(cx, window);

    tick_amend(cx, window, MESSAGE);
    press(cx, window, "alt-m");
    let (amending, message) = commit_panel(cx).read_with(cx, |panel, _| {
        (panel.amending(), panel.message().to_string())
    });
    assert!(!amending, "the untick takes Amend off");
    assert_eq!(message, "a note", "the text the field had comes back");
}

#[ui_kit::gpui::test]
fn unticking_amend_keeps_the_text_the_user_wrote_after_the_tick(cx: &mut TestAppContext) {
    let root = repository("amend-untick-kept", MESSAGE);
    let (window, _state) = open(cx, &root);
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("a note", cx));
    draw(cx, window);

    tick_amend(cx, window, MESSAGE);
    commit_panel(cx).update(cx, |panel, cx| panel.set_message("changed by hand", cx));
    draw(cx, window);
    press(cx, window, "alt-m");
    assert_eq!(
        commit_panel(cx).read_with(cx, |panel, _| panel.message().to_string()),
        "changed by hand",
        "a text the user changed after the tick is theirs, and an untick keeps it"
    );
}
