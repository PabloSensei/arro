//! The Console tab of the Git window: the commands the IDE ran with their output, the red
//! of a command that failed, and Show in Console of a notification about a failed push.
//! Every repository lives in a scratch folder with its own identity.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{App, TestAppContext, WindowHandle};
use vcs::{CommandRecord, GitRunner, command_console};
use workspace::Workspace;

use crate::git_log::{ActiveGitLog, GitLogPanel, GitTab};
use crate::script::ActiveCommitPanel;
use crate::window_tests::{
    click, draw, open_window, press, scratch, type_text, wait_for_settled_bounds,
};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

fn repository(name: &str) -> (PathBuf, GitRunner) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "alpha\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    (root, git)
}

/// A repository whose remote is no repository at all: every Push runs git and fails,
/// which is what the Console and its notification are read for.
fn repository_with_broken_remote(name: &str) -> (PathBuf, GitRunner) {
    let (root, git) = repository(name);
    git.run(&root, ["remote", "add", "origin", "no-such-remote"])
        .unwrap();
    (root, git)
}

fn open_console(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "alt-9");
    wait_for(cx, "the Git window", |cx| {
        log(cx, |panel, _| panel.tab()) == GitTab::Log
    });
    click(cx, window, "git-tab-console");
    wait_for(cx, "the Console tab", |cx| {
        log(cx, |panel, _| panel.tab()) == GitTab::Console
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn log<T>(cx: &TestAppContext, read: impl FnOnce(&GitLogPanel, &App) -> T) -> T {
    cx.read(|cx| read(cx.global::<ActiveGitLog>().0.read(cx), cx))
}

/// The position of the first command of the Console the test looks for, with the text of
/// its own line and output.
fn console_command(
    cx: &TestAppContext,
    wanted: impl Fn(&vcs::CommandRecord) -> bool,
) -> Option<(usize, vcs::CommandRecord)> {
    log(cx, |panel, cx| {
        panel
            .console_entries(cx)
            .into_iter()
            .enumerate()
            .find(|(_, entry)| wanted(entry))
    })
}

/// A notification about the failed push, as the button of the Console waits for a click.
fn wait_for_failed_push(cx: &mut TestAppContext, window: WindowHandle<Workspace>, root: &Path) {
    let state = git_state(cx);
    state.update(cx, |git, cx| git.push(cx));
    wait_for(cx, "the notification of the failed push", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(title, _)| title == "Push")
                    && workspace
                        .last_notification_buttons()
                        .iter()
                        .any(|button| button == "Show in Console")
            })
            .unwrap_or(false)
    });
    wait_for(cx, "the push in the journal", |cx| {
        log(cx, |panel, cx| {
            panel.console_entries(cx).iter().any(|entry| {
                entry.failed()
                    && entry.args.first().map(String::as_str) == Some("push")
                    && entry.directory.starts_with(root)
            })
        })
    });
}

#[ui_kit::gpui::test]
fn a_commit_shows_its_command_and_its_output_in_the_console(cx: &mut TestAppContext) {
    let (root, _git) = repository("git-console-commit");
    fs::write(root.join("main.txt"), "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| {
            state
                .status()
                .files
                .iter()
                .any(|file| file.path == "main.txt")
        })
    });

    // The Commit window commits the changed file with the message.
    press(cx, window, "alt-0");
    let commit = cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone());
    let handle = commit.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
    type_text(cx, window, "console commit");
    commit.update(cx, |panel, cx| panel.set_checked("main.txt", true, cx));
    commit.update(cx, |panel, cx| panel.commit(false, cx));

    open_console(cx, window);
    wait_for(cx, "the commit in the Console", |cx| {
        console_command(cx, |entry| {
            entry.args.first().map(String::as_str) == Some("commit")
                && entry.stdout.contains("console commit")
        })
        .is_some()
    });
    let (index, entry) = console_command(cx, |entry| {
        entry.args.first().map(String::as_str) == Some("commit")
            && entry.stdout.contains("console commit")
    })
    .expect("the commit is a command of the Console");
    assert_eq!(entry.code, Some(0), "the commit succeeded");
    assert!(
        entry.line().starts_with("git commit"),
        "the Console shows the line of the command: {}",
        entry.line()
    );

    wait_for_settled_bounds(
        cx,
        window,
        Box::leak(format!("git-console-{index}").into_boxed_str()),
    );
    let selector: &'static str = Box::leak(format!("git-console-{index}").into_boxed_str());
    assert!(
        has_selector(cx, window, selector),
        "the Console draws the row of the commit"
    );

    // Copy of the toolbar puts the commands of the tab on the clipboard.
    click(cx, window, "git-console-copy");
    let copied = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy puts the commands of the Console on the clipboard");
    assert!(
        copied.contains("git commit") && copied.contains("console commit"),
        "the clipboard keeps the line and the output of the commit: {copied}"
    );

    // The commands of the ide itself stay out of sight until the checkbox asks for them.
    assert!(
        !log(cx, |panel, cx| panel
            .console_entries(cx)
            .iter()
            .any(|entry| entry.background)),
        "the Console hides the commands of the ide"
    );
    click(cx, window, "git-console-background");
    wait_for(cx, "the commands of the ide", |cx| {
        log(cx, |panel, cx| {
            panel
                .console_entries(cx)
                .iter()
                .any(|entry| entry.background)
        })
    });
}

