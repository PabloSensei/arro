//! The header of the tool windows: one row of the height of the Project header, the title
//! and the tabs on the left, the Options button and the Hide button on the right. The
//! Services header shows no second title, the tabs of Git sit in its row, and the empty
//! state of Database links to the Data Sources window.

use ui_kit::VisualTestContext;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::metrics::CONTROL_HEIGHT;

use workspace::{
    COMMIT_TOOL, DATABASE_TOOL, DEBUG_TOOL, LOG_TOOL, PROBLEMS_TOOL, PROJECT_TOOL, RUN_TOOL,
    SERVICES_TOOL, SystemAppearance, TODO_TOOL, Workspace,
};

use crate::window_tests::{bounds_of, click, draw, open_sample_window};
use crate::window_tests_database_sources::data_sources_of;

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
