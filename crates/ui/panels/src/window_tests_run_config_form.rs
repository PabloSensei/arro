//! The Modify options menu and the titles of the Run/Debug Configurations dialog: an
//! optional field shows when the menu checks it or it holds a value, the choice is stored
//! in the configuration, and the step buttons share the row of the Before launch title.
//!
//! `cargo test -p panels window_tests_run_config`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use runner::{BeforeLaunchStep, RunConfig, configs_file, load_configs, save_configs};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{click, draw, sample_project, type_text};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};
use crate::window_tests_run_configs::{FixtureRun, open_dialog, open_run};

/// The box of the element with `selector` in the last frame, when it is drawn.
fn bounds(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    VisualTestContext::from_window(window.into(), cx).debug_bounds(selector)
}

/// Whether the element with `selector` is drawn in the last frame.
fn drawn(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    bounds(cx, window, selector).is_some()
}

/// A configuration of the substitute provider, as a project file keeps it.
fn stored(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "fixture".into(),
        ..RunConfig::default()
    }
}

/// Opens a project whose configuration file is written before the window opens.
fn open_stored(
    cx: &mut TestAppContext,
    name: &str,
    configs: &[RunConfig],
) -> (PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    save_configs(&configs_file(&root), configs).unwrap();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureRun {
        planned: Arc::new(Mutex::new(Vec::new())),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the stored configurations", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    (root, window)
}

/// The titles of the dialog's sections in the last frame, with their labels and boxes.
fn section_titles() -> Vec<(String, gpui::Bounds<gpui::Pixels>)> {
    ui_kit::test_bounds()
        .into_iter()
        .filter(|(name, _)| name.starts_with("run-config-section-"))
        .map(|(_, record)| {
            let label = record
                .label
                .map(|label| label.to_string())
                .unwrap_or_default();
            (label, record.bounds)
        })
        .collect()
}

/// The box of the title with `label` among the titles of the last frame.
fn title_box(
    titles: &[(String, gpui::Bounds<gpui::Pixels>)],
    label: &str,
) -> gpui::Bounds<gpui::Pixels> {
    titles
        .iter()
        .find(|(title, _)| title.as_str() == label)
        .unwrap_or_else(|| panic!("no title {label} in the form"))
        .1
}

#[ui_kit::gpui::test]
fn the_before_launch_title_holds_its_buttons_and_the_form_has_no_logs(cx: &mut TestAppContext) {
    let mut built = stored("Built");
    built.before_launch = Some(vec![BeforeLaunchStep::Build]);
    let (_root, window) = open_stored(cx, "run-config-title-row", &[built]);
    open_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
    ui_kit::clear_test_bounds();
    draw(cx, window);

    let titles = section_titles();
    let mut labels: Vec<String> = titles.iter().map(|(label, _)| label.clone()).collect();
    labels.sort();
    assert_eq!(
        labels,
        ["Before launch"],
        "the form has the one section and no Logs section"
    );
    let labelled = ui_kit::test_bounds()
        .into_iter()
        .filter(|(_, record)| {
            record
                .label
                .as_ref()
                .is_some_and(|label| label.as_ref() == "Before launch")
        })
        .count();
    assert_eq!(labelled, 1, "the form shows the text Before launch once");
    // The hint of the step buttons is their tooltip: no drawn text of the form carries it.
    let hint_drawn = ui_kit::test_bounds().iter().any(|(_, record)| {
        record
            .label
            .as_ref()
            .is_some_and(|label| label.as_ref() == "Select a step to move or remove it")
    });
    assert!(
        !hint_drawn,
        "the step hint is not a permanent text of the form"
    );

    let before = title_box(&titles, "Before launch");
    for selector in [
        "run-config-step-add",
        "run-config-step-remove",
        "run-config-step-up",
        "run-config-step-down",
    ] {
        let button =
            bounds(cx, window, selector).unwrap_or_else(|| panic!("no {selector} on screen"));
        assert!(
            button.center().y >= before.top() && button.center().y <= before.bottom(),
            "{selector} sits in the row of the Before launch title"
        );
        assert!(
            button.left() > before.left(),
            "{selector} is at the right of the title"
        );
    }
    let step = bounds(cx, window, "run-config-step-0").expect("the Build step is drawn");
    assert!(
        step.top() >= before.bottom(),
        "the steps come under the Before launch title"
    );

    let name = bounds(cx, window, "run-config-name").expect("the name field is drawn");
    let link =
        bounds(cx, window, "run-config-modify-options").expect("the Modify options link is drawn");
    assert!(
        link.top() >= name.bottom(),
        "the Modify options link comes under the name"
    );
    assert!(
        link.left() > name.left(),
        "the Modify options link is at the right of the form"
    );
}

#[ui_kit::gpui::test]
fn the_modify_options_menu_shows_the_fields_it_checks(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "run-config-options-menu");
    open_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-multiple"),
        "an option the menu does not check keeps its field hidden"
    );
    assert!(
        !drawn(cx, window, "run-config-environment"),
        "the environment file is hidden until the menu checks it"
    );
    assert!(
        !drawn(cx, window, "run-config-option-environment_file"),
        "the menu is closed when the dialog opens"
    );

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-option-environment_file"),
        "the menu lists the environment file"
    );
    assert!(
        drawn(cx, window, "run-config-option-allow_multiple_instances"),
        "the menu lists allow multiple instances"
    );

    click(cx, window, "run-config-option-allow_multiple_instances");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-multiple"),
        "a checked option shows its field"
    );
    assert!(
        !drawn(cx, window, "run-config-option-environment_file"),
        "picking an option closes the menu"
    );
    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    assert!(
        drawn(
            cx,
            window,
            "run-config-option-allow_multiple_instances-mark"
        ),
        "the menu row of a shown field is checked"
    );
    assert!(
        !drawn(cx, window, "run-config-environment"),
        "the other option stays hidden"
    );

    click(cx, window, "run-config-option-allow_multiple_instances");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-multiple"),
        "unchecking the option hides its field"
    );
    assert!(
        drawn(cx, window, "run-config-apply-disabled"),
        "checking and unchecking leaves nothing to apply"
    );

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-option-environment_file"),
        "the Modify options link closes the menu"
    );
    assert!(
        drawn(cx, window, "run-configs"),
        "and the dialog stays open"
    );
}

