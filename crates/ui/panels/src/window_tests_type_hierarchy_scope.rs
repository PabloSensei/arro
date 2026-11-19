//! The Scope buttons of the Hierarchy window: the project model names the source and the
//! test folders, and the window hides the rows of the other kind without asking the
//! language again.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base::Cancel;
use editor_view::Languages;
use lang_api::{
    Answer, Confidence, HierarchyDirection, LanguageFn, LanguageId, LanguageSupport, Provider,
    Queries, SourceId, SourceSpan, SyntaxProvider, TypeHierarchyItem, TypeHierarchyQuery, TypeKind,
};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, TEST_UNIT, Unit};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::outline::SymbolsFor;
use crate::run::ProjectSetup;
use crate::type_hierarchy::ActiveTypeHierarchy;
use crate::window_tests::{bounds_of, click, draw, open_path, press, scratch, wait_for_signal};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("scope-type-hierarchy");

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

/// A language that answers the type hierarchy from the values a test sets.
struct TypeLanguage {
    syntax: NoSyntax,
    roots: Mutex<Vec<TypeHierarchyItem>>,
    neighbours: Mutex<Vec<(String, HierarchyDirection, Vec<TypeHierarchyItem>)>>,
}

impl TypeLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            roots: Mutex::new(Vec::new()),
            neighbours: Mutex::new(Vec::new()),
        })
    }

    fn root(&self, item: TypeHierarchyItem) {
        *self.roots.lock().unwrap() = vec![item];
    }

    fn subtypes(&self, name: &str, items: Vec<TypeHierarchyItem>) {
        self.neighbours.lock().unwrap().push((
            name.to_owned(),
            HierarchyDirection::Subtypes,
            items,
        ));
    }
}

impl Provider<TypeHierarchyQuery, Vec<TypeHierarchyItem>> for TypeLanguage {
    fn start(
        &self,
        query: TypeHierarchyQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<TypeHierarchyItem>>,
    ) {
        let items = match query {
            TypeHierarchyQuery::Root(_) => self.roots.lock().unwrap().clone(),
            TypeHierarchyQuery::Neighbours { item, direction } => self
                .neighbours
                .lock()
                .unwrap()
                .iter()
                .find(|(name, found, _)| *name == item.name && *found == direction)
                .map(|(_, _, items)| items.clone())
                .unwrap_or_default(),
        };
        let _ = reply.send(Answer::new(items, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for TypeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-type-hierarchy-scope")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn type_hierarchy(&self) -> Option<&lang_api::TypeHierarchyProvider> {
        Some(self)
    }
}

/// The build tool of the test: the model names the source and the test folders of the
/// module, as a real tool does.
struct ModelTool {
    model: ProjectModel,
}

impl BuildSystem for ModelTool {
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

fn setup(root: &Path, main: PathBuf, test: PathBuf) -> ProjectSetup {
    let module = Module {
        id: "root".to_owned(),
        name: "demo".to_owned(),
        dir: root.to_path_buf(),
        units: vec![
            Unit {
                name: MAIN_UNIT.to_owned(),
                source_roots: vec![main],
                ..Unit::default()
            },
            Unit {
                name: TEST_UNIT.to_owned(),
                source_roots: vec![test],
                ..Unit::default()
            },
        ],
        ..Module::default()
    };
    let model = ProjectModel {
        root: root.to_path_buf(),
        tool: "fake".to_owned(),
        resolved: true,
        modules: vec![module],
        ..ProjectModel::default()
    };
    ProjectSetup {
        tool: Some(Arc::new(ModelTool { model })),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("type-hierarchy-scope-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<TypeLanguage>,
    main: PathBuf,
    test: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("type-hierarchy-scope-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        cx.set_global(SymbolsFor(|_| None));
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root: root.clone(),
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    let setup = setup(&root, main, test);
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// A type the fake language answers with, at the declaration of its name.
fn item(name: &str, path: &Path, text: &str) -> TypeHierarchyItem {
    let offset = text
        .find("class ")
        .map_or(0, |start| start + "class ".len());
    TypeHierarchyItem {
        name: name.to_owned(),
        detail: None,
        kind: TypeKind::Class,
        abstract_type: false,
        place: Some(SourceSpan {
            uri: uri_of(path),
            span: span_in(text, offset, offset + name.len()),
        }),
        handle: name.to_owned(),
    }
}

fn write(root: &Path, file: &str, text: &str) -> PathBuf {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

fn panel(cx: &TestAppContext) -> gpui::Entity<crate::type_hierarchy::TypeHierarchyPanel> {
    cx.update(|cx| cx.global::<ActiveTypeHierarchy>().0.clone())
}

fn lines(cx: &TestAppContext) -> Vec<String> {
    panel(cx).read_with(cx, |panel, _| panel.lines())
}

#[ui_kit::gpui::test]
fn scope_production_hides_tests_and_test_hides_sources(cx: &mut TestAppContext) {
    let root = scratch("type-hierarchy-scope");
    let main = root.join("src").join("sources");
    let test = root.join("src").join("tests");
    let outside = write(&root, "libs/Shape.txt", "class Shape {\n}\n");
    let circle = write(&root, "src/sources/Circle.txt", "class Circle {\n}\n");
    let test_circle = write(&root, "src/tests/CircleTest.txt", "class CircleTest {\n}\n");
    let language = TypeLanguage::new();
    language.root(item("Shape", &outside, "class Shape {\n}\n"));
    language.subtypes(
        "Shape",
        vec![
            item("Circle", &circle, "class Circle {\n}\n"),
            item("CircleTest", &test_circle, "class CircleTest {\n}\n"),
        ],
    );
    let window = open_window(cx, root.clone(), language, main, test);
    wait_for_signal(cx, "the project model", |cx| {
        cx.read(|cx| {
            cx.try_global::<crate::script::ActiveRunPanel>()
                .and_then(|runs| runs.0.read(cx).model().map(|_| ()))
                .is_some()
        })
    });
    draw(cx, window);
    open_path(cx, window, &circle);
    press(cx, window, "ctrl-h");
    draw(cx, window);
    click(cx, window, "type-hierarchy-view-subtypes");
    draw(cx, window);
    wait_for_signal(cx, "the subtypes of the type", |cx| {
        lines(cx).iter().any(|line| line.trim() == "CircleTest")
    });

    assert_eq!(
        lines(cx),
        ["Shape", "  Circle", "  CircleTest"],
        "All shows the types of both folders"
    );

    click(cx, window, "type-hierarchy-scope-production");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Shape", "  Circle"],
        "Production hides the type of the test folder"
    );
    assert!(
        bounds_of(cx, window, "type-hierarchy-scope-production")
            .size
            .width
            > px(0.),
        "the chosen scope stays in the header"
    );

    click(cx, window, "type-hierarchy-scope-test");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Shape", "  CircleTest"],
        "Test hides the type of the source folder"
    );

    click(cx, window, "type-hierarchy-scope-all");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Shape", "  Circle", "  CircleTest"],
        "All brings both kinds back"
    );
}
