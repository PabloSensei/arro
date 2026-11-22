//! T-0370: the forms of the dialogs share one label column and one field height, and the
//! repository of Settings is a read-only field: the mouse selects its text, Ctrl+C copies it,
//! and typing changes nothing. The pointer is simulated, as in `window_tests_field_mouse`.
//!
//! `cargo test -p panels window_tests_form_columns`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use ui_kit::gpui;
use ui_kit::gpui::{
    Bounds, Modifiers, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::px;
use workspace::Workspace;

use lang_api::{ActionChoice, ActionField, ActionParameters};

use crate::window_tests::{
    bounds_of, click, draw, open_path, open_sample_window, press, type_text,
};
use crate::window_tests_database_sources::{StandIn, open as open_data_sources};
use crate::window_tests_refactor_this::{
    RefactorLanguage, action, open_window as open_refactor_window, project, select_refactoring,
};
use crate::window_tests_run_configs::{open_dialog as open_run_dialog, open_run};

/// Whether the element with `selector` is on the screen.
fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The text the clipboard holds.
fn copied(cx: &mut TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

/// The fields start on one vertical line and have one height.
fn assert_one_column(fields: &[Bounds<Pixels>], what: &str) {
    assert!(
        fields.len() >= 2,
        "{what}: at least two fields are on the screen"
    );
    let first = fields[0];
    for field in &fields[1..] {
        assert_eq!(
            field.left(),
            first.left(),
            "{what} start on one vertical line"
        );
        assert_eq!(
            field.size.height, first.size.height,
            "{what} have one height"
        );
    }
}

#[ui_kit::gpui::test]
fn the_fields_of_data_sources_start_on_one_line_and_have_one_height(cx: &mut TestAppContext) {
    let (window, _panel) = open_data_sources(
        cx,
        "form-columns-data-sources",
        StandIn::support("form-columns-data-sources"),
        Vec::new(),
    );
    click(cx, window, "database-add");
    draw(cx, window);
    let selectors: [&'static str; 8] = [
        "database-field-0",
        "database-field-1",
        "database-field-2",
        "database-field-3",
        "database-field-4",
        "database-field-5",
        "database-field-6",
        "database-field-7",
    ];
    let shown: Vec<&'static str> = selectors
        .into_iter()
        .filter(|selector| on_screen(cx, window, selector))
        .collect();
    let fields: Vec<Bounds<Pixels>> = shown
        .into_iter()
        .map(|selector| bounds_of(cx, window, selector))
        .collect();
    assert_one_column(&fields, "the fields of Data Sources");
}

#[ui_kit::gpui::test]
fn the_fields_of_run_configurations_start_on_one_line_and_have_one_height(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "form-columns-run");
    open_run_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
    let fields = [
        bounds_of(cx, window, "run-config-name"),
        bounds_of(cx, window, "run-config-environment"),
        bounds_of(cx, window, "run-config-field-args"),
    ];
    assert_one_column(&fields, "the fields of Run Configurations");
}

#[ui_kit::gpui::test]
fn the_fields_of_the_rename_form_start_on_one_line_and_have_one_height(cx: &mut TestAppContext) {
    let root = project("form-columns-rename", &[("src/Shape.txt", "shape\n")]);
    let shape = root.join("src").join("Shape.txt");
    let language = RefactorLanguage::new();
    language.set_actions(vec![action(
        "Extract interface...",
        "refactor.extract.interface",
        Some("handle"),
    )]);
    language.set_form(ActionParameters {
        title: "Extract interface".to_owned(),
        members: Vec::new(),
        members_label: "Members".to_owned(),
        fields: vec![
            ActionField {
                key: "name".to_owned(),
                label: "Interface name".to_owned(),
                value: "ShapeView".to_owned(),
            },
            ActionField {
                key: "package".to_owned(),
                label: "Package".to_owned(),
                value: "app".to_owned(),
            },
        ],
        choices: vec![ActionChoice {
            name: "probe".to_owned(),
            detail: Some("/src".to_owned()),
            value: "{\"uri\":\"probe\"}".to_owned(),
        }],
        choices_label: "Destination".to_owned(),
    });
    let window = open_refactor_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &shape);
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    select_refactoring(cx, window, &shape, "Extract Interface...");
    press(cx, window, "enter");
    draw(cx, window);

    let fields = [
        bounds_of(cx, window, "refactor-form-field-0"),
        bounds_of(cx, window, "refactor-form-field-1"),
    ];
    assert_one_column(&fields, "the fields of the Rename form");
}

#[ui_kit::gpui::test]
fn the_repository_in_settings_is_selected_and_copied_but_not_changed(cx: &mut TestAppContext) {
    // The names of the build tools are set before Settings opens: its pages take them then.
    let (first, second) = window_language::build_tool_names();
    crate::settings_catalog::set_build_tool_names(first.clone(), second);
    let (_root, window) = open_sample_window(cx, "form-columns-settings");
    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    // The page of the first build tool is named after the tool.
    let page: &'static str = Box::leak(
        format!("settings-page-{}", first.to_lowercase().replace(' ', "-")).into_boxed_str(),
    );
    assert!(
        on_screen(cx, window, page),
        "the Settings window lists the build tool page"
    );
    click(cx, window, page);
    draw(cx, window);
    // The repository is read off the UI thread: it is shown once the read has finished.
    cx.run_until_parked();
    draw(cx, window);

    let field = bounds_of(cx, window, "settings-repository");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    // A press at the left edge puts the caret before the first character, and a Shift+click at the
    // right edge takes the whole text.
    let start = Point {
        x: field.left() + px(10.),
        y: field.center().y,
    };
    let end = Point {
        x: field.right() - px(10.),
        y: field.center().y,
    };
    visual.simulate_click(start, Modifiers::default());
    visual.simulate_click(
        end,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    cx.run_until_parked();
    press(cx, window, "ctrl-c");
    let selected = copied(cx).unwrap_or_default();
    assert!(
        !selected.is_empty(),
        "the mouse selects the text of the repository"
    );

    // Typing changes nothing: the whole text is still the same after it.
    type_text(cx, window, "xyz");
    draw(cx, window);
    press(cx, window, "ctrl-a");
    press(cx, window, "ctrl-c");
    assert_eq!(
        copied(cx).as_deref(),
        Some(selected.as_str()),
        "typing does not change the repository"
    );
}
