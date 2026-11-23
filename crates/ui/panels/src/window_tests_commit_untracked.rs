//! The Commit window with files Git does not track yet: they start without a tick, the
//! count and the Commit button follow the ticks alone, a ticked new file goes into the
//! commit, a reset after a commit leaves the new files unticked, and the header names no
//! tab twice.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{COMMIT_TOOL, Workspace};

use crate::commit::CommitPanel;
use crate::git::GitState;
use crate::script::ActiveCommitPanel;
use crate::window_tests::{click, draw, press, scratch, type_text};
use crate::window_tests_commit_tree::selector;
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};
use crate::window_tests_git_log_icons::open_window_with_icons;

/// A repository with a committed file, changed when `changed` is set, and a file Git does
/// not know yet when `untracked` is set.
fn repository(name: &str, changed: bool, untracked: bool) -> PathBuf {
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
    if changed {
        fs::write(root.join("main.txt"), "two\n").unwrap();
    }
    if untracked {
        fs::write(root.join("new.txt"), "new\n").unwrap();
    }
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

/// Opens a window over the repository, waits for the files of the status and shows the
/// Commit window.
fn open(
    cx: &mut TestAppContext,
    name: &str,
    changed: bool,
    untracked: bool,
) -> (PathBuf, WindowHandle<Workspace>, Entity<GitState>) {
    let root = repository(name, changed, untracked);
    let window = open_window_with_icons(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let wanted = usize::from(changed) + usize::from(untracked);
    wait_for(cx, "the files of the status", |cx| {
        state.read_with(cx, |state, _| state.status().files.len() == wanted)
    });
    show_commit(cx, window);
    (root, window, state)
}

fn index_of(cx: &TestAppContext, state: &Entity<GitState>, path: &str) -> usize {
    state.read_with(cx, |state, _| {
        state
            .status()
            .files
            .iter()
            .position(|file| file.path == path)
            .unwrap_or_else(|| panic!("the status has no file {path}"))
    })
}

fn ticked(cx: &TestAppContext, path: &str) -> bool {
    commit_panel(cx).read_with(cx, |panel, _| panel.is_checked(path))
}

fn summary(cx: &TestAppContext) -> String {
    commit_panel(cx).read_with(cx, |panel, cx| panel.summary_text(cx))
}

/// A click on a tick leaves the keys in the list: Tab gives them back to the message.
fn type_message_after_tick(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    message: &str,
) {
    press(cx, window, "tab");
    type_text(cx, window, message);
}

#[ui_kit::gpui::test]
fn an_untracked_file_starts_without_a_tick_and_only_the_changed_file_counts(
    cx: &mut TestAppContext,
) {
    let (_root, _window, _state) = open(cx, "commit-untracked-mixed", true, true);

    assert!(
        ticked(cx, "main.txt"),
        "a changed tracked file starts with a tick"
    );
    assert!(
        !ticked(cx, "new.txt"),
        "an untracked file starts without a tick, as IDEA leaves it"
    );
    assert!(
        summary(cx).starts_with("1 changed"),
        "the count takes the ticked file alone: {}",
        summary(cx)
    );
}

#[ui_kit::gpui::test]
fn only_untracked_files_leave_the_commit_button_inactive(cx: &mut TestAppContext) {
    let (_root, window, state) = open(cx, "commit-untracked-only", false, true);

    assert!(!ticked(cx, "new.txt"), "nothing is ticked by itself");
    assert!(
        summary(cx).starts_with("0 changed"),
        "no file is in the commit: {}",
        summary(cx)
    );
    wait_for(cx, "the inactive Commit button", |cx| {
        has_selector(cx, window, "commit-go-disabled")
    });

    click(cx, window, "commit-go");
    draw(cx, window);
    assert_eq!(
        state.read_with(cx, |state, _| state.status().files.len()),
        1,
        "the inactive button commits nothing"
    );
}

#[ui_kit::gpui::test]
fn a_ticked_untracked_file_joins_the_count_and_the_commit(cx: &mut TestAppContext) {
    let (_root, window, state) = open(cx, "commit-untracked-ticked", true, true);
    let index = index_of(cx, &state, "new.txt");

    click(cx, window, selector(format!("commit-check-{index}")));
    draw(cx, window);
    assert!(
        ticked(cx, "new.txt"),
        "the tick of the file starts its commit"
    );
    assert!(
        summary(cx).starts_with("2 changed"),
        "the ticked untracked file joins the count: {}",
        summary(cx)
    );

    type_message_after_tick(cx, window, "add the note");
    click(cx, window, "commit-go");
    wait_for(cx, "the commit of both files", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.status().files.is_empty()
        })
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "add the note");
    let mut files = repo.commit_files(&log[0].id).unwrap();
    files.sort();
    assert_eq!(
        files,
        ["main.txt", "new.txt"],
        "the ticked untracked file goes into the index and the commit"
    );
}

#[ui_kit::gpui::test]
fn a_new_untracked_file_after_a_commit_starts_without_a_tick(cx: &mut TestAppContext) {
    let (root, window, state) = open(cx, "commit-untracked-after", true, true);

    type_text(cx, window, "only the change");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit of the changed file", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none()
                && state.status().files.len() == 1
                && state.status().files[0].path == "new.txt"
        })
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "only the change");

    fs::write(root.join("later.txt"), "later\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the new file in the status", |cx| {
        state.read_with(cx, |state, _| state.status().files.len() == 2)
    });
    draw(cx, window);
    assert!(
        !ticked(cx, "new.txt") && !ticked(cx, "later.txt"),
        "the reset after the commit and the new file keep untracked files unticked"
    );
    assert!(
        summary(cx).starts_with("0 changed"),
        "the untracked files stay out of the count: {}",
        summary(cx)
    );
}

#[ui_kit::gpui::test]
fn the_header_names_commit_once_and_lists_the_two_tabs(cx: &mut TestAppContext) {
    let (_root, window, _state) = open(cx, "commit-untracked-header", true, false);

    assert!(has_selector(cx, window, "commit-header"));
    assert!(has_selector(cx, window, "commit-tab"));
    assert!(has_selector(cx, window, "shelf-tab"));
    assert!(
        !has_selector(cx, window, "commit-title"),
        "the header draws the tabs alone, so the Commit title does not repeat the Commit tab"
    );
}
