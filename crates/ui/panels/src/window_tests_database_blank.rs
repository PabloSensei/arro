//! The Database window without a source: the empty square over the table is gone and the
//! icons of the header name the action they run.

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{DATABASE_TOOL, Workspace};

use crate::database_panel::DatabasePanel;
use crate::window_tests::{draw, open_sample_window};
use crate::window_tests_tooltips::assert_tooltip;

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| {
        window.refresh();
        window.draw(cx).clear(cx);
    });
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_empty_database_has_no_unlabelled_checkbox_above_the_table(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "database-blank");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        !shown(cx, window, "database-console-stop"),
        "the Stop square is not drawn while nothing runs"
    );
    assert!(
        !shown(cx, window, "database-result"),
        "an empty window has no table to select rows of"
    );
}

#[gpui::test]
fn the_icons_of_the_database_header_name_their_action(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "database-tooltips");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    assert_tooltip(cx, window, "database-add", "Data Source", None);
    assert_tooltip(cx, window, "database-refresh", "Refresh Schema", None);
    let copy_cell = DatabasePanel::copy_cell_key();
    assert_tooltip(
        cx,
        window,
        "database-copy-cell",
        "Copy Cell",
        Some(copy_cell.as_str()),
    );
    assert_tooltip(cx, window, "database-copy-row", "Copy Row", None);
}
