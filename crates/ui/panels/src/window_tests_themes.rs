//! Themes as data in a window: the list of the Appearance page, the quick switch that
//! shows a theme under the arrows, the theme files of the settings folder and Sync with
//! OS choosing any theme of the catalog.

use std::time::{Duration, Instant};

use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, SystemAppearance, Workspace};

use crate::window_tests::{draw, open_sample_window, press};

/// The id of the theme the window shows.
fn theme_id(cx: &TestAppContext) -> &'static str {
    cx.read(|cx| cx.global::<Theme>().id())
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// Draws the frames of a fade until the window shows the theme of `id`.
fn wait_for_theme(cx: &mut TestAppContext, window: WindowHandle<Workspace>, id: &str) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        draw(cx, window);
        if theme_id(cx) == id {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
        cx.run_until_parked();
    }
    panic!(
        "the theme did not become {id} in two seconds, it is {}",
        theme_id(cx)
    );
}

#[ui_kit::gpui::test]
fn the_appearance_page_lists_every_built_in_theme(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-list");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    for selector in [
        "theme-card-dark",
        "theme-card-light",
        "theme-card-high-contrast",
        "theme-card-retro",
        "theme-card-nord",
        "theme-card-mocha",
        "theme-card-paper",
        "settings-theme-import",
    ] {
        assert!(has(cx, window, selector), "{selector} is on the page");
    }
}

#[ui_kit::gpui::test]
fn the_quick_switch_shows_a_theme_at_once_and_escape_brings_the_old_one_back(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-quick-switch");
    assert_eq!(theme_id(cx), "dark");
    press(cx, window, "ctrl-`");
    draw(cx, window);
    assert!(has(cx, window, "scheme-popup"));

    // The three scheme rows come first, then the themes: dark, light, high contrast and
    // so on. The fourth step lands on the light theme.
    for _ in 0..4 {
        press(cx, window, "down");
    }
    assert_eq!(
        theme_id(cx),
        "light",
        "the arrows show a theme before Enter"
    );

    press(cx, window, "escape");
    assert_eq!(
        theme_id(cx),
        "dark",
        "Esc brings the theme of the open back"
    );
    assert!(!has(cx, window, "scheme-popup"));
}

#[ui_kit::gpui::test]
fn a_theme_of_the_settings_folder_joins_the_list(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-user");
    let settings_dir = window
        .read_with(cx, |workspace, _| workspace.settings_dir().to_path_buf())
        .unwrap();
    let themes = settings_dir.join("themes");
    std::fs::create_dir_all(&themes).unwrap();
    std::fs::write(
        themes.join("mine.theme.json"),
        r##"{
            "name": "Mine",
            "dark": true,
            "parent": "dark",
            "ui": {"accent": "#ff00aa"}
        }"##,
    )
    .unwrap();
    window
        .update(cx, |workspace, _, cx| workspace.reload_themes(cx))
        .unwrap();
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    assert!(
        has(cx, window, "theme-card-mine"),
        "a theme file of the settings folder is in the list"
    );
}

#[ui_kit::gpui::test]
fn sync_with_os_chooses_between_new_themes(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-sync-new");
    assert_eq!(theme_id(cx), "dark");
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings| {
                    settings.theme_sync_with_os = true;
                    settings.theme_dark = "nord".to_owned();
                    settings.theme_light = "paper".to_owned();
                },
                cx,
            )
        })
        .unwrap();
    wait_for_theme(cx, window, "nord");
    assert!(
        cx.read(|cx| cx.global::<Settings>().theme_sync_with_os),
        "a system change keeps the sync on"
    );

    // The system turns light: the light slot names Paper.
    cx.update(|cx| cx.set_global(SystemAppearance { dark: false }));
    window
        .update(cx, |workspace, _, cx| {
            workspace.on_system_appearance_changed(cx)
        })
        .unwrap();
    wait_for_theme(cx, window, "paper");
}
