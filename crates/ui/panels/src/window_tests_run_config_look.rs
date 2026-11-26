//! The look of the Run/Debug Configurations dialog: the tree without counts, the first row
//! of runtime and module, the inline fields and the options they hang on, the picker of the
//! main class, the chips of the options without a field, and the split button that saves
//! and starts. The dialog runs on a substitute provider that names its fields the way a
//! program provider does and knows no language.
//!
//! `cargo test -p panels window_tests_run_config_look`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel};
use runner::{
    ConfigField, ConfigTemplate, RunConfig, RunConfigProvider, RunPlan, configs_file, load_configs,
    save_configs,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::window_tests::{click, draw, press, sample_project};
use crate::window_tests_run::{MockService, open_with, run_panel, setup, wait_for};
use crate::window_tests_run_configs::open_dialog;

/// The keys of the fields of the substitute provider.
const RUNTIME: &str = "runtime";
const MODULE: &str = "module";
const VM: &str = "vm";
const MAIN: &str = "main";
const ARGS: &str = "args";
const FOLDER: &str = "folder";
const ENV: &str = "env";
/// A plain argument field, drawn with its label at the left.
const SHELL: &str = "shell";
/// The runtimes the substitute offers: the empty value is the project's.
const RUNTIMES: [&str; 3] = ["", "/opt/runtime/one", "/opt/runtime/two"];
/// The entry points the substitute finds.
const ENTRIES: [&str; 2] = ["app.Main", "app.Other"];

type Planned = Arc<Mutex<Vec<RunConfig>>>;

/// A provider whose configurations keep their values in their settings, as a program does.
struct LookRun {
    planned: Planned,
}

impl RunConfigProvider for LookRun {
    fn id(&self) -> &'static str {
        "look"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(ENTRIES
            .iter()
            .map(|entry| {
                let mut config = config(entry.rsplit('.').next().unwrap_or(entry));
                config
                    .settings
                    .insert(MAIN.into(), (*entry).to_owned().into());
                config
            })
            .collect())
    }

    fn plan(&self, config: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        self.planned.lock().unwrap().push(config.clone());
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "echo", "ran"]),
            grace: Duration::from_secs(1),
        })
    }

    fn fields(&self, _: &RunConfig) -> Vec<ConfigField> {
        vec![
            ConfigField::choice(
                RUNTIME,
                "runtime",
                RUNTIMES
                    .iter()
                    .map(|runtime| (*runtime).to_owned())
                    .collect(),
            ),
            ConfigField::module(MODULE, "-cp"),
            ConfigField::arguments(VM, "VM options")
                .inline()
                .option("Options"),
            ConfigField::entry(MAIN, "Main class").inline(),
            ConfigField::arguments(ARGS, "Program arguments").inline(),
            ConfigField::path(FOLDER, "Working directory").option("Operating System"),
            ConfigField::env(ENV, "Environment variables")
                .hint("Separate variables with semicolon: VAR=value; VAR1=value1")
                .option("Operating System"),
            ConfigField::arguments(SHELL, "Command"),
        ]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        config
            .settings
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned()
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        config.settings.insert(key.into(), value.to_owned().into());
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

/// A configuration of the substitute provider, with nothing set.
fn config(name: &str) -> RunConfig {
    RunConfig {
        name: name.into(),
        provider: "look".into(),
        assemble_first: false,
        ..RunConfig::default()
    }
}

/// Opens a project whose configuration file is written before the window opens.
fn open_stored(
    cx: &mut TestAppContext,
    name: &str,
    configs: &[RunConfig],
) -> (PathBuf, WindowHandle<Workspace>, Planned) {
    let root = sample_project(name);
    save_configs(&configs_file(&root), configs).unwrap();
    let planned: Planned = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(LookRun {
        planned: planned.clone(),
    })];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the stored configurations", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    (root, window, planned)
}

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

/// Opens the dialog on the first configuration of the list and lets it draw.
fn open_first(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    open_dialog(cx, window);
    click(cx, window, "run-config-item-0");
    draw(cx, window);
}

/// The configuration the file holds first, after the dialog wrote it.
fn saved_first(root: &Path) -> RunConfig {
    load_configs(&configs_file(root))
        .expect("the file reads")
        .into_iter()
        .next()
        .expect("the file holds the configuration")
}

#[ui_kit::gpui::test]
fn the_tree_has_no_counts_and_no_templates_link(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-tree", &[config("One"), config("Two")]);
    open_dialog(cx, window);
    draw(cx, window);

    assert!(drawn(cx, window, "run-config-group-application"));
    assert!(drawn(cx, window, "run-config-item-0"));
    assert!(
        !drawn(cx, window, "run-config-group-count-application-2"),
        "a group shows no count of its configurations"
    );
    assert!(
        !drawn(cx, window, "run-config-edit-templates"),
        "there is no templates editor, so no link"
    );
}

