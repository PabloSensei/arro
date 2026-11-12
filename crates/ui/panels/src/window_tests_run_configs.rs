//! The Run/Debug Configurations dialog without a real window and with a substitute
//! provider: the list of configurations, the fields it edits and the file it writes.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{
    ConfigField, ConfigTemplate, RunConfig, RunConfigProvider, RunPlan, configs_file, load_configs,
    split_arguments,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{draw, sample_project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The key of the only field of the substitute provider.
const ARGS: &str = "args";

/// A provider whose program prints its arguments, so a test sees what a run got.
struct FixtureRun {
    planned: Arc<Mutex<Vec<RunConfig>>>,
}

impl RunConfigProvider for FixtureRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![config("Echo")])
    }

    fn plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        self.planned.lock().unwrap().push(config.clone());
        let arguments = split_arguments(&read_args(config));
        let program = if arguments.is_empty() {
            ProcessSpec::new("cmd").args(["/d", "/c", "echo", "no arguments"])
        } else {
            ProcessSpec::new("cmd")
                .args(["/d", "/c", "echo"])
                .args(arguments)
        };
        Ok(RunPlan {
            before: None,
            program,
            grace: Duration::from_secs(1),
        })
    }

    fn fields(&self, _: &RunConfig) -> Vec<ConfigField> {
        vec![ConfigField::arguments(ARGS, "Program arguments")]
    }

    fn read_field(&self, config: &RunConfig, _: &str) -> String {
        read_args(config)
    }

    fn write_field(&self, config: &mut RunConfig, _: &str, value: &str) {
        config.settings.insert(ARGS.into(), value.to_owned().into());
    }

    fn templates(&self) -> Vec<ConfigTemplate> {
        vec![ConfigTemplate {
            id: "application",
            name: "Application",
        }]
    }

    fn create_config(&self, template: &str, _: &ProjectModel) -> base::Result<RunConfig> {
        if template != "application" {
            return Err("unknown template".into());
        }
        Ok(config("Unnamed"))
    }
}

fn config(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "fixture".into(),
        assemble_first: false,
        settings: Default::default(),
    }
}

fn read_args(config: &RunConfig) -> String {
    config
        .settings
        .get(ARGS)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The planned configurations of a test and the window that runs them.
type Planned = Arc<Mutex<Vec<RunConfig>>>;

fn open_run(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    PathBuf,
    Planned,
    WindowHandle<Workspace>,
    gpui::Entity<RunPanel>,
) {
    let root = sample_project(name);
    let planned = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureRun {
        planned: planned.clone(),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    (root, planned, window, panel)
}

fn configuration_names(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .state()
            .configs
            .iter()
            .map(ToString::to_string)
            .collect()
    })
}

fn bounds(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &str,
) -> Option<gpui::Bounds<gpui::Pixels>> {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    VisualTestContext::from_window(window.into(), cx).debug_bounds(selector)
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.menu_entries().is_some_and(|entries| {
                    entries.iter().any(|entry| entry.label.as_ref() == label)
                })
            })
            .unwrap()
    });
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    click_selector(cx, window, &format!("menu-{index}"));
}

fn open_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "t-config");
    click_menu(cx, window, "Edit Configurations...");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-configs").is_some(),
        "the configuration dialog is on screen"
    );
}

fn run_arguments(planned: &Arc<Mutex<Vec<RunConfig>>>) -> Vec<Vec<String>> {
    planned
        .lock()
        .unwrap()
        .iter()
        .map(|config| split_arguments(&read_args(config)))
        .collect()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = crate::window_tests::bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn the_configuration_menu_opens_the_dialog(cx: &mut TestAppContext) {
    let (_root, _planned, window, _panel) = open_run(cx, "run-configs-open");

    open_dialog(cx, window);

    assert!(
        bounds(cx, window, "run-config-item-0").is_some(),
        "the list shows the found configuration"
    );
    assert!(
        bounds(cx, window, "run-config-field-args").is_some(),
        "the fields of the provider are on the right"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn arguments_typed_in_the_dialog_reach_the_file_and_the_next_run(cx: &mut TestAppContext) {
    let (root, planned, window, panel) = open_run(cx, "run-configs-args");
    open_dialog(cx, window);

    click(cx, window, "run-config-field-args");
    cx.simulate_input(window.into(), "alpha \"b c\"");
    draw(cx, window);
    click(cx, window, "run-config-ok");
    wait_for(cx, "the saved arguments", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| {
            configs.iter().any(|config| {
                config.settings.get(ARGS).and_then(|value| value.as_str()) == Some("alpha \"b c\"")
            })
        })
    });

    let saved = load_configs(&configs_file(&root)).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "Echo");
    assert_eq!(read_args(&saved[0]), "alpha \"b c\"");

    crate::window_tests::press(cx, window, "shift-f10");
    wait_for(cx, "the program", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("Process finished"))
    });
    let text = panel.read_with(cx, |panel, _| panel.text());
    assert!(
        text.iter().any(|line| line.contains("alpha")),
        "the output holds the arguments: {text:?}"
    );
    assert_eq!(run_arguments(&planned), vec![vec!["alpha", "b c"]]);
}

