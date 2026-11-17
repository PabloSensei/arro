//! The branch popup as a tree without a window: a node for every remote, a node for
//! every folder of the branch names and the switch that lists the branches flat. Every
//! repository and remote is a folder of a scratch directory, so the machine and the
//! network are left alone.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::{point, px};
use workspace::{Settings, Workspace};

use crate::branches::BranchPopup;
use crate::compare::CompareView;
use crate::window_tests::{click, draw, open_window, press, scratch, type_text};
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

/// The repository of `repository_with_origin` with a local branch `side` that changes
/// `main.txt` but is not merged into the current branch.
fn repository_with_side(name: &str) -> (PathBuf, vcs::GitRunner, String, String) {
    let (root, _bare, git) = repository_with_origin(name);
    let base = git
        .run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    let base = String::from_utf8_lossy(&base).trim().to_owned();
    git.run(&root, ["switch", "-c", "side"]).unwrap();
    fs::write(root.join("main.txt"), "side\n").unwrap();
    git.run(&root, ["add", "main.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "side change"]).unwrap();
    let head = git.run(&root, ["rev-parse", "side"]).unwrap();
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    git.run(&root, ["switch", &base]).unwrap();
    (root, git, base, head)
}

/// Adds a second bare folder as a remote named `upstream` and pushes `branch` there.
fn add_upstream(git: &vcs::GitRunner, root: &std::path::Path, branch: &str, name: &str) -> PathBuf {
    let bare = scratch(&format!("{name}-upstream"));
    git.run(&bare, ["init", "--bare"]).unwrap();
    git.run(
        root,
        [
            "remote",
            "add",
            "upstream",
            bare.to_str().expect("a UTF-8 scratch path"),
        ],
    )
    .unwrap();
    git.run(root, ["push", "upstream", branch]).unwrap();
    bare
}

fn create_folder_branches(git: &vcs::GitRunner, root: &std::path::Path) {
    git.run(root, ["branch", "feature/login"]).unwrap();
    git.run(root, ["branch", "feature/signup"]).unwrap();
    git.run(root, ["push", "origin", "feature/login"]).unwrap();
    git.run(root, ["push", "origin", "feature/signup"]).unwrap();
}

#[ui_kit::gpui::test]
fn every_remote_is_a_node_and_its_branches_lack_the_remote_prefix(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-remotes");
    git.run(&root, ["branch", "feature"]).unwrap();
    git.run(&root, ["push", "origin", "feature"]).unwrap();
    let _upstream = add_upstream(&git, &root, "feature", "branches-tree-remotes");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    let view = popup(cx, window);
    let nodes = view.read_with(cx, |view, _| view.nodes());
    assert!(nodes.contains(&"origin".to_owned()), "{nodes:?}");
    assert!(nodes.contains(&"upstream".to_owned()), "{nodes:?}");

    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.contains(&"origin/feature".to_owned()), "{names:?}");
    assert!(names.contains(&"upstream/feature".to_owned()), "{names:?}");

    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(
        tree.iter().all(|(_, label)| !label.contains('/')),
        "a branch of a remote shows without the remote prefix: {tree:?}"
    );
}

#[ui_kit::gpui::test]
fn folders_of_the_branch_names_group_the_local_and_the_remote_branches(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-folders");
    create_folder_branches(&git, &root);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    let view = popup(cx, window);
    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(tree.contains(&(0, "feature".to_owned())), "{tree:?}");
    assert!(tree.contains(&(1, "login".to_owned())), "{tree:?}");
    assert!(tree.contains(&(1, "signup".to_owned())), "{tree:?}");
    assert!(tree.contains(&(0, "origin".to_owned())), "{tree:?}");
    assert!(tree.contains(&(1, "feature".to_owned())), "{tree:?}");
    assert!(tree.contains(&(2, "login".to_owned())), "{tree:?}");
    assert!(tree.contains(&(2, "signup".to_owned())), "{tree:?}");
    let nodes = view.read_with(cx, |view, _| view.nodes());
    assert_eq!(
        nodes.iter().filter(|node| *node == "feature").count(),
        2,
        "one folder for Local and one for Remote: {nodes:?}"
    );
}

#[ui_kit::gpui::test]
fn the_group_switch_lists_the_branches_flat_and_is_saved(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-flat");
    create_folder_branches(&git, &root);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    let view = popup(cx, window);
    assert!(
        !view.read_with(cx, |view, _| view.nodes()).is_empty(),
        "the tree is on by default"
    );
    click(cx, window, "branch-group-directories");
    cx.run_until_parked();
    draw(cx, window);

    assert!(view.read_with(cx, |view, _| view.nodes()).is_empty());
    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(tree.contains(&(0, "feature/login".to_owned())), "{tree:?}");
    assert!(
        tree.contains(&(0, "origin/feature/login".to_owned())),
        "{tree:?}"
    );
    assert!(
        !cx.read(|cx| cx.global::<Settings>().git_group_by_directory),
        "the switch is kept in the Git settings"
    );
}

#[ui_kit::gpui::test]
fn the_search_opens_the_nodes_with_the_matches(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-search");
    create_folder_branches(&git, &root);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "login");
    let view = popup(cx, window);
    let tree = view.read_with(cx, |view, _| view.tree());
    assert!(tree.contains(&(1, "login".to_owned())), "{tree:?}");

    // The selection starts on the first branch; one step up is the folder of Local.
    press(cx, window, "up");
    press(cx, window, "left");
    let folded = view.read_with(cx, |view, _| view.tree());
    assert!(
        !folded.contains(&(1, "login".to_owned())),
        "the local folder is folded: {folded:?}"
    );
    assert!(
        folded.contains(&(2, "login".to_owned())),
        "the remote folder stays open: {folded:?}"
    );

    // A change of the search opens the nodes that hold a match again.
    press(cx, window, "backspace");
    let opened = view.read_with(cx, |view, _| view.tree());
    assert!(
        opened.contains(&(1, "login".to_owned())),
        "the search opens the node: {opened:?}"
    );
}

