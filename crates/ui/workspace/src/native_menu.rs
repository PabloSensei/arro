//! The menu bar of the screen on macOS. The window draws no menu bar of its own there, so
//! the same sections become the menus of the application, and the rows keep the paths they
//! have in the menu of the window.
//!
//! The builder compiles on every system, so that the other systems keep checking it; only
//! macOS has a menu bar to put it in.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use ui_kit::{Menu, MenuEntry, MenuItem};

use crate::window::{MenuRow, QuitApp};

/// The menus of the screen and the rows of them that have a key of their own.
pub struct ScreenMenus {
    pub menus: Vec<Menu>,
    /// One row for every row that has a key, with the key it has. GPUI shows a key beside
    /// a row only when the action of the row has a binding in the keymap, so each of these
    /// rows gets a binding of its own.
    pub hints: Vec<MenuRow>,
}

/// The menus of the screen: the menu of the application in front of the sections of the
/// window menu, in IDEA's order.
pub fn menus(application: &str, sections: &[MenuEntry]) -> ScreenMenus {
    let mut hints = Vec::new();
    let mut menus = Vec::with_capacity(sections.len() + 1);
    menus.push(Menu::new(application).items(vec![MenuItem::action(
        format!("Quit {application}"),
        QuitApp,
    )]));
    for (index, section) in sections.iter().enumerate() {
        let rows = items(&section.submenu, &[index], &mut hints);
        menus.push(Menu::new(section.label.clone()).items(rows));
    }
    ScreenMenus { menus, hints }
}

/// The rows of one menu, with the path of each through the menu of the window.
fn items(entries: &[MenuEntry], prefix: &[usize], hints: &mut Vec<MenuRow>) -> Vec<MenuItem> {
    let mut rows = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let mut path = prefix.to_vec();
        path.push(index);
        if entry.label.is_empty() {
            rows.push(MenuItem::separator());
        } else if entry.has_submenu() {
            let rows_of_submenu = items(&entry.submenu, &path, hints);
            let menu = Menu::new(entry.label.clone()).items(rows_of_submenu);
            rows.push(MenuItem::submenu(menu));
        } else {
            let row = MenuRow {
                path,
                key: entry.keys.clone(),
            };
            if row.key.is_some() {
                hints.push(row.clone());
            }
            let item = MenuItem::action(entry.label.clone(), row).disabled(!entry.selectable());
            rows.push(item);
        }
    }
    rows
}

/// The context of the bindings that only make GPUI show a key. No focus of the window has
/// this context, so such a binding never runs its action: the key goes to the window as
/// before, and only a window that leaves the key alone lets the menu run the row, once.
#[cfg(target_os = "macos")]
const HINT_CONTEXT: &str = "NativeMenuHint";

