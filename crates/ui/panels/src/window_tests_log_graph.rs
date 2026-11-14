//! The commit graph of the Git Log window without a window: the dot of every row, the
//! labels of the branches and tags at their commits and the accent of HEAD. The
//! repository lives in a scratch folder, so the machine is left alone.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{LOG_TOOL, Workspace};

use crate::git_log::ActiveGitLog;
use crate::window_tests::{draw, open_window, scratch};
use crate::window_tests_git::{git, git_state, has_selector, wait_for, wait_for_repository};

/// A selector the test builds from an index: GPUI takes selectors as `&'static str`.
fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The log of the panel, oldest row last.
fn commit_ids(cx: &TestAppContext) -> Vec<String> {
    cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .commits()
            .iter()
            .map(|commit| commit.id.clone())
            .collect()
    })
}

/// The labels of one row with `true` for the branch of HEAD.
fn labels_of(cx: &TestAppContext, index: usize) -> Vec<(String, bool)> {
    cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).labels_of(index))
}

/// The full id of a revision of the test repository.
fn revision(git: &vcs::GitRunner, root: &std::path::Path, name: &str) -> String {
    let output = git.run(root, ["rev-parse", name]).unwrap();
    String::from_utf8_lossy(&output).trim().to_owned()
}

fn show_log(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(LOG_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(5000));
    cx.run_until_parked();
    draw(cx, window);
}

/// `main` with two commits, a tag on the first of them, and a branch `feature` that is
/// merged back into `main`.
fn repository_with_branch_and_merge(name: &str) -> (PathBuf, vcs::GitRunner) {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    // The first commit may not exist yet, so the branch it will land on is named first.
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    fs::write(root.join("main.txt"), "base\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base"]).unwrap();
    git.run(&root, ["tag", "v0.1.0"]).unwrap();
    git.run(&root, ["switch", "-c", "feature"]).unwrap();
    fs::write(root.join("feature.txt"), "feature\n").unwrap();
    git.run(&root, ["add", "feature.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "feature change"]).unwrap();
    git.run(&root, ["switch", "main"]).unwrap();
    fs::write(root.join("main.txt"), "base\nmore\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "main change"]).unwrap();
    git.run(
        &root,
        ["merge", "--no-ff", "feature", "-m", "merge feature"],
    )
    .unwrap();
    (root, git)
}

#[ui_kit::gpui::test]
fn the_log_draws_dots_and_puts_every_label_on_its_commit(cx: &mut TestAppContext) {
    let (root, git) = repository_with_branch_and_merge("log-graph");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    show_log(cx, window);
    wait_for(cx, "the commits of the log", |cx| {
        cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).commits().len() == 4)
    });

    let commits = commit_ids(cx);
    let head = revision(&git, &root, "HEAD");
    let feature = revision(&git, &root, "feature");
    let tagged = revision(&git, &root, "v0.1.0^{commit}");
    assert_eq!(commits[0], head, "the graph starts at the merge of HEAD");
    let feature_row = commits
        .iter()
        .position(|id| *id == feature)
        .expect("the log shows the commit of feature");
    let tagged_row = commits
        .iter()
        .position(|id| *id == tagged)
        .expect("the log shows the tagged commit");

    // The dots of the graph, one for every row.
    for index in 0..commits.len() {
        assert!(
            has_selector(cx, window, selector(format!("log-graph-dot-{index}"))),
            "row {index} has a dot"
        );
    }

    // Every branch and tag hangs on its own commit, and only HEAD is current.
    let head_labels = labels_of(cx, 0);
    assert!(
        head_labels
            .iter()
            .any(|(name, current)| name == "main" && *current),
        "main is HEAD at the merge: {head_labels:?}"
    );
    let feature_labels = labels_of(cx, feature_row);
    assert!(
        feature_labels
            .iter()
            .any(|(name, current)| name == "feature" && !current),
        "feature is on its own commit: {feature_labels:?}"
    );
    let tagged_labels = labels_of(cx, tagged_row);
    assert!(
        tagged_labels.iter().any(|(name, _)| name == "v0.1.0"),
        "the tag is on the base commit: {tagged_labels:?}"
    );

    // The pills are elements of the row, and the one of HEAD takes its own accent.
    assert!(
        has_selector(cx, window, "log-ref-0-0"),
        "the pill of main is on the first row"
    );
    assert!(
        has_selector(cx, window, selector(format!("log-ref-{feature_row}-0"))),
        "the pill of feature is on its row"
    );
    assert!(
        has_selector(cx, window, "log-ref-head-0"),
        "the pill of HEAD stands out"
    );
    assert!(
        !has_selector(cx, window, selector(format!("log-ref-head-{feature_row}"))),
        "the side branch is not HEAD"
    );
}
