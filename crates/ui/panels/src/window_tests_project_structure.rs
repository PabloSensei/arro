//! Project Structure without a real window: Ctrl+Alt+Shift+S opens the dialog, the SDK
//! of the project changes and is stored, an SDK folder is checked, and the Settings page
//! leads to the same dialog. The language is a stand-in, so no real SDK is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use project_model::{EnvironmentProvider, ProjectEnvironmentInfo, RuntimeInfo, RuntimeReason};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::project_structure::ProjectStructureDialog;
use crate::window_tests::{
    bounds_of, click, draw, open_window_with, press, project_setup, sample_project, scratch,
    wait_for_signal,
};

#[derive(Default)]
struct State {
    selected: Option<PathBuf>,
    added: Vec<PathBuf>,
}

/// A language that knows two SDK folders and one that is not an SDK.
struct FakeEnvironment {
    root: PathBuf,
    runtimes: Vec<RuntimeInfo>,
    state: Mutex<State>,
}

impl FakeEnvironment {
    fn new(root: PathBuf) -> Self {
        let runtime = |name: &str, version: &str, feature: u32| RuntimeInfo {
            name: name.to_owned(),
            version: version.to_owned(),
            feature,
            home: root.join(name),
            added: false,
        };
        let runtimes = vec![
            runtime("sdk-17", "17.0.2", 17),
            runtime("sdk-21", "21.0.2", 21),
        ];
        let selected = Some(root.join("sdk-17"));
        Self {
            root,
            runtimes,
            state: Mutex::new(State {
                selected,
                added: Vec::new(),
            }),
        }
    }

    fn selected(&self) -> Option<PathBuf> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .selected
            .clone()
    }

    fn state_file(&self) -> PathBuf {
        self.root.join(".arro").join("project.toml")
    }
}

impl EnvironmentProvider for FakeEnvironment {
    fn describe(&self) -> ProjectEnvironmentInfo {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .iter()
            .map(|runtime| RuntimeInfo {
                added: state.added.contains(&runtime.home),
                ..runtime.clone()
            })
            .collect();
        let variables = state
            .selected
            .as_ref()
            .map(|home| vec![("SDK_HOME".to_owned(), home.display().to_string())])
            .unwrap_or_default();
        ProjectEnvironmentInfo {
            runtimes,
            selected: state.selected.clone(),
            reason: if state.selected.is_some() {
                RuntimeReason::User
            } else {
                RuntimeReason::Default
            },
            notice: None,
            language: None,
            variables,
        }
    }

    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String> {
        let found = self
            .runtimes
            .iter()
            .find(|runtime| runtime.home == home)
            .cloned();
        found.ok_or_else(|| {
            format!(
                "The folder is not an SDK: {} has no bin\\tool.exe",
                home.display()
            )
        })
    }

    fn apply(
        &self,
        selected: Option<&Path>,
        added: &[PathBuf],
    ) -> Result<ProjectEnvironmentInfo, String> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            state.selected = selected.map(Path::to_path_buf);
            state.added = added.to_vec();
        }
        if let Some(directory) = self.state_file().parent() {
            fs::create_dir_all(directory).map_err(|error| error.to_string())?;
        }
        let selected_text = selected
            .map(|home| home.display().to_string().replace('\\', "/"))
            .unwrap_or_default();
        fs::write(
            self.state_file(),
            format!("selected = \"{selected_text}\"\n"),
        )
        .map_err(|error| error.to_string())?;
        Ok(self.describe())
    }
}

fn window_with_environment(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, Arc<FakeEnvironment>) {
    let root = sample_project(name);
    let environment = Arc::new(FakeEnvironment::new(root.clone()));
    let mut setup = project_setup(scratch("cache"));
    setup.environment = Some(environment.clone());
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    (root, window, environment)
}

fn dialog_of(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Entity<ProjectStructureDialog>> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()?;
    view.downcast::<ProjectStructureDialog>().ok()
}

fn wait_for_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    wait_for_signal(cx, "Project Structure", |cx| {
        dialog_of(cx, window)
            .is_some_and(|dialog| dialog.read_with(cx, |dialog, _| dialog.runtime_count() > 0))
    });
}

#[ui_kit::gpui::test]
fn ctrl_alt_shift_s_opens_project_structure(cx: &mut TestAppContext) {
    let (_, window, _) = window_with_environment(cx, "structure-open");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);

    assert!(dialog_of(cx, window).is_some(), "the dialog did not open");
    let _ = bounds_of(cx, window, "project-structure-title");
}

#[ui_kit::gpui::test]
fn changing_the_sdk_and_ok_stores_the_project_choice(cx: &mut TestAppContext) {
    let (root, window, environment) = window_with_environment(cx, "structure-choose");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);

    click(cx, window, "project-structure-sdk");
    draw(cx, window);
    click(cx, window, "project-structure-sdk-item-1");
    draw(cx, window);
    click(cx, window, "project-structure-ok");

    wait_for_signal(cx, "the stored SDK", |_| environment.state_file().is_file());
    draw(cx, window);

    let text = fs::read_to_string(environment.state_file()).unwrap();
    assert!(text.contains("sdk-21"), "{text}");
    assert_eq!(environment.selected(), Some(root.join("sdk-21")));
    let variables = environment.describe().variables;
    assert!(
        variables
            .iter()
            .any(|(key, value)| key == "SDK_HOME" && value.ends_with("sdk-21")),
        "{variables:?}"
    );
    assert!(
        dialog_of(cx, window).is_none(),
        "OK did not close the dialog"
    );
}

#[ui_kit::gpui::test]
fn adding_a_folder_that_is_not_an_sdk_shows_an_error(cx: &mut TestAppContext) {
    let (root, window, environment) = window_with_environment(cx, "structure-bad-sdk");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);

    click(cx, window, "project-structure-section-sdks");
    draw(cx, window);
    let dialog = dialog_of(cx, window).expect("the dialog is open");
    dialog.update(cx, |dialog, cx| {
        dialog.add_folder(root.join("not-an-sdk"), cx)
    });
    wait_for_signal(cx, "the refused folder", |cx| {
        dialog.read_with(cx, |dialog, _| dialog.error_text().is_some())
    });
    draw(cx, window);

    let error = dialog.read_with(cx, |dialog, _| {
        dialog.error_text().unwrap_or_default().to_owned()
    });
    assert!(error.contains("bin\\tool.exe"), "{error}");
    assert!(!environment.state_file().is_file(), "nothing was stored");
    let _ = bounds_of(cx, window, "project-structure-error");
}

#[ui_kit::gpui::test]
fn the_project_page_of_settings_leads_to_project_structure(cx: &mut TestAppContext) {
    let (_, window, _) = window_with_environment(cx, "structure-settings");

    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-project");
    draw(cx, window);
    let _ = bounds_of(cx, window, "settings-project-sdk");

    click(cx, window, "settings-project-sdk");
    draw(cx, window);

    assert!(dialog_of(cx, window).is_some(), "the dialog did not open");
}
