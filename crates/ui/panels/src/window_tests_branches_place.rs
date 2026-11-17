//! Where the branch popup opens and what a hover does to it, in the window without a window:
//! a click on the branch name of the title bar, the step `branches` of a script and
//! Branches... of the VCS menu all open it under that name, and a hover over a row keeps
//! the same popup with the same rows. Every remote is a bare folder in a scratch directory,
//! so the machine and the network are left alone.

use std::time::Duration;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::SPACE_XS;
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::branches::BranchPopup;
use crate::window_tests::{bounds_of, click, draw, open_window, press};
use crate::window_tests_git::{
    git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};

/// The two rows of Local the repository of these tests lists, as their ids are named.
const LOCAL_ROWS: [&str; 2] = ["menu-2", "menu-3"];
/// The rows of a menu by their index.
const MENU_ROWS: [&str; 10] = [
    "menu-0", "menu-1", "menu-2", "menu-3", "menu-4", "menu-5", "menu-6", "menu-7", "menu-8",
    "menu-9",
];

fn popup(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Entity<BranchPopup> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the branch popup is open");
    view.downcast::<BranchPopup>()
        .map_err(|_| ())
        .expect("the open popup is the branch popup")
}

/// Which of the local rows are on the screen now.
fn rows_on_screen(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<bool> {
    LOCAL_ROWS
        .into_iter()
        .map(|row| has_selector(cx, window, row))
        .collect()
}

/// The popup stands under the branch name of the title bar: at its left edge and a gap below.
fn assert_under_title(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let title = bounds_of(cx, window, "t-branch");
    let popup = bounds_of(cx, window, "branches-popup");
    assert_eq!(
        popup.left(),
        title.left(),
        "the popup starts at the branch name: {popup:?} under {title:?}"
    );
    assert_eq!(
        popup.top(),
        title.bottom() + px(SPACE_XS),
        "the popup stands under the branch name: {popup:?} under {title:?}"
    );
}

#[ui_kit::gpui::test]
fn a_hover_over_a_row_keeps_the_popup_and_its_rows(cx: &mut TestAppContext) {
    let (root, _bare, git) = repository_with_origin("branches-hover-keeps");
    git.run(&root, ["branch", "feature"]).unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    draw(cx, window);

    click(cx, window, "t-branch");
    wait_for(cx, "the branch popup", |cx| {
        has_selector(cx, window, "branches-popup")
    });
    draw(cx, window);
    assert_under_title(cx, window);
    let view = popup(cx, window);
    let rows = rows_on_screen(cx, window);
    assert_eq!(rows, [true, true], "both local branches are listed");
    let frame = bounds_of(cx, window, "branches-popup");

    // The pointer rests on a row: the row is highlighted and, after its delay, its actions
    // open. The popup keeps its entity, its place and its rows through both.
    let row = bounds_of(cx, window, "menu-3").center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(row, MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(rows_on_screen(cx, window), rows);
    assert_eq!(popup(cx, window).entity_id(), view.entity_id());
    assert_eq!(bounds_of(cx, window, "branches-popup"), frame);

    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    draw(cx, window);
    let labels = view.read_with(cx, |view, _| view.submenu_labels());
    assert!(!labels.is_empty(), "the rest on the row opens its actions");
    assert_eq!(rows_on_screen(cx, window), rows);
    assert_eq!(popup(cx, window).entity_id(), view.entity_id());
    assert_eq!(bounds_of(cx, window, "branches-popup"), frame);
}

#[ui_kit::gpui::test]
fn the_branches_step_of_a_script_opens_the_popup_under_the_title(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("branches-step-under-title");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    draw(cx, window);

    window
        .update(cx, |_, window, cx| {
            crate::script::start("branches", window, cx)
        })
        .unwrap();
    wait_for(cx, "the branch popup of the script", |cx| {
        has_selector(cx, window, "branches-popup")
    });
    draw(cx, window);
    assert_under_title(cx, window);
}

#[ui_kit::gpui::test]
fn branches_from_the_vcs_menu_opens_under_the_title(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("branches-vcs-menu-under-title");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    draw(cx, window);

    // Alt+` opens the VCS menu at the caret, but its Branches... opens the popup under the
    // branch name of the title bar, as IDEA does.
    press(cx, window, "alt-`");
    draw(cx, window);
    let labels: Vec<String> = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap();
    let at = labels
        .iter()
        .position(|label| label == "Branches...")
        .expect("the VCS menu lists Branches...");
    click(cx, window, MENU_ROWS[at]);
    wait_for(cx, "the branch popup of the VCS menu", |cx| {
        has_selector(cx, window, "branches-popup")
    });
    draw(cx, window);
    assert_under_title(cx, window);
}
