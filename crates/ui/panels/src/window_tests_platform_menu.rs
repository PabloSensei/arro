//! The menu bar of the screen of macOS: the menu of the application, the rows the system
//! fills in and the rows that must not appear twice. The CI of macOS runs these; the
//! other systems have no menu bar of the screen.

#![cfg(target_os = "macos")]

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::{Menu, MenuItem, SystemMenuType};

use workspace::ScreenMenus;

use crate::window_tests::open_sample_window;

fn labels(items: &[MenuItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            MenuItem::Action { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect()
}

fn menu<'a>(menus: &'a ScreenMenus, name: &str) -> &'a Menu {
    menus
        .menus
        .iter()
        .find(|menu| menu.name.as_ref() == name)
        .unwrap_or_else(|| panic!("the menu bar has {name}"))
}

#[ui_kit::gpui::test]
fn the_menu_bar_has_the_application_menu_of_a_native_program(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-menu-application");
    let menus = window
        .update(cx, |workspace, _, cx| workspace.screen_menus(cx))
        .unwrap();

    let application = &menus.menus[0];
    assert_eq!(application.name.as_ref(), "Arro");
    let names = labels(&application.items);
    for label in [
        "About Arro",
        "Settings…",
        "Check for Updates…",
        "Hide Arro",
        "Hide Others",
        "Show All",
        "Quit Arro",
    ] {
        assert!(
            names.iter().any(|name| name == label),
            "{label} is missing from {names:?}"
        );
    }
    assert!(
        application
            .items
            .iter()
            .any(|item| matches!(item, MenuItem::SystemMenu(menu)
                if menu.menu_type == SystemMenuType::Services)),
        "Services is the menu of the system"
    );
}

#[ui_kit::gpui::test]
fn the_rows_of_the_application_menu_do_not_repeat_in_file_and_help(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-menu-duplicates");
    let menus = window
        .update(cx, |workspace, _, cx| workspace.screen_menus(cx))
        .unwrap();

    let file = labels(&menu(&menus, "File").items);
    for label in ["Settings...", "Exit"] {
        assert!(
            !file.iter().any(|name| name == label),
            "File repeats {label}: {file:?}"
        );
    }
    let help = labels(&menu(&menus, "Help").items);
    for label in ["About Arro", "Check for Updates…"] {
        assert!(
            !help.iter().any(|name| name == label),
            "Help repeats {label}: {help:?}"
        );
    }
    assert!(
        help.iter().any(|name| name == "Support Arro…"),
        "the rest of Help stays: {help:?}"
    );
}

#[ui_kit::gpui::test]
fn the_window_menu_ends_with_the_rows_of_the_system(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "platform-menu-window");
    let menus = window
        .update(cx, |workspace, _, cx| workspace.screen_menus(cx))
        .unwrap();

    let names = labels(&menu(&menus, "Window").items);
    assert!(
        names.len() >= 3,
        "the window menu has its own rows first: {names:?}"
    );
    assert_eq!(
        &names[names.len() - 3..],
        ["Minimize", "Zoom", "Bring All to Front"]
    );
}
