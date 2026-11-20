//! View > Tool Windows lists Beans right after Endpoints, with its hint and icon, and the row
//! opens the Beans window.

use ui_kit::MenuEntry;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::BEANS_TOOL;

use crate::window_tests::open_sample_window;
use crate::window_tests_menu_main::{click_row, open_parents, open_section, path_to, sections};

/// The row at a path of indexes, from the top section down.
fn entry_at<'a>(entries: &'a [MenuEntry], path: &[usize]) -> &'a MenuEntry {
    let (first, rest) = path.split_first().expect("the path has a row");
    let entry = &entries[*first];
    if rest.is_empty() {
        entry
    } else {
        entry_at(&entry.submenu, rest)
    }
}

#[ui_kit::gpui::test]
fn tool_windows_has_beans_after_endpoints_and_the_row_opens_it(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "beans-menu");
    let entries = sections(cx, window);
    let beans = path_to(&entries, &["View", "Tool Windows", "Beans"]);
    let endpoints = path_to(&entries, &["View", "Tool Windows", "Endpoints"]);
    let (beans_row, endpoints_row) = (*beans.last().unwrap(), *endpoints.last().unwrap());
    assert_eq!(
        beans_row,
        endpoints_row + 1,
        "Beans is the row right after Endpoints"
    );

    let row = entry_at(&entries, &beans);
    assert_eq!(row.icon, Some("nodes/class"), "Beans has its window icon");
    assert!(row.tip.is_some(), "Beans has a hint");

    open_section(cx, window, beans[0]);
    let beans = &beans[1..];
    open_parents(cx, window, beans);
    click_row(cx, window, beans);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(BEANS_TOOL))
            .unwrap(),
        "View > Tool Windows > Beans did not open the Beans window"
    );
}
