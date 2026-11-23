//! Escape in a dialog closes what is open inside it first: a list or a menu of the dialog,
//! a filled search or a question of Settings, and only then the dialog itself. The Name
//! field of the Run/Debug Configurations dialog keeps its edit while its menu closes. The
//! New Class dialog has no layer of its own, so its first Escape closes it.
//!
//! `cargo test -p panels window_tests_dialog_escape`

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::prompt::{Prompt, PromptKind};
use crate::run_configs::RunConfigDialog;
use crate::window_tests::{click, draw, open_sample_window, press, type_text};
use crate::window_tests_project_structure_select::{wait_for_dialog, window_with_environment};
use crate::window_tests_run_configs::{open_dialog, open_run};
use crate::window_tests_signature::open_form;

/// Whether the element with `selector` is on the screen in the last frame.
fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

/// The text of the Name field of the Run/Debug Configurations dialog on the window.
fn name_of(cx: &TestAppContext, window: WindowHandle<Workspace>) -> String {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()
        .and_then(|view| view.downcast::<RunConfigDialog>().ok())
        .map(|dialog| dialog.read_with(cx, |dialog, _| dialog.name_text().to_owned()))
        .unwrap_or_default()
}

#[ui_kit::gpui::test]
fn escape_closes_the_modify_options_menu_and_keeps_the_dialog(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "dialog-escape-options");
    open_dialog(cx, window);
    click(cx, window, "run-config-name");
    type_text(cx, window, "Edited");
    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    assert!(
        has(cx, window, "run-config-option-environment_file"),
        "the Modify options menu is open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "run-config-option-environment_file"),
        "the first Escape closes the menu"
    );
    assert!(
        has(cx, window, "run-configs"),
        "the first Escape leaves the dialog open"
    );
    assert!(
        name_of(cx, window).ends_with("Edited"),
        "the edit of the Name field stays: {}",
        name_of(cx, window)
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "run-configs"),
        "the second Escape closes the dialog"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_open_dropdown_and_keeps_the_settings_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-escape-dropdown");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-appearance");
    draw(cx, window);
    click(cx, window, "settings-theme-light");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-choice-menu"),
        "the list of the theme is open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "settings-choice-menu"),
        "Escape closes the list"
    );
    assert!(
        has(cx, window, "settings"),
        "Escape leaves the Settings window open"
    );
}

#[ui_kit::gpui::test]
fn escape_clears_the_settings_search_and_then_closes_the_window(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-escape-search");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-search");
    type_text(cx, window, "font");
    draw(cx, window);
    assert!(
        !has(cx, window, "settings-page-project"),
        "the query leaves the pages without a match"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-page-project"),
        "the first Escape clears the query"
    );
    assert!(
        has(cx, window, "settings"),
        "the first Escape leaves the Settings window open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "settings"),
        "the second Escape, with an empty query, closes the window"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_sdk_list_of_project_structure_before_the_dialog(cx: &mut TestAppContext) {
    let (_root, window, _environment) = window_with_environment(cx, "dialog-escape-sdk");
    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);
    click(cx, window, "project-structure-sdk");
    draw(cx, window);
    assert!(
        has(cx, window, "project-structure-sdk-menu"),
        "the SDK list is open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "project-structure-sdk-menu"),
        "the first Escape closes the SDK list"
    );
    assert!(
        has(cx, window, "project-structure"),
        "the first Escape leaves the dialog open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "project-structure"),
        "the second Escape closes the dialog"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_the_visibility_list_of_change_signature_before_the_form(cx: &mut TestAppContext) {
    let (window, _language, _open) = open_form(cx, "dialog-escape-signature");
    click(cx, window, "change-signature-visibility");
    draw(cx, window);
    assert!(
        has(cx, window, "change-signature-visibility-menu"),
        "the visibility list is open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "change-signature-visibility-menu"),
        "the first Escape closes the visibility list"
    );
    assert!(
        has(cx, window, "change-signature"),
        "the first Escape leaves the form open"
    );

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !has(cx, window, "change-signature"),
        "the second Escape closes the form"
    );
}

#[ui_kit::gpui::test]
fn escape_closes_new_class_because_its_kinds_are_no_layer(cx: &mut TestAppContext) {
    let (_root, window) = open_sample_window(cx, "dialog-escape-new-class");
    window
        .update(cx, |workspace, window, cx| {
            Prompt::show_with_kinds(
                "New Class",
                "",
                vec![
                    PromptKind {
                        label: "Class".into(),
                        icon: "general/newFile",
                    },
                    PromptKind {
                        label: "Interface".into(),
                        icon: "general/newFile",
                    },
                ],
                0,
                |_, _, _| {},
                workspace,
                window,
                cx,
            )
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let open = |cx: &TestAppContext| {
        window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap()
    };
    assert!(open(cx), "the New Class dialog is open");

    press(cx, window, "escape");
    draw(cx, window);
    assert!(!open(cx), "Escape closes the dialog");
}
