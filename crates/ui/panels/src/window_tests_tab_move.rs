//! Moving a tab by drag and drop between the tab strips of the window, and closing the
//! last tab of a group. A strip that handles a drop or a press may lose its group and
//! the strips are renumbered then: the window must not update the strip that is handling
//! the event while it is updated already.

use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, point, px};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, draw, open_path, open_sample_window};

/// The paths of the tabs group by group.
fn tabs(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<Vec<PathBuf>> {
    window
        .read_with(cx, |workspace, _| workspace.open_tabs())
        .unwrap()
}

/// The group that has the focus.
fn active_group(cx: &TestAppContext, window: WindowHandle<Workspace>) -> usize {
    window
        .read_with(cx, |workspace, _| workspace.active_group())
        .unwrap()
}

fn split(cx: &mut TestAppContext, window: WindowHandle<Workspace>, down: bool) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.split(down, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// A press and a release of a button at a point, as the pointer does.
fn click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: Point<Pixels>,
    button: MouseButton,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, button, Modifiers::default());
    visual.simulate_mouse_up(position, button, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

fn drag_to(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: Point<Pixels>,
    to: Point<Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// The middle of the cross of a tab: the tab keeps one spacing step at its right edge
/// around the square of the cross.
fn cross_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    tab: &'static str,
) -> Point<Pixels> {
    let bounds = bounds_of(cx, window, tab);
    point(bounds.right() - px(12.), bounds.center().y)
}

/// The end of a tab strip after its last tab, where a drop appends a tab.
fn end_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    strip: &'static str,
) -> Point<Pixels> {
    let bounds = bounds_of(cx, window, strip);
    point(bounds.right() - px(6.), bounds.center().y)
}

/// A project with `main.txt` and `notes.txt` open in one group, and nothing else.
fn open_two_files(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, PathBuf, WindowHandle<Workspace>) {
    let (root, window) = open_sample_window(cx, name);
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    (main, notes, window)
}

#[ui_kit::gpui::test]
fn the_last_tab_of_a_group_moves_to_the_bar_of_another_group(cx: &mut TestAppContext) {
    let (main, notes, window) = open_two_files(cx, "tab-move-bar");
    split(cx, window, true);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );

    // The only tab of the lower group goes to the empty end of the upper strip.
    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = end_of(cx, window, "tabs-0");
    drag_to(cx, window, from, to);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone(), notes.clone()]],
        "the group of the moved tab went away and the tab is in the other one"
    );

    // The same tab goes onto the tab of the upper group, which puts it there.
    split(cx, window, true);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );
    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = bounds_of(cx, window, "tab-0-0").center();
    drag_to(cx, window, from, to);
    assert_eq!(
        tabs(cx, window),
        vec![vec![notes, main.clone()]],
        "the tab landed on the tab it was dropped on"
    );
    assert_eq!(active_group(cx, window), 0);
}

#[ui_kit::gpui::test]
fn a_tab_moves_inside_its_group_and_between_three_groups(cx: &mut TestAppContext) {
    let (main, notes, window) = open_two_files(cx, "tab-move-groups");
    let readme = window
        .read_with(cx, |workspace, _| workspace.root().join("README.txt"))
        .unwrap();

    // Three groups, one tab in the last two.
    split(cx, window, true);
    open_path(cx, window, &readme);
    split(cx, window, false);
    assert_eq!(
        tabs(cx, window),
        vec![
            vec![main.clone()],
            vec![notes.clone()],
            vec![readme.clone()]
        ]
    );

    // The last tab of the middle group goes onto the tab of the last one: the middle
    // group goes away and the strips after it take their new numbers.
    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = bounds_of(cx, window, "tab-2-0").center();
    drag_to(cx, window, from, to);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone(), readme.clone()]]
    );

    // Two tabs of one group swap places by a drop of one onto the other.
    let from = bounds_of(cx, window, "tab-1-1").center();
    let to = bounds_of(cx, window, "tab-1-0").center();
    drag_to(cx, window, from, to);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![readme.clone(), notes.clone()]]
    );

    // A drop after the last tab of the group puts the tab back at the end.
    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = end_of(cx, window, "tabs-1");
    drag_to(cx, window, from, to);
    assert_eq!(tabs(cx, window), vec![vec![main], vec![notes, readme]]);
}

