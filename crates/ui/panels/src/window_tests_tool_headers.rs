//! The header of the tool windows: one row of the height of the Project header, the title
//! and the tabs on the left, the Options button and the Hide button on the right. The
//! Services header shows no second title, the tabs of Git and Commit sit in its row, the
//! empty states link to their actions, and Move to keeps the dock of a window with the
//! layout of the project.

use std::path::Path;

use ui_kit::MenuAction;
use ui_kit::VisualTestContext;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::metrics::CONTROL_HEIGHT;

use workspace::{
    BEANS_TOOL, COMMIT_TOOL, COVERAGE_TOOL, DATABASE_TOOL, DEBUG_TOOL, DockSide, ENDPOINTS_TOOL,
    LOG_TOOL, PERFORMANCE_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL, SELECTION_HISTORY_TOOL,
    SERVICES_TOOL, SystemAppearance, TODO_TOOL, Workspace, load_layout,
};

use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, open_sample_window, press, with_window,
};
use crate::window_tests_database_sources::data_sources_of;
use crate::window_tests_nested_splits::open_saved_window;

/// The tolerance of a measured height: layout rounds logical pixels to device pixels.
const TOLERANCE: f32 = 0.5;

/// The debug selectors of one tool window header: the row, the container of its tools that
/// show under the pointer and the Options button.
struct Header {
    row: &'static str,
    hide: &'static str,
    options: &'static str,
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn show_tool(cx: &mut TestAppContext, window: WindowHandle<Workspace>, tool: &'static str) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(tool, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

/// One header: the height of the Project header, the Options and Hide buttons inside the
/// row, Hide at the right end and Options left of it.
fn check_header(cx: &mut TestAppContext, window: WindowHandle<Workspace>, header: &Header) {
    draw(cx, window);
    let row = bounds_of(cx, window, header.row);
    let height = f32::from(row.size.height);
    assert!(
        (height - CONTROL_HEIGHT).abs() <= TOLERANCE,
        "{} is {height} high, the Project header is {CONTROL_HEIGHT}",
        header.row
    );

    let hide = bounds_of(cx, window, header.hide);
    let options = bounds_of(cx, window, header.options);
    assert!(
        row.contains(&hide.center()) && row.contains(&options.center()),
        "the buttons of {} sit in its row: row {row:?}, hide {hide:?}, options {options:?}",
        header.row
    );
    assert!(
        hide.center().x > row.center().x,
        "Hide of {} is on the right",
        header.row
    );
    assert!(
        options.center().x < hide.center().x,
        "Options of {} is left of Hide",
        header.row
    );
}

#[ui_kit::gpui::test]
fn eight_tool_windows_share_one_header_row_with_options_and_hide(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "tool-headers");
    let windows = [
        (
            PROJECT_TOOL,
            Header {
                row: "project-header",
                hide: "project-hide",
                options: "project-options",
            },
        ),
        (
            RUN_TOOL,
            Header {
                row: "run-header",
                hide: "run-hide",
                options: "run-options",
            },
        ),
        (
            DEBUG_TOOL,
            Header {
                row: "debug-header",
                hide: "debug-hide",
                options: "debug-options",
            },
        ),
        (
            COMMIT_TOOL,
            Header {
                row: "commit-header",
                hide: "commit-hide",
                options: "commit-options",
            },
        ),
        (
            LOG_TOOL,
            Header {
                row: "log-header",
                hide: "log-hide",
                options: "log-options",
            },
        ),
        (
            PROBLEMS_TOOL,
            Header {
                row: "problems-header",
                hide: "problems-hide",
                options: "problems-options",
            },
        ),
        (
            TODO_TOOL,
            Header {
                row: "todo-header",
                hide: "todo-hide",
                options: "todo-options",
            },
        ),
        (
            SERVICES_TOOL,
            Header {
                row: "services-header",
                hide: "services-hide",
                options: "services-options",
            },
        ),
    ];
    for (tool, header) in &windows {
        show_tool(cx, window, tool);
        check_header(cx, window, header);
    }
}

#[ui_kit::gpui::test]
fn the_tabs_of_git_sit_in_the_header_row(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "tool-headers-git");
    show_tool(cx, window, LOG_TOOL);
    let row = bounds_of(cx, window, "log-header");
    for tab in ["git-tab-local", "git-tab-log", "git-tab-console"] {
        let bounds = bounds_of(cx, window, tab);
        assert!(
            row.contains(&bounds.center()),
            "the tab {tab} sits in the header row"
        );
    }
}

