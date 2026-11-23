//! The search of the Settings window follows the tree it keeps. The page of the window stays
//! while the tree keeps it; otherwise the first page of the tree opens. A query without a match
//! says Nothing found, and clearing the query brings the whole tree back on the page last open.
//! Up and Down walk the pages with content of the tree, and a group row opens a part the tree
//! keeps.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::settings::{SettingsView, match_ranges};
use crate::settings_catalog::{self, SettingsPage};
use crate::window_tests::{bounds_of, click, draw, open_sample_window, press, type_text};

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The selector of the row of `page` in the tree, on the row that is the open page.
fn chosen(page: SettingsPage) -> &'static str {
    Box::leak(
        format!(
            "settings-page-chosen-{}",
            page.title().to_lowercase().replace(' ', "-")
        )
        .into_boxed_str(),
    )
}

/// Opens the Settings window on the page of `setting`, as Find Action does.
fn open_settings_on(
    cx: &mut TestAppContext,
    name: &str,
    setting: &'static str,
) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    window
        .update(cx, |workspace, window, cx| {
            SettingsView::show_at(workspace, window, cx, Some(setting));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    bounds_of(cx, window, "settings");
    window
}

#[gpui::test]
fn the_search_opens_the_first_page_of_the_tree_when_the_open_page_is_gone(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-first-page", "terminal_shell");
    assert!(
        has(cx, window, chosen(SettingsPage::Terminal)),
        "the window opens on Terminal"
    );

    type_text(cx, window, "Database");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::Database)),
        "the first page of the tree opens and is chosen in the tree"
    );
    assert!(
        has(cx, window, "settings-database-page-rows-up"),
        "the page of Database shows its settings"
    );
    assert!(
        !has(cx, window, "settings-terminal-shell"),
        "the page of Terminal is no longer drawn"
    );
    assert!(
        !has(cx, window, "settings-page-terminal"),
        "Terminal leaves the tree"
    );
}

#[gpui::test]
fn a_query_the_open_page_keeps_leaves_the_page_open(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-keep", "terminal_shell");

    type_text(cx, window, "shell");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::Terminal)),
        "the open page stays chosen while the tree keeps it"
    );
    assert!(
        has(cx, window, "settings-terminal-shell"),
        "the settings of the open page stay on it"
    );
}

#[gpui::test]
fn a_query_without_a_match_says_nothing_found(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-nothing", "terminal_shell");

    type_text(cx, window, "zzqx");
    draw(cx, window);

    assert!(
        has(cx, window, "settings-nothing-found"),
        "a query without a match says Nothing found"
    );
    assert!(
        !has(cx, window, "settings-terminal-shell"),
        "the page last open is not drawn"
    );
    assert!(
        !has(cx, window, "settings-page-appearance"),
        "the tree is empty"
    );
    assert!(has(cx, window, "settings"), "the window stays open");
}

#[gpui::test]
fn clearing_the_query_brings_the_whole_tree_back_on_the_page_last_open(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-clear", "terminal_shell");
    type_text(cx, window, "Database");
    draw(cx, window);

    press(cx, window, "escape");
    draw(cx, window);

    assert!(
        has(cx, window, "settings-page-appearance"),
        "the whole tree is back"
    );
    assert!(
        has(cx, window, "settings-page-terminal"),
        "Terminal is back in the tree"
    );
    assert!(
        has(cx, window, chosen(SettingsPage::Database)),
        "the page last open stays open"
    );
    assert!(
        has(cx, window, "settings"),
        "the first Escape only clears the search"
    );
}

#[gpui::test]
fn the_arrows_walk_the_pages_of_the_tree_and_cross_its_group_rows(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-arrows-tree", "terminal_shell");
    type_text(cx, window, "terminal");
    draw(cx, window);
    assert!(
        has(cx, window, chosen(SettingsPage::Terminal)),
        "the open page stays while the query keeps it"
    );

    press(cx, window, "down");
    draw(cx, window);
    assert!(
        has(cx, window, chosen(SettingsPage::Mcp)),
        "Down passes the AI group row and moves to the MCP Server page"
    );

    press(cx, window, "up");
    draw(cx, window);
    assert!(
        has(cx, window, chosen(SettingsPage::Terminal)),
        "Up returns to Terminal"
    );

    press(cx, window, "up");
    draw(cx, window);
    assert!(
        has(cx, window, chosen(SettingsPage::BuildToolsSecond)),
        "Up passes the Tools group row and moves to the page before Terminal"
    );
}

#[gpui::test]
fn a_click_on_a_group_row_opens_a_page_of_the_filtered_tree(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-group-click", "terminal_shell");
    type_text(cx, window, "Database");
    draw(cx, window);

    click(cx, window, "settings-page-tools");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::Database)),
        "the Tools row opens the part the tree keeps, not Terminal"
    );
}

#[gpui::test]
fn a_match_on_a_child_of_a_group_opens_the_child_not_the_group(cx: &mut TestAppContext) {
    let window = open_settings_on(cx, "settings-search-child", "font_size");

    type_text(cx, window, "ignored");
    draw(cx, window);

    assert!(
        has(cx, window, chosen(SettingsPage::EditorFileTypes)),
        "the page that holds the match opens"
    );
    assert!(
        !has(cx, window, chosen(SettingsPage::Editor)),
        "the Editor page above it does not open"
    );
    assert!(
        has(cx, window, "settings-page-editor"),
        "Editor stays in the tree as the group of that page"
    );
}

#[test]
fn the_pages_with_content_of_a_query_come_in_the_order_of_the_tree() {
    let terminal = settings_catalog::pages_matching("terminal");
    assert_eq!(
        settings_catalog::shown_pages(&terminal),
        vec![
            SettingsPage::BuildToolsFirst,
            SettingsPage::BuildToolsSecond,
            SettingsPage::Terminal,
            SettingsPage::Mcp,
        ]
    );
    let database = settings_catalog::pages_matching("Database");
    assert_eq!(
        settings_catalog::shown_pages(&database),
        vec![SettingsPage::Database]
    );
    assert!(settings_catalog::pages_matching("zzqx").is_empty());

    let all = settings_catalog::shown_pages(&SettingsPage::ALL);
    for group in [
        SettingsPage::BuildTools,
        SettingsPage::VersionControl,
        SettingsPage::Tools,
        SettingsPage::ToolsAi,
    ] {
        assert!(
            !all.contains(&group),
            "{group:?} is a group row, not a page"
        );
    }
}

#[test]
fn the_words_of_a_query_are_marked_in_order_and_without_overlap() {
    assert_eq!(match_ranges("Shell path", "path shell"), vec![0..5, 6..10]);
    assert_eq!(match_ranges("Data tables", "DATA tab"), vec![0..4, 5..8]);
    assert_eq!(match_ranges("abcxyzabc", "abc cx"), vec![0..4, 6..9]);
    assert!(match_ranges("Shell path", "").is_empty());
    assert!(match_ranges("Shell path", "zzqx").is_empty());
}

#[test]
fn a_search_names_the_page_that_holds_the_match() {
    assert_eq!(
        settings_catalog::matching_pages("ignored"),
        vec![SettingsPage::EditorFileTypes]
    );
}
