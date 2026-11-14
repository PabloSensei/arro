//! The dependencies of the build window and the Dependency Analyzer without a window: a
//! stand-in tool reports a model with two paths to one artifact and a build file that
//! declares the first of them. The analyzer searches, filters, copies and excludes, and
//! the window makes the exclusion in the editor of the build file.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Coordinates, Dependency, DependencyScope,
    Exclusion, FileEdit, Module, ModuleDependencies, ProjectModel, TaskOptions,
};
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{TASKS_TOOL, Workspace};

use crate::build::TOOLTIPS;
use crate::run::ProjectSetup;
use crate::script::ActiveBuildPanel;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_window_with, press, sample_project, scratch,
};
use crate::window_tests_run::wait_for;

const POM: &str = r#"<project>
    <dependencies>
        <dependency>
            <groupId>org.springframework.boot</groupId>
            <artifactId>spring-boot-starter-web</artifactId>
            <version>3.5.0</version>
        </dependency>
    </dependencies>
</project>
"#;

/// The goal of the analyzer: the second version loses to the one spelled below.
const STARTER: &str = "org.springframework.boot:spring-boot-starter-web:3.5.0";
const WEB: &str = "org.springframework:spring-web:6.2.0";
const LOST: &str = "com.fasterxml.jackson.core:jackson-databind:2.15.0";
const CHOSEN: &str = "com.fasterxml.jackson.core:jackson-databind:2.17.0";
const TEST_LIB: &str = "org.example:test-support-lib:5.11.0";

struct FakeTool {
    model: ProjectModel,
    syncs: Arc<AtomicUsize>,
    excluded: Arc<Mutex<Vec<Exclusion>>>,
}

fn node(coordinates: &str, scope: &str, children: Vec<Dependency>) -> Dependency {
    Dependency {
        coordinates: coordinates.into(),
        scope: scope.into(),
        children,
        ..Dependency::default()
    }
}

impl FakeTool {
    fn new(root: &Path) -> Self {
        let mut lost = node(LOST, "compile", Vec::new());
        lost.winner = Some(CHOSEN.into());
        let app = Module {
            id: "app".into(),
            name: "app".into(),
            dir: root.join("app"),
            ..Module::default()
        };
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fake".into(),
                resolved: true,
                modules: vec![
                    Module {
                        id: "demo".into(),
                        name: "demo".into(),
                        dir: root.to_path_buf(),
                        ..Module::default()
                    },
                    app,
                ],
                dependencies: vec![ModuleDependencies {
                    module: "app".into(),
                    build_file: Some(root.join("app").join("pom.xml")),
                    scopes: vec![
                        DependencyScope {
                            name: "compile".into(),
                            roots: vec![
                                node(STARTER, "compile", vec![node(WEB, "compile", vec![lost])]),
                                node(CHOSEN, "compile", Vec::new()),
                            ],
                        },
                        DependencyScope {
                            name: "test".into(),
                            roots: vec![node(TEST_LIB, "test", Vec::new())],
                        },
                    ],
                }],
                ..ProjectModel::default()
            },
            syncs: Arc::new(AtomicUsize::new(0)),
            excluded: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl BuildSystem for FakeTool {
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
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn task_tree(&self, _: &ProjectModel) -> Vec<BuildModule> {
        let task = |name: &str| BuildTask {
            name: name.into(),
            program: false,
        };
        vec![
            BuildModule {
                id: "demo".into(),
                name: "demo".into(),
                groups: vec![BuildGroup {
                    name: "Lifecycle".into(),
                    tasks: vec![task("clean")],
                }],
            },
            BuildModule {
                id: "app".into(),
                name: "app".into(),
                groups: vec![BuildGroup {
                    name: "Lifecycle".into(),
                    tasks: vec![task("compile"), task("test")],
                }],
            },
        ]
    }

    fn task_in(
        &self,
        _: &Path,
        _: Option<&Module>,
        _: &str,
        _: TaskOptions,
    ) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", "task"]))
    }

    /// The exclusion of the stand-in tool: the text goes before the closing tag of the
    /// dependency the artifact is declared in.
    fn exclude(
        &self,
        _: &Path,
        _: &Module,
        build_file: &Path,
        request: &Exclusion,
        text: &str,
    ) -> base::Result<Option<FileEdit>> {
        self.excluded.lock().unwrap().push(request.clone());
        let declared = Coordinates::parse(&request.declared).unwrap();
        let needle = format!("<artifactId>{}</artifactId>", declared.artifact);
        let Some(at) = text.find(&needle) else {
            return Ok(None);
        };
        let Some(close) = text[at..].find("</dependency>").map(|found| at + found) else {
            return Ok(None);
        };
        Ok(Some(FileEdit {
            path: build_file.to_owned(),
            start: close,
            end: close,
            text: format!(
                "<exclusion>\n                <groupId>{}</groupId>\n                <artifactId>{}</artifactId>\n            </exclusion>\n        ",
                request.group, request.artifact
            ),
        }))
    }
}

