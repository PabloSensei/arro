//! The Distribution row of the page of the second build tool and the notification of a
//! project without a wrapper. The settings pages are the ones the product shows; the
//! project runs the real search of the tool, so only the machine's own copies decide
//! whether it finds one. No test reaches the network.

use std::fs;
use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::run::ProjectSetup;
use crate::settings::SettingsView;
use crate::window_tests::{click, draw, open_sample_window, open_window_with, scratch};
use crate::window_tests_run::wait_for;

/// Opens the Settings window on the page of the second build tool.
fn open_settings_on_distribution(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (_root, window) = open_sample_window(cx, name);
    window
        .update(cx, |workspace, window, cx| {
            SettingsView::show_at(workspace, window, cx, Some("build_tool_second_home"));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    assert!(has(cx, window, "settings"), "the Settings window is drawn");
    assert!(
        has(cx, window, "settings-build-tool-second-home"),
        "the Distribution row is on the page of the second build tool"
    );
    window
}

fn option(id: &str) -> &'static str {
    Box::leak(id.to_owned().into_boxed_str())
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[gpui::test]
fn the_distribution_list_offers_the_three_sources(cx: &mut TestAppContext) {
    let window = open_settings_on_distribution(cx, "gradle-wrapper-distribution");

    click(cx, window, "settings-build-tool-second-home");
    draw(cx, window);

    assert!(
        has(cx, window, option("settings-choice-option-wrapper")),
        "the wrapper of the project"
    );
    assert!(
        has(
            cx,
            window,
            option("settings-choice-option-local-installation")
        ),
        "a local installation"
    );
    assert!(
        has(
            cx,
            window,
            option("settings-choice-option-downloaded-by-arro")
        ),
        "the copy the IDE downloaded"
    );
}

#[gpui::test]
fn local_installation_shows_the_path_field(cx: &mut TestAppContext) {
    let window = open_settings_on_distribution(cx, "gradle-wrapper-local-path");
    assert!(
        !has(cx, window, "settings-build-tool-second-home-path"),
        "the wrapper is the default source, so the path is hidden"
    );

    click(cx, window, "settings-build-tool-second-home");
    draw(cx, window);
    click(
        cx,
        window,
        option("settings-choice-option-local-installation"),
    );
    draw(cx, window);

    assert!(
        has(cx, window, "settings-build-tool-second-home-path"),
        "a local installation needs its folder"
    );
    assert!(
        has(cx, window, "settings-build-tool-second-home-browse"),
        "the folder comes with a button that asks for it"
    );
}

/// A build tool of the test whose notification is the one of the real search: the real
/// Gradle finds nothing in the folder, because the chosen source is a folder that is not
/// there and the project has no wrapper.
struct MissingGradle {
    gradle: window_language::Gradle,
}

impl project_model::BuildSystem for MissingGradle {
    fn detect(&self, root: &std::path::Path) -> bool {
        self.gradle.detect(root)
    }

    fn scan(&self, root: &std::path::Path) -> base::Result<project_model::ProjectModel> {
        self.gradle.scan(root)
    }

    fn sync(
        &self,
        root: &std::path::Path,
        cancel: &base::Cancel,
    ) -> base::Result<project_model::ProjectModel> {
        self.gradle.sync(root, cancel)
    }

    fn assemble(&self, root: &std::path::Path) -> base::Result<proc_sup::ProcessSpec> {
        self.gradle.assemble(root)
    }

    fn missing_tool(&self, root: &std::path::Path) -> Option<project_model::ToolSetup> {
        self.gradle.missing_tool(root)
    }
}

fn setup(tool: Arc<MissingGradle>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("gradle-wrapper-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

#[ui_kit::gpui::test]
fn a_project_without_a_wrapper_offers_to_create_one(cx: &mut TestAppContext) {
    let root = scratch("gradle-wrapper-create-project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("build.gradle"), "// a Gradle build\n").unwrap();
    let data = scratch("gradle-wrapper-create-data");
    // The chosen source is a folder that is not there, so the test does not depend on
    // the Gradle the machine happens to have on `PATH`.
    let mut build_tools = window_language::BuildTools::default();
    build_tools.second.home_kind = project_model::ToolHome::Custom;
    let env = window_language::BuildEnv {
        data_dir: Some(data),
        build_tools,
        ..window_language::BuildEnv::default()
    };
    let tool = Arc::new(MissingGradle {
        gradle: window_language::Gradle::new(env),
    });
    let window = open_window_with(cx, root, setup(tool));

    wait_for(
        cx,
        "the notification of the project without a wrapper",
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.last_notification().is_some())
                .unwrap()
        },
    );
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();

    assert!(
        buttons.iter().any(|label| label == "Create Gradle Wrapper"),
        "{buttons:?}"
    );
    assert!(
        buttons.iter().any(|label| label == "Use Downloaded Gradle"),
        "{buttons:?}"
    );
}
