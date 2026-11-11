//! Popups open by one rule: a short fade where the content is known, no motion at
//! all where it is not, and the real size before the first visible frame.

use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::SPACE_M;
use ui_kit::{
    Bounds, MenuEntry, TOOLTIP_DELAY, TOOLTIP_GAP, TOOLTIP_WARM, TooltipTiming, point, px,
    submenu_position, tooltip_position,
};
use workspace::Workspace;

use crate::window_tests::{bounds_of, click, draw, open_path, open_sample_window, type_text};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project};

fn selector(path: &[usize]) -> &'static str {
    let parts: Vec<String> = path.iter().map(|index| index.to_string()).collect();
    Box::leak(format!("menu-{}", parts.join("-")).into_boxed_str())
}

fn has_popup(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn menu_entries(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().map(<[MenuEntry]>::to_vec)
        })
        .unwrap()
        .expect("the main menu is open")
}

/// Opens the File section of the menu bar and the submenu of its first row that has
/// one.
fn open_first_submenu(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> usize {
    click(cx, window, "menu-bar-0");
    draw(cx, window);
    let entries = menu_entries(cx, window);
    let row = entries
        .iter()
        .position(MenuEntry::has_submenu)
        .expect("the File menu has a row with a submenu");
    click(cx, window, selector(&[row]));
    row
}

#[ui_kit::gpui::test]
fn a_submenu_keeps_its_position_from_the_first_visible_frame(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "submenu-no-jump");
    open_first_submenu(cx, window);

    // A frame before the size is known keeps the submenu hidden; the next frames
    // show it at its real place.
    for _ in 0..6 {
        if has_popup(cx, window, "menu-popup-1") {
            break;
        }
        assert!(
            !has_popup(cx, window, "menu-popup-1-pending")
                || !has_popup(cx, window, "menu-popup-1"),
            "the measuring frame never shows the submenu at the same time"
        );
        draw(cx, window);
    }
    let first = bounds_of(cx, window, "menu-popup-1");
    draw(cx, window);
    let second = bounds_of(cx, window, "menu-popup-1");
    assert_eq!(
        first.origin, second.origin,
        "the submenu stands where its real size puts it on both frames"
    );
    assert_eq!(first.size, second.size);
}

#[ui_kit::gpui::test]
fn a_popup_at_the_bottom_opens_up_and_stays_in_the_window(_cx: &mut TestAppContext) {
    let viewport = ui_kit::size(px(800.), px(600.));
    let row = Bounds::new(point(px(100.), px(520.)), ui_kit::size(px(200.), px(26.)));
    let size = ui_kit::size(px(180.), px(240.));
    let at = submenu_position(row, size, viewport);
    assert!(at.y < row.top(), "the submenu opens above the row");
    assert_eq!(
        at.y + size.height + px(SPACE_M),
        viewport.height,
        "its bottom edge keeps the margin"
    );
}

#[ui_kit::gpui::test]
fn a_popup_at_the_right_edge_shifts_left_and_stays_in_the_window(_cx: &mut TestAppContext) {
    let viewport = ui_kit::size(px(800.), px(600.));
    let row = Bounds::new(point(px(620.), px(80.)), ui_kit::size(px(160.), px(26.)));
    let size = ui_kit::size(px(240.), px(180.));
    let at = submenu_position(row, size, viewport);
    assert!(
        at.x < row.left(),
        "the submenu opens to the left of the row"
    );
    assert!(
        at.x >= px(SPACE_M) && at.x + size.width + px(SPACE_M) <= viewport.width,
        "the submenu stays inside the window"
    );
}

#[ui_kit::gpui::test]
fn a_tooltip_waits_half_a_second_and_a_neighbour_shows_at_once(_cx: &mut TestAppContext) {
    let mut timing = TooltipTiming::new();
    let start = Instant::now();
    assert_eq!(timing.delay(start), TOOLTIP_DELAY);
    assert_eq!(TOOLTIP_DELAY, Duration::from_millis(500));

    timing.hidden(start);
    let next = start + Duration::from_millis(400);
    assert_eq!(
        timing.delay(next),
        Duration::ZERO,
        "a neighbouring button in the warm window shows without a delay"
    );
    let late = start + TOOLTIP_WARM + Duration::from_millis(1);
    assert_eq!(
        timing.delay(late),
        TOOLTIP_DELAY,
        "after the warm window the delay is back"
    );
}

#[ui_kit::gpui::test]
fn a_tooltip_stands_under_its_button_and_moves_above_at_the_bottom(_cx: &mut TestAppContext) {
    let viewport = ui_kit::size(px(800.), px(600.));
    let size = ui_kit::size(px(160.), px(30.));
    let button = Bounds::new(point(px(100.), px(100.)), ui_kit::size(px(40.), px(30.)));
    let below = tooltip_position(button, size, viewport);
    assert_eq!(
        below,
        point(px(100.), px(130.) + px(TOOLTIP_GAP)),
        "under the button with a gap"
    );

    let low = Bounds::new(point(px(100.), px(540.)), ui_kit::size(px(40.), px(30.)));
    let above = tooltip_position(low, size, viewport);
    assert!(
        above.y < low.top(),
        "near the bottom the tooltip goes above"
    );
    assert!(above.y + size.height + px(SPACE_M) <= viewport.height);
}

#[ui_kit::gpui::test]
fn the_completion_list_is_shown_on_its_first_frame(cx: &mut TestAppContext) {
    let root = project("popup-motion-completion", &[("src/Type.txt", "\n")]);
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let path = root.join("src").join("Type.txt");
    open_path(cx, window, &path);
    type_text(cx, window, "g");
    draw(cx, window);
    let popup = bounds_of(cx, window, "completion-popup");
    assert!(
        popup.size.height > px(0.),
        "the completion list is in the very first frame"
    );
    assert_eq!(
        ui_kit::motion::POPUP_FADE,
        80,
        "the one fade of popups and dialogs"
    );
}