fn setup(tool: Arc<FakeTool>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(tool),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("dependencies-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn project(name: &str) -> PathBuf {
    let root = sample_project(name);
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app").join("pom.xml"), POM).unwrap();
    root
}

fn open(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    Arc<FakeTool>,
    Entity<crate::build::BuildPanel>,
) {
    let root = project(name);
    let tool = Arc::new(FakeTool::new(&root));
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
    (window, tool, panel)
}

fn labels(cx: &TestAppContext, panel: &Entity<crate::build::BuildPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, cx| panel.labels(cx))
}

#[ui_kit::gpui::test]
fn a_module_shows_its_dependencies_by_scope_with_the_version_a_conflict_dropped(
    cx: &mut TestAppContext,
) {
    let (_window, _tool, panel) = open(cx, "dependencies-tree");

    let lines = labels(cx, &panel);
    assert!(
        lines.contains(&"  Dependencies".to_owned()),
        "the module has the node of its dependencies: {lines:?}"
    );
    assert!(lines.contains(&"    Compile".to_owned()), "{lines:?}");
    assert!(lines.contains(&"    Test".to_owned()), "{lines:?}");
    assert!(
        lines.contains(&"      spring-boot-starter-web:3.5.0".to_owned()),
        "the first-level dependency stays whole: {lines:?}"
    );
    assert!(
        lines.contains(&"        spring-web:6.2.0".to_owned()),
        "the tree keeps what the starter pulls in: {lines:?}"
    );
    assert!(
        lines.contains(&"          jackson-databind:2.15.0".to_owned()),
        "the edge that lost is still shown: {lines:?}"
    );
    let hints = panel.read_with(cx, |panel, cx| panel.hints(cx));
    assert!(
        hints.contains(&(
            "jackson-databind:2.15.0".to_owned(),
            "omitted for conflict with 2.17.0".to_owned()
        )),
        "the lost version says which one won: {hints:?}"
    );
}

#[ui_kit::gpui::test]
fn the_analyzer_finds_artifacts_filters_conflicts_and_lists_their_paths(cx: &mut TestAppContext) {
    let (_window, _tool, panel) = open(cx, "dependencies-analyzer");
    panel.update(cx, |panel, cx| panel.show_analyzer_now(cx));

    let listed = panel.update(cx, |panel, cx| panel.analyzer_labels(cx));
    assert!(
        listed.contains(&"org.springframework.boot:spring-boot-starter-web:3.5.0".to_owned()),
        "{listed:?}"
    );
    assert!(
        listed.contains(&"com.fasterxml.jackson.core:jackson-databind:2.17.0".to_owned()),
        "{listed:?}"
    );

    panel.update(cx, |panel, cx| panel.search_for("jackson", cx));
    let found = panel.update(cx, |panel, cx| panel.analyzer_labels(cx));
    assert_eq!(
        found,
        ["com.fasterxml.jackson.core:jackson-databind:2.17.0".to_owned()],
        "the search keeps the artifacts the query names"
    );

    panel.update(cx, |panel, cx| panel.search_for("", cx));
    panel.update(cx, |panel, cx| panel.set_only_conflicts(true, cx));
    let conflicts = panel.update(cx, |panel, cx| panel.analyzer_labels(cx));
    assert_eq!(
        conflicts,
        ["com.fasterxml.jackson.core:jackson-databind:2.17.0".to_owned()],
        "only the artifact with a losing version stays"
    );
    assert!(panel.read_with(cx, |panel, _| panel.only_conflicts()));

    panel.update(cx, |panel, cx| panel.select_artifact(0, cx));
    let details = panel.update(cx, |panel, cx| panel.analyzer_details(cx));
    assert!(
        details
            .contains(&"spring-boot-starter-web → spring-web → jackson-databind:2.15.0".to_owned()),
        "the paths name every step: {details:?}"
    );
    assert!(
        details
            .iter()
            .any(|line| line.contains("omitted for conflict with 2.17.0")),
        "{details:?}"
    );
    assert!(
        details.iter().any(|line| line.trim() == "app [compile]"),
        "the path names its module and scope: {details:?}"
    );
}