#[ui_kit::gpui::test]
fn the_left_and_right_keys_fold_and_open_a_node(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-arrows");
    create_folder_branches(&git, &root);
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "login");
    let view = popup(cx, window);
    press(cx, window, "up");
    press(cx, window, "left");
    let folded = view.read_with(cx, |view, _| view.tree());
    assert!(!folded.contains(&(1, "login".to_owned())), "{folded:?}");

    press(cx, window, "right");
    let opened = view.read_with(cx, |view, _| view.tree());
    assert!(opened.contains(&(1, "login".to_owned())), "{opened:?}");
}

#[ui_kit::gpui::test]
fn delete_of_a_remote_branch_asks_and_removes_it_on_the_remote(cx: &mut TestAppContext) {
    let (root, bare, git) = repository_with_origin("branches-tree-remote-delete");
    git.run(&root, ["branch", "feature"]).unwrap();
    git.run(&root, ["push", "origin", "feature"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "origin/feature");
    let view = popup(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.visible()),
        ["origin/feature"]
    );

    // The actions of a remote branch: Checkout, New Branch, Compare, Show Diff,
    // Rebase, Merge, Delete.
    press(cx, window, "right");
    press(cx, window, "down down down down down down");
    press(cx, window, "enter");
    wait_for(cx, "the delete question", |cx| {
        has_selector(cx, window, "prompt")
    });
    press(cx, window, "enter");
    wait_for(cx, "the branch that left the remote", |_| {
        git.run(&bare, ["branch", "--list", "feature"])
            .is_ok_and(|output| String::from_utf8_lossy(&output).trim().is_empty())
    });
}

#[ui_kit::gpui::test]
fn restore_after_a_force_delete_puts_the_branch_back_on_its_commit(cx: &mut TestAppContext) {
    let (root, _git, _base, head) = repository_with_side("branches-tree-restore");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, "side");
    let view = popup(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.visible()), ["side"]);

    // The actions of a local branch: Checkout, New Branch, Checkout and Rebase,
    // Compare, Show Diff, Rebase, Merge, Push, Rename, Delete.
    press(cx, window, "right");
    press(cx, window, "down down down down down down down down down");
    press(cx, window, "enter");
    wait_for(cx, "the force delete question", |cx| {
        has_selector(cx, window, "prompt")
    });
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
    let restored = state
        .read_with(cx, |state, _| state.repo())
        .and_then(|repo| repo.branch_head("side").ok())
        .expect("the restored branch");
    assert_eq!(
        restored, head,
        "Restore brings the branch back on its commit"
    );
}

#[ui_kit::gpui::test]
fn show_diff_with_working_tree_opens_the_compare_window(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-tree-diff");
    let current = git
        .run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    let current = String::from_utf8_lossy(&current).trim().to_owned();
    fs::write(root.join("main.txt"), "changed in the working tree\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    show_branches(cx, window);
    type_text(cx, window, &current);
    let view = popup(cx, window);
    let names = view.read_with(cx, |view, _| view.visible());
    assert!(names.contains(&current), "{names:?}");

    // The actions of the current branch: New Branch, Show Diff, Push, Rename.
    press(cx, window, "right");
    press(cx, window, "down");
    press(cx, window, "enter");
    wait_for(cx, "the compare window", |cx| {
        has_selector(cx, window, "compare")
    });
    let compare = compare_view(cx, window);
    let right = compare.read_with(cx, |view, _| view.right().to_owned());
    assert_eq!(right, "Working Tree");
    let files = compare.read_with(cx, |view, _| view.files().to_vec());
    assert!(files.contains(&"main.txt".to_owned()), "{files:?}");
    draw(cx, window);
}
