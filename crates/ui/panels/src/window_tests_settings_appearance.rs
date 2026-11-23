//! The Appearance page keeps the theme, the interface scale and the parts of the window,
//! while the editor font and the update check have pages of their own: Editor > Font and
//! System Settings > Updates. The search and Find Action lead to those pages.

use std::fs;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::settings::SettingsView;
use crate::window_tests::{click, draw, open_sample_window, press, type_text};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn settings_view(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> gpui::Entity<SettingsView> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window answers")
        .expect("the Settings window is open")
        .downcast::<SettingsView>()
        .unwrap_or_else(|_| panic!("the overlay is the Settings window"))
}

fn open_settings(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    window
}

#[gpui::test]
fn the_font_page_has_the_typeface_the_size_and_the_line_height(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-font-page");

    click(cx, window, "settings-page-font");
    draw(cx, window);

    for selector in [
        "settings-editor-font-family",
        "settings-font-up",
        "settings-font-down",
        "settings-line-spacing-up",
        "settings-line-spacing-down",
        "settings-font-sample",
    ] {
        assert!(has(cx, window, selector), "Editor > Font shows {selector}");
    }
}

#[gpui::test]
fn a_step_of_the_size_changes_the_editor_font(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-font-size-step");
    click(cx, window, "settings-page-font");
    draw(cx, window);

    let before = cx.update(|cx| crate::editor_tab::style_of(cx).font_size);
    let sample_before = crate::window_tests::bounds_of(cx, window, "settings-font-sample")
        .size
        .height;
    click(cx, window, "settings-font-up");
    draw(cx, window);
    let sample_after = crate::window_tests::bounds_of(cx, window, "settings-font-sample")
        .size
        .height;
    assert!(
        sample_after > sample_before,
        "the sample follows the size at once: {sample_before:?} to {sample_after:?}"
    );
    click(cx, window, "settings-apply");
    draw(cx, window);

    assert_eq!(
        cx.update(|cx| crate::editor_tab::style_of(cx).font_size),
        before + 1.,
        "Size reaches the font of the editor"
    );
    assert_eq!(
        cx.read(|cx| cx.global::<Settings>().font_size),
        before + 1.,
        "the size reaches the settings of the window"
    );
}

#[gpui::test]
fn the_appearance_page_has_no_editor_font_or_update_rows(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-appearance-groups");

    assert!(
        has(cx, window, "settings-theme-sync-with-os"),
        "the theme group keeps Sync with OS"
    );
    assert!(
        has(cx, window, "settings-theme-light") && has(cx, window, "settings-theme-dark"),
        "the theme group keeps the two system slots"
    );
    assert!(
        has(cx, window, "settings-ui-scale-up"),
        "the UI group keeps the interface scale"
    );
    assert!(
        has(cx, window, "settings-presentation-font-size-up"),
        "the Window group keeps the Presentation Mode font size"
    );
    assert!(
        has(cx, window, "settings-page-font") && has(cx, window, "settings-page-updates"),
        "the tree names the pages of the moved rows"
    );
    for selector in [
        "settings-editor-font-family",
        "settings-font-up",
        "settings-line-spacing-up",
        "settings-updates-check-automatically",
    ] {
        assert!(
            !has(cx, window, selector),
            "Appearance has no {selector} any more"
        );
    }
}

#[gpui::test]
fn the_search_of_the_font_size_opens_the_font_page(cx: &mut TestAppContext) {
    let window = open_settings(cx, "settings-font-search");

    click(cx, window, "settings-search");
    type_text(cx, window, "font size");
    draw(cx, window);

    assert!(
        has(cx, window, "settings-page-font"),
        "the filtered tree keeps Editor > Font"
    );
    assert!(
        has(cx, window, "settings-font-up"),
        "the window opens the page with the setting, not the group above it"
    );
    assert!(
        !has(cx, window, "settings-page-project"),
        "the pages without a match leave the tree"
    );
}

#[gpui::test]
fn the_update_check_has_its_own_page(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "settings-updates-page");
    window
        .update(cx, |workspace, window, cx| {
            SettingsView::show_at(workspace, window, cx, Some("updates_check_automatically"));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        has(cx, window, "settings-updates-check-automatically"),
        "System Settings > Updates holds the update check"
    );
    click(cx, window, "settings-updates-check-automatically");
    draw(cx, window);
    assert!(
        !settings_view(cx, window).read_with(cx, |view, _| view
            .draft_settings()
            .updates_check_automatically),
        "the switch changes the draft of the page"
    );
}

#[test]
fn the_old_keys_of_the_moved_fields_still_load() {
    let dir = test_support::scratch("settings-appearance-keys");
    let file = dir.join("settings.json");
    fs::write(
        &file,
        r#"{"editor_font_family":"Consolas","font_size":17.0,"line_spacing":1.4,"updates_check_automatically":false}"#,
    )
    .unwrap();

    let settings = Settings::load(&file);
    assert_eq!(settings.editor_font_family, "Consolas");
    assert_eq!(settings.font_size, 17.);
    assert_eq!(settings.line_spacing, 1.4);
    assert!(!settings.updates_check_automatically);
}
