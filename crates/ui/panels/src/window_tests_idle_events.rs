//! Events of the idle window: the repository metadata and the keymap file of the
//! settings folder reach the window without a clock and without a process of a tool.

use std::fs;

use keymap::editor as editor_keys;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{draw, open_window, scratch, wait_for_signal};
use crate::window_tests_idle::{git_state, repository, wait_for_repository, wait_for_status};

#[ui_kit::gpui::test]
fn a_head_written_by_another_program_updates_the_branch(cx: &mut TestAppContext) {
    let (root, git) = repository("idle-head");
    let branch = String::from_utf8(
        git.run(&root, ["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap(),
    )
    .unwrap()
    .trim()
    .to_owned();
    // A second branch at the same commit, so the file can point at it.
    git.run(&root, ["branch", "other"]).unwrap();
    let window = open_window(cx, root.clone());
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    wait_for_status(cx, &state, "the branch", |status| {
        status.branch.as_deref() == Some(branch.as_str())
    });

    // Only `.git/HEAD` changes: a commit or a checkout of another program.
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
    wait_for_status(cx, &state, "the other branch", |status| {
        status.branch.as_deref() == Some("other")
    });
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_keymap_file_written_on_disk_reaches_the_window_without_a_poll(cx: &mut TestAppContext) {
    let root = scratch("idle-keymap");
    let window = open_window(cx, root);
    let (path, before) = window
        .read_with(cx, |workspace, _| {
            (
                workspace.keymap_path().to_path_buf(),
                workspace.keymap_generation(),
            )
        })
        .unwrap();

    fs::write(
        &path,
        r#"{"version": 1, "actions": {"editor.duplicate": {"add": ["ctrl-alt-shift-d"]}}}"#,
    )
    .unwrap();
    wait_for_signal(cx, "the keymap file of the settings folder", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.keymap_generation() > before)
            .unwrap()
    });
    let shortcut = window
        .read_with(cx, |workspace, _| {
            workspace.shortcut_of(editor_keys::DUPLICATE)
        })
        .unwrap();
    assert_eq!(shortcut.as_deref(), Some("ctrl-alt-shift-d"));
    draw(cx, window);
}
