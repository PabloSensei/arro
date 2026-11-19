//! The Modules page of Project Structure without a window: a stand-in project with two
//! modules shows their folders and dependencies, a double click opens a declaration in
//! the build file, and the language level of a module is written to its pom so that one
//! undo takes it back.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang_api::{DocumentEdit, SourceEdit, TextPoint, TextSpan, WorkspaceChange};
use platform::CanonPath;
use project_model::{
    Dependency, DependencyScope, EnvironmentProvider, LanguageLevelEdit, LanguageLevelInfo, Module,
    ModuleDependencies, ProjectEnvironmentInfo, ProjectModel, RuntimeInfo, RuntimeReason,
};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, MouseDownEvent, MouseUpEvent};
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::project_structure::ProjectStructureDialog;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, open_window_with, press,
    project_setup, scratch, wait_for_signal,
};

const PARENT: &str = "com.example:parent:1.0";
const APP: &str = "com.example:app:1.0";
const LEVELS: [&str; 18] = [
    "8", "9", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "20", "21", "22", "23",
    "24", "25",
];

/// A project with a parent module and one module under it. The levels are read from
/// the poms and written back the way the real language does it.
struct FakeProject {
    root: PathBuf,
}

impl FakeProject {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn pom_of(&self, module: Option<&str>) -> PathBuf {
        match module {
            Some(APP) => self.root.join("app").join("pom.xml"),
            _ => self.root.join("pom.xml"),
        }
    }

    fn language_of(&self, file: &Path) -> Option<LanguageLevelInfo> {
        let selected = level_in(file)?;
        Some(LanguageLevelInfo {
            values: LEVELS.iter().map(|level| (*level).to_owned()).collect(),
            selected: Some(selected),
            file: Some(file.to_path_buf()),
            line: Some(0),
        })
    }

    fn modules(&self) -> Vec<Module> {
        let parent = self.root.clone();
        let app = self.root.join("app");
        vec![
            module(
                PARENT,
                "parent",
                ModuleRoots {
                    dir: parent.clone(),
                    sources: parent.join("src/main/source"),
                    tests: parent.join("src/test/source"),
                    resources: parent.join("src/main/resources"),
                    generated: Vec::new(),
                    excluded: vec![parent.join("output")],
                },
            ),
            module(
                APP,
                "app",
                ModuleRoots {
                    dir: app.clone(),
                    sources: app.join("src/main/source"),
                    tests: app.join("src/test/source"),
                    resources: app.join("src/main/resources"),
                    generated: vec![app.join("generated-sources")],
                    excluded: vec![app.join("output")],
                },
            ),
        ]
    }

    fn runtime(&self) -> RuntimeInfo {
        RuntimeInfo {
            name: "sdk-17".to_owned(),
            version: "17.0.2".to_owned(),
            feature: 17,
            home: self.root.join("sdk-17"),
            added: false,
        }
    }
}

struct ModuleRoots {
    dir: PathBuf,
    sources: PathBuf,
    tests: PathBuf,
    resources: PathBuf,
    generated: Vec<PathBuf>,
    excluded: Vec<PathBuf>,
}

fn module(id: &str, name: &str, roots: ModuleRoots) -> Module {
    Module {
        id: id.to_owned(),
        name: name.to_owned(),
        dir: roots.dir.clone(),
        build_file: Some(roots.dir.join("pom.xml")),
        generated_roots: roots.generated,
        excluded_roots: roots.excluded,
        units: vec![
            project_model::Unit {
                name: project_model::MAIN_UNIT.to_owned(),
                source_roots: vec![roots.sources],
                resource_roots: vec![roots.resources],
                ..project_model::Unit::default()
            },
            project_model::Unit {
                name: project_model::TEST_UNIT.to_owned(),
                source_roots: vec![roots.tests],
                ..project_model::Unit::default()
            },
        ],
    }
}

fn level_in(file: &Path) -> Option<String> {
    let text = fs::read_to_string(file).ok()?;
    let marker = "<release>";
    let start = text.find(marker)? + marker.len();
    let end = start + text[start..].find('<')?;
    Some(text[start..end].to_owned())
}

