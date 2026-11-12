//! The menu bar of the screen on macOS. The window draws no menu bar of its own there, so
//! the same sections become the menus of the application, and the rows keep the paths they
//! have in the menu of the window.
//!
//! The builder compiles on every system, so that the other systems keep checking it; only
//! macOS has a menu bar to put it in.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use ui_kit::{Menu, MenuEntry, MenuItem};

use crate::window::{MenuRow, QuitApp};

/// The menus of the screen: the menu of the application in front of the sections of the
/// window menu, in IDEA's order.
pub fn menus(application: &str, sections: &[MenuEntry]) -> Vec<Menu> {
    let mut menus = Vec::with_capacity(sections.len() + 1);
    menus.push(Menu::new(application).items(vec![MenuItem::action(
        format!("Quit {application}"),
        QuitApp,
    )]));
    for (index, section) in sections.iter().enumerate() {
        menus.push(Menu::new(section.label.clone()).items(items(&section.submenu, &[index])));
    }
    menus
}

/// The rows of one menu, with the path of each through the menu of the window.
fn items(entries: &[MenuEntry], prefix: &[usize]) -> Vec<MenuItem> {
    let mut rows = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(index);
        if entry.label.is_empty() {
            rows.push(MenuItem::separator());
        } else if entry.has_submenu() {
            let menu = Menu::new(entry.label.clone()).items(items(&entry.submenu, &path));
            rows.push(MenuItem::submenu(menu));
        } else {
            let row = MenuRow { path };
            let item = MenuItem::action(entry.label.clone(), row).disabled(!entry.selectable());
            rows.push(item);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(label: &str) -> MenuEntry {
        MenuEntry::new(label, |_: &mut ui_kit::Window, _: &mut ui_kit::App| {})
    }

    /// The menu of the screen names a row by the path it has in the menu of the window, so
    /// that a row chosen on the screen runs what the same row of the window runs.
    #[test]
    fn the_rows_name_their_path_in_the_menu_of_the_window() {
        let sections = vec![
            MenuEntry::submenu(
                "File",
                vec![
                    leaf("Open..."),
                    MenuEntry::separator(),
                    MenuEntry::submenu("Open Recent", vec![leaf("shop")]),
                ],
            ),
            MenuEntry::submenu("Edit", vec![leaf("Copy")]),
        ];

        let menus = menus("Arro", &sections);

        assert_eq!(menus.len(), 3);
        assert_eq!(menus[0].name.as_ref(), "Arro");
        assert!(matches!(&menus[0].items[0], MenuItem::Action { .. }));
        assert_eq!(menus[1].name.as_ref(), "File");
        assert!(matches!(&menus[1].items[1], MenuItem::Separator));
        let MenuItem::Submenu(recent) = &menus[1].items[2] else {
            panic!("Open Recent is a submenu");
        };
        assert_eq!(recent.name.as_ref(), "Open Recent");
        let MenuItem::Action { action, .. } = &recent.items[0] else {
            panic!("the project is a row");
        };
        let row = action
            .as_any()
            .downcast_ref::<MenuRow>()
            .expect("the row carries its path");
        assert_eq!(row.path, [0, 2, 0]);
        assert_eq!(menus[2].name.as_ref(), "Edit");
    }

    /// A row that the window draws grey stays grey in the menu of the screen.
    #[test]
    fn a_disabled_row_of_the_window_is_disabled_on_the_screen() {
        let sections = vec![MenuEntry::submenu(
            "Edit",
            vec![MenuEntry::disabled("Paste"), leaf("Copy")],
        )];

        let menus = menus("Arro", &sections);

        assert!(menus[1].items[0].is_disabled());
        assert!(!menus[1].items[1].is_disabled());
    }
}
