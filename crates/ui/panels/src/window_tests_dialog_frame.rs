//! The frame of the modal dialogs as a real window: the title bar with its close cross, the
//! drag by the title bar, the resize by the edges and the corners, the minimum size, the
//! place a dialog keeps for the session, and a click outside a dialog, which does nothing.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::choice::{ChoiceDialog, DialogChoice};
use crate::window_tests::{
    bounds_of, click, draw, git_repository, open_path, open_sample_window, open_window, press,
};
use crate::window_tests_run_configs::{open_dialog as open_run_dialog, open_run};

/// Whether the element with `selector` is on the screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Drags from `from` by (`dx`, `dy`) with the left button held, the way a pointer moves.
fn drag_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    dx: f32,
    dy: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            gpui::point(from.x + px(dx * fraction), from.y + px(dy * fraction)),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(
        gpui::point(from.x + px(dx), from.y + px(dy)),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// The Choice dialog with a question and two answers, Reload and Cancel.
fn open_choice(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            ChoiceDialog::show(
                "Reload the project?",
                "The files changed on disk.",
                vec![
                    DialogChoice::new("Reload", true, |_, _, _| {}),
                    DialogChoice::new("Cancel", false, |_, _, _| {}),
                ],
                workspace,
                window,
                cx,
            );
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Opens the Project Structure dialog with Ctrl+Alt+Shift+S and proves it is on screen.
fn open_structure(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    assert!(on_screen(cx, window, "project-structure"));
}

/// Offset of a window's left and top edges from the same edges of the earlier bounds.
fn moved_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
    before: gpui::Bounds<gpui::Pixels>,
) -> (f32, f32) {
    let after = bounds_of(cx, window, selector);
    (
        f32::from(after.left()) - f32::from(before.left()),
        f32::from(after.top()) - f32::from(before.top()),
    )
}

#[gpui::test]
fn the_dialogs_have_a_title_bar_with_a_close_cross(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-frame-titles");

    open_structure(cx, window);
    assert!(on_screen(cx, window, "project-structure-title"));
    assert!(on_screen(cx, window, "project-structure-close"));
    press(cx, window, "escape");
    draw(cx, window);

    let (_root, _planned, run, _panel) = open_run(cx, "dialog-frame-titles-run");
    open_run_dialog(cx, run);
    assert!(on_screen(cx, run, "run-configs-title"));
    assert!(on_screen(cx, run, "run-configs-close"));

    open_choice(cx, window);
    assert!(on_screen(cx, window, "choice-dialog"));
    assert!(on_screen(cx, window, "choice-title"));
    assert!(on_screen(cx, window, "choice-close"));
}

#[gpui::test]
fn the_close_cross_closes_the_dialogs(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-frame-cross");

    open_structure(cx, window);
    click(cx, window, "project-structure-close");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "project-structure"),
        "the cross closes Project Structure"
    );

    open_choice(cx, window);
    click(cx, window, "choice-close");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "choice-dialog"),
        "the cross closes the question"
    );

    let (_root, _planned, run, _panel) = open_run(cx, "dialog-frame-cross-run");
    open_run_dialog(cx, run);
    click(cx, run, "run-configs-close");
    draw(cx, run);
    assert!(
        !on_screen(cx, run, "run-configs"),
        "the cross closes Run Configurations"
    );
}

#[gpui::test]
fn the_close_cross_of_push_closes_the_window(cx: &mut TestAppContext) {
    let root = git_repository("dialog-frame-push");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);

    press(cx, window, "ctrl-shift-k");
    draw(cx, window);
    assert!(on_screen(cx, window, "push-dialog"), "Push is on screen");
    assert!(on_screen(cx, window, "push-title"));
    click(cx, window, "push-close");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "push-dialog"),
        "the cross closes Push, which Cancel does not do yet"
    );
}

#[gpui::test]
fn dragging_the_title_moves_the_dialog_by_the_pointer_offset(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-frame-move");
    open_structure(cx, window);
    let before = bounds_of(cx, window, "project-structure");
    let title = bounds_of(cx, window, "project-structure-title");

    drag_by(cx, window, title.center(), 120., 60.);

    let (moved_x, moved_y) = moved_by(cx, window, "project-structure", before);
    assert!(
        (moved_x - 120.).abs() <= 4.,
        "the dialog follows the pointer to the right: {moved_x}"
    );
    assert!(
        (moved_y - 60.).abs() <= 4.,
        "the dialog follows the pointer down: {moved_y}"
    );
}

#[gpui::test]
fn the_title_bar_stays_on_the_window_when_dragged_off_it(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "dialog-frame-clamp-run");
    open_run_dialog(cx, window);
    let title = bounds_of(cx, window, "run-configs-title");

    drag_by(cx, window, title.center(), -5000., -5000.);

    let dialog = bounds_of(cx, window, "run-configs");
    assert!(
        f32::from(dialog.top()) >= -0.5,
        "the title bar does not go above the window: {:?}",
        dialog.top()
    );
    assert!(
        f32::from(dialog.right()) >= 100.,
        "part of the dialog stays on the window, so its cross stays reachable: {:?}",
        dialog.right()
    );
}