#[ui_kit::gpui::test]
fn copy_and_remove_change_the_list_and_the_file(cx: &mut TestAppContext) {
    let (root, _planned, window, panel) = open_run(cx, "run-configs-copy");
    open_dialog(cx, window);

    click(cx, window, "run-config-copy");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-config-item-1").is_some(),
        "Copy adds a configuration to the list"
    );
    click(cx, window, "run-config-ok");
    wait_for(cx, "the copied configuration in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| configs.len() == 2)
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.state().selected),
        Some(1),
        "the configuration highlighted in the dialog becomes the selected one"
    );
    let names: Vec<String> = load_configs(&configs_file(&root))
        .unwrap()
        .into_iter()
        .map(|config| config.name)
        .collect();
    assert_eq!(names, vec!["Echo", "Echo (2)"]);

    open_dialog(cx, window);
    click(cx, window, "run-config-item-1");
    click(cx, window, "run-config-remove");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-config-item-1").is_none(),
        "Remove takes the configuration out of the list"
    );
    click(cx, window, "run-config-ok");
    wait_for(cx, "the removed configuration in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| configs.len() == 1)
    });
    assert_eq!(load_configs(&configs_file(&root)).unwrap()[0].name, "Echo");
}

#[ui_kit::gpui::test]
fn the_add_button_creates_a_configuration_of_a_template(cx: &mut TestAppContext) {
    let (root, _planned, window, _panel) = open_run(cx, "run-configs-add");
    open_dialog(cx, window);

    click(cx, window, "run-config-add");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-config-template-application").is_some(),
        "Add lists the templates of the providers"
    );
    click(cx, window, "run-config-template-application");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-config-item-1").is_some(),
        "the template of the provider creates a configuration"
    );
    click(cx, window, "run-config-ok");
    wait_for(cx, "the added configuration in the file", |_| {
        load_configs(&configs_file(&root))
            .is_ok_and(|configs| configs.iter().any(|config| config.name == "Unnamed"))
    });
}

#[ui_kit::gpui::test]
fn an_empty_name_keeps_ok_from_saving(cx: &mut TestAppContext) {
    let (root, _planned, window, _panel) = open_run(cx, "run-configs-empty-name");
    open_dialog(cx, window);

    click(cx, window, "run-config-name");
    crate::window_tests::press(cx, window, "ctrl-a backspace");
    draw(cx, window);
    assert!(
        bounds(cx, window, "run-config-error").is_some(),
        "the empty name is reported under the field"
    );
    click(cx, window, "run-config-ok");
    draw(cx, window);

    assert!(
        bounds(cx, window, "run-configs").is_some(),
        "OK does not close the dialog while the name is empty"
    );
    assert!(
        !configs_file(&root).exists(),
        "nothing is written with an empty name"
    );
}

#[ui_kit::gpui::test]
fn a_broken_file_is_not_overwritten(cx: &mut TestAppContext) {
    let root = sample_project("run-configs-broken");
    let file = configs_file(&root);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not [ toml").unwrap();
    let planned = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FixtureRun { planned })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the discovered configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    wait_for(cx, "the broken file in a notification", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace
                    .last_notification()
                    .is_some_and(|(_, text)| text.contains("run.toml"))
            })
            .unwrap()
    });
    assert_eq!(configuration_names(cx, &panel), vec!["Echo"]);

    crate::window_tests::open_palette(cx, window, "run.edit_configurations");
    draw(cx, window);
    click(cx, window, "run-config-name");
    crate::window_tests::press(cx, window, "end");
    cx.simulate_input(window.into(), "X");
    click(cx, window, "run-config-ok");
    cx.run_until_parked();
    draw(cx, window);

    assert_eq!(std::fs::read_to_string(&file).unwrap(), "not [ toml");
    assert!(
        bounds(cx, window, "run-configs").is_some(),
        "the dialog stays open with the error"
    );
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn a_configuration_written_on_disk_reaches_the_list(cx: &mut TestAppContext) {
    let (root, _planned, _window, panel) = open_run(cx, "run-configs-reload");
    let mut stored = config("Stored");
    stored
        .settings
        .insert(ARGS.into(), "from disk".to_owned().into());
    std::fs::create_dir_all(configs_file(&root).parent().unwrap()).unwrap();
    runner::save_configs(&configs_file(&root), &[stored]).unwrap();

    wait_for(cx, "the stored configuration", |cx| {
        configuration_names(cx, &panel) == vec!["Stored", "Echo"]
    });
}
