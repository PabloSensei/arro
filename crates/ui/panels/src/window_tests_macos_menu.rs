//! The keys the rows of the main menu carry to the menu of the screen of macOS. The menu
//! of the window draws the label of a key; the menu of the screen finds a key of the
//! keymap by the action of a row, so each row hands its key over as the keymap writes it.

use ui_kit::MenuEntry;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::open_sample_window;

fn collect_rows<'a>(
    entries: &'a [MenuEntry],
    prefix: &mut Vec<usize>,
    out: &mut Vec<(Vec<usize>, &'a MenuEntry)>,
) {
    for (index, entry) in entries.iter().enumerate() {
        prefix.push(index);
        if entry.has_submenu() {
            collect_rows(&entry.submenu, prefix, out);
        } else {
            out.push((prefix.clone(), entry));
        }
        prefix.pop();
    }
}

fn row<'a>(rows: &'a [(Vec<usize>, &MenuEntry)], label: &str) -> &'a MenuEntry {
    rows.iter()
        .find(|(_, entry)| entry.label.as_ref() == label)
        .unwrap_or_else(|| panic!("the main menu has {label}"))
        .1
}

/// Every row that shows a key hands the key itself over with the label, and no row hands
/// over a key it does not show: the menu of the screen shows the key it is given.
#[ui_kit::gpui::test]
fn the_main_menu_hands_the_key_of_a_row_to_the_menu_of_the_screen(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "macos-menu-keys");
    let entries = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let mut rows = Vec::new();
    collect_rows(&entries, &mut Vec::new(), &mut rows);

    for (path, entry) in &rows {
        let shown = entry.shortcut.as_deref();
        let handed = entry.keys.as_deref();
        assert_eq!(
            shown.is_some(),
            handed.is_some(),
            "{:?} at {path:?} shows {shown:?} and hands {handed:?} over",
            entry.label
        );
        if let Some(keys) = handed {
            let label = keymap::shortcut_label(keys, keymap::platform());
            assert_eq!(
                shown,
                Some(label.as_str()),
                "{:?} shows a label of another key",
                entry.label
            );
        }
    }

    for (label, keys) in [("Copy", "ctrl-c"), ("Save All", "ctrl-s")] {
        assert_eq!(
            row(&rows, label).keys.as_deref(),
            Some(keys),
            "{label} hands its key over"
        );
    }
    assert!(
        row(&rows, "Find...").keys.is_some(),
        "Find hands a key over too"
    );
    assert!(
        row(&rows, "About Arro").keys.is_none(),
        "a row without a key has nothing to hand over"
    );
}
