//! The branch popup of the window without a window: the search, the remote branches, the
//! submenu of a branch and the operations behind it. Every remote is a bare folder in a
//! scratch directory, so the machine and the network are left alone.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::{point, px};
use workspace::Workspace;

use crate::branches::BranchPopup;
use crate::compare::CompareView;
use crate::window_tests::{click, draw, open_window, press, type_text};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

fn popup(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Entity<BranchPopup> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the branch popup is open");
    view.downcast::<BranchPopup>()
        .map_err(|_| ())
        .expect("the open popup is the branch popup")
}

fn compare_view(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Entity<CompareView> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the compare window is open");
    view.downcast::<CompareView>()
        .map_err(|_| ())
        .expect("the open popup is the compare window")
}

fn show_branches(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let git = git_state(cx);
    git.update(cx, |git, cx| {
        git.show_branches(point(px(200.), px(200.)), cx)
    });
    wait_for(cx, "the branch popup", |cx| {
        has_selector(cx, window, "branches-popup")
    });
}

fn current_branch(cx: &mut TestAppContext) -> Option<String> {
    let git = git_state(cx);
    let repo = git.read_with(cx, |git, _| git.repo())?;
    repo.branches()
        .ok()?
        .into_iter()
        .find(|branch| branch.current)
        .map(|branch| branch.name)
}

/// The repository of `repository_with_origin` with a branch `side` that changes
/// `main.txt`, and the current branch checked out again.
fn repository_with_side(name: &str) -> (PathBuf, vcs::GitRunner, String) {
    let (root, _bare, git) = repository_with_origin(name);
    let base = git
        .run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    git.run(&root, ["switch", "-c", "side"]).unwrap();
    fs::write(root.join("main.txt"), "side\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "side change"]).unwrap();
    git.run(&root, ["switch", &base]).unwrap();
    (root, git, base)
}

#[ui_kit::gpui::test]
fn the_search_keeps_the_local_and_remote_branches_that_contain_the_text(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-search");
    git.run(&root, ["branch", "feature"]).unwrap();
    git.run(&root, ["push", "origin", "feature"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    // Ctrl+Shift+` opens the popup, as the keymap of IDEA says.
    press(cx, window, "ctrl-shift-`");
    wait_for(cx, "the branch popup", |cx| {
        has_selector(cx, window, "branches-popup")
    });
    let view = popup(cx, window);
    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.contains(&"feature".to_owned()), "{names:?}");
    assert!(names.contains(&"origin/feature".to_owned()), "{names:?}");
    let current = current_branch(cx).expect("the current branch");
    assert!(names.contains(&current), "{names:?}");

    // The typing filters the list at once.
    type_text(cx, window, "feat");
    let names = view.read_with(cx, |view, _| view.visible());
    assert_eq!(names, ["feature", "origin/feature"]);

    type_text(cx, window, "zzz");
    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.is_empty(), "{names:?}");

    press(
        cx,
        window,
        "backspace backspace backspace backspace backspace backspace backspace",
    );
    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.contains(&"feature".to_owned()), "{names:?}");
}

