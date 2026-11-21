//! The dialogs moved onto the shared frame: the Import Settings window and the Find Usages
//! dialog drag by the title bar, resize by the bottom right corner and close by the cross,
//! and they open again at the place and size the user left them.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::usages::FindUsagesSettings;
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press};

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

/// The offset of the left and top edges of an element from the same edges before a drag.
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

/// Opens the Import Settings dialog the way the File menu does.
fn open_import(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            crate::idea_import::open(workspace, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Opens the Find Usages dialog the way Ctrl+Alt+Shift+F7 does.
fn open_find_usages(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            FindUsagesSettings::show(workspace, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// Checks one dialog of the shared frame: it drags by its title bar, grows by its bottom
/// right corner, closes by its cross, and opens again where and how large it was left.
fn check_frame(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    dialog: &'static str,
    title: &'static str,
    close: &'static str,
    corner: &'static str,
    open: impl Fn(&mut TestAppContext, WindowHandle<Workspace>),
) {
    open(cx, window);
    assert!(on_screen(cx, window, dialog), "{dialog} is on screen");
    let before = bounds_of(cx, window, dialog);
    let title_bar = bounds_of(cx, window, title);

    drag_by(cx, window, title_bar.center(), 90., 50.);

    let (moved_x, moved_y) = moved_by(cx, window, dialog, before);
    assert!(
        (moved_x - 90.).abs() <= 4.,
        "{dialog} follows the pointer to the right: {moved_x}"
    );
    assert!(
        (moved_y - 50.).abs() <= 4.,
        "{dialog} follows the pointer down: {moved_y}"
    );
    let moved = bounds_of(cx, window, dialog);

    let grip = bounds_of(cx, window, corner);
    drag_by(cx, window, grip.center(), 60., 40.);

    let resized = bounds_of(cx, window, dialog);
    let wider = f32::from(resized.size.width) - f32::from(moved.size.width);
    let taller = f32::from(resized.size.height) - f32::from(moved.size.height);
    assert!(
        (wider - 60.).abs() <= 4.,
        "{dialog} grows by the right edge: {wider}"
    );
    assert!(
        (taller - 40.).abs() <= 4.,
        "{dialog} grows by the bottom edge: {taller}"
    );

    click(cx, window, close);
    draw(cx, window);
    assert!(!on_screen(cx, window, dialog), "the cross closes {dialog}");

    open(cx, window);
    let again = bounds_of(cx, window, dialog);
    assert!(
        (f32::from(again.left()) - f32::from(resized.left())).abs() <= 1.,
        "{dialog} opens where it was left"
    );
    assert!(
        (f32::from(again.top()) - f32::from(resized.top())).abs() <= 1.,
        "{dialog} opens at the height it was left"
    );
    assert_eq!(
        again.size, resized.size,
        "{dialog} opens at the size it was left"
    );
}

#[gpui::test]
fn the_import_settings_dialog_drags_resizes_and_closes(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-windows-import");
    check_frame(
        cx,
        window,
        "idea-import",
        "idea-import-title",
        "idea-import-close",
        "idea-import-resize-corner",
        open_import,
    );

    // The dialog is open again: Escape closes it, as Enter starts the import.
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "idea-import"),
        "Escape closes Import Settings"
    );
}

#[gpui::test]
fn the_find_usages_dialog_drags_resizes_and_closes(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-windows-usages");
    check_frame(
        cx,
        window,
        "find-usages-settings",
        "find-usages-title",
        "find-usages-close",
        "find-usages-resize-corner",
        open_find_usages,
    );
}
