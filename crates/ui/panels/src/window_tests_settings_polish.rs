//! The controls of the Settings pages of IDEA: a boolean setting is a checkbox with its
//! label, a choice is a dropdown, and the tree of pages has room for the longest name.

use std::fs;
use std::path::PathBuf;

use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use workspace::{Settings, Workspace};

use crate::settings::SettingsView;
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, wait_for_signal};

fn settings_file(cx: &TestAppContext, window: WindowHandle<Workspace>) -> PathBuf {
    window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(workspace::SETTINGS_FILE)
        })
        .unwrap()
}

fn settings_view(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<SettingsView> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window answers")
        .expect("the Settings window is open")
        .downcast::<SettingsView>()
        .unwrap_or_else(|_| panic!("the overlay is the Settings window"))
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn settings(cx: &TestAppContext) -> Settings {
    cx.read(|cx| cx.global::<Settings>().clone())
}

/// Opens a project and Ctrl+Alt+S on it, and proves the Settings window is on screen.
fn open_settings(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    bounds_of(cx, window, "settings");
    window
}

#[gpui::test]
fn a_boolean_setting_is_a_checkbox_and_its_label_toggles_and_saves(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-polish-checkbox");
    let file = settings_file(cx, window);

    assert!(
        has(cx, window, "settings-theme-sync-with-os-box"),
        "the boolean setting starts with a checkbox"
    );

    // The label sits right of the box: a click on it flips the whole row.
    let row = bounds_of(cx, window, "settings-theme-sync-with-os");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(
        point(row.left() + px(60.), row.center().y),
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);

    let view = settings_view(cx, window);
    assert!(
        !view.read_with(cx, |view, _| view.draft_settings().theme_sync_with_os),
        "a click on the label turns the checkbox off"
    );

    click(cx, window, "settings-apply");
    draw(cx, window);
    wait_for_signal(cx, "the saved switch", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"theme_sync_with_os\": false"))
    });
}

#[gpui::test]
fn a_theme_is_chosen_from_a_dropdown(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-polish-theme");
    let file = settings_file(cx, window);

    click(cx, window, "settings-theme-light");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-choice-option-nord"),
        "the light theme row opens the list of themes"
    );

    click(cx, window, "settings-choice-option-nord");
    draw(cx, window);
    assert_eq!(
        settings(cx).theme_light,
        "nord",
        "the value of the list is the theme of the light system look"
    );
    assert!(
        !has(cx, window, "settings-choice-option-nord"),
        "the list closes after the choice"
    );

    click(cx, window, "settings-apply");
    draw(cx, window);
    wait_for_signal(cx, "the saved theme", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains("\"theme_light\": \"nord\""))
    });
}

#[gpui::test]
fn the_keys_of_a_dropdown_walk_its_values(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-polish-keys");

    click(cx, window, "settings-theme-light");
    draw(cx, window);
    press(cx, window, "down");
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(
        settings(cx).theme_light,
        "high-contrast",
        "Down and Enter pick the next theme of the list"
    );
}

/// Drags the pointer from `from` by `dx` with the left button held. The window is drawn
/// after each event, as the frames of the app are drawn between the events of a drag: the
/// layer that takes the moves of a drag is in the frame that follows the press.
fn drag_by(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    dx: f32,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, Modifiers::default());
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
    drop(visual);
    draw(cx, window);
    for step in 1..=8 {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual.simulate_mouse_move(
            point(from.x + px(dx * step as f32 / 8.), from.y),
            Some(gpui::MouseButton::Left),
            Modifiers::default(),
        );
        drop(visual);
        draw(cx, window);
    }
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_up(
        point(from.x + px(dx), from.y),
        gpui::MouseButton::Left,
        Modifiers::default(),
    );
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

#[gpui::test]
fn the_tree_widens_by_its_divider(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-polish-divider");
    let splitter = bounds_of(cx, window, "settings-pages-splitter");
    let before = splitter.left();
    drag_by(cx, window, splitter.center(), 40.);
    let after = bounds_of(cx, window, "settings-pages-splitter").left();
    assert!(
        after > before,
        "a drag right widens the tree: {before:?} to {after:?}"
    );
}

#[gpui::test]
fn the_longest_page_name_has_room_in_the_tree(cx: &mut TestAppContext) {
    let _window = open_settings(cx, "settings-polish-tree");
    let probe = ui_kit::test_bounds()
        .into_iter()
        .find(|(name, _)| name == "settings-page-title-file-and-code-templates")
        .map(|(_, bounds)| bounds)
        .expect("the longest page name is drawn");
    assert!(
        probe.label_width <= probe.bounds.size.width,
        "the row has room for the whole name: {} of {:?}",
        f32::from(probe.label_width),
        probe.bounds.size.width
    );
}
