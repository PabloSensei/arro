//! The last polish of the tool windows: the two copy buttons of Database show two icons and
//! do nothing without a selected cell, Build says once that it has no build, and the count
//! of Endpoints names what it counts.
//!
//! `cargo test -p panels window_tests_tool_polish`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use ui_kit::gpui;
use ui_kit::gpui::{ClipboardItem, TestAppContext};
use workspace::{BUILD_TOOL, DATABASE_TOOL};

use crate::database_panel::DatabasePanel;
use crate::endpoints::ActiveEndpoints;
use crate::window_tests::{click, draw, open_sample_window, open_window, sample_project};
use crate::window_tests_endpoints::open as open_endpoints;
use crate::window_tests_icons::{assert_icon_buttons, icon_path};

/// The text the probe with the name shows in the last frame, if it was drawn.
fn probe_label(name: &str) -> Option<String> {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(selector, _)| selector == name)
        .and_then(|(_, bounds)| bounds.label)
        .map(|label| label.to_string())
}

#[ui_kit::gpui::test]
fn the_two_copy_buttons_of_database_differ_and_do_nothing_without_a_cell(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "tool-polish-database");
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(DATABASE_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);

    let copy_cell = DatabasePanel::copy_cell_key();
    assert_icon_buttons(
        cx,
        window,
        &[
            (
                "database-copy-cell",
                "general/copy",
                "Copy Cell",
                Some(copy_cell.as_str()),
                &[],
            ),
            (
                "database-copy-row",
                "general/editorAndPreview",
                "Copy Row",
                None,
                &[],
            ),
        ],
    );
    assert_ne!(
        icon_path(cx, "general/copy"),
        icon_path(cx, "general/editorAndPreview"),
        "the two copy buttons do not show the same icon"
    );

    // No result is open and no row is selected: the buttons copy nothing.
    let before = ClipboardItem::new_string("before".to_owned());
    cx.write_to_clipboard(before);
    click(cx, window, "database-copy-cell");
    click(cx, window, "database-copy-row");
    draw(cx, window);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("before".to_owned()),
        "a copy button of an empty window changes nothing on the clipboard"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn an_empty_build_says_it_once_and_names_the_build_key(cx: &mut TestAppContext) {
    let root = sample_project("tool-polish-build");
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(BUILD_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    ui_kit::clear_test_bounds();
    draw(cx, window);

    let panel = cx.read(|cx| {
        cx.global::<crate::build_output::ActiveBuildOutput>()
            .0
            .clone()
    });
    let mut texts = panel.read_with(cx, |panel, cx| panel.labels(cx));
    texts.extend(
        panel
            .read_with(cx, |panel, cx| panel.hints(cx))
            .into_iter()
            .map(|(_, hint)| hint),
    );
    texts.extend(
        ui_kit::test_bounds()
            .into_iter()
            .filter_map(|(_, bounds)| bounds.label)
            .map(|label| label.to_string()),
    );
    let said = texts
        .iter()
        .map(|text| text.to_lowercase().matches("no builds yet").count())
        .sum::<usize>();
    assert_eq!(said, 1, "the window says it once: {texts:?}");
    assert!(
        texts
            .iter()
            .any(|text| text.contains("No builds yet.") && text.contains("Ctrl+F9")),
        "the one line names the build key: {texts:?}"
    );
}

#[ui_kit::gpui::test]
fn the_count_of_endpoints_names_what_it_counts(cx: &mut TestAppContext) {
    let (window, _root) = open_endpoints(cx, "tool-polish-endpoints");
    ui_kit::clear_test_bounds();
    draw(cx, window);
    let label = probe_label("endpoints-count").expect("the header shows its count");
    assert!(label.ends_with("endpoints"), "{label}");
    assert!(
        !label.contains(" of "),
        "no filter shows one number: {label}"
    );

    window
        .update(cx, |_, _, cx| {
            let panel = cx.global::<ActiveEndpoints>().0.clone();
            panel.update(cx, |panel, cx| panel.set_query("vets", cx));
        })
        .unwrap();
    cx.run_until_parked();
    ui_kit::clear_test_bounds();
    draw(cx, window);
    let label = probe_label("endpoints-count").expect("the header shows its count");
    assert!(label.contains("endpoints"), "{label}");
    assert_eq!(label, "1 of 3 endpoints", "{label}");
}
