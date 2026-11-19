//! The read tools an agent asks the window for: the uses of a symbol, the text of a file
//! with unsaved edits, the changes of the repository and the structure of the project.
//!
//! The stand-in language and the stand-in build tool need no server. The project structure
//! on a real build is the ignored test at the end:
//!
//! `cargo test -p panels window_tests_ai_tools -- --ignored --test-threads=1`

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::Cancel;
use lang_api::SourceSpan;
use proc_sup::ProcessSpec;
use project_model::{
    BuildSystem, Dependency, DependencyScope, EnvironmentProvider, Module, ModuleDependencies,
    ProjectEnvironmentInfo, ProjectModel, RuntimeInfo, RuntimeReason, Unit,
};
use serde_json::json;
use tool_gateway::{AuditLog, SessionScope, ToolGateway};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::run::ProjectSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, open_window_with, project_setup, scratch,
};
use crate::window_tests_git::repository_with_origin;
use crate::window_tests_lang::{
    FakeLanguage, open_window_with as open_language_window, project, span_in, uri_of,
};
use crate::window_tests_ui::wait_for;

/// The gateway of the window, as the MCP server builds it for a session.
fn gateway(cx: &TestAppContext) -> ToolGateway {
    let tools = cx.read(|cx| cx.global::<crate::AgentTools>().0.clone());
    ToolGateway::new(tools, AuditLog::default())
}

fn scope(root: &Path) -> SessionScope {
    SessionScope {
        session: "window-tests-ai-tools".to_string(),
        project: root.to_path_buf(),
        cwd: root.to_path_buf(),
    }
}

/// The window writes what the tools read: called every frame and by the tests.
fn sync(cx: &mut TestAppContext) {
    let panel = cx.update(|cx| cx.global::<crate::agents::ActiveAgents>().0.clone());
    panel.update(cx, |panel, cx| panel.sync_tools(cx));
}