impl EnvironmentProvider for FakeProject {
    fn describe(&self) -> ProjectEnvironmentInfo {
        let runtimes = vec![self.runtime()];
        ProjectEnvironmentInfo {
            runtimes,
            selected: Some(self.root.join("sdk-17")),
            reason: RuntimeReason::User,
            notice: None,
            language: self.language_of(&self.root.join("pom.xml")),
            variables: Vec::new(),
        }
    }

    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String> {
        Ok(RuntimeInfo {
            name: home
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            version: "17.0.2".to_owned(),
            feature: 17,
            home: home.to_path_buf(),
            added: true,
        })
    }

    fn apply(&self, _: Option<&Path>, _: &[PathBuf]) -> Result<ProjectEnvironmentInfo, String> {
        Ok(self.describe())
    }

    fn model(&self) -> Option<Arc<ProjectModel>> {
        Some(Arc::new(ProjectModel {
            root: self.root.clone(),
            tool: "stand-in".to_owned(),
            resolved: true,
            modules: self.modules(),
            dependencies: vec![
                ModuleDependencies {
                    module: PARENT.to_owned(),
                    build_file: Some(self.root.join("pom.xml")),
                    scopes: vec![DependencyScope {
                        name: "compile".to_owned(),
                        roots: vec![Dependency {
                            coordinates: "org.example:library:1.0".to_owned(),
                            scope: "compile".to_owned(),
                            ..Dependency::default()
                        }],
                    }],
                },
                ModuleDependencies {
                    module: APP.to_owned(),
                    build_file: Some(self.root.join("app").join("pom.xml")),
                    scopes: vec![DependencyScope {
                        name: "compile".to_owned(),
                        roots: vec![Dependency {
                            coordinates: "org.example:app-lib:2.0".to_owned(),
                            scope: "compile".to_owned(),
                            ..Dependency::default()
                        }],
                    }],
                },
            ],
            ..ProjectModel::default()
        }))
    }

    fn module_language(&self, module: &str) -> Option<LanguageLevelInfo> {
        self.language_of(&self.pom_of(Some(module)))
    }

    fn language_change(
        &self,
        level: &str,
        module: Option<&str>,
        text: Option<&str>,
    ) -> Result<LanguageLevelEdit, String> {
        let file = self.pom_of(module);
        let text = text
            .map(str::to_owned)
            .or_else(|| fs::read_to_string(&file).ok())
            .ok_or_else(|| format!("Cannot read {}", file.display()))?;
        let marker = "<release>";
        let start = text
            .find(marker)
            .ok_or_else(|| "The pom names no release".to_owned())?
            + marker.len();
        let end = start
            + text[start..]
                .find('<')
                .ok_or_else(|| "The release has no value".to_owned())?;
        let uri = CanonPath::new(&file)
            .map_err(|error| error.to_string())?
            .to_uri();
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(LanguageLevelEdit {
            file: file.clone(),
            line: Some(0),
            summary: format!("Changes release in {name} to {level}"),
            added: false,
            change: WorkspaceChange {
                edits: vec![SourceEdit {
                    uri,
                    edits: vec![DocumentEdit {
                        span: TextSpan {
                            start: point_of(&text, start),
                            end: point_of(&text, end),
                        },
                        text: level.to_owned(),
                    }],
                }],
                renames: Vec::new(),
            },
        })
    }
}

/// The zero-based place of a byte offset, in UTF-16 units of its line.
fn point_of(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[start..offset]
        .chars()
        .map(|character| character.len_utf16() as u32)
        .sum();
    TextPoint { line, character }
}

