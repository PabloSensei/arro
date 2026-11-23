//! The Scope and the Expand All and Collapse All buttons of the Call Hierarchy window, which
//! the header of the Type Hierarchy has too. The project model names the source and the test
//! folders, and the window hides the calls of the other kind without asking the language again.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use base::Cancel;
use editor_view::Languages;
use lang_api::{
    Answer, CallHierarchyItem, CallHierarchyProvider, CallHierarchyQuery, Confidence, LanguageFn,
    LanguageId, LanguageSupport, Provider, Queries, SourceId, SourceSpan, SyntaxProvider,
};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, TEST_UNIT, Unit};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::call_hierarchy::{ActiveCallHierarchy, CallHierarchyPanel};
use crate::index::SearchSetup;
use crate::outline::SymbolsFor;
use crate::run::ProjectSetup;
use crate::window_tests::{click, draw, open_path, press, scratch, wait_for_signal};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("scope-call-hierarchy");

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

/// A language that answers the call hierarchy from the values a test sets: the roots, and the
/// callers of each function by its name.
struct CallLanguage {
    syntax: NoSyntax,
    roots: Mutex<Vec<CallHierarchyItem>>,
    calls: Mutex<Vec<(String, Vec<CallHierarchyItem>)>>,
}

impl CallLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            roots: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn root(&self, item: CallHierarchyItem) {
        *self.roots.lock().unwrap() = vec![item];
    }

    fn calls_of(&self, name: &str, items: Vec<CallHierarchyItem>) {
        self.calls.lock().unwrap().push((name.to_owned(), items));
    }
}

impl Provider<CallHierarchyQuery, Vec<CallHierarchyItem>> for CallLanguage {
    fn start(
        &self,
        query: CallHierarchyQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<CallHierarchyItem>>,
    ) {
        let items = match query {
            CallHierarchyQuery::Roots(_) => self.roots.lock().unwrap().clone(),
            CallHierarchyQuery::Calls { item, .. } => self
                .calls
                .lock()
                .unwrap()
                .iter()
                .find(|(name, _)| *name == item.name)
                .map(|(_, items)| items.clone())
                .unwrap_or_default(),
        };
        let _ = reply.send(Answer::new(items, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for CallLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-call-hierarchy-scope")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn call_hierarchy(&self) -> Option<&CallHierarchyProvider> {
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
        cache_root: scratch("call-hierarchy-scope-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<CallLanguage>,
    main: PathBuf,
    test: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("call-hierarchy-scope-state");
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

/// A caller the fake language answers with: its name at the start of its file.
fn caller(name: &str, path: &Path) -> CallHierarchyItem {
    CallHierarchyItem {
        name: name.to_owned(),
        detail: None,
        place: Some(SourceSpan {
            uri: uri_of(path),
            span: span_in(&format!("{name}\n"), 0, name.len()),
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

fn panel(cx: &TestAppContext) -> gpui::Entity<CallHierarchyPanel> {
    cx.update(|cx| cx.global::<ActiveCallHierarchy>().0.clone())
}

fn lines(cx: &TestAppContext) -> Vec<String> {
    panel(cx).read_with(cx, |panel, _| panel.lines())
}

/// Waits until the project model of the window is read, so that the scope knows the folders.
fn wait_for_model(cx: &mut TestAppContext) {
    wait_for_signal(cx, "the project model", |cx| {
        cx.read(|cx| {
            cx.try_global::<crate::script::ActiveRunPanel>()
                .and_then(|runs| runs.0.read(cx).model().map(|_| ()))
                .is_some()
        })
    });
}

#[ui_kit::gpui::test]
fn window_tests_call_hierarchy_scope_production_hides_the_caller_of_the_test_folder(
    cx: &mut TestAppContext,
) {
    let root = scratch("call-hierarchy-scope");
    let main = root.join("src").join("sources");
    let test = root.join("src").join("tests");
    let callee = write(&root, "src/sources/Callee.txt", "Callee\n");
    let source_caller = write(&root, "src/sources/Caller.txt", "Caller\n");
    let test_caller = write(&root, "src/tests/CallerTest.txt", "CallerTest\n");
    let language = CallLanguage::new();
    language.root(caller("Callee", &callee));
    language.calls_of(
        "Callee",
        vec![
            caller("Caller", &source_caller),
            caller("CallerTest", &test_caller),
        ],
    );
    let window = open_window(cx, root, language, main, test);
    wait_for_model(cx);
    draw(cx, window);
    open_path(cx, window, &callee);
    press(cx, window, "ctrl-alt-h");
    draw(cx, window);
    wait_for_signal(cx, "the callers of the function", |cx| lines(cx).len() >= 3);
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Callee", "  Caller", "  CallerTest"],
        "All shows the callers of both folders"
    );

    click(cx, window, "call-hierarchy-scope-production");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Callee", "  Caller"],
        "Production hides the caller of the test folder"
    );

    click(cx, window, "call-hierarchy-scope-test");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Callee", "  CallerTest"],
        "Test hides the caller of the source folder and keeps the function itself"
    );

    click(cx, window, "call-hierarchy-scope-all");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Callee", "  Caller", "  CallerTest"],
        "All brings both kinds back"
    );
}

#[ui_kit::gpui::test]
fn window_tests_call_hierarchy_collapse_all_closes_the_root_and_expand_all_opens_it(
    cx: &mut TestAppContext,
) {
    let root = scratch("call-hierarchy-collapse");
    let main = root.join("src").join("sources");
    let test = root.join("src").join("tests");
    let callee = write(&root, "src/sources/Callee.txt", "Callee\n");
    let source_caller = write(&root, "src/sources/Caller.txt", "Caller\n");
    let language = CallLanguage::new();
    language.root(caller("Callee", &callee));
    language.calls_of("Callee", vec![caller("Caller", &source_caller)]);
    let window = open_window(cx, root, language, main, test);
    wait_for_model(cx);
    draw(cx, window);
    open_path(cx, window, &callee);
    press(cx, window, "ctrl-alt-h");
    draw(cx, window);
    wait_for_signal(cx, "the caller of the function", |cx| lines(cx).len() >= 2);
    assert_eq!(lines(cx), ["Callee", "  Caller"]);

    click(cx, window, "call-hierarchy-collapse-all");
    draw(cx, window);
    assert_eq!(lines(cx), ["Callee"], "Collapse All closes the root");
    assert!(
        !panel(cx).read_with(cx, |panel, _| panel.root_expanded()),
        "the root is closed"
    );

    click(cx, window, "call-hierarchy-expand-all");
    draw(cx, window);
    assert_eq!(
        lines(cx),
        ["Callee", "  Caller"],
        "Expand All opens the root again"
    );
}