#[ui_kit::gpui::test]
fn checkout_of_a_remote_branch_creates_a_local_one_that_tracks_it(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-remote-checkout");
    git.run(&root, ["branch", "feature"]).unwrap();
    git.run(&root, ["push", "origin", "feature"]).unwrap();
    git.run(&root, ["branch", "-D", "feature"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "origin/feature");
    let view = popup(cx, window);
    let names = view.read_with(cx, |view, _| view.visible());
    assert_eq!(names, ["origin/feature"]);

    press(cx, window, "enter");
    wait_for(cx, "the local branch of the remote one", |cx| {
        current_branch(cx).as_deref() == Some("feature")
    });
    let repo = state
        .read_with(cx, |state, _| state.repo())
        .expect("the repository");
    let branch = repo
        .branches()
        .unwrap()
        .into_iter()
        .find(|branch| branch.name == "feature")
        .expect("the local branch");
    assert!(!branch.remote);
    assert_eq!(branch.upstream.as_deref(), Some("origin/feature"));
}

#[ui_kit::gpui::test]
fn the_submenu_merges_a_branch_into_the_current_one(cx: &mut TestAppContext) {
    let (root, git, base) = repository_with_side("branches-merge");
    fs::write(root.join("base.txt"), "base\n").unwrap();
    git.run(&root, ["add", "base.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "base change"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "side");
    let view = popup(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.visible()), ["side"]);

    // Right opens the actions of the branch; Merge is below the first six of them.
    press(cx, window, "right");
    let labels = view.read_with(cx, |view, _| view.submenu_labels());
    assert!(
        labels.iter().any(|label| label.starts_with("Merge 'side'")),
        "{labels:?}"
    );
    press(cx, window, "down down down down down down");
    press(cx, window, "enter");
    wait_for(cx, "the merge commit", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none()
                && state
                    .repo()
                    .is_some_and(|repo| repo.log(1, 0).unwrap()[0].parents.len() == 2)
        })
    });
    assert_eq!(
        fs::read_to_string(root.join("main.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "side\n"
    );
    assert_eq!(current_branch(cx).as_deref(), Some(base.as_str()));
}

#[ui_kit::gpui::test]
fn a_rebase_conflict_opens_the_conflicts_window_and_abort_keeps_the_branch(
    cx: &mut TestAppContext,
) {
    let (root, git, base) = repository_with_side("branches-rebase-conflict");
    fs::write(root.join("main.txt"), "local\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "local change"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    state.update(cx, |state, cx| state.rebase_onto("side".to_owned(), cx));
    wait_for(cx, "the conflicts window", |cx| {
        has_selector(cx, window, "conflict-row-0")
            && state.read_with(cx, |state, _| state.operation() == vcs::Operation::Rebase)
    });
    click(cx, window, "conflict-abort");
    wait_for(cx, "the aborted rebase", |cx| {
        state.read_with(cx, |state, _| state.operation() == vcs::Operation::None)
    });
    assert_eq!(
        fs::read_to_string(root.join("main.txt"))
            .unwrap()
            .replace("\r\n", "\n"),
        "local\n"
    );
    let log = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.log(1, 0).ok())
        .expect("the log");
    assert_eq!(log[0].subject, "local change");
    assert_eq!(log[0].parents.len(), 1);
    assert_eq!(current_branch(cx).as_deref(), Some(base.as_str()));
}

#[ui_kit::gpui::test]
fn delete_of_an_unmerged_branch_offers_force_and_restore_puts_it_back(cx: &mut TestAppContext) {
    let (root, git, _base) = repository_with_side("branches-delete");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    state.update(cx, |state, cx| {
        state.delete_branch("side".to_owned(), false, cx)
    });
    wait_for(cx, "the force delete question", |cx| {
        has_selector(cx, window, "prompt")
    });
    assert!(
        state
            .read_with(cx, |state, _| state.repo())
            .is_some_and(|repo| repo
                .branches()
                .unwrap()
                .iter()
                .any(|branch| branch.name == "side")),
        "the branch waits for the answer"
    );
    press(cx, window, "enter");
    wait_for(cx, "the deleted branch", |cx| {
        state.read_with(cx, |state, _| state.busy().is_none())
            && state
                .read_with(cx, |state, _| state.repo())
                .is_some_and(|repo| {
                    repo.branches()
                        .unwrap()
                        .iter()
                        .all(|branch| branch.name != "side")
                })
    });
    wait_for(cx, "the restore button", |cx| {
        has_selector(cx, window, "notification-action")
    });
    click(cx, window, "notification-action");
    wait_for(cx, "the restored branch", |cx| {
        state
            .read_with(cx, |state, _| state.repo())
            .is_some_and(|repo| {
                repo.branches()
                    .unwrap()
                    .iter()
                    .any(|branch| branch.name == "side")
            })
    });
    let branches = git.run(&root, ["branch", "--list", "side"]).unwrap();
    assert!(!branches.is_empty(), "the branch is back in git");
}

#[ui_kit::gpui::test]
fn compare_lists_the_commits_of_both_branches(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-compare");
    let base = git
        .run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    git.run(&root, ["switch", "-c", "side"]).unwrap();
    fs::write(root.join("side.txt"), "side\n").unwrap();
    git.run(&root, ["add", "side.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "side change"]).unwrap();
    git.run(&root, ["switch", &base]).unwrap();
    fs::write(root.join("main.txt"), "main\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "main change"]).unwrap();

    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    state.update(cx, |state, cx| state.compare_branch("side".to_owned(), cx));
    wait_for(cx, "the compare window", |cx| {
        has_selector(cx, window, "compare")
    });
    let view = compare_view(cx, window);
    let (left_name, right_name) = view.read_with(cx, |view, _| {
        (view.left().to_owned(), view.right().to_owned())
    });
    assert_eq!(left_name, "side");
    assert_eq!(right_name, base);
    let (left, right) = view.read_with(cx, |view, _| {
        (
            view.only_left()
                .iter()
                .map(|commit| commit.subject.clone())
                .collect::<Vec<_>>(),
            view.only_right()
                .iter()
                .map(|commit| commit.subject.clone())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(left, ["side change"]);
    assert_eq!(right, ["main change"]);
    let files = view.read_with(cx, |view, _| view.files().to_vec());
    assert!(files.contains(&"main.txt".to_owned()), "{files:?}");
    assert!(files.contains(&"side.txt".to_owned()), "{files:?}");
    draw(cx, window);
}