/// Adds the hints of the rows to the keymap of the application, before the menus are set:
/// the platform reads the keymap when it builds the menu, and a key it finds becomes the
/// key equivalent of the row. A key GPUI cannot parse, a double press of a modifier for
/// one, has no key equivalent and so no hint.
#[cfg(target_os = "macos")]
pub fn add_hints(cx: &mut ui_kit::App, hints: &[MenuRow]) {
    use std::rc::Rc;

    use ui_kit::{KeyBinding, KeyBindingContextPredicate};

    let Ok(predicate) = KeyBindingContextPredicate::parse(HINT_CONTEXT) else {
        return;
    };
    let predicate = Rc::new(predicate);
    let keymap = cx.key_bindings();
    let mut missing = Vec::new();
    {
        let keymap = keymap.borrow();
        for row in hints {
            let Some(key) = row.key.as_ref() else {
                continue;
            };
            if keymap
                .bindings()
                .any(|bound| bound.action().partial_eq(row))
            {
                continue;
            }
            let action = Box::new(row.clone());
            if let Ok(binding) =
                KeyBinding::load(key, action, Some(predicate.clone()), false, None, cx)
            {
                missing.push(binding);
            }
        }
    }
    if !missing.is_empty() {
        cx.bind_keys(missing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(label: &str) -> MenuEntry {
        MenuEntry::new(label, |_: &mut ui_kit::Window, _: &mut ui_kit::App| {})
    }

    /// A row whose key the window menu shows as `label`, `Ctrl+C` for `ctrl-c`.
    fn keyed(label: &str, keys: &str) -> MenuEntry {
        leaf(label)
            .shortcut(keymap::shortcut_label(keys, keymap::platform()))
            .keys(keys)
    }

    fn row_of(item: &MenuItem) -> &MenuRow {
        let MenuItem::Action { action, .. } = item else {
            panic!("the row is an action");
        };
        action
            .as_any()
            .downcast_ref::<MenuRow>()
            .expect("the row carries its path")
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

        let screen = menus("Arro", &sections);
        let menus = &screen.menus;

        assert_eq!(menus.len(), 3);
        assert_eq!(menus[0].name.as_ref(), "Arro");
        assert!(matches!(&menus[0].items[0], MenuItem::Action { .. }));
        assert_eq!(menus[1].name.as_ref(), "File");
        assert!(matches!(&menus[1].items[1], MenuItem::Separator));
        let MenuItem::Submenu(recent) = &menus[1].items[2] else {
            panic!("Open Recent is a submenu");
        };
        assert_eq!(recent.name.as_ref(), "Open Recent");
        assert_eq!(row_of(&recent.items[0]).path, [0, 2, 0]);
        assert_eq!(menus[2].name.as_ref(), "Edit");
    }

    /// A row that the window draws grey stays grey in the menu of the screen.
    #[test]
    fn a_disabled_row_of_the_window_is_disabled_on_the_screen() {
        let sections = vec![MenuEntry::submenu(
            "Edit",
            vec![MenuEntry::disabled("Paste"), leaf("Copy")],
        )];

        let screen = menus("Arro", &sections);

        assert!(screen.menus[1].items[0].is_disabled());
        assert!(!screen.menus[1].items[1].is_disabled());
    }

    /// The menu of the screen shows a key beside a row only when the row carries the key,
    /// so every row with a key of the window menu hands it over, and the other rows do
    /// not: Save All, Copy and Find have a key, Open and Paste have none.
    #[test]
    fn a_row_with_a_key_of_the_window_menu_carries_it_to_the_screen() {
        let sections = vec![
            MenuEntry::submenu("File", vec![keyed("Save All", "ctrl-s"), leaf("Open...")]),
            MenuEntry::submenu(
                "Edit",
                vec![
                    keyed("Copy", "ctrl-c"),
                    keyed("Find...", "ctrl-f"),
                    leaf("Paste"),
                ],
            ),
        ];

        let screen = menus("Arro", &sections);

        let hints: Vec<(&[usize], &str)> = screen
            .hints
            .iter()
            .map(|row| {
                (
                    row.path.as_slice(),
                    row.key.as_deref().expect("a hint has a key"),
                )
            })
            .collect();
        assert_eq!(
            hints,
            [
                ([0, 0].as_slice(), "ctrl-s"),
                ([1, 0].as_slice(), "ctrl-c"),
                ([1, 1].as_slice(), "ctrl-f"),
            ]
        );
        assert_eq!(
            row_of(&screen.menus[2].items[0]).key.as_deref(),
            Some("ctrl-c")
        );
        assert_eq!(
            row_of(&screen.menus[2].items[2]).key.as_deref(),
            None,
            "a row without a key of the window menu has no key on the screen"
        );
    }

    /// The hint of a row names the row itself, or GPUI shows the key of another row.
    #[test]
    fn a_hint_is_the_action_of_its_own_row() {
        let sections = vec![MenuEntry::submenu("Edit", vec![keyed("Copy", "ctrl-c")])];

        let screen = menus("Arro", &sections);

        let row = row_of(&screen.menus[1].items[0]);
        assert_eq!(row.path, [0, 0]);
        assert_eq!(row.key.as_deref(), Some("ctrl-c"));
        assert_eq!(&screen.hints[0], row);
    }
}