#[gpui::test]
fn dragging_the_corner_and_the_left_edge_resizes_the_dialog(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "dialog-frame-resize-run");
    open_run_dialog(cx, window);
    let before = bounds_of(cx, window, "run-configs");

    let corner = bounds_of(cx, window, "run-configs-resize-corner");
    drag_by(cx, window, corner.center(), 60., 40.);
    let after = bounds_of(cx, window, "run-configs");
    let wider = f32::from(after.size.width) - f32::from(before.size.width);
    let taller = f32::from(after.size.height) - f32::from(before.size.height);
    assert!(
        (wider - 60.).abs() <= 4.,
        "the right edge follows the corner: {wider}"
    );
    assert!(
        (taller - 40.).abs() <= 4.,
        "the bottom edge follows the corner: {taller}"
    );

    let left = bounds_of(cx, window, "run-configs-resize-left");
    drag_by(cx, window, left.center(), -40., 0.);
    let last = bounds_of(cx, window, "run-configs");
    assert!(
        (f32::from(after.left()) - f32::from(last.left()) - 40.).abs() <= 4.,
        "the left edge follows the pointer"
    );
    assert!(
        (f32::from(last.right()) - f32::from(after.right())).abs() <= 4.,
        "the right edge stays where it was"
    );
}

#[gpui::test]
fn a_resize_never_goes_below_the_minimum(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "dialog-frame-minimum-run");
    open_run_dialog(cx, window);

    let corner = bounds_of(cx, window, "run-configs-resize-corner");
    drag_by(cx, window, corner.center(), -5000., -5000.);

    let small = bounds_of(cx, window, "run-configs");
    assert!(
        (f32::from(small.size.width) - 640.).abs() <= 4.,
        "the width stops at the minimum: {:?}",
        small.size.width
    );
    assert!(
        (f32::from(small.size.height) - 400.).abs() <= 4.,
        "the height stops at the minimum: {:?}",
        small.size.height
    );
}

#[gpui::test]
fn a_dialog_opens_again_at_the_place_and_size_it_had(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-frame-remember");
    open_structure(cx, window);
    let title = bounds_of(cx, window, "project-structure-title");
    drag_by(cx, window, title.center(), 100., 50.);
    let kept = bounds_of(cx, window, "project-structure");

    click(cx, window, "project-structure-close");
    draw(cx, window);
    open_structure(cx, window);

    let again = bounds_of(cx, window, "project-structure");
    assert!(
        (f32::from(again.left()) - f32::from(kept.left())).abs() <= 1.,
        "the dialog opens where it was: {:?}",
        again.left()
    );
    assert!(
        (f32::from(again.top()) - f32::from(kept.top())).abs() <= 1.,
        "the dialog opens at the same height"
    );
    assert_eq!(again.size, kept.size, "the dialog opens at the size it had");
}

#[gpui::test]
fn a_click_outside_a_dialog_neither_closes_it_nor_reaches_the_editor(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "dialog-frame-outside");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_structure(cx, window);
    let dialog = bounds_of(cx, window, "project-structure");
    let editor = bounds_of(cx, window, "editor-body-0");
    // The top left corner of the editor lies outside the dialog, which is centred.
    let outside = gpui::point(editor.left() + px(6.), editor.top() + px(6.));
    assert!(
        !dialog.contains(&outside),
        "the test point is outside the dialog"
    );

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(outside, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        on_screen(cx, window, "project-structure"),
        "a click outside does not close the dialog"
    );
    // Escape closes the dialog only while the dialog has the focus: a click that reached
    // the editor under it would have moved the focus there.
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "project-structure"),
        "Escape closes the dialog after the click outside"
    );
}

#[gpui::test]
fn the_cancel_button_closes_the_dialogs(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-frame-cancel");

    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    assert!(on_screen(cx, window, "settings"));
    click(cx, window, "settings-cancel");
    draw(cx, window);
    assert!(!on_screen(cx, window, "settings"), "Cancel closes Settings");

    open_structure(cx, window);
    click(cx, window, "project-structure-cancel");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "project-structure"),
        "Cancel closes Project Structure"
    );

    open_choice(cx, window);
    click(cx, window, "choice-1");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "choice-dialog"),
        "the second answer closes the question"
    );

    let (_root, _planned, run, _panel) = open_run(cx, "dialog-frame-cancel-run");
    open_run_dialog(cx, run);
    click(cx, run, "run-config-cancel");
    draw(cx, run);
    assert!(
        !on_screen(cx, run, "run-configs"),
        "Cancel closes Run Configurations"
    );
}
