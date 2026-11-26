//! The tree of Run/Debug Configurations grouped by the kind of configuration, the Add
//! menu that searches the kinds, and the keys and Apply button of the dialog.

use std::path::Path;
use std::sync::Arc;

use project_model::ProjectModel;
use runner::{ConfigTemplate, RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{draw, press, sample_project};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};
use crate::window_tests_run_configs::open_dialog;

const KIND: &str = "kind";

/// A provider with two kinds of configuration, so the dialog has two groups.
struct TypedRun;

impl RunConfigProvider for TypedRun {
    fn id(&self) -> &'static str {
        "typed"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Err("the tests of the tree do not start a program".into())
    }

    fn group_name(&self, config: &RunConfig) -> &'static str {
        if kind_of(config) == "tests" {
            "Tests"
        } else {
            "Application"
        }
    }

    fn group_icon(&self, config: &RunConfig) -> &'static str {
        if kind_of(config) == "tests" {
            "run/debug"
        } else {
            "run/application"
        }
    }

    fn template_icon(&self, template: &ConfigTemplate) -> &'static str {
        if template.id == "tests" {
            "run/debug"
        } else {
            "run/application"
        }
    }

    fn templates(&self) -> Vec<ConfigTemplate> {
        vec![
            ConfigTemplate {
                id: "application",
                name: "Application",
            },
            ConfigTemplate {
                id: "tests",
                name: "Tests",
            },
        ]
    }

    fn create_config(&self, template: &str, _: &ProjectModel) -> base::Result<RunConfig> {
        if template == "tests" {
            return Ok(config("Tests", "tests"));
        }
        if template == "application" {
            return Ok(config("Application", "application"));
        }
        Err("unknown template".into())
    }
}

fn config(name: &str, kind: &str) -> RunConfig {
    let mut config = RunConfig {
        name: name.into(),
        provider: "typed".into(),
        assemble_first: false,
        ..RunConfig::default()
    };
    config.settings.insert(KIND.into(), kind.to_owned().into());
    config
}

fn kind_of(config: &RunConfig) -> &str {
    config
        .settings
        .get(KIND)
        .and_then(|value| value.as_str())
        .unwrap_or("application")
}

fn store(root: &Path, configs: &[RunConfig]) {
    let file = runner::configs_file(root);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    runner::save_configs(&file, configs).unwrap();
}

fn open_typed(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    std::path::PathBuf,
    WindowHandle<Workspace>,
    gpui::Entity<RunPanel>,
) {
    let root = sample_project(name);
    store(
        &root,
        &[
            config("Shop", "application"),
            config("Tools", "application"),
            config("Tests", "tests"),
        ],
    );
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(TypedRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the stored configurations", |cx| {
        panel.read_with(cx, |panel, _| panel.state().configs.len()) == 3
    });
    open_dialog(cx, window);
    (root, window, panel)
}

fn on_screen(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

#[ui_kit::gpui::test]
fn the_tree_groups_configurations_by_their_kind(cx: &mut TestAppContext) {
    let (_root, window, _panel) = open_typed(cx, "run-tree-groups");

    assert!(on_screen(cx, window, "run-config-group-application"));
    assert!(on_screen(cx, window, "run-config-group-tests"));
    assert!(on_screen(cx, window, "run-config-item-0"));
    assert!(on_screen(cx, window, "run-config-item-2"));
    assert!(
        !on_screen(cx, window, "run-config-group-count-application-2"),
        "a group shows no count of its configurations"
    );
    assert!(
        !on_screen(cx, window, "run-config-edit-templates"),
        "there are no templates to edit, so the tree has no link"
    );
}

#[ui_kit::gpui::test]
fn the_add_menu_searches_the_kinds_and_creates_in_their_group(cx: &mut TestAppContext) {
    let (root, window, _panel) = open_typed(cx, "run-tree-add");

    crate::window_tests::click(cx, window, "run-config-add");
    draw(cx, window);
    assert!(on_screen(cx, window, "run-config-template-application"));
    assert!(on_screen(cx, window, "run-config-template-tests"));

    cx.simulate_input(window.into(), "tes");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "run-config-template-application"),
        "the search hides the other kinds"
    );
    assert!(on_screen(cx, window, "run-config-template-tests"));

    crate::window_tests::click(cx, window, "run-config-template-tests");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "run-config-group-tests"),
        "the new configuration is in the Tests group"
    );

    crate::window_tests::click(cx, window, "run-config-ok");
    wait_for(cx, "the new configuration in the file", |_| {
        runner::load_configs(&runner::configs_file(&root)).is_ok_and(|configs| {
            configs
                .iter()
                .filter(|config| kind_of(config) == "tests")
                .count()
                == 2
        })
    });
}

#[ui_kit::gpui::test]
fn ctrl_d_copies_and_delete_removes(cx: &mut TestAppContext) {
    let (_root, window, _panel) = open_typed(cx, "run-tree-keys");

    press(cx, window, "ctrl-d");
    draw(cx, window);
    assert!(
        on_screen(cx, window, "run-config-item-3"),
        "Ctrl+D adds a configuration"
    );
    press(cx, window, "delete");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "run-config-item-3"),
        "Delete removes the selected configuration"
    );
    assert!(on_screen(cx, window, "run-config-group-application"));
}

#[ui_kit::gpui::test]
fn apply_is_grey_until_the_name_changes(cx: &mut TestAppContext) {
    let (root, window, _panel) = open_typed(cx, "run-tree-apply");

    assert!(
        on_screen(cx, window, "run-config-apply-disabled"),
        "nothing to apply on the unchanged configuration"
    );

    crate::window_tests::click(cx, window, "run-config-name");
    cx.simulate_input(window.into(), "Renamed");
    draw(cx, window);
    assert!(
        !on_screen(cx, window, "run-config-apply-disabled"),
        "the edited name turns Apply on"
    );
    assert!(
        on_screen(cx, window, "run-config-item-0"),
        "the changed configuration stays in the tree"
    );

    crate::window_tests::click(cx, window, "run-config-apply");
    wait_for(cx, "the renamed configuration", |_| {
        runner::load_configs(&runner::configs_file(&root))
            .is_ok_and(|configs| configs.iter().any(|config| config.name == "ShopRenamed"))
    });
}
