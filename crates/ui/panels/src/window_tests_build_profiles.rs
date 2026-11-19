//! The build window with named build settings: the Profiles node of the model, the check
//! box that reaches the next build, the values a build tool takes, and the choice kept
//! when the project is opened again. The build tool is a stand-in that records the
//! arguments of its commands and keeps the choice in a file of the project.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildProfile, BuildSelection, BuildSystem, BuildTask, Module,
    ProfileChoice, ProjectModel, TaskOptions,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{TASKS_TOOL, Workspace};

use crate::run::ProjectSetup;
use crate::script::{ActiveBuildPanel, ActiveRunPanel};
use crate::window_tests::{draw, open_window_with, sample_project, scratch};
use crate::window_tests_run::wait_for;

/// The file the stand-in build tool keeps the choice in.
const CHOICE_FILE: &str = ".build-choice.json";

/// The stand-in build tool: it reads and writes the choice as a real tool does, so a
/// second window of the same project starts from it.
struct ProfileTool {
    model: ProjectModel,
    syncs: Arc<AtomicUsize>,
    /// The arguments of every build, in the order they were asked for.
    commands: Arc<Mutex<Vec<Vec<String>>>>,
    /// The tool takes named values on its command line.
    properties: bool,
}

impl ProfileTool {
    fn new(root: &Path, properties: bool) -> Self {
        let module = |id: &str, name: &str, dir: PathBuf| Module {
            id: id.into(),
            name: name.into(),
            dir,
            ..Module::default()
        };
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fake".into(),
                resolved: true,
                modules: vec![
                    module("root", "demo", root.to_path_buf()),
                    module("app", "app", root.join("app")),
                ],
                profiles: vec![
                    BuildProfile {
                        name: "dev".into(),
                        active_by_default: false,
                        source: Some(root.join("pom.xml")),
                        condition: None,
                    },
                    BuildProfile {
                        name: "prod".into(),
                        active_by_default: true,
                        source: Some(root.join("pom.xml")),
                        condition: Some("property release = true".into()),
                    },
                ],
                ..ProjectModel::default()
            },
            syncs: Arc::new(AtomicUsize::new(0)),
            commands: Arc::new(Mutex::new(Vec::new())),
            properties,
        }
    }

    fn choice_file(root: &Path) -> PathBuf {
        root.join(CHOICE_FILE)
    }

    /// The arguments of the chosen profiles, in the shape the real tool writes them.
    fn arguments(selection: &BuildSelection) -> Vec<String> {
        let mut names: Vec<String> = selection.enabled.clone();
        names.extend(selection.disabled.iter().map(|name| format!("!{name}")));
        let mut arguments = if names.is_empty() {
            Vec::new()
        } else {
            vec!["-P".to_owned(), names.join(",")]
        };
        arguments.extend(
            selection
                .properties
                .iter()
                .map(|property| format!("-P{}={}", property.name, property.value)),
        );
        arguments
    }

    fn task(&self, name: &str, program: bool) -> BuildTask {
        BuildTask {
            name: name.into(),
            program,
        }
    }
}

impl BuildSystem for ProfileTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ok_command())
    }

    fn build(&self, root: &Path, _: bool) -> base::Result<ProcessSpec> {
        let arguments = Self::arguments(&self.selection(root));
        self.commands.lock().unwrap().push(arguments.clone());
        Ok(ok_command().args(arguments))
    }

    fn task_tree(&self, _: &ProjectModel) -> Vec<BuildModule> {
        vec![BuildModule {
            id: "root".into(),
            name: "demo".into(),
            groups: vec![BuildGroup {
                name: "Lifecycle".into(),
                tasks: vec![self.task("clean", false), self.task("install", false)],
            }],
        }]
    }

    fn task_in(
        &self,
        _: &Path,
        _: Option<&Module>,
        task: &str,
        _: TaskOptions,
    ) -> base::Result<ProcessSpec> {
        Ok(ok_command().arg(task))
    }

    fn takes_properties(&self) -> bool {
        self.properties
    }

    fn selection(&self, root: &Path) -> BuildSelection {
        fs::read_to_string(Self::choice_file(root))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn choose(&self, root: &Path, selection: &BuildSelection) -> base::Result {
        let text = serde_json::to_string(selection).map_err(|error| error.to_string())?;
        fs::write(Self::choice_file(root), text)?;
        Ok(())
    }
}

#[cfg(windows)]
fn ok_command() -> ProcessSpec {
    ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"])
}

#[cfg(not(windows))]
fn ok_command() -> ProcessSpec {
    ProcessSpec::new("sh").args(["-c", "exit 0"])
}

fn setup(tool: Arc<ProfileTool>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("build-profiles-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn open_root(
    cx: &mut TestAppContext,
    root: PathBuf,
    properties: bool,
) -> (
    WindowHandle<Workspace>,
    Arc<ProfileTool>,
    ui_kit::gpui::Entity<crate::build::BuildPanel>,
) {
    let tool = Arc::new(ProfileTool::new(&root, properties));
    let window = open_window_with(cx, root, setup(tool.clone()));
    let panel = cx.read(|cx| cx.global::<ActiveBuildPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TASKS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the task tree", |cx| {
        !panel.read_with(cx, |panel, cx| panel.labels(cx)).is_empty()
    });
    wait_for(cx, "the first import", |_| {
        tool.syncs.load(Ordering::SeqCst) >= 1
    });
    (window, tool, panel)
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    Arc<ProfileTool>,
    ui_kit::gpui::Entity<crate::build::BuildPanel>,
) {
    open_root(cx, sample_project(name), false)
}

