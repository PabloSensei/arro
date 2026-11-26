//! The run list of a project: a found plain program is not a configuration of the list,
//! an application the provider offers is listed until the project has its own, and an
//! empty list offers the Add Configuration row. No real window, no processes.
//!
//! cargo test -p panels window_tests_run_list

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{ConfigTemplate, RunConfig, RunConfigProvider, RunPlan, configs_file, load_configs};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{click, draw, sample_project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// The setting that names the entry class of a found configuration.
const CLASS: &str = "entry_class";
/// The setting that says a found configuration starts a server.
const APPLICATION: &str = "application";

/// A provider that finds `configs` in every project. A found configuration with the
/// application setting is offered by the list, as a framework's application is.
struct FoundRuns {
    configs: Vec<RunConfig>,
}

impl RunConfigProvider for FoundRuns {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(self.configs.clone())
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }

    fn main_class(&self, config: &RunConfig) -> Option<String> {
        config
            .settings
            .get(CLASS)
            .and_then(|value| value.as_str())
            .map(str::to_owned)
    }

    fn suggestions(&self, stored: &[RunConfig], discovered: &[RunConfig]) -> Vec<RunConfig> {
        let stored: Vec<&str> = stored
            .iter()
            .filter_map(|config| config.settings.get(CLASS))
            .filter_map(|value| value.as_str())
            .collect();
        discovered
            .iter()
            .filter(|config| {
                config
                    .settings
                    .get(APPLICATION)
                    .and_then(|value| value.as_bool())
                    == Some(true)
            })
            .filter(|config| match self.main_class(config) {
                Some(class) => !stored.contains(&class.as_str()),
                None => false,
            })
            .cloned()
            .collect()
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
        Ok(found("demo.Unnamed", false))
    }
}

/// A found configuration of `class`: a plain program, or an application with `APPLICATION`.
fn found(class: &str, application: bool) -> RunConfig {
    let mut config = RunConfig {
        name: class.rsplit('.').next().unwrap_or(class).to_owned(),
        provider: "fixture".into(),
        assemble_first: false,
        settings: Default::default(),
        ..RunConfig::default()
    };
    config
        .settings
        .insert(CLASS.into(), class.to_owned().into());
    config
        .settings
        .insert(APPLICATION.into(), application.into());
    config
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
    configs: Vec<RunConfig>,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<RunPanel>) {
    let root = sample_project(name);
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(FoundRuns { configs })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    (root, window, panel)
}

fn names(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| {
        panel
            .state()
            .configs
            .iter()
            .map(ToString::to_string)
            .collect()
    })
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

fn menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
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
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    click(cx, window, selector);
}

#[ui_kit::gpui::test]
fn plain_programs_leave_the_list_empty_and_the_add_row_opens_the_dialog(cx: &mut TestAppContext) {
    let (_root, window, panel) = open(
        cx,
        "run-list-plain",
        vec![
            found("demo.Alpha", false),
            found("demo.Beta", false),
            found("demo.Gamma", false),
        ],
    );
    wait_for(cx, "the found classes", |cx| {
        panel.read_with(cx, |panel, _| panel.discovered_main_classes().len()) == 3
    });

    assert_eq!(
        panel.read_with(cx, |panel, _| panel.discovered_main_classes()),
        vec!["demo.Alpha", "demo.Beta", "demo.Gamma"],
        "the dialog gets every found class for its Main class choice"
    );
    assert_eq!(names(cx, &panel), Vec::<String>::new());
    let state = panel.read_with(cx, |panel, _| panel.state());
    assert!(state.selected.is_none(), "nothing is selected");
    assert!(!state.can_run(), "Run and Debug wait for a configuration");

    draw(cx, window);
    click(cx, window, "t-config");
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Add Configuration…"),
        "an empty list offers the Add row: {labels:?}"
    );
    click_menu(cx, window, "Add Configuration…");
    draw(cx, window);
    assert!(
        has(cx, window, "run-configs"),
        "the Add row opens the configurations dialog"
    );
    assert!(
        has(cx, window, "run-config-template-application"),
        "the dialog opens with its Add menu"
    );
}

#[ui_kit::gpui::test]
fn a_temporary_configuration_keeps_the_list_to_five(cx: &mut TestAppContext) {
    let (_root, window, panel) = open(cx, "run-list-temporary", vec![found("demo.Alpha", false)]);
    wait_for(cx, "the found class", |cx| {
        panel.read_with(cx, |panel, _| panel.discovered_main_classes().len()) == 1
    });

    for number in 1..=6 {
        panel.update(cx, |panel, cx| {
            panel.choose_configuration(
                RunConfig {
                    name: format!("Temporary {number}"),
                    provider: "fixture".into(),
                    assemble_first: false,
                    settings: Default::default(),
                    ..RunConfig::default()
                },
                cx,
            )
        });
    }
    draw(cx, window);

    assert_eq!(
        names(cx, &panel),
        vec![
            "Temporary 2",
            "Temporary 3",
            "Temporary 4",
            "Temporary 5",
            "Temporary 6"
        ],
        "the sixth temporary pushes the oldest out"
    );
    let state = panel.read_with(cx, |panel, _| panel.state());
    assert!(state.is_temporary(0));
    assert!(state.is_temporary(4));
}

#[ui_kit::gpui::test]
fn a_found_application_is_offered_once_until_it_is_saved(cx: &mut TestAppContext) {
    let (root, _window, panel) = open(cx, "run-list-application", vec![found("demo.App", true)]);
    wait_for(cx, "the found application", |cx| {
        panel.read_with(cx, |panel, _| panel.state().configs.len()) == 1
    });

    let state = panel.read_with(cx, |panel, _| panel.state());
    assert_eq!(
        state
            .configs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["App"]
    );
    assert!(
        state.is_temporary(0),
        "a found application is offered as a finding"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.discovered_main_classes()),
        vec!["demo.App"]
    );

    panel.update(cx, |panel, cx| panel.save_selected(cx));
    wait_for(cx, "the saved application", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| configs.len() == 1)
    });
    wait_for(cx, "one configuration after the save", |cx| {
        panel.read_with(cx, |panel, _| panel.state().configs.len()) == 1
    });
    let state = panel.read_with(cx, |panel, _| panel.state());
    assert_eq!(
        state
            .configs
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec!["App"],
        "the stored configuration replaces the found one instead of duplicating it"
    );
    assert!(
        !state.is_temporary(0),
        "the configuration is in the file now"
    );
    assert_eq!(load_configs(&configs_file(&root)).unwrap()[0].name, "App");
}
