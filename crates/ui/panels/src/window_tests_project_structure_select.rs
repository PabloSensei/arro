//! The Project page of Project Structure: the SDK and the language level are dropdowns of
//! the kit, the level list is the one IDEA shows, and Apply stores both choices.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use project_model::{
    EnvironmentProvider, LanguageLevelEdit, LanguageLevelInfo, ProjectEnvironmentInfo, RuntimeInfo,
    RuntimeReason,
};
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
    level: Option<String>,
}

/// A language that knows two SDK folders.
pub(crate) struct FakeEnvironment {
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
        Self {
            runtimes,
            state: Mutex::new(State {
                selected: Some(root.join("sdk-17")),
                level: Some("17".to_owned()),
            }),
            root,
        }
    }

    fn state(&self) -> (Option<PathBuf>, Option<String>) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (state.selected.clone(), state.level.clone())
    }

    fn pom(&self) -> PathBuf {
        self.root.join("pom.xml")
    }
}

impl EnvironmentProvider for FakeEnvironment {
    fn describe(&self) -> ProjectEnvironmentInfo {
        let (selected, level) = self.state();
        let runtimes: Vec<RuntimeInfo> = self
            .runtimes
            .iter()
            .map(|runtime| RuntimeInfo {
                added: false,
                ..runtime.clone()
            })
            .collect();
        ProjectEnvironmentInfo {
            runtimes,
            reason: if selected.is_some() {
                RuntimeReason::User
            } else {
                RuntimeReason::Default
            },
            selected,
            notice: None,
            language: Some(LanguageLevelInfo {
                values: (8..=25).map(|level| level.to_string()).collect(),
                selected: level,
                file: Some(self.pom()),
                line: Some(0),
            }),
            variables: Vec::new(),
        }
    }

    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.home == home)
            .cloned()
            .ok_or_else(|| format!("The folder is not an SDK: {}", home.display()))
    }

    fn language_change(
        &self,
        level: &str,
        _module: Option<&str>,
        _text: Option<&str>,
    ) -> Result<LanguageLevelEdit, String> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .level = Some(level.to_owned());
        Ok(LanguageLevelEdit {
            file: self.pom(),
            line: Some(0),
            summary: format!("Changes the release to {level}"),
            added: false,
            change: lang_api::WorkspaceChange::default(),
        })
    }

    fn apply(
        &self,
        selected: Option<&Path>,
        _added: &[PathBuf],
    ) -> Result<ProjectEnvironmentInfo, String> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .selected = selected.map(Path::to_path_buf);
        Ok(self.describe())
    }
}

pub(crate) fn window_with_environment(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, Arc<FakeEnvironment>) {
    let root = sample_project(name);
    fs::write(
        root.join("pom.xml"),
        "<project><release>17</release></project>\n",
    )
    .unwrap();
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

pub(crate) fn wait_for_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    wait_for_signal(cx, "Project Structure", |cx| {
        dialog_of(cx, window)
            .is_some_and(|dialog| dialog.read_with(cx, |dialog, _| dialog.runtime_count() > 0))
    });
}

#[gpui::test]
fn the_level_list_offers_the_levels_of_idea_and_the_sdk_default(cx: &mut TestAppContext) {
    let (_, window, _) = window_with_environment(cx, "structure-select-levels");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);

    click(cx, window, "project-structure-language");
    draw(cx, window);
    for level in ["8", "11", "17", "21", "25", "sdk-default"] {
        let selector = format!("project-structure-language-item-{level}");
        let selector: &'static str = Box::leak(selector.into_boxed_str());
        let _ = bounds_of(cx, window, selector);
    }
}

#[gpui::test]
fn the_sdk_and_the_level_are_picked_from_lists_and_apply_stores_them(cx: &mut TestAppContext) {
    let (root, window, environment) = window_with_environment(cx, "structure-select-apply");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);

    click(cx, window, "project-structure-sdk");
    draw(cx, window);
    click(cx, window, "project-structure-sdk-item-1");
    draw(cx, window);
    click(cx, window, "project-structure-language");
    draw(cx, window);
    click(cx, window, "project-structure-language-item-21");
    draw(cx, window);
    click(cx, window, "project-structure-apply");

    wait_for_signal(cx, "the stored SDK and level", |_| {
        environment.state() == (Some(root.join("sdk-21")), Some("21".to_owned()))
    });
    draw(cx, window);
    assert!(
        dialog_of(cx, window).is_some(),
        "Apply keeps the dialog open"
    );
}

#[gpui::test]
fn the_sdk_default_item_lets_the_language_choose_the_sdk(cx: &mut TestAppContext) {
    let (_, window, environment) = window_with_environment(cx, "structure-select-default");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);

    click(cx, window, "project-structure-sdk");
    draw(cx, window);
    click(cx, window, "project-structure-sdk-default");
    draw(cx, window);
    click(cx, window, "project-structure-apply");

    wait_for_signal(cx, "the default SDK", |_| environment.state().0.is_none());
    draw(cx, window);
    let _ = bounds_of(cx, window, "project-structure-reason");
}
