//! The theme that follows the system: Sync with OS in Settings | Appearance, the theme of
//! each system look and the theme a window starts with. The test platform has no
//! appearance of its own, so a test puts `SystemAppearance` in place of the window's.

use std::time::{Duration, Instant};

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{SETTINGS_FILE, Settings, SystemAppearance, Workspace};

use crate::window_tests::{click, draw, open_sample_window, press, wait_for_signal};

fn settings(cx: &TestAppContext) -> Settings {
    cx.read(|cx| cx.global::<Settings>().clone())
}

fn theme_is_dark(cx: &TestAppContext) -> bool {
    cx.read(|cx| cx.global::<Theme>().dark)
}

fn settings_file(cx: &TestAppContext, window: WindowHandle<Workspace>) -> std::path::PathBuf {
    window
        .read_with(cx, |workspace, _| {
            workspace.settings_dir().join(SETTINGS_FILE)
        })
        .unwrap()
}

/// The system changes its look: a test writes the new one and lets the window follow it,
/// as `observe_window_appearance` does in a real window.
fn system_changes(cx: &mut TestAppContext, window: WindowHandle<Workspace>, dark: bool) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark }));
    window
        .update(cx, |workspace, _, cx| {
            workspace.on_system_appearance_changed(cx)
        })
        .unwrap();
    cx.run_until_parked();
}

/// Draws the frames of the fade until the theme of the window is the wanted one.
fn wait_for_theme(cx: &mut TestAppContext, window: WindowHandle<Workspace>, dark: bool) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        draw(cx, window);
        if theme_is_dark(cx) == dark {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    panic!("the theme did not become {dark} in two seconds");
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_window_follows_the_system_appearance_while_the_sync_is_on(cx: &mut TestAppContext) {
    // The theme is taken at the open, before the first frame.
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-sync-follows");
    cx.run_until_parked();
    assert!(theme_is_dark(cx), "the window opens with the dark look");
    assert!(settings(cx).theme_sync_with_os, "a new installation syncs");

    // The system turns light: the window goes light without a restart.
    system_changes(cx, window, false);
    wait_for_theme(cx, window, false);
    assert!(
        settings(cx).theme_sync_with_os,
        "a system change keeps the sync on"
    );

    // And back, in the same window.
    system_changes(cx, window, true);
    wait_for_theme(cx, window, true);
}

#[gpui::test]
fn a_window_without_the_sync_keeps_its_theme(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: false }));
    let (_, window) = open_sample_window(cx, "theme-sync-off");
    assert!(!theme_is_dark(cx), "the window opens with the light look");

    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.theme_sync_with_os = false, cx)
        })
        .unwrap();
    cx.run_until_parked();
    assert!(!settings(cx).theme_sync_with_os);

    system_changes(cx, window, true);
    for _ in 0..10 {
        draw(cx, window);
    }
    assert!(
        !theme_is_dark(cx),
        "the system change does not reach a window that does not sync"
    );
}

#[gpui::test]
fn a_theme_chosen_by_hand_turns_the_sync_off(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: false }));
    let (_, window) = open_sample_window(cx, "theme-sync-hand");
    let file = settings_file(cx, window);
    assert!(!theme_is_dark(cx));

    window
        .update(cx, |workspace, _, cx| workspace.toggle_theme(cx))
        .unwrap();
    wait_for_theme(cx, window, true);
    assert!(
        !settings(cx).theme_sync_with_os,
        "a theme chosen by hand turns the sync off, as in IDEA"
    );
    wait_for_signal(cx, "the saved sync", |_| {
        std::fs::read_to_string(&file)
            .is_ok_and(|text| text.contains("\"theme_sync_with_os\": false"))
    });

    // The system no longer has a say in this window.
    system_changes(cx, window, false);
    for _ in 0..10 {
        draw(cx, window);
    }
    assert!(theme_is_dark(cx), "the chosen theme stays");
}

#[gpui::test]
fn the_appearance_page_offers_the_system_theme(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: false }));
    let (_, window) = open_sample_window(cx, "theme-sync-settings");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    for selector in [
        "settings-theme-sync-with-os",
        "settings-theme-light",
        "settings-theme-dark",
    ] {
        assert!(has(cx, window, selector), "{selector} is on the page");
    }

    click(cx, window, "settings-theme-sync-with-os");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);
    assert!(
        !settings(cx).theme_sync_with_os,
        "the switch turns the sync off"
    );

    click(cx, window, "settings-theme-sync-with-os");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);
    assert!(
        settings(cx).theme_sync_with_os,
        "the switch turns the sync on again"
    );

    // The two choices name the themes of the system looks.
    click(cx, window, "settings-theme-light");
    draw(cx, window);
    click(cx, window, "settings-choice-option-dark");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);
    assert_eq!(settings(cx).theme_light, "dark", "the list picks the slot");
    click(cx, window, "settings-theme-dark");
    draw(cx, window);
    click(cx, window, "settings-choice-option-light");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);
    assert_eq!(settings(cx).theme_dark, "light", "the list picks the slot");
}

#[test]
fn a_start_after_a_chosen_theme_keeps_the_sync_off() {
    // An installation of the old release: a theme was chosen and the settings file
    // names no Sync with OS.
    let state = test_support::scratch("theme-sync-start-state");
    let settings_dir = test_support::scratch("theme-sync-start-settings");
    let store = store::Store::open(&state.join("state.sqlite")).unwrap();
    store.put("ui.theme", "light").unwrap();
    drop(store);
    std::fs::write(settings_dir.join(SETTINGS_FILE), r#"{"font_size": 15.0}"#).unwrap();

    let context = workspace::start_context(&state, &settings_dir);
    assert!(!context.dark, "the chosen light theme stays");
    assert!(
        !context.settings.theme_sync_with_os,
        "an old settings file leaves the sync off when a theme was chosen"
    );
    assert_eq!(context.settings.font_size, 15.);

    // A fresh installation follows the system.
    let fresh_state = test_support::scratch("theme-sync-fresh-state");
    let fresh_settings = test_support::scratch("theme-sync-fresh-settings");
    let fresh = workspace::start_context(&fresh_state, &fresh_settings);
    assert!(fresh.settings.theme_sync_with_os);
    assert!(
        fresh.dark,
        "a fresh installation starts dark until told otherwise"
    );
}