fn project(root: &Path) {
    fs::create_dir_all(root.join("app/src/main/source")).unwrap();
    fs::create_dir_all(root.join("app/src/test/source")).unwrap();
    fs::create_dir_all(root.join("app/src/main/resources")).unwrap();
    fs::create_dir_all(root.join("app/generated-sources")).unwrap();
    fs::create_dir_all(root.join("src/main/source")).unwrap();
    fs::create_dir_all(root.join("src/test/source")).unwrap();
    fs::create_dir_all(root.join("src/main/resources")).unwrap();
    let parent = "<project>\n  <groupId>com.example</groupId>\n  <artifactId>parent</artifactId>\n  <version>1.0</version>\n  <packaging>pom</packaging>\n  <modules>\n    <module>app</module>\n  </modules>\n  <properties>\n    <release>17</release>\n  </properties>\n</project>\n";
    let app = "<project>\n  <parent>\n    <groupId>com.example</groupId>\n    <artifactId>parent</artifactId>\n    <version>1.0</version>\n  </parent>\n  <artifactId>app</artifactId>\n  <properties>\n    <release>17</release>\n  </properties>\n  <dependencies>\n    <dependency>\n      <groupId>org.example</groupId>\n      <artifactId>app-lib</artifactId>\n      <version>2.0</version>\n    </dependency>\n  </dependencies>\n</project>\n";
    fs::write(root.join("pom.xml"), parent).unwrap();
    fs::write(root.join("app/pom.xml"), app).unwrap();
}

fn window_with_project(
    cx: &mut TestAppContext,
    name: &str,
) -> (PathBuf, WindowHandle<Workspace>, Arc<FakeProject>) {
    let root = scratch(name);
    project(&root);
    let environment = Arc::new(FakeProject::new(root.clone()));
    let mut setup = project_setup(scratch(&format!("{name}-cache")));
    setup.environment = Some(environment.clone());
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    (root, window, environment)
}

fn dialog_of(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<ui_kit::gpui::Entity<ProjectStructureDialog>> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()?;
    view.downcast::<ProjectStructureDialog>().ok()
}

fn wait_for_dialog(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    wait_for_signal(cx, "Project Structure", |cx| {
        dialog_of(cx, window).is_some_and(|dialog| {
            dialog.read_with(cx, |dialog, _| {
                dialog.runtime_count() > 0 && dialog.module_count() == 2
            })
        })
    });
}

fn open_modules_page(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);
    click(cx, window, "project-structure-section-modules");
    draw(cx, window);
}

fn select_app(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "project-structure-module-1");
    draw(cx, window);
    wait_for_signal(cx, "the level of the app module", |cx| {
        dialog_of(cx, window)
            .is_some_and(|dialog| dialog.read_with(cx, |dialog, _| dialog.module_level_count() > 0))
    });
    draw(cx, window);
}

fn double_click<T: 'static>(
    cx: &mut TestAppContext,
    window: WindowHandle<T>,
    selector: &'static str,
) {
    let bounds = bounds_of(cx, window, selector);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let position = bounds.center();
    visual.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers: Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    visual.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers: Modifiers::default(),
        click_count: 2,
    });
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn changing_the_project_level_and_ok_writes_the_root_pom(cx: &mut TestAppContext) {
    let (root, window, _) = window_with_project(cx, "structure-project-level");
    let pom = root.join("pom.xml");

    press(cx, window, "ctrl-alt-shift-s");
    draw(cx, window);
    wait_for_dialog(cx, window);
    draw(cx, window);
    click(cx, window, "project-structure-language");
    draw(cx, window);
    click(cx, window, "project-structure-language-item-21");
    draw(cx, window);
    click(cx, window, "project-structure-ok");

    wait_for_signal(cx, "the new project level", |_| {
        fs::read_to_string(&pom).is_ok_and(|text| text.contains(">21<"))
    });
}