#[ui_kit::gpui::test]
fn a_filled_environment_file_shows_without_the_menu(cx: &mut TestAppContext) {
    let mut filled = stored("Filled");
    filled.environment_file = Some(".env".into());
    let (_root, window) = open_stored(cx, "run-config-filled-env", &[filled]);
    open_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-environment"),
        "a value shows its field without the menu"
    );
    assert!(
        !drawn(cx, window, "run-config-option-environment_file"),
        "the menu is closed"
    );

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-option-environment_file-mark"),
        "the menu checks a field that holds a value"
    );
    click(cx, window, "run-config-option-environment_file");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-environment"),
        "a field that holds a value cannot be hidden"
    );
}

#[ui_kit::gpui::test]
fn the_chosen_field_is_stored_in_the_configuration(cx: &mut TestAppContext) {
    let (root, _planned, window, _panel) = open_run(cx, "run-config-option-stored");
    open_dialog(cx, window);
    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-option-environment_file");
    draw(cx, window);
    click(cx, window, "run-config-environment");
    type_text(cx, window, ".env");
    draw(cx, window);
    click(cx, window, "run-config-ok");
    wait_for(
        cx,
        "the configuration with the chosen field in the file",
        |_| {
            load_configs(&configs_file(&root)).is_ok_and(|configs| {
                configs.iter().any(|config| {
                    config.name == "Echo" && config.shown_options == ["environment_file"]
                })
            })
        },
    );
    let saved = load_configs(&configs_file(&root)).unwrap();
    assert_eq!(
        saved[0].environment_file.as_deref(),
        Some(Path::new(".env"))
    );

    open_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-environment"),
        "the stored choice shows the field on the next open"
    );
    assert!(
        !drawn(cx, window, "run-config-option-environment_file"),
        "the menu stays closed on the next open"
    );
}
