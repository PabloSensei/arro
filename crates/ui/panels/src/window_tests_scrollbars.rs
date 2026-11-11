//! Scrollbars of the window without a window: the project tree, Settings and Search
//! Everywhere. The wheel and the thumb move the real `ScrollHandle` of the area, and a
//! list that fits shows no bar at all.

use std::fs;
use std::path::PathBuf;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, Pixels, Point, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext,
    WindowHandle, size,
};
use ui_kit::{Modifiers, point, px};
use workspace::Workspace;

use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    bounds_of, draw, open_palette, open_sample_window, open_window, press, scratch,
    wait_for_project_index, wait_for_project_tree,
};

/// A project that fills the tree: `count` files next to a small folder.
fn project_with(name: &str, count: usize) -> PathBuf {
    let root = scratch(name);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src").join("main.txt"), "alpha\n").unwrap();
    for index in 0..count {
        fs::write(root.join(format!("file_{index:03}.txt")), "text\n").unwrap();
    }
    root
}

fn scroll_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: Point<Pixels>,
    lines: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position,
        delta: ScrollDelta::Lines(point(0., -lines)),
        modifiers: Modifiers::default(),
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn resize(cx: &mut TestAppContext, window: WindowHandle<Workspace>, width: f32, height: f32) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.resize(size(px(width), px(height)));
        window.bounds_changed(cx);
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Moves the pointer onto the strip of `name` so that its bar shows.
fn hover_strip(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let Some(bounds) = VisualTestContext::from_window(window.into(), cx).debug_bounds(selector)
    else {
        return false;
    };
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(bounds.center(), None, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    true
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

#[ui_kit::gpui::test]
fn the_project_tree_shows_a_bar_after_scrolling_and_drags_it(cx: &mut TestAppContext) {
    let root = project_with("scroll-tree", 100);
    let window = open_window(cx, root);
    wait_for_project_tree(cx);
    draw(cx, window);

    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.scroll_offset()),
        0.,
        "the tree starts at the top"
    );
    assert!(
        !has(cx, window, "scrollbar-thumb-v-project-tree"),
        "no bar before the tree moves"
    );

    let area = bounds_of(cx, window, "scroll-area-project-tree");
    scroll_at(cx, window, area.center(), 10.);
    let scrolled = panel.read_with(cx, |panel, _| panel.scroll_offset());
    assert!(scrolled > 0., "the wheel moved the tree by {scrolled}");
    assert!(
        has(cx, window, "scrollbar-thumb-v-project-tree"),
        "the bar shows after the tree moves"
    );

    let thumb = bounds_of(cx, window, "scrollbar-thumb-v-project-tree");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(thumb.center(), MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(
        thumb.center() + point(px(0.), px(60.)),
        Some(MouseButton::Left),
        Modifiers::default(),
    );
    visual.simulate_mouse_up(
        thumb.center() + point(px(0.), px(60.)),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.run_until_parked();
    draw(cx, window);

    let dragged = panel.read_with(cx, |panel, _| panel.scroll_offset());
    assert!(
        dragged > scrolled,
        "the dragged thumb scrolled the tree: {dragged} against {scrolled}"
    );
}

#[ui_kit::gpui::test]
fn a_short_tree_has_no_bar(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "scroll-short");
    wait_for_project_tree(cx);
    draw(cx, window);

    assert!(
        has(cx, window, "scroll-area-project-tree"),
        "the tree is on screen"
    );
    assert!(
        !has(cx, window, "scrollbar-thumb-v-project-tree"),
        "a tree that fits shows no bar"
    );
}

#[ui_kit::gpui::test]
fn settings_shows_a_bar_on_a_long_column(cx: &mut TestAppContext) {
    let root = project_with("scroll-settings", 20);
    let window = open_window(cx, root);
    wait_for_project_tree(cx);
    // A short window makes the pages and the content longer than their columns.
    resize(cx, window, 640., 280.);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);

    assert!(
        has(cx, window, "scroll-area-settings-pages"),
        "the pages column is on screen"
    );
    let pages = hover_strip(cx, window, "scrollbar-v-settings-pages");
    assert!(pages, "the pages of Settings have a bar");
    assert!(
        has(cx, window, "scrollbar-thumb-v-settings-pages"),
        "the bar of the pages shows under the pointer"
    );

    let content = hover_strip(cx, window, "scrollbar-v-settings-content");
    assert!(content, "the content of Settings has a bar");
    assert!(
        has(cx, window, "scrollbar-thumb-v-settings-content"),
        "the bar of the content shows under the pointer"
    );
}

#[ui_kit::gpui::test]
fn search_everywhere_shows_a_bar_on_a_long_list(cx: &mut TestAppContext) {
    let root = project_with("scroll-palette", 100);
    let window = open_window(cx, root);
    wait_for_project_index(cx);
    wait_for_project_tree(cx);
    open_palette(cx, window, keys::SEARCH_EVERYWHERE);
    press(cx, window, "tab");
    press(cx, window, "tab");
    cx.simulate_keystrokes(window.into(), "f");
    draw(cx, window);

    assert!(
        has(cx, window, "scroll-area-palette-list"),
        "the list of Search Everywhere is on screen"
    );
    assert!(
        hover_strip(cx, window, "scrollbar-v-palette-list"),
        "the list of Search Everywhere has a bar"
    );
    assert!(
        has(cx, window, "scrollbar-thumb-v-palette-list"),
        "the bar shows under the pointer"
    );
}