#[ui_kit::gpui::test]
fn the_last_tab_of_the_right_group_moves_to_the_left_one(cx: &mut TestAppContext) {
    let (main, notes, window) = open_two_files(cx, "tab-move-right");
    split(cx, window, false);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.split_is_down())
            .unwrap(),
        "the first split went to the right"
    );

    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = end_of(cx, window, "tabs-0");
    drag_to(cx, window, from, to);
    assert_eq!(tabs(cx, window), vec![vec![main, notes]]);
}

#[ui_kit::gpui::test]
fn the_last_tab_of_a_group_closes_by_the_cross_the_middle_button_and_the_menu(
    cx: &mut TestAppContext,
) {
    let (main, notes, window) = open_two_files(cx, "tab-close-group");
    split(cx, window, true);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );

    // The cross of the only tab of the lower group takes the group away.
    let cross = cross_of(cx, window, "tab-1-0");
    click_at(cx, window, cross, MouseButton::Left);
    assert_eq!(tabs(cx, window), vec![vec![main.clone()]]);
    assert_eq!(active_group(cx, window), 0);

    // The middle button of a tab does the same.
    split(cx, window, true);
    open_path(cx, window, &notes);
    let middle = bounds_of(cx, window, "tab-1-0").center();
    click_at(cx, window, middle, MouseButton::Middle);
    assert_eq!(tabs(cx, window), vec![vec![main.clone()]]);

    // Close from the menu of the tab.
    split(cx, window, true);
    open_path(cx, window, &notes);
    let right = bounds_of(cx, window, "tab-1-0").center();
    click_at(cx, window, right, MouseButton::Right);
    click(cx, window, "menu-0");
    assert_eq!(tabs(cx, window), vec![vec![main]]);
}

/// Opens the menu of the only tab of the lower group and runs the row at `index`.
fn menu_of_the_last_tab(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    index: &'static str,
) {
    let tab = bounds_of(cx, window, "tab-1-0").center();
    click_at(cx, window, tab, MouseButton::Right);
    click(cx, window, index);
}

#[ui_kit::gpui::test]
fn the_tab_menu_closes_splits_moves_and_pins_the_last_tab_of_a_group(cx: &mut TestAppContext) {
    let (main, notes, window) = open_two_files(cx, "tab-menu-group");
    split(cx, window, true);

    // Close All Tabs takes the group of the last tab away. The row runs from the menu,
    // which is not the strip that was pressed.
    menu_of_the_last_tab(cx, window, "menu-2");
    assert_eq!(tabs(cx, window), vec![vec![main.clone()]]);

    // Move to Other Group does the same through the move of the tab.
    split(cx, window, true);
    open_path(cx, window, &notes);
    menu_of_the_last_tab(cx, window, "menu-8");
    assert_eq!(tabs(cx, window), vec![vec![main.clone(), notes.clone()]]);

    // Close Other Tabs keeps the tab and its group.
    split(cx, window, true);
    open_path(cx, window, &notes);
    menu_of_the_last_tab(cx, window, "menu-1");
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );

    // Pin Tab and Unpin Tab keep the tab in its group.
    menu_of_the_last_tab(cx, window, "menu-4");
    let pinned = window
        .read_with(cx, |workspace, _| workspace.pinned_tabs())
        .unwrap();
    assert_eq!(pinned, vec![notes.clone()]);
    menu_of_the_last_tab(cx, window, "menu-4");
    let pinned = window
        .read_with(cx, |workspace, _| workspace.pinned_tabs())
        .unwrap();
    assert!(pinned.is_empty());

    // Split Right and Split Down add a group after the one of the last tab.
    menu_of_the_last_tab(cx, window, "menu-6");
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()], Vec::new()]
    );

    // The empty group takes the tab, so the lower one keeps a single tab again.
    let from = bounds_of(cx, window, "tab-1-0").center();
    let to = end_of(cx, window, "tabs-2");
    drag_to(cx, window, from, to);
    assert_eq!(
        tabs(cx, window),
        vec![vec![main.clone()], vec![notes.clone()]]
    );
    menu_of_the_last_tab(cx, window, "menu-7");
    assert_eq!(tabs(cx, window), vec![vec![main], vec![notes], Vec::new()]);
}
