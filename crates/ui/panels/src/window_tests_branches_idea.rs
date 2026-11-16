//! The branch popup of the window without a window, as the new UI of IDEA shows it: the
//! actions above the list, the folders of the branch names and the search. The repository
//! has `main`, `feature/a`, `feature/b/c` and `fix-1`, and its remote is a bare folder of a
//! scratch directory, so the machine and the network are left alone.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::{point, px};
use workspace::Workspace;

use crate::branches::BranchPopup;
use crate::window_tests::{bounds_of, draw, open_window, press, type_text};
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

fn show_branches(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let git = git_state(cx);
    git.update(cx, |git, cx| {
        git.show_branches(point(px(200.), px(200.)), cx)
    });
    wait_for(cx, "the branch popup", |cx| {
        has_selector(cx, window, "branches-popup")
    });
}

/// The branch the repository has checked out.
fn current_branch(cx: &mut TestAppContext) -> Option<String> {
    let git = git_state(cx);
    let repo = git.read_with(cx, |git, _| git.repo())?;
    repo.branches()
        .ok()?
        .into_iter()
        .find(|branch| branch.current)
        .map(|branch| branch.name)
}

/// The repository of `repository_with_origin` with the branches the popup of IDEA is
/// looked at with, and its current branch renamed to `main`.
fn repository(name: &str) -> (PathBuf, PathBuf, vcs::GitRunner) {
    let (root, bare, git) = repository_with_origin(name);
    git.run(&root, ["branch", "-M", "main"]).unwrap();
    git.run(&root, ["branch", "feature/a"]).unwrap();
    git.run(&root, ["branch", "feature/b/c"]).unwrap();
    git.run(&root, ["branch", "fix-1"]).unwrap();
    git.run(&root, ["push", "-u", "origin", "main"]).unwrap();
    let _ = git.run(&root, ["push", "origin", "--delete", "master"]);
    let _ = git.run(&root, ["fetch", "--prune", "origin"]);
    (root, bare, git)
}

#[ui_kit::gpui::test]
fn the_actions_and_the_search_stand_above_the_list(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-actions");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    let view = popup(cx, window);
    let actions = view.read_with(cx, |view, _| view.actions());
    for wanted in [
        "Update Project...",
        "Commit...",
        "Push...",
        "New Branch...",
        "Checkout Tag or Revision...",
        "Stash Changes...",
        "Unstash Changes...",
    ] {
        assert!(
            actions.iter().any(|label| label == wanted),
            "{wanted}: {actions:?}"
        );
    }
    for selector in [
        "branch-update",
        "branch-commit",
        "branch-push",
        "menu-0",
        "branch-checkout-tag",
        "branch-stash",
        "branch-unstash",
    ] {
        assert!(has_selector(cx, window, selector), "{selector} is missing");
    }
    let update = bounds_of(cx, window, "branch-update");
    let list = bounds_of(cx, window, "branches-list");
    assert!(
        update.origin.y < list.origin.y,
        "the actions stand above the list"
    );
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_feature_folder_folds_and_opens(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-folders");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "feature");
    let view = popup(cx, window);
    let tree = view.read_with(cx, |view, _| view.tree());
    for wanted in [
        (0, "feature".to_owned()),
        (1, "a".to_owned()),
        (1, "b".to_owned()),
        (2, "c".to_owned()),
    ] {
        assert!(tree.contains(&wanted), "{wanted:?} is missing: {tree:?}");
    }

    // The selection starts on the first branch of the tree; two steps up are the folder
    // `b` and then `feature`.
    press(cx, window, "up up");
    press(cx, window, "left");
    let folded = view.read_with(cx, |view, _| view.tree());
    assert!(
        !folded.contains(&(1, "a".to_owned())) && !folded.contains(&(1, "b".to_owned())),
        "the folder is folded: {folded:?}"
    );
    assert!(folded.contains(&(0, "feature".to_owned())), "{folded:?}");

    press(cx, window, "right");
    let opened = view.read_with(cx, |view, _| view.tree());
    assert!(
        opened.contains(&(1, "a".to_owned())),
        "the folder opens again: {opened:?}"
    );
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_search_shows_the_deep_branch(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-search");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "b/c");
    let view = popup(cx, window);
    let names = view.read_with(cx, |view, _| view.visible());
    assert_eq!(names, ["feature/b/c"]);
    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(
        tree.contains(&(0, "feature".to_owned()))
            && tree.contains(&(1, "b".to_owned()))
            && tree.contains(&(2, "c".to_owned())),
        "the search opens the folders of the branch: {tree:?}"
    );
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn enter_checks_out_the_branch_the_search_keeps(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-checkout");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "fix-1");
    let view = popup(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.visible()), ["fix-1"]);

    press(cx, window, "enter");
    wait_for(cx, "the checked out branch", |cx| {
        current_branch(cx).as_deref() == Some("fix-1")
    });
}

#[ui_kit::gpui::test]
fn the_right_key_opens_the_actions_of_a_branch(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-submenu");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "feature/a");
    let view = popup(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.visible()), ["feature/a"]);

    press(cx, window, "right");
    let labels = view.read_with(cx, |view, _| view.submenu_labels());
    assert!(labels.iter().any(|label| label == "Checkout"), "{labels:?}");
    assert!(
        labels
            .iter()
            .any(|label| label == "New Branch from 'feature/a'..."),
        "{labels:?}"
    );
    assert!(
        labels.iter().any(|label| label.starts_with("Merge ")),
        "{labels:?}"
    );
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_tags_group_lists_the_tags_of_the_repository(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository("branches-idea-tags");
    git.run(&root, ["tag", "release/1.0"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    let view = popup(cx, window);
    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(
        tree.contains(&(0, "release".to_owned())) && tree.contains(&(1, "1.0".to_owned())),
        "the tags fold by their slashes: {tree:?}"
    );
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_star_saves_the_favourites_of_the_project(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository("branches-idea-favourites");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "main");
    let view = popup(cx, window);
    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.contains(&"main".to_owned()), "{names:?}");

    // `main` is starred by default; the star takes it away and writes the choice into
    // the settings of the project.
    crate::window_tests::click(cx, window, "branch-favourite-1");
    let file = root.join(".arro").join("git.toml");
    wait_for(cx, "the settings of the project", |_| {
        fs::read_to_string(&file)
            .is_ok_and(|text| text.contains("favourite_branches") && !text.contains("\"main\""))
    });
    draw(cx, window);
}