#[ui_kit::gpui::test]
fn services_has_no_second_title_when_nothing_is_selected(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "tool-headers-services");
    show_tool(cx, window, SERVICES_TOOL);
    assert!(has(cx, window, "services-header"), "the Services header");
    assert!(
        !has(cx, window, "docker-title"),
        "the Services window shows no title of its own inside its header"
    );
}

#[ui_kit::gpui::test]
fn the_database_header_is_one_row_and_its_empty_state_opens_data_sources(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "tool-headers-database");
    let header = Header {
        row: "database-header",
        hide: "database-hide",
        options: "database-options",
    };
    show_tool(cx, window, DATABASE_TOOL);
    check_header(cx, window, &header);

    assert!(
        has(cx, window, "database-empty-create"),
        "the empty list of Database has its action"
    );
    assert!(
        data_sources_of(cx, window).is_none(),
        "Data Sources is closed at first"
    );
    click(cx, window, "database-empty-create");
    draw(cx, window);
    assert!(
        data_sources_of(cx, window).is_some(),
        "the action of the empty list opens Data Sources"
    );
}

/// A saved version of the sample file, so that Local History has a window to draw.
fn save_a_version(cx: &mut TestAppContext, window: WindowHandle<Workspace>, root: &Path) {
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "one", cx);
        let error = view.save_blocking(cx);
        assert!(error.is_none(), "{error:?}");
    });
    cx.run_until_parked();
    window
        .update(cx, |workspace, window, cx| {
            crate::history_view::HistoryView::show(path, workspace, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn coverage_endpoints_and_local_history_share_the_header_row(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (root, window) = open_sample_window(cx, "tool-headers-rest");

    show_tool(cx, window, COVERAGE_TOOL);
    check_header(
        cx,
        window,
        &Header {
            row: "coverage-header",
            hide: "coverage-hide",
            options: "coverage-options",
        },
    );
    assert!(
        has(cx, window, "coverage-empty-run"),
        "the empty Coverage window links to Run with Coverage"
    );

    show_tool(cx, window, ENDPOINTS_TOOL);
    check_header(
        cx,
        window,
        &Header {
            row: "endpoints-header",
            hide: "endpoints-hide",
            options: "endpoints-options",
        },
    );
    assert!(
        has(cx, window, "endpoints-header"),
        "the Endpoints header is on screen"
    );

    show_tool(cx, window, BEANS_TOOL);
    check_header(
        cx,
        window,
        &Header {
            row: "beans-header",
            hide: "beans-hide",
            options: "beans-options",
        },
    );

    show_tool(cx, window, PERFORMANCE_TOOL);
    check_header(
        cx,
        window,
        &Header {
            row: "performance-header",
            hide: "performance-hide",
            options: "performance-options",
        },
    );

    show_tool(cx, window, SELECTION_HISTORY_TOOL);
    check_header(
        cx,
        window,
        &Header {
            row: "selection-history-header",
            hide: "selection-history-hide",
            options: "selection-history-options",
        },
    );

    save_a_version(cx, window, &root);
    check_header(
        cx,
        window,
        &Header {
            row: "history-header",
            hide: "history-hide",
            options: "history-options",
        },
    );
}

#[ui_kit::gpui::test]
fn the_tabs_of_commit_sit_in_the_header_row(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "tool-headers-commit-tabs");
    show_tool(cx, window, COMMIT_TOOL);
    let row = bounds_of(cx, window, "commit-header");
    for tab in ["commit-tab", "shelf-tab"] {
        let bounds = bounds_of(cx, window, tab);
        assert!(
            row.contains(&bounds.center()),
            "the tab {tab} sits in the header row"
        );
    }
    check_header(
        cx,
        window,
        &Header {
            row: "commit-header",
            hide: "commit-hide",
            options: "commit-options",
        },
    );
}

#[ui_kit::gpui::test]
fn the_empty_run_window_opens_run_debug_configurations(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "tool-headers-run-empty");
    show_tool(cx, window, RUN_TOOL);
    assert!(
        has(cx, window, "run-empty"),
        "a Run window with nothing to run says so"
    );
    assert!(
        has(cx, window, "run-empty-configs"),
        "the empty Run window links to the configurations"
    );
    click(cx, window, "run-empty-configs");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("run-configs").is_some(),
        "the link opens Run/Debug Configurations"
    );
}

