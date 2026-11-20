//! The files of the Push window as a tree of folders. A chain of folders that holds one
//! folder and no file is one row, and a click on a folder closes it and opens it again.

use std::fs;
use std::path::Path;

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{click, open_window, press};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

fn commit_all(git: &vcs::GitRunner, root: &Path, message: &str) {
    git.run(root, ["add", "-A"]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

#[ui_kit::gpui::test]
fn the_files_of_the_commit_form_a_tree_that_folds_a_chain_of_folders(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("push-tree-fold");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::create_dir_all(root.join("src/main/acme/owner")).unwrap();
    fs::write(
        root.join("src/main/acme/owner/Owner.txt"),
        "class Owner {}\n",
    )
    .unwrap();
    fs::write(root.join("src/main/acme/owner/Pet.txt"), "class Pet {}\n").unwrap();
    fs::write(root.join("README.md"), "readme\n").unwrap();
    commit_all(&git, &root, "owner classes");
    state.update(cx, |state, cx| state.refresh(cx));

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-dialog")
    });
    wait_for(cx, "the files of the commit", |cx| {
        has_selector(cx, window, "push-file-2")
    });
    assert!(
        has_selector(cx, window, "push-folder-src/main/acme/owner"),
        "the chain of folders with one folder inside each is one row"
    );
    assert!(
        !has_selector(cx, window, "push-folder-src"),
        "the folded chain has no row for its first folder"
    );
    assert!(
        !has_selector(cx, window, "push-folder-src/main"),
        "the folded chain has no row for its middle folders"
    );
    assert!(
        !has_selector(cx, window, "push-file-3"),
        "the commit has three files: two classes and README.md"
    );

    click(cx, window, "push-folder-src/main/acme/owner");
    wait_for(cx, "the folder to close", |cx| {
        !has_selector(cx, window, "push-file-1")
    });
    assert!(
        has_selector(cx, window, "push-file-0"),
        "README.md at the top of the tree stays drawn while the folder is closed"
    );
    click(cx, window, "push-folder-src/main/acme/owner");
    wait_for(cx, "the folder to open", |cx| {
        has_selector(cx, window, "push-file-2")
    });
}
