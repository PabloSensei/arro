//! The details pane of the Git Log without a window: the subject is drawn once, the body
//! follows it, the short hash, the author and the date sit on one line, the hash copies the
//! whole id, a web address opens through the links of the window, and the branch line names
//! every branch that contains the commit. Every repository lives in a scratch folder.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use editor_view::ExternalLinks;
use ui_kit::NotificationLevel;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Workspace;

use crate::window_tests::{click, open_window, scratch};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};
use crate::window_tests_git_window::{log, open_log};

/// A repository with one commit on `main` and the id of that commit.
fn repository(name: &str, message: &str) -> (PathBuf, String) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "base\n").unwrap();
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", message]).unwrap();
    let id = String::from_utf8_lossy(&git.run(&root, ["rev-parse", "HEAD"]).unwrap())
        .trim()
        .to_owned();
    (root, id)
}

/// Opens the Log, selects its first commit and waits for the details of it.
fn open_details(cx: &mut TestAppContext, window: ui_kit::gpui::WindowHandle<Workspace>) {
    open_log(cx, window);
    click(cx, window, "log-commit-0");
    wait_for(cx, "the details of the commit", |cx| {
        log(cx, |panel| panel.details().is_some())
    });
}

#[ui_kit::gpui::test]
fn the_details_of_a_commit_without_a_body_show_the_subject_once(cx: &mut TestAppContext) {
    let (root, _id) = repository("git-log-details-subject", "subject only");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    assert!(
        has_selector(cx, window, "log-details-subject"),
        "the subject is the first line of the details"
    );
    assert_eq!(
        log(cx, |panel| panel.details_body()),
        None,
        "a commit without a body has no second copy of the subject"
    );
    assert!(
        !has_selector(cx, window, "log-details-message"),
        "the body takes no row when the message has none"
    );
}

#[ui_kit::gpui::test]
fn the_details_show_the_short_hash_the_author_and_copy_the_whole_id(cx: &mut TestAppContext) {
    let (root, id) = repository("git-log-details-identity", "identity");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    assert_eq!(
        log(cx, |panel| panel.details_hash()),
        Some(id.chars().take(7).collect()),
        "the line starts with the short hash"
    );
    let identity = log(cx, |panel| panel.details_identity()).unwrap_or_default();
    assert!(identity.starts_with("Window Test <window-test@example.com> on "));
    assert!(
        has_selector(cx, window, "log-details-hash")
            && has_selector(cx, window, "log-details-author"),
        "the short hash and the author sit on one line of the details"
    );

    click(cx, window, "log-details-hash");
    let copied = cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(
        copied.as_deref(),
        Some(id.as_str()),
        "a click on the short hash copies the whole id"
    );
    let level = window
        .read_with(cx, |workspace, _| workspace.last_notification_level())
        .unwrap();
    assert_eq!(
        level,
        Some(NotificationLevel::Info),
        "the window tells the user that the id is copied"
    );
}

#[ui_kit::gpui::test]
fn the_details_of_a_commit_with_a_body_show_it_and_open_its_link(cx: &mut TestAppContext) {
    let (root, _id) = repository(
        "git-log-details-body",
        "subject line\n\nBody text\n\nSee https://example.invalid/docs for details",
    );
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = opened.clone();
    cx.update(|cx| {
        cx.set_global(ExternalLinks(Some(Arc::new(move |url: &str| {
            recorder.lock().unwrap().push(url.to_owned());
        }))));
    });
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    assert_eq!(
        log(cx, |panel| panel.details_body()),
        Some("Body text\n\nSee https://example.invalid/docs for details".to_owned()),
        "the body is the message without its subject"
    );
    assert!(
        has_selector(cx, window, "log-details-message"),
        "the body has a row of its own"
    );

    click(cx, window, "log-details-link-2-1");
    wait_for(cx, "the opened link", |_| {
        !opened.lock().unwrap().is_empty()
    });
    assert_eq!(
        opened.lock().unwrap().as_slice(),
        ["https://example.invalid/docs"],
        "the web address of the body opens through the links of the window"
    );
}

#[ui_kit::gpui::test]
fn the_branch_line_names_both_branches_that_contain_the_commit(cx: &mut TestAppContext) {
    let (root, _id) = repository("git-log-details-branches", "shared");
    git().run(&root, ["branch", "feature"]).unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    let line = log(cx, |panel| panel.details_branch_line()).unwrap_or_default();
    assert!(
        line.contains("main") && line.contains("feature"),
        "the branch line names both branches: {line}"
    );
    assert!(
        has_selector(cx, window, "log-details-branches"),
        "the details draw the branch line"
    );
}

#[ui_kit::gpui::test]
fn more_than_five_branches_collapse_and_the_rest_open_on_a_click(cx: &mut TestAppContext) {
    let (root, _id) = repository("git-log-details-many-branches", "shared");
    for branch in ["one", "two", "three", "four", "five", "six"] {
        git().run(&root, ["branch", branch]).unwrap();
    }
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    let collapsed = log(cx, |panel| panel.details_branch_line()).unwrap_or_default();
    assert!(
        collapsed.contains("and 2 more"),
        "seven branches keep five and count the rest: {collapsed}"
    );
    assert!(
        has_selector(cx, window, "log-details-branches-more"),
        "the rest of the branches is a link"
    );

    click(cx, window, "log-details-branches-more");
    let expanded = log(cx, |panel| panel.details_branch_line()).unwrap_or_default();
    assert!(
        !expanded.contains("more"),
        "a click opens every branch: {expanded}"
    );
    for branch in ["one", "two", "three", "four", "five", "six"] {
        assert!(expanded.contains(branch), "{branch} is listed: {expanded}");
    }
}

#[ui_kit::gpui::test]
fn a_committer_other_than_the_author_gets_a_line_of_their_own(cx: &mut TestAppContext) {
    let (root, _id) = repository("git-log-details-committer", "amended");
    let output = std::process::Command::new("git")
        .current_dir(&root)
        .args(["commit", "--amend", "--no-edit"])
        .env("GIT_COMMITTER_NAME", "Carol Committer")
        .env("GIT_COMMITTER_EMAIL", "carol@example.invalid")
        .env("GIT_COMMITTER_DATE", "2024-01-02 03:04:05 +0000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_details(cx, window);

    let line = log(cx, |panel| panel.details_committer_line()).unwrap_or_default();
    assert!(
        line.starts_with("committed by Carol Committer on 2024-01-02"),
        "the committer line names the person and the date: {line}"
    );
    assert!(
        has_selector(cx, window, "log-details-committer"),
        "the details draw the committer line"
    );
}
