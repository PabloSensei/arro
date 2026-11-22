//! The Appearance page marks the theme the window shows. A window that does not follow
//! the system marks the one theme of its settings; a window that follows the system marks
//! the theme of each system look, as it always has.

use ui_kit::Theme;
use ui_kit::ThemeCatalog;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::settings::SettingsView;
use crate::window_tests::{click, draw, open_sample_window, press};

/// The Settings window on screen as its view, for the state of the page.
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

/// The ids of the theme cards the page marks as chosen, in the order of the list.
fn marked_themes(cx: &TestAppContext, view: &gpui::Entity<SettingsView>) -> Vec<String> {
    view.read_with(cx, |view, cx| {
        let theme = *cx.global::<Theme>();
        ThemeCatalog::library_of(cx)
            .themes()
            .iter()
            .filter(|definition| view.theme_card_chosen(&definition.id, definition.dark, &theme))
            .map(|definition| definition.id.clone())
            .collect::<Vec<String>>()
    })
}

fn settings(cx: &TestAppContext) -> Settings {
    cx.read(|cx| cx.global::<Settings>().clone())
}

#[gpui::test]
fn without_the_sync_the_page_marks_the_chosen_theme(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "theme-choice-hand");
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings| {
                    settings.theme_sync_with_os = false;
                    settings.theme = "mocha".to_owned();
                    settings.theme_light = "light".to_owned();
                    settings.theme_dark = "dark".to_owned();
                },
                cx,
            )
        })
        .unwrap();
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);

    let view = settings_view(cx, window);
    assert_eq!(
        marked_themes(cx, &view),
        ["mocha"],
        "the card of the theme of the window is the only one marked"
    );

    // A click on another card takes it for the window at once.
    click(cx, window, "theme-card-nord");
    draw(cx, window);
    assert_eq!(
        marked_themes(cx, &view),
        ["nord"],
        "the card of the new theme is marked"
    );
    let chosen = settings(cx);
    assert_eq!(chosen.theme, "nord", "the theme of the window changed");
    assert_eq!(chosen.theme_dark, "nord", "the card filled its slot");
    assert!(!chosen.theme_sync_with_os, "the sync stays off");
}

#[gpui::test]
fn with_the_sync_the_page_marks_the_theme_of_each_system_look(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(workspace::SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "theme-choice-sync");
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings| {
                    settings.theme_sync_with_os = true;
                    settings.theme = "mocha".to_owned();
                    settings.theme_light = "paper".to_owned();
                    settings.theme_dark = "nord".to_owned();
                },
                cx,
            )
        })
        .unwrap();
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);

    let view = settings_view(cx, window);
    assert_eq!(
        marked_themes(cx, &view),
        ["nord", "paper"],
        "the two slots of the system are marked, not the theme of an old file"
    );

    // A click on a card fills the slot of its look and leaves the other one alone.
    click(cx, window, "theme-card-light");
    draw(cx, window);
    let chosen = settings(cx);
    assert_eq!(chosen.theme_light, "light", "the light slot changed");
    assert_eq!(chosen.theme_dark, "nord", "the dark slot kept its theme");
    assert_eq!(
        chosen.theme, "mocha",
        "the theme of a window that follows the system is not the card"
    );
    assert!(chosen.theme_sync_with_os, "the sync stays on");
}