#[ui_kit::gpui::test]
fn alt_m_opens_and_closes_the_modify_options_menu(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-alt-m", &[config("One")]);
    open_first(cx, window);
    assert!(!drawn(cx, window, "run-config-option-env"));

    press(cx, window, "alt-m");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-option-env"),
        "Alt+M opens the Modify options menu"
    );
    assert!(drawn(cx, window, "run-configs"), "the dialog stays open");

    press(cx, window, "alt-m");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-option-env"),
        "Alt+M again closes the menu"
    );
}

#[ui_kit::gpui::test]
fn the_vm_options_field_shows_while_its_option_is_checked(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-vm", &[config("One")]);
    open_first(cx, window);
    assert!(
        !drawn(cx, window, "run-config-field-vm"),
        "an option the menu does not check keeps its field hidden"
    );
    assert!(
        drawn(cx, window, "run-config-field-main"),
        "the main class is always on the form"
    );

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-option-vm");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-field-vm"),
        "a checked option shows its field"
    );

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-option-vm");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-field-vm"),
        "an unchecked option hides its field again"
    );
}

#[ui_kit::gpui::test]
fn the_runtime_dropdown_saves_its_home_in_the_file(cx: &mut TestAppContext) {
    let (root, window, _planned) = open_stored(cx, "look-runtime", &[config("One")]);
    open_first(cx, window);

    click(cx, window, "run-config-field-runtime");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-choice-runtime-2"),
        "the dropdown lists the runtimes"
    );
    click(cx, window, "run-config-choice-runtime-2");
    draw(cx, window);
    click(cx, window, "run-config-ok");
    wait_for(cx, "the runtime in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| {
            configs.first().is_some_and(|config| {
                config
                    .settings
                    .get("runtime")
                    .and_then(|value| value.as_str())
                    == Some(RUNTIMES[2])
            })
        })
    });
}

#[ui_kit::gpui::test]
fn the_main_class_picker_fills_the_field_from_the_entry_points(cx: &mut TestAppContext) {
    let (root, window, _planned) = open_stored(cx, "look-entry", &[config("One")]);
    open_first(cx, window);

    click(cx, window, "run-config-field-main");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-entry-1"),
        "a click on the main class lists the entry points"
    );
    click(cx, window, "run-config-entry-1");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-entry-1"),
        "a picked entry closes the list"
    );
    click(cx, window, "run-config-ok");
    wait_for(cx, "the main class in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| {
            configs.first().is_some_and(|config| {
                config.settings.get(MAIN).and_then(|value| value.as_str()) == Some(ENTRIES[1])
            })
        })
    });
}

#[ui_kit::gpui::test]
fn the_chip_turns_multiple_instances_off(cx: &mut TestAppContext) {
    let (root, window, _planned) = open_stored(cx, "look-chip", &[config("One")]);
    open_first(cx, window);

    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-option-allow_multiple_instances");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-multiple"),
        "a checked option shows its chip under the form"
    );

    click(cx, window, "run-config-multiple-remove");
    draw(cx, window);
    assert!(
        !drawn(cx, window, "run-config-multiple"),
        "the cross of the chip takes the option off"
    );
    click(cx, window, "run-config-ok");
    wait_for(cx, "the flag in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| {
            configs
                .first()
                .is_some_and(|config| !config.allow_multiple_instances)
        })
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_split_button_saves_and_starts_the_configuration(cx: &mut TestAppContext) {
    let (root, window, planned) = open_stored(cx, "look-launch", &[config("One")]);
    open_first(cx, window);

    click(cx, window, "run-config-launch-run");
    wait_for(cx, "the configuration starts", |_| {
        planned
            .lock()
            .unwrap()
            .iter()
            .any(|config| config.name == "One")
    });
    assert!(
        !drawn(cx, window, "run-configs"),
        "the dialog closes when the configuration starts"
    );
    assert_eq!(
        saved_first(&root).name,
        "One",
        "the file keeps the configuration"
    );
}

#[ui_kit::gpui::test]
fn the_launch_menu_chooses_what_the_split_button_starts(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-launch-menu", &[config("One")]);
    open_first(cx, window);
    assert!(drawn(cx, window, "run-config-launch-run"));

    click(cx, window, "run-config-launch-menu");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-launch-pick-debug"),
        "the menu lists Run and Debug"
    );
    click(cx, window, "run-config-launch-pick-debug");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-launch-debug"),
        "the split button now starts under the debugger"
    );
    assert!(
        !drawn(cx, window, "run-config-launch-pick-debug"),
        "the menu closes after the choice"
    );
}