#[ui_kit::gpui::test]
fn find_usages_reaches_a_reference_in_another_file(cx: &mut TestAppContext) {
    let root = project(
        "ai-tools-usages",
        &[
            ("src/Main.txt", "greet()\n"),
            ("src/Other.txt", "greet()\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&main),
            span: span_in("greet()\n", 0, 5),
        },
        SourceSpan {
            uri: uri_of(&other),
            span: span_in("greet()\n", 0, 5),
        },
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    sync(cx);

    let answer = gateway(cx)
        .call(
            &scope(&root),
            "find_usages",
            &json!({"path": "src/Main.txt", "line": 1, "column": 1}),
        )
        .expect("find_usages answers");

    assert!(answer.contains("2 usages of "), "{answer}");
    assert!(answer.contains("Main.txt:1:1: greet()"), "{answer}");
    assert!(
        answer.contains("Other.txt:1:1: greet()"),
        "a use in another file is in the answer: {answer}"
    );
    assert!(
        answer.contains("source: fake-server, confidence: certain"),
        "{answer}"
    );
}

#[ui_kit::gpui::test]
fn read_file_shows_an_unsaved_edit(cx: &mut TestAppContext) {
    let root = project("ai-tools-read", &[("src/Main.txt", "one\ntwo\n")]);
    let main = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.replace_text(0..3, "ONE", cx));
    assert_eq!(buffer_of(cx, &view), "ONE\ntwo\n");
    cx.run_until_parked();
    sync(cx);

    let answer = gateway(cx)
        .call(
            &scope(&root),
            "read_file",
            &json!({"path": "src/Main.txt", "start_line": 1, "end_line": 1}),
        )
        .expect("read_file answers");

    assert!(
        answer.contains("lines 1-1 of 2, unsaved changes"),
        "{answer}"
    );
    assert!(
        answer.contains("1|ONE"),
        "the editor text is read: {answer}"
    );
    assert!(
        !answer.contains("1|one"),
        "the disk text is not shown: {answer}"
    );
}

#[ui_kit::gpui::test]
fn the_vcs_tool_sees_a_changed_file_and_its_diff(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("ai-tools-vcs");
    std::fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    let window: WindowHandle<Workspace> =
        open_window_with(cx, root.clone(), project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    sync(cx);

    let answer = gateway(cx)
        .call(
            &scope(&root),
            "get_vcs_changes",
            &json!({"path": "main.txt"}),
        )
        .expect("get_vcs_changes answers");

    assert!(answer.contains("1 changed file"), "{answer}");
    assert!(answer.contains("modified: main.txt"), "{answer}");
    assert!(answer.contains("diff main.txt:"), "{answer}");
    assert!(answer.contains("-alpha"), "{answer}");
    assert!(answer.contains("+ALPHA"), "{answer}");
}

/// A build tool that answers from memory, so the window has a model without a real one.
struct FakeTool {
    model: ProjectModel,
}

impl FakeTool {
    fn new(root: &Path) -> Self {
        let source = |path: &str| PathBuf::from(path);
        let module = Module {
            id: "app".to_string(),
            name: "app".to_string(),
            dir: root.to_path_buf(),
            units: vec![
                Unit {
                    name: "main".to_string(),
                    source_roots: vec![source("src/main/source")],
                    ..Unit::default()
                },
                Unit {
                    name: "test".to_string(),
                    source_roots: vec![source("src/test/source")],
                    ..Unit::default()
                },
            ],
            ..Module::default()
        };
        let dependencies = ModuleDependencies {
            module: "app".to_string(),
            build_file: None,
            scopes: vec![DependencyScope {
                name: "compile".to_string(),
                roots: vec![Dependency {
                    coordinates: "org.example:library:1.2".to_string(),
                    ..Dependency::default()
                }],
            }],
        };
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "Stand-In".to_string(),
                tool_version: Some("1.0".to_string()),
                modules: vec![module],
                dependencies: vec![dependencies],
                profiles: Vec::new(),
                resolved: true,
                problems: Vec::new(),
            },
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
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// A runtime and a language level without an SDK on disk.
struct FakeEnvironment {
    home: PathBuf,
    version: String,
}

impl EnvironmentProvider for FakeEnvironment {
    fn describe(&self) -> ProjectEnvironmentInfo {
        let runtime = RuntimeInfo {
            name: self.version.clone(),
            version: format!("{}.0", self.version),
            feature: self.version.parse().unwrap_or(0),
            home: self.home.clone(),
            added: false,
        };
        ProjectEnvironmentInfo {
            runtimes: vec![runtime],
            selected: Some(self.home.clone()),
            reason: RuntimeReason::Project,
            notice: None,
            language: Some(project_model::LanguageLevelInfo {
                values: vec![self.version.clone()],
                selected: Some(self.version.clone()),
                file: None,
                line: None,
            }),
            variables: Vec::new(),
        }
    }

    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String> {
        Ok(RuntimeInfo {
            name: self.version.clone(),
            version: format!("{}.0", self.version),
            feature: self.version.parse().unwrap_or(0),
            home: home.to_path_buf(),
            added: false,
        })
    }

    fn apply(&self, _: Option<&Path>, _: &[PathBuf]) -> Result<ProjectEnvironmentInfo, String> {
        Ok(self.describe())
    }
}

fn structure_setup(root: &Path, environment: Arc<dyn EnvironmentProvider>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(FakeTool::new(root))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("ai-tools-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: Some(environment),
        debug: None,
    }
}

fn wait_for_model(cx: &mut TestAppContext) {
    wait_for(cx, "the project model", |cx| {
        let panel = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        panel.read_with(cx, |panel, _| panel.model().is_some())
    });
}

#[ui_kit::gpui::test]
fn the_project_structure_names_the_module_the_roots_and_the_runtime(cx: &mut TestAppContext) {
    let root = scratch("ai-tools-structure");
    std::fs::create_dir_all(root.join("src/main/source")).unwrap();
    let environment = Arc::new(FakeEnvironment {
        home: root.join("jdk-21"),
        version: "21".to_string(),
    });
    let environment: Arc<dyn EnvironmentProvider> = environment;
    let setup = structure_setup(&root, environment);
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_model(cx);
    sync(cx);

    let answer = gateway(cx)
        .call(&scope(&root), "get_project_structure", &json!({}))
        .expect("get_project_structure answers");

    assert!(
        answer.contains("project: Stand-In 1.0, resolved, 1 modules"),
        "{answer}"
    );
    assert!(answer.contains("runtime: 21 (21.0)"), "{answer}");
    assert!(answer.contains("language level: 21"), "{answer}");
    assert!(answer.contains("  app at ."), "{answer}");
    assert!(answer.contains("sources: src/main/source"), "{answer}");
    assert!(answer.contains("tests: src/test/source"), "{answer}");
    assert!(
        answer.contains("dependencies: org.example:library:1.2"),
        "{answer}"
    );
}

/// The structure of a copy of the sample project, read from its build files: the module,
/// its source and test roots, and the runtime it builds with.
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS"]
fn the_project_structure_reads_a_real_project(cx: &mut TestAppContext) {
    let corpus = window_language::corpus_dir();
    let root = scratch("ai-tools-project").join("petclinic");
    window_language::prepare_project(&corpus.join("src/petclinic"), &root);
    let home = corpus.join("jdk").join("21");
    let model = window_language::scanned_project_model(&root, &scratch("ai-tools-project-cache"))
        .expect("the build files give a model");
    let module = model.modules.first().expect("the project has a module");
    let root_of = |name: &str| {
        module
            .units
            .iter()
            .find(|unit| unit.name == name)
            .and_then(|unit| unit.source_roots.first())
            .cloned()
    };
    let sources = root_of("main").expect("the module has a source root");
    let tests = root_of("test");
    struct RealTool(ProjectModel);
    impl BuildSystem for RealTool {
        fn detect(&self, _: &Path) -> bool {
            true
        }
        fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
            Ok(self.0.clone())
        }
        fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
            Ok(self.0.clone())
        }
        fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
            Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
        }
    }
    let environment = Arc::new(FakeEnvironment {
        home,
        version: "21".to_string(),
    });
    let setup = ProjectSetup {
        tool: Some(Arc::new(RealTool(model))),
        cache_root: scratch("ai-tools-project-model"),
        environment: Some(environment),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        debug: None,
    };
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_model(cx);
    sync(cx);

    let answer = gateway(cx)
        .call(&scope(&root), "get_project_structure", &json!({}))
        .expect("get_project_structure answers");

    assert!(answer.contains("modules ("), "{answer}");
    let shown = |path: &Path| {
        path.strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    assert!(
        answer.contains(&format!("sources: {}", shown(&sources))),
        "{answer}"
    );
    if let Some(tests) = tests {
        assert!(
            answer.contains(&format!("tests: {}", shown(&tests))),
            "{answer}"
        );
    }
    assert!(answer.contains("runtime: 21"), "{answer}");
}