#[ui_kit::gpui::test]
fn a_failed_command_is_red_in_the_console(cx: &mut TestAppContext) {
    let (root, _git) = repository_with_broken_remote("git-console-failure");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for_failed_push(cx, window, &root);

    open_console(cx, window);
    let (index, entry) = console_command(cx, |entry| {
        entry.failed()
            && entry.args.first().map(String::as_str) == Some("push")
            && entry.directory.starts_with(&root)
    })
    .expect("the failed push is a command of the Console");
    assert!(
        !entry.stderr.is_empty(),
        "the journal keeps what git wrote about the failure"
    );

    cx.run_until_parked();
    draw(cx, window);
    let failed: &'static str = Box::leak(format!("git-console-error-{index}").into_boxed_str());
    assert!(
        has_selector(cx, window, failed),
        "the command that failed is drawn as an error"
    );
}

#[ui_kit::gpui::test]
fn show_in_console_of_a_notification_opens_the_console_on_the_command(cx: &mut TestAppContext) {
    let (root, _git) = repository_with_broken_remote("git-console-show");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for_failed_push(cx, window, &root);
    let pushed = log(cx, |panel, cx| {
        panel
            .console_entries(cx)
            .into_iter()
            .rev()
            .find(|entry| {
                entry.failed()
                    && entry.directory.starts_with(&root)
                    && entry.args.first().map(String::as_str) == Some("push")
            })
            .map(|entry| entry.id)
    })
    .expect("the failed push is in the journal");

    // A command of the ide, and a command of another window of the same project, fail
    // later: the notification still opens the command it was made for.
    command_console().record(CommandRecord::new(
        &root,
        &["status", "--porcelain=v1"],
        Some(1),
        std::time::Duration::ZERO,
        b"",
        b"fatal: a background read failed",
    ));
    command_console().record(CommandRecord::new(
        &root,
        &["pull"],
        Some(1),
        std::time::Duration::ZERO,
        b"",
        b"fatal: a later pull failed",
    ));

    click(cx, window, "notification-action");

    wait_for(cx, "the Console tab of the failed command", |cx| {
        log(cx, |panel, _| panel.tab()) == GitTab::Console
    });
    cx.run_until_parked();
    draw(cx, window);
    let selected = log(cx, |panel, _| panel.console_selected()).expect("a command is picked");
    assert_eq!(
        selected, pushed,
        "the Console opens on the failed push, not on a later command of the ide"
    );
    let entry = log(cx, |panel, cx| {
        panel
            .console_entries(cx)
            .into_iter()
            .find(|entry| entry.id == selected)
    })
    .expect("the picked command is shown");
    assert!(
        entry.failed() && entry.args.first().map(String::as_str) == Some("push"),
        "the Console opens on the failed push: {}",
        entry.line()
    );
    assert!(
        entry.directory.starts_with(&root),
        "the command of the project is the one shown: {}",
        entry.directory.display()
    );
}