#[ui_kit::gpui::test]
fn the_tree_marks_a_configuration_that_cannot_start(cx: &mut TestAppContext) {
    let mut named = config("Named");
    named
        .settings
        .insert(MAIN.into(), "app.Main".to_owned().into());
    let (_root, window, _planned) = open_stored(cx, "look-failed", &[config("Broken"), named]);
    open_dialog(cx, window);
    draw(cx, window);

    assert!(
        drawn(cx, window, "run-config-failed-0"),
        "a configuration without a main class shows the failed mark"
    );
    assert!(
        !drawn(cx, window, "run-config-failed-1"),
        "a configuration that names its main class shows no mark"
    );
}

#[ui_kit::gpui::test]
fn the_expand_button_opens_the_arguments_in_a_multi_line_editor(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-expand", &[config("One")]);
    open_first(cx, window);
    click(cx, window, "run-config-modify-options");
    draw(cx, window);
    click(cx, window, "run-config-option-vm");
    draw(cx, window);

    click(cx, window, "run-config-expand-vm");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-editor-vm"),
        "the expanded argument is a multi-line editor"
    );
    assert!(
        !drawn(cx, window, "run-config-field-vm"),
        "the single line gives way to the editor"
    );

    click(cx, window, "run-config-expand-vm");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-field-vm"),
        "collapsing brings the single line back"
    );
    assert!(!drawn(cx, window, "run-config-editor-vm"));
}

#[ui_kit::gpui::test]
fn enter_in_an_expanded_argument_starts_a_line_and_keeps_the_dialog(cx: &mut TestAppContext) {
    let (_root, window, _planned) = open_stored(cx, "look-enter", &[config("One")]);
    open_first(cx, window);

    click(cx, window, "run-config-expand-shell");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-editor-shell"),
        "an argument with its label at the left expands as well"
    );

    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-configs"),
        "Enter in the editor starts a line and does not press OK"
    );
    assert!(drawn(cx, window, "run-config-editor-shell"));
}

/// A build tool that answers with a model of the test's own, so the dialog has modules.
struct ModelTool(ProjectModel);

impl BuildSystem for ModelTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &base::Cancel) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Err("not used by the test".into())
    }
}

#[ui_kit::gpui::test]
fn the_module_dropdown_lists_the_project_modules_and_saves_the_one_chosen(cx: &mut TestAppContext) {
    let root = sample_project("look-module");
    save_configs(&configs_file(&root), &[config("One")]).unwrap();
    let planned: Planned = Arc::new(Mutex::new(Vec::new()));
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(LookRun {
        planned: planned.clone(),
    })];
    setup.tool = Some(Arc::new(ModelTool(ProjectModel {
        root: root.clone(),
        tool: "fixture".into(),
        resolved: true,
        modules: vec![Module {
            id: "core".into(),
            name: "core".into(),
            dir: root.clone(),
            ..Module::default()
        }],
        ..ProjectModel::default()
    })));
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    open_first(cx, window);

    click(cx, window, "run-config-field-module");
    draw(cx, window);
    assert!(
        drawn(cx, window, "run-config-choice-module-0"),
        "the dropdown lists the modules of the project"
    );
    click(cx, window, "run-config-choice-module-0");
    draw(cx, window);
    click(cx, window, "run-config-ok");
    wait_for(cx, "the module in the file", |_| {
        load_configs(&configs_file(&root)).is_ok_and(|configs| {
            configs.first().is_some_and(|config| {
                config.settings.get(MODULE).and_then(|value| value.as_str()) == Some("core")
            })
        })
    });
}

#[cfg(windows)]
#[ui_kit::gpui::test]
fn the_split_button_in_debug_mode_starts_the_debugger(cx: &mut TestAppContext) {
    let root = sample_project("look-debug");
    save_configs(&configs_file(&root), &[config("One")]).unwrap();
    let planned: Planned = Arc::new(Mutex::new(Vec::new()));
    let service = MockService::new();
    let mut setup = setup(&root);
    setup.providers = vec![Arc::new(LookRun { planned })];
    setup.debug = Some(service.clone());
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "the configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    open_first(cx, window);

    click(cx, window, "run-config-launch-menu");
    draw(cx, window);
    click(cx, window, "run-config-launch-pick-debug");
    draw(cx, window);
    click(cx, window, "run-config-launch-debug");
    wait_for(cx, "the debugger starts", |_| service.starts() == 1);
}