fn labels(
    cx: &TestAppContext,
    panel: &ui_kit::gpui::Entity<crate::build::BuildPanel>,
) -> Vec<String> {
    panel.read_with(cx, |panel, cx| panel.labels(cx))
}

fn choices(
    cx: &TestAppContext,
    panel: &ui_kit::gpui::Entity<crate::build::BuildPanel>,
) -> Vec<(String, ProfileChoice)> {
    panel.read_with(cx, |panel, cx| panel.profile_choices(cx))
}

fn toggle(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &ui_kit::gpui::Entity<crate::build::BuildPanel>,
    name: &str,
) {
    let index = panel
        .read_with(cx, |panel, cx| panel.profile_row(name, cx))
        .unwrap_or_else(|| panic!("no profile {name}"));
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.activate_row(index, window, cx))
        })
        .unwrap();
}

fn run(cx: &TestAppContext) -> ui_kit::gpui::Entity<crate::run::RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

fn wait_idle(cx: &mut TestAppContext) {
    let run = run(cx);
    wait_for(cx, "the build to finish", |cx| {
        !run.read_with(cx, |run, _| run.state().running)
    });
}

/// Builds the project and returns the arguments of the command the tool built.
fn build(cx: &mut TestAppContext, tool: &Arc<ProfileTool>) -> Vec<String> {
    let run = run(cx);
    let start = tool.commands.lock().unwrap().len();
    run.update(cx, |run, cx| run.build_project(cx));
    wait_for(cx, "the build command", |_| {
        tool.commands.lock().unwrap().len() > start
    });
    let arguments = tool.commands.lock().unwrap().last().cloned().unwrap();
    wait_idle(cx);
    arguments
}

#[ui_kit::gpui::test]
fn the_profiles_of_the_model_are_a_node_with_their_conditions(cx: &mut TestAppContext) {
    let (_window, _tool, panel) = open(cx, "build-profiles-list");

    let lines = labels(cx, &panel);
    assert_eq!(lines[0], "Profiles", "the settings come before the modules");
    assert!(lines.contains(&"  dev".to_owned()), "{lines:?}");
    assert!(lines.contains(&"  prod".to_owned()), "{lines:?}");
    assert!(
        lines.contains(&"demo".to_owned()),
        "the modules stay below: {lines:?}"
    );
    let hints = panel.read_with(cx, |panel, cx| panel.hints(cx));
    assert!(
        hints
            .iter()
            .any(|(name, hint)| name == "prod" && hint == "property release = true"),
        "the condition of the build file is shown: {hints:?}"
    );
    assert_eq!(
        panel.read_with(cx, |panel, cx| panel.profile_rows(cx)),
        [("dev".to_owned(), false), ("prod".to_owned(), true),],
        "a profile turned on by the build file is checked"
    );
}

#[ui_kit::gpui::test]
fn a_check_box_cycles_the_three_states_and_each_reaches_the_build(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "build-profiles-command");
    assert_eq!(
        choices(cx, &panel),
        [
            ("dev".to_owned(), ProfileChoice::Default),
            ("prod".to_owned(), ProfileChoice::Default),
        ]
    );
    assert!(
        build(cx, &tool).is_empty(),
        "the build file decides when the user said nothing"
    );

    toggle(cx, window, &panel, "dev");
    assert_eq!(choices(cx, &panel)[0].1, ProfileChoice::On);
    assert_eq!(build(cx, &tool), ["-P".to_owned(), "dev".to_owned()]);
    wait_for(cx, "the project to be imported again", |_| {
        tool.syncs.load(Ordering::SeqCst) >= 2
    });

    toggle(cx, window, &panel, "dev");
    assert_eq!(choices(cx, &panel)[0].1, ProfileChoice::Off);
    assert_eq!(build(cx, &tool), ["-P".to_owned(), "!dev".to_owned()]);

    toggle(cx, window, &panel, "dev");
    assert_eq!(choices(cx, &panel)[0].1, ProfileChoice::Default);
    assert!(build(cx, &tool).is_empty(), "the build file decides again");
}

#[ui_kit::gpui::test]
fn the_choice_survives_reopening_the_project(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "build-profiles-reopen");
    toggle(cx, window, &panel, "prod");
    assert_eq!(choices(cx, &panel)[1].1, ProfileChoice::On);
    assert!(
        ProfileTool::choice_file(&tool.model.root).is_file(),
        "the choice is kept in the project"
    );

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let root = tool.model.root.clone();
    let (_window, _reopened, panel) = open_root(cx, root, false);

    assert_eq!(
        choices(cx, &panel),
        [
            ("dev".to_owned(), ProfileChoice::Default),
            ("prod".to_owned(), ProfileChoice::On),
        ],
        "the new window starts from the choice of the project"
    );
}

#[ui_kit::gpui::test]
fn the_values_of_a_tool_that_takes_them_reach_the_build(cx: &mut TestAppContext) {
    let root = sample_project("build-profiles-values");
    let (window, tool, panel) = open_root(cx, root, true);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TASKS_TOOL))
            .unwrap(),
        "the build window is open"
    );
    assert_eq!(panel.read_with(cx, |panel, _| panel.properties_text()), "");
    panel.update(cx, |panel, cx| panel.set_properties("mode=fast", cx));

    assert_eq!(
        panel.read_with(cx, |panel, _| panel.properties_text()),
        "mode=fast"
    );
    assert_eq!(
        build(cx, &tool),
        ["-Pmode=fast".to_owned()],
        "the values reach the command of the build"
    );
}