#[ui_kit::gpui::test]
fn the_modules_page_shows_the_modules_their_folders_and_dependencies(cx: &mut TestAppContext) {
    let (_, window, _) = window_with_project(cx, "structure-modules-page");

    open_modules_page(cx, window);

    let _ = bounds_of(cx, window, "project-structure-module-0");
    let _ = bounds_of(cx, window, "project-structure-module-1");
    select_app(cx, window);
    let _ = bounds_of(cx, window, "project-structure-module-folder-0");
    let _ = bounds_of(cx, window, "project-structure-module-folder-1");
    let _ = bounds_of(cx, window, "project-structure-module-folder-2");
    let _ = bounds_of(cx, window, "project-structure-module-folder-3");
    let _ = bounds_of(cx, window, "project-structure-module-folder-4");

    click(cx, window, "project-structure-module-tab-dependencies");
    draw(cx, window);
    let _ = bounds_of(cx, window, "project-structure-module-dependency-0");
}

#[ui_kit::gpui::test]
fn a_double_click_on_a_dependency_opens_its_declaration(cx: &mut TestAppContext) {
    let (root, window, _) = window_with_project(cx, "structure-modules-navigate");

    open_modules_page(cx, window);
    select_app(cx, window);
    click(cx, window, "project-structure-module-tab-dependencies");
    draw(cx, window);

    double_click(cx, window, "project-structure-module-dependency-0");

    let pom = root.join("app").join("pom.xml");
    wait_for_signal(cx, "the declaration in the build file", |cx| {
        active_path(cx, window).as_deref() == Some(pom.as_path())
    });
    assert!(
        dialog_of(cx, window).is_none(),
        "the dialog stayed over the build file"
    );
}

#[ui_kit::gpui::test]
fn changing_the_module_level_and_ok_writes_the_build_file(cx: &mut TestAppContext) {
    let (root, window, _) = window_with_project(cx, "structure-modules-level");
    let pom = root.join("app").join("pom.xml");

    open_modules_page(cx, window);
    select_app(cx, window);
    click(cx, window, "project-structure-module-language");
    draw(cx, window);
    click(cx, window, "project-structure-module-language-item-21");
    draw(cx, window);
    click(cx, window, "project-structure-ok");

    wait_for_signal(cx, "the new level in the pom", |_| {
        fs::read_to_string(&pom).is_ok_and(|text| text.contains(">21<"))
    });
    wait_for_signal(cx, "the closed dialog", |cx| {
        dialog_of(cx, window).is_none()
    });
    draw(cx, window);
    click(cx, window, "editor-body-0");
    press(cx, window, "ctrl-z");
    draw(cx, window);

    let editor = editor_of(cx, window, &pom);
    let text = buffer_of(cx, &editor);
    assert!(
        text.contains(">17<"),
        "Ctrl+Z did not take the level back: {text}"
    );
}

#[ui_kit::gpui::test]
fn apply_writes_the_level_and_keeps_the_dialog_open(cx: &mut TestAppContext) {
    let (root, window, _) = window_with_project(cx, "structure-modules-apply");
    let pom = root.join("app").join("pom.xml");

    open_modules_page(cx, window);
    select_app(cx, window);
    click(cx, window, "project-structure-module-language");
    draw(cx, window);
    click(cx, window, "project-structure-module-language-item-21");
    draw(cx, window);
    click(cx, window, "project-structure-apply");

    wait_for_signal(cx, "the new level in the pom", |_| {
        fs::read_to_string(&pom).is_ok_and(|text| text.contains(">21<"))
    });
    draw(cx, window);

    assert!(dialog_of(cx, window).is_some(), "Apply closed the dialog");
}

#[ui_kit::gpui::test]
fn cancel_after_choosing_a_level_writes_nothing(cx: &mut TestAppContext) {
    let (root, window, _) = window_with_project(cx, "structure-modules-cancel");
    let pom = root.join("app").join("pom.xml");

    open_modules_page(cx, window);
    select_app(cx, window);
    click(cx, window, "project-structure-module-language");
    draw(cx, window);
    click(cx, window, "project-structure-module-language-item-21");
    draw(cx, window);
    click(cx, window, "project-structure-cancel");
    draw(cx, window);

    assert!(
        dialog_of(cx, window).is_none(),
        "Cancel did not close the dialog"
    );
    let text = fs::read_to_string(&pom).unwrap();
    assert!(text.contains(">17<"), "Cancel wrote the level: {text}");
}