#[ui_kit::gpui::test]
fn exclude_edits_the_build_file_in_the_editor_and_reads_the_model_again(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "dependencies-exclude");
    panel.update(cx, |panel, cx| panel.show_analyzer_now(cx));
    panel.update(cx, |panel, cx| panel.search_for("jackson", cx));
    panel.update(cx, |panel, cx| panel.select_artifact(0, cx));
    let path = window
        .read_with(cx, |workspace, _| {
            workspace.root().join("app").join("pom.xml")
        })
        .unwrap();

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.exclude_selected(window, cx))
        })
        .unwrap();

    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.message()),
        None,
        "the window found the declaration"
    );
    wait_for(cx, "the exclusion of the tool", |_| {
        !tool.excluded.lock().unwrap().is_empty()
    });
    let requested = tool.excluded.lock().unwrap().clone();
    assert_eq!(requested.len(), 1);
    assert_eq!(
        requested[0],
        Exclusion {
            group: "com.fasterxml.jackson.core".into(),
            artifact: "jackson-databind".into(),
            declared: STARTER.into(),
        },
        "the exclusion goes into the first-level dependency of the path"
    );

    draw(cx, window);
    let editor = editor_of(cx, window, &path);
    let text = buffer_of(cx, &editor);
    let declared = text.find("<artifactId>spring-boot-starter-web").unwrap();
    let exclusion = text
        .find("<exclusion>")
        .expect("the exclusion is in the text");
    assert!(
        exclusion > declared,
        "it goes inside the declared dependency"
    );
    assert!(text[exclusion..].contains("<artifactId>jackson-databind</artifactId>"));
    assert!(
        exclusion < text[exclusion..].find("</dependency>").unwrap() + exclusion,
        "the exclusion stays inside the dependency element"
    );

    wait_for(cx, "the model read again", |_| {
        tool.syncs.load(Ordering::SeqCst) >= 2
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.message()),
        None,
        "the window reports no problem with the edit"
    );

    // The edit came through the editor, so one undo takes it back.
    press(cx, window, "ctrl-z");
    let reverted = buffer_of(cx, &editor);
    assert!(
        !reverted.contains("<exclusion>"),
        "undo returns the text of the build file: {reverted}"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_on_a_dependency_opens_its_declaration(cx: &mut TestAppContext) {
    let (window, _tool, panel) = open(cx, "dependencies-declaration");
    let lines = labels(cx, &panel);
    let row = lines
        .iter()
        .position(|line| line.trim() == "spring-boot-starter-web:3.5.0")
        .expect("the first-level dependency is in the tree");
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.activate_row(row, window, cx))
        })
        .unwrap();

    let path = window
        .read_with(cx, |workspace, _| {
            workspace.root().join("app").join("pom.xml")
        })
        .unwrap();
    wait_for(cx, "the declaration in the editor", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.view_of(&path))
            .ok()
            .flatten()
            .is_some()
    });
    let editor = editor_of(cx, window, &path);
    wait_for(cx, "the text of the build file", |cx| {
        editor.read_with(cx, |editor, _| editor.text().is_some())
    });
    let text = buffer_of(cx, &editor);
    let caret = editor
        .read_with(cx, |editor, _| editor.head_offset())
        .expect("the editor has a caret");
    assert!(
        text[caret..].starts_with("<artifactId>spring-boot-starter-web"),
        "the caret stands on the declaration, not at the start: {caret}"
    );
}

#[ui_kit::gpui::test]
fn a_first_level_dependency_has_nothing_to_be_excluded_from(cx: &mut TestAppContext) {
    let (window, tool, panel) = open(cx, "dependencies-direct");
    panel.update(cx, |panel, cx| panel.show_analyzer_now(cx));
    panel.update(cx, |panel, cx| {
        panel.search_for("spring-boot-starter-web", cx)
    });
    panel.update(cx, |panel, cx| panel.select_artifact(0, cx));

    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.exclude_selected(window, cx))
        })
        .unwrap();
    cx.run_until_parked();

    assert!(tool.excluded.lock().unwrap().is_empty());
    assert!(
        panel
            .read_with(cx, |panel, _| panel.message())
            .is_some_and(|message| message.contains("first-level")),
        "the analyzer says why there is no exclusion"
    );
}

#[ui_kit::gpui::test]
fn find_action_opens_the_analyzer(cx: &mut TestAppContext) {
    let (window, _tool, panel) = open(cx, "dependencies-find-action");
    assert!(!panel.read_with(cx, |panel, _| panel.analyzer_open()));

    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(crate::build::ANALYZE_COMMAND, window, cx)
        })
        .unwrap();

    assert!(ran, "the command of Find Action is registered");
    draw(cx, window);
    assert!(
        panel.read_with(cx, |panel, _| panel.analyzer_open()),
        "the command opens the analyzer in the build window"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TASKS_TOOL))
            .unwrap()
    );
}

#[ui_kit::gpui::test]
fn the_analyzer_button_has_a_hint(cx: &mut TestAppContext) {
    let (window, _tool, _panel) = open(cx, "dependencies-tooltips");
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, _| window.refresh());
    assert!(TOOLTIPS.iter().any(|(id, _)| *id == "build-analyze"));
    assert!(
        visual.debug_bounds("build-analyze").is_some(),
        "the analyzer button is on screen"
    );
}