#[ui_kit::gpui::test]
fn the_empty_debug_window_offers_the_configurations(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (_, window) = open_sample_window(cx, "tool-headers-debug-empty");
    show_tool(cx, window, DEBUG_TOOL);
    assert!(
        has(cx, window, "debug-empty"),
        "a Debug window without a session says so"
    );
    assert!(
        has(cx, window, "debug-empty-debug"),
        "the empty Debug window links to the configuration chooser"
    );
    click(cx, window, "debug-empty-debug");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("palette").is_some(),
        "the link opens the chooser of the debug configurations"
    );
}

/// The action of the row of the open menu, found by the label of the row and of its
/// submenu row.
fn menu_action(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    section: &str,
    row: &str,
) -> (Vec<String>, bool, MenuAction) {
    window
        .read_with(cx, |workspace, _| {
            let entries = workspace.menu_entries().expect("the menu is open");
            let section = entries
                .iter()
                .find(|entry| entry.label == section)
                .expect("the menu has the section");
            let labels = section
                .submenu
                .iter()
                .map(|entry| entry.label.to_string())
                .collect();
            let entry = section
                .submenu
                .iter()
                .find(|entry| entry.label == row)
                .expect("the submenu has the row");
            let action = entry.on_click.clone().expect("the row acts");
            (labels, entry.enabled, action)
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn move_to_right_moves_project_and_the_layout_keeps_it_there(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(SystemAppearance { dark: true }));
    let (root, window) = open_sample_window(cx, "tool-headers-move");
    show_tool(cx, window, PROJECT_TOOL);
    click(cx, window, "project-options");
    draw(cx, window);

    let (labels, right_enabled, right) = menu_action(cx, window, "Move to", "Right");
    assert_eq!(labels, ["Left", "Right", "Bottom"]);
    assert!(right_enabled, "Project is on the left, so Right applies");
    with_window(cx, window, |window, cx| right(window, cx));
    press(cx, window, "escape");
    draw(cx, window);

    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.tool_side(PROJECT_TOOL))
            .unwrap(),
        Some(DockSide::Right),
        "Move to Right puts Project in the right dock"
    );
    let viewport = window
        .update(cx, |_, window, _| window.viewport_size())
        .unwrap();
    let header = bounds_of(cx, window, "project-header");
    assert!(
        header.center().x > viewport.width / 2.,
        "the Project header is on the right: {:?} of {}",
        header,
        viewport.width
    );

    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let store = window
        .read_with(cx, |workspace, _| workspace.store())
        .unwrap()
        .expect("the test window has a store");
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let layout = load_layout(&store, &root);
    assert_eq!(
        layout.tool_side(PROJECT_TOOL),
        Some(DockSide::Right),
        "the saved layout keeps the dock of Project"
    );
    let second = open_saved_window(cx, &root, &state_dir, layout, Some(store));
    draw(cx, second);
    show_tool(cx, second, PROJECT_TOOL);
    assert_eq!(
        second
            .read_with(cx, |workspace, _| workspace.tool_side(PROJECT_TOOL))
            .unwrap(),
        Some(DockSide::Right),
        "a new window opens Project where the layout has it"
    );
}
