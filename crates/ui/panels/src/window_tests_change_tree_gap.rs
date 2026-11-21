//! T-0374: the name of a changed file starts the gap of the project tree after its icon, in the
//! Commit and the Push windows; the empty message of Commit shows its hint, which goes once the
//! message has text; and the upstream of the branch stands on the row of the Branch field in Push.
//! The windows are drawn without a window of the user's own.
//!
//! `cargo test -p panels window_tests_change_tree_gap`

use std::fs;

use ui_kit::TREE_ICON_GAP;
use ui_kit::gpui;
use ui_kit::gpui::{Bounds, Pixels, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{bounds_of, draw, open_window, press};
use crate::window_tests_commit_tree::{commit_panel, open_commit, row_of, selector};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// The text of the hint of the field `name` in the last frame, when the frame draws it.
fn hint_of(name: &str) -> Option<String> {
    let key = format!("{name}-placeholder");
    ui_kit::test_bounds()
        .into_iter()
        .find(|(shown, _)| *shown == key)
        .and_then(|(_, shown)| shown.label.map(|label| label.to_string()))
}

/// The name starts one gap of the tree after the right edge of its icon, as the project tree
/// draws it.
fn assert_gap(icon: Bounds<Pixels>, name: Bounds<Pixels>, window: &str) {
    let gap = f32::from(name.left() - icon.right());
    assert!(
        (gap - TREE_ICON_GAP).abs() < 0.01,
        "in {window} the name starts {gap} px after its icon, and the project tree gives {TREE_ICON_GAP} px"
    );
}

#[ui_kit::gpui::test]
fn the_name_of_a_changed_file_starts_one_gap_after_its_icon_in_commit(cx: &mut TestAppContext) {
    let (_root, window, state) = open_commit(cx, "change-gap-commit");
    let row = row_of(cx, &state, "Owner.kls");
    let icon = bounds_of(cx, window, selector(format!("{row}-icon")));
    let name = bounds_of(cx, window, selector(format!("{row}-name")));
    assert_gap(icon, name, "Commit");
}

#[ui_kit::gpui::test]
fn the_name_of_a_changed_file_starts_one_gap_after_its_icon_in_push(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("change-gap-push");
    let window: WindowHandle<Workspace> = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("Owner.kls"), "class Owner {}\n").unwrap();
    git.run(&root, ["add", "-A"]).unwrap();
    git.run(&root, ["commit", "-m", "owner class"]).unwrap();
    state.update(cx, |state, cx| state.refresh(cx));

    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-dialog")
    });
    wait_for(cx, "the files of the commit", |cx| {
        has_selector(cx, window, "push-file-0")
    });
    // The first row of the tree is `push-tree-row-0`, and its icon and name have selectors of
    // their own.
    let icon = bounds_of(cx, window, "push-tree-row-0-icon");
    let name = bounds_of(cx, window, "push-tree-row-0-label");
    assert_gap(icon, name, "Push");
}

#[ui_kit::gpui::test]
fn the_commit_message_shows_its_hint_while_it_is_empty_and_not_after_typing(
    cx: &mut TestAppContext,
) {
    let (_root, window, _state) = open_commit(cx, "change-gap-hint");
    ui_kit::clear_test_bounds();
    draw(cx, window);
    assert_eq!(
        hint_of("commit-message").as_deref(),
        Some("Commit Message"),
        "the empty message shows its hint"
    );

    commit_panel(cx).update(cx, |panel, cx| panel.set_message("Fix the owner", cx));
    ui_kit::clear_test_bounds();
    draw(cx, window);
    assert_eq!(
        hint_of("commit-message"),
        None,
        "the hint goes once the message has text"
    );
}

#[ui_kit::gpui::test]
fn the_upstream_of_push_stands_on_the_row_of_the_branch(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("change-gap-upstream");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    press(cx, window, "ctrl-shift-k");
    wait_for(cx, "the Push window", |cx| {
        has_selector(cx, window, "push-upstream")
    });

    let branch = bounds_of(cx, window, "push-destination");
    let upstream = bounds_of(cx, window, "push-upstream");
    assert!(
        upstream.left() >= branch.right(),
        "the upstream stands on the right of the Branch field"
    );
    let shift = (f32::from(upstream.center().y) - f32::from(branch.center().y)).abs();
    assert!(
        shift < 1.,
        "the upstream and the Branch field share one row: {shift} px apart"
    );
}
