//! Find Action lists every tool window by the name of the window: Enter opens the
//! window itself, not the settings page that carries the same word. A name that matches
//! better comes first, and the scattered letters of longer names stay out of the list
//! while a name matches better.
//!
//! cargo test -p panels window_tests_find_action_ranking

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{
    AI_TOOL, BUILD_TOOL, COMMIT_TOOL, COVERAGE_TOOL, DATABASE_TOOL, DEBUG_TOOL, ENDPOINTS_TOOL,
    HTTP_TOOL, LOG_TOOL, PROBLEMS_TOOL, PROFILER_TOOL, PROJECT_TOOL, RUN_TOOL, SERVICES_TOOL,
    STRUCTURE_TOOL, TERMINAL_TOOL, TODO_TOOL, USAGES_TOOL, Workspace,
};

use crate::palette::Palette;
use crate::window_tests::{draw, open_palette, open_sample_window, press, type_text};

/// The popup that Find Action opened.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

/// Opens Find Action and types `query` into it.
fn find_action(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    query: &str,
) -> gpui::Entity<Palette> {
    open_palette(cx, window, keys::FIND_ACTION);
    type_text(cx, window, query);
    draw(cx, window);
    palette(cx, window)
}

/// The titles of the rows on screen, in the order Find Action lists them.
fn titles(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    palette(cx, window).read_with(cx, |palette, _| {
        palette
            .rows_text()
            .into_iter()
            .map(|(title, _)| title)
            .collect()
    })
}

/// Closes the tool window a row opened, so that the next row opens in a clean dock.
fn close_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            if workspace.tool_open(tool) {
                workspace.toggle_tool(tool, window, cx);
            }
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn todo_lists_the_window_first_and_enter_opens_it(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-todo-window");
    let palette = find_action(cx, window, "TODO");

    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_row()),
        0,
        "Find Action starts on the first row"
    );
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.selected_tool()),
        Some(TODO_TOOL),
        "the TODO window comes before the settings of TODO: {:?}",
        titles(cx, window)
    );
    assert_eq!(titles(cx, window).first().map(String::as_str), Some("TODO"));

    press(cx, window, "enter");
    draw(cx, window);
    let opened = window
        .read_with(cx, |workspace, _| workspace.tool_open(TODO_TOOL))
        .unwrap();
    assert!(opened, "Enter on the TODO row opens the TODO window");
}

#[ui_kit::gpui::test]
fn database_services_and_problems_open_their_windows_from_find_action(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-window-names");

    for (query, title, tool) in [
        ("Database", "Database", DATABASE_TOOL),
        ("Services", "Services", SERVICES_TOOL),
        ("Problems", "Problems", PROBLEMS_TOOL),
    ] {
        find_action(cx, window, query);
        assert_eq!(
            titles(cx, window).first().map(String::as_str),
            Some(title),
            "{query} does not start with its window: {:?}",
            titles(cx, window)
        );

        press(cx, window, "enter");
        draw(cx, window);
        let opened = window
            .read_with(cx, |workspace, _| workspace.tool_open(tool))
            .unwrap();
        assert!(opened, "Enter on {query} opens the {title} window");
        close_tool(cx, window, tool);
    }
}

#[ui_kit::gpui::test]
fn todo_does_not_list_the_scattered_letters_of_longer_names(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-todo-scattered");
    find_action(cx, window, "TODO");
    let titles = titles(cx, window);

    for scattered in ["Toggle rendered doc", "Goto declaration"] {
        assert!(
            !titles.iter().any(|title| title == scattered),
            "{scattered} matches only by its scattered letters: {titles:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn every_tool_window_is_found_by_the_name_of_its_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-every-window");
    for (name, tool) in [
        ("Project", PROJECT_TOOL),
        ("Commit", COMMIT_TOOL),
        ("Git Log", LOG_TOOL),
        ("Structure", STRUCTURE_TOOL),
        ("Run", RUN_TOOL),
        ("Debug", DEBUG_TOOL),
        ("Terminal", TERMINAL_TOOL),
        ("Services", SERVICES_TOOL),
        ("Database", DATABASE_TOOL),
        ("TODO", TODO_TOOL),
        ("Problems", PROBLEMS_TOOL),
        ("Usages", USAGES_TOOL),
        ("AI Sessions", AI_TOOL),
        ("Coverage", COVERAGE_TOOL),
        ("Profiler", PROFILER_TOOL),
        ("HTTP Response", HTTP_TOOL),
        ("Endpoints", ENDPOINTS_TOOL),
        ("Build", BUILD_TOOL),
    ] {
        let palette = find_action(cx, window, name);
        assert_eq!(
            palette.read_with(cx, |palette, _| palette.selected_tool()),
            Some(tool),
            "{name} does not start with its window: {:?}",
            titles(cx, window)
        );
        press(cx, window, "escape");
    }
}

#[ui_kit::gpui::test]
fn a_query_of_scattered_letters_alone_still_finds_a_row(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-scattered-only");
    find_action(cx, window, "tdo");

    assert!(
        !titles(cx, window).is_empty(),
        "a query with no better match than scattered letters still finds rows"
    );
}

#[ui_kit::gpui::test]
fn a_setting_that_spells_the_word_comes_before_a_longer_name(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "find-action-whitespace");
    find_action(cx, window, "whitespace");
    let titles = titles(cx, window);

    assert_eq!(
        titles.first().map(String::as_str),
        Some("Show whitespace"),
        "{titles:?}"
    );
}
