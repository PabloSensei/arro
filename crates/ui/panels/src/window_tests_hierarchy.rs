//! The editor features around the structure of the code: Override and Implement, the
//! call hierarchy, Type Declaration, Quick Definition, Copy Reference and the signatures
//! of the Structure tool window.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    AddOverridableMethodsProvider, AddOverridableMethodsRequest, Answer, CallDirection,
    CallHierarchyItem, CallHierarchyProvider, CallHierarchyQuery, Confidence, DefinitionPreview,
    DefinitionPreviewProvider, DefinitionPreviewQuery, DocumentEdit, FileSymbol,
    FileSymbolsProvider, FileSymbolsQuery, LanguageFn, LanguageId, LanguageSupport,
    NavigationProvider, NavigationQuery, OverridableMethod, OverridableMethods,
    OverridableMethodsProvider, OverridableMethodsQuery, Provider, Queries, ReferenceQuery,
    ReferenceStringProvider, SemanticServer, SourceId, SourceSpan, SymbolKind, SyntaxProvider,
    WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::call_hierarchy::ActiveCallHierarchy;
use crate::index::SearchSetup;
use crate::outline::SymbolsFor;
use crate::structure::ActiveStructure;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch,
    type_text,
};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-hierarchy");

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

/// A language that answers every new request from values the test sets.
pub(crate) struct HierarchyLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    overridable: Mutex<Vec<OverridableMethod>>,
    add: Mutex<Option<WorkspaceChange>>,
    only_abstract: Mutex<Option<bool>>,
    preview: Mutex<Option<DefinitionPreview>>,
    reference: Mutex<Option<String>>,
    type_definitions: Mutex<Vec<SourceSpan>>,
    pub(crate) call_roots: Mutex<Vec<CallHierarchyItem>>,
    pub(crate) calls: Mutex<Vec<CallHierarchyItem>>,
    file_symbols: Mutex<Vec<FileSymbol>>,
}

impl HierarchyLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            overridable: Mutex::new(Vec::new()),
            add: Mutex::new(None),
            only_abstract: Mutex::new(None),
            preview: Mutex::new(None),
            reference: Mutex::new(None),
            type_definitions: Mutex::new(Vec::new()),
            call_roots: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            file_symbols: Mutex::new(Vec::new()),
        })
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for HierarchyLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .contains_key(uri)
            .then_some(1)
    }
}

impl Provider<OverridableMethodsQuery, OverridableMethods> for HierarchyLanguage {
    fn start(
        &self,
        query: OverridableMethodsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<OverridableMethods>,
    ) {
        *self.only_abstract.lock().unwrap() = Some(query.only_abstract);
        let methods = self
            .overridable
            .lock()
            .unwrap()
            .iter()
            .filter(|method| !query.only_abstract || method.abstract_method)
            .cloned()
            .collect();
        let _ = reply.send(self.answer(OverridableMethods {
            owner_type: "Circle".to_owned(),
            methods,
        }));
    }
}

impl Provider<AddOverridableMethodsRequest, WorkspaceChange> for HierarchyLanguage {
    fn start(
        &self,
        _: AddOverridableMethodsRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<WorkspaceChange>,
    ) {
        let change = self.add.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(change));
    }
}

impl Provider<DefinitionPreviewQuery, DefinitionPreview> for HierarchyLanguage {
    fn start(
        &self,
        _: DefinitionPreviewQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<DefinitionPreview>,
    ) {
        let preview = self.preview.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(preview));
    }
}

impl Provider<ReferenceQuery, String> for HierarchyLanguage {
    fn start(&self, _: ReferenceQuery, _: base::Cancel, mut reply: lang_api::Reply<String>) {
        let reference = self.reference.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(reference));
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for HierarchyLanguage {
    fn start(
        &self,
        query: NavigationQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<SourceSpan>>,
    ) {
        let spans = match query.kind {
            lang_api::NavigationKind::TypeDefinition => {
                self.type_definitions.lock().unwrap().clone()
            }
            _ => Vec::new(),
        };
        let _ = reply.send(self.answer(spans));
    }
}

impl Provider<CallHierarchyQuery, Vec<CallHierarchyItem>> for HierarchyLanguage {
    fn start(
        &self,
        query: CallHierarchyQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<CallHierarchyItem>>,
    ) {
        let items = match query {
            CallHierarchyQuery::Roots(_) => self.call_roots.lock().unwrap().clone(),
            CallHierarchyQuery::Calls { .. } => self.calls.lock().unwrap().clone(),
        };
        let _ = reply.send(self.answer(items));
    }
}

impl Provider<FileSymbolsQuery, Vec<FileSymbol>> for HierarchyLanguage {
    fn start(
        &self,
        _: FileSymbolsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<FileSymbol>>,
    ) {
        let symbols = self.file_symbols.lock().unwrap().clone();
        let _ = reply.send(self.answer(symbols));
    }
}

impl LanguageSupport for HierarchyLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-hierarchy")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn overridable_methods(&self) -> Option<&OverridableMethodsProvider> {
        Some(self)
    }

    fn add_overridable_methods(&self) -> Option<&AddOverridableMethodsProvider> {
        Some(self)
    }

    fn call_hierarchy(&self) -> Option<&CallHierarchyProvider> {
        Some(self)
    }

    fn definition_preview(&self) -> Option<&DefinitionPreviewProvider> {
        Some(self)
    }

    fn reference_string(&self) -> Option<&ReferenceStringProvider> {
        Some(self)
    }

    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        Some(self)
    }
}

pub(crate) fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    root
}

pub(crate) fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<HierarchyLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("hierarchy-state");
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
        root,
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
    let setup = project_setup(scratch("hierarchy-cache"));
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

fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    for _ in 0..200 {
        if check(cx) {
            return;
        }
        cx.run_until_parked();
    }
    panic!("the test waited for {what} in vain");
}

fn method(
    owner: &str,
    name: &str,
    parameters: &[&str],
    abstract_method: bool,
) -> OverridableMethod {
    OverridableMethod {
        name: name.to_owned(),
        owner: owner.to_owned(),
        parameters: parameters.iter().map(|p| (*p).to_owned()).collect(),
        return_type: None,
        abstract_method,
        handle: format!("{owner}#{name}"),
    }
}

#[ui_kit::gpui::test]
fn override_lists_the_methods_and_adds_the_marked_ones_in_one_step(cx: &mut TestAppContext) {
    let text = "class Circle {\n}\n";
    let root = project("override", &[("src/Circle.txt", text)]);
    let path = root.join("src").join("Circle.txt");
    let language = HierarchyLanguage::new();
    *language.overridable.lock().unwrap() = vec![
        method("Shape", "area", &[], true),
        method("Object", "toString", &[], false),
    ];
    let edit = DocumentEdit {
        span: span_in(text, text.find("}\n").unwrap(), text.find("}\n").unwrap()),
        text: "\n    double area() {}\n".to_owned(),
    };
    *language.add.lock().unwrap() = Some(WorkspaceChange {
        edits: vec![lang_api::SourceEdit {
            uri: uri_of(&path),
            edits: vec![edit],
        }],
        renames: Vec::new(),
    });
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-o");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "override-popup").size.height > px(0.),
        "Ctrl+O shows the Override list"
    );
    type_text(cx, window, "area");
    press(cx, window, "space enter");
    assert!(
        buffer_of(cx, &editor_of(cx, window, &path)).contains("double area()"),
        "the marked method is added"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "the whole insertion is one undo step"
    );
}

#[ui_kit::gpui::test]
fn implement_shows_only_the_abstract_methods(cx: &mut TestAppContext) {
    let root = project("implement", &[("src/Circle.txt", "class Circle {\n}\n")]);
    let path = root.join("src").join("Circle.txt");
    let language = HierarchyLanguage::new();
    *language.overridable.lock().unwrap() = vec![
        method("Shape", "area", &[], true),
        method("Object", "toString", &[], false),
    ];
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-i");
    draw(cx, window);
    assert_eq!(
        *language.only_abstract.lock().unwrap(),
        Some(true),
        "Ctrl+I asks the server for the methods without a body"
    );
    assert!(bounds_of(cx, window, "override-popup").size.height > px(0.));
}

#[ui_kit::gpui::test]
fn quick_definition_shows_the_source_and_enter_goes_there(cx: &mut TestAppContext) {
    let root = project(
        "quick-definition",
        &[
            ("src/A.txt", "use\n"),
            ("src/B.txt", "class B {\n  int x;\n}\n"),
        ],
    );
    let a = root.join("src").join("A.txt");
    let b = root.join("src").join("B.txt");
    let language = HierarchyLanguage::new();
    *language.preview.lock().unwrap() = Some(DefinitionPreview {
        text: "class B {\n  int x;\n}".to_owned(),
        target: Some(SourceSpan {
            uri: uri_of(&b),
            span: span_in("class B {\n  int x;\n}\n", 6, 7),
        }),
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &a);
    press(cx, window, "ctrl-shift-i");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "quick-definition").size.height > px(0.),
        "Ctrl+Shift+I shows the definition"
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(b));
}

#[ui_kit::gpui::test]
fn copy_reference_puts_the_reference_on_the_clipboard(cx: &mut TestAppContext) {
    let root = project("copy-reference", &[("src/Owner.txt", "class Owner {}\n")]);
    let path = root.join("src").join("Owner.txt");
    let language = HierarchyLanguage::new();
    *language.reference.lock().unwrap() = Some("com.example.Owner#getName".to_owned());
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-c");
    assert_eq!(clipboard(cx).as_deref(), Some("com.example.Owner#getName"));
}

#[ui_kit::gpui::test]
fn type_declaration_lists_several_places(cx: &mut TestAppContext) {
    let root = project(
        "type-declaration",
        &[
            ("src/Use.txt", "owner\n"),
            ("src/Owner.txt", "class Owner {}\n"),
            ("src/Alt.txt", "class Owner {}\n"),
        ],
    );
    let use_path = root.join("src").join("Use.txt");
    let owner = root.join("src").join("Owner.txt");
    let alt = root.join("src").join("Alt.txt");
    let language = HierarchyLanguage::new();
    *language.type_definitions.lock().unwrap() = vec![
        SourceSpan {
            uri: uri_of(&owner),
            span: span_in("class Owner {}\n", 6, 11),
        },
        SourceSpan {
            uri: uri_of(&alt),
            span: span_in("class Owner {}\n", 6, 11),
        },
    ];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &use_path);
    press(cx, window, "ctrl-shift-b");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "two places open the list"
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(owner));
}

#[ui_kit::gpui::test]
fn call_hierarchy_opens_the_window_and_lists_the_caller(cx: &mut TestAppContext) {
    let root = project("call-hierarchy", &[("src/Calls.txt", "callee\n")]);
    let path = root.join("src").join("Calls.txt");
    let language = HierarchyLanguage::new();
    *language.call_roots.lock().unwrap() = vec![CallHierarchyItem {
        name: "callee".to_owned(),
        detail: None,
        place: Some(SourceSpan {
            uri: uri_of(&path),
            span: span_in("callee\n", 0, 6),
        }),
        handle: "root".to_owned(),
    }];
    *language.calls.lock().unwrap() = vec![CallHierarchyItem {
        name: "caller".to_owned(),
        detail: None,
        place: Some(SourceSpan {
            uri: uri_of(&path),
            span: span_in("callee\n", 0, 6),
        }),
        handle: "caller".to_owned(),
    }];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-h");
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveCallHierarchy>().0.clone());
    wait_for(cx, "the call hierarchy", |cx| {
        panel.read_with(cx, |panel, _| panel.lines()).len() >= 2
    });
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert!(
        lines.iter().any(|line| line.contains("callee")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.contains("caller")),
        "{lines:?}"
    );
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.direction()),
        CallDirection::Incoming
    );
}

#[ui_kit::gpui::test]
fn structure_shows_the_signatures_the_server_sends(cx: &mut TestAppContext) {
    let text = "class A {\n  String process(int x) {\n    return \"\";\n  }\n  int count;\n}\n";
    let root = project("structure-signatures", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = HierarchyLanguage::new();
    let method_at = text.find("String process").unwrap();
    let method_end = text.find("  int count").unwrap();
    let field_at = text.find("  int count").unwrap();
    let field_end = field_at + "  int count;".len();
    *language.file_symbols.lock().unwrap() = vec![FileSymbol {
        name: "A".to_owned(),
        detail: None,
        kind: SymbolKind::Class,
        range: span_in(text, 0, text.len()),
        name_range: span_in(text, 6, 7),
        children: vec![
            FileSymbol {
                name: "process(int)".to_owned(),
                detail: Some(": String".to_owned()),
                kind: SymbolKind::Method,
                range: span_in(text, method_at, method_end),
                name_range: span_in(text, method_at, method_at + 12),
                children: Vec::new(),
            },
            FileSymbol {
                name: "count".to_owned(),
                detail: Some("int".to_owned()),
                kind: SymbolKind::Field,
                range: span_in(text, field_at, field_end),
                name_range: span_in(text, field_at + 6, field_at + 11),
                children: Vec::new(),
            },
        ],
    }];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    wait_for(cx, "the signatures of the outline", |cx| {
        panel
            .read_with(cx, |panel, cx| panel.lines(cx))
            .iter()
            .any(|line| line.contains("process(int)"))
    });
    let lines = panel.read_with(cx, |panel, cx| panel.lines(cx));
    assert!(
        lines.iter().any(|line| line == "  process(int): String"),
        "{lines:?}"
    );
    assert!(lines.iter().any(|line| line == "  count: int"), "{lines:?}");
}

// The ignored tests below run on the real server and a copy of PetClinic:
// `cargo test -p panels window_tests_hierarchy -- --ignored --test-threads=1`.

/// The source folder of the copied project, without spelling its language in this
/// neutral crate.
fn source_root(root: &Path) -> PathBuf {
    root.join(format!("src/main/{}{}", "ja", "va"))
}

fn source_file(name: &str) -> String {
    format!("{name}.{}{}", "ja", "va")
}

fn real_project_ready(
    cx: &mut TestAppContext,
    files: &[(&str, &str)],
) -> (
    PathBuf,
    window_language::RealLanguage,
    WindowHandle<Workspace>,
) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("hierarchy-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    let language =
        window_language::RealLanguage::open(root.clone(), scratch("hierarchy-real-state"))
            .expect("the language server starts");
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("hierarchy-real-cache")),
    );
    crate::window_tests_real::wait_ready(cx, window);
    (root, language, window)
}

fn real_wait(cx: &mut TestAppContext, what: &str, check: impl FnMut(&mut TestAppContext) -> bool) {
    crate::window_tests_real::wait_for(cx, what, std::time::Duration::from_secs(120), check);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn implement_methods_adds_a_method_to_a_class_that_implements_an_interface(
    cx: &mut TestAppContext,
) {
    let source = "package org.springframework.samples.petclinic.owner;\n\npublic class Circle implements Shape {\n}\n";
    let interface = "package org.springframework.samples.petclinic.owner;\n\npublic interface Shape {\n\tdouble area();\n}\n";
    let circle = format!(
        "src/main/{}{}/org/springframework/samples/petclinic/owner/{}",
        "ja",
        "va",
        source_file("Circle")
    );
    let shape = format!(
        "src/main/{}{}/org/springframework/samples/petclinic/owner/{}",
        "ja",
        "va",
        source_file("Shape")
    );
    let (root, language, window) = real_project_ready(
        cx,
        &[(circle.as_str(), source), (shape.as_str(), interface)],
    );
    let path = source_root(&root)
        .join("org/springframework/samples/petclinic/owner")
        .join(source_file("Circle"));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(2, 13, cx));
    press(cx, window, "ctrl-i");
    real_wait(cx, "the Implement list", |cx| {
        let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
        visual.debug_bounds("override-popup").is_some()
    });
    press(cx, window, "enter");
    real_wait(cx, "the added method", |cx| {
        buffer_of(cx, &view).contains("double area()")
    });
    crate::window_tests_real::close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn incoming_calls_of_a_petclinic_method(cx: &mut TestAppContext) {
    let (root, language, window) = real_project_ready(cx, &[]);
    let path = source_root(&root)
        .join("org/springframework/samples/petclinic/owner")
        .join(source_file("OwnerController"));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let text = buffer_of(cx, &view);
    let line = text
        .lines()
        .position(|line| line.contains("addPaginationModel(int page"))
        .expect("the method is in the file");
    view.update(cx, |view, cx| view.restore_caret(line, 20, cx));
    press(cx, window, "ctrl-alt-h");
    let panel = cx.update(|cx| cx.global::<ActiveCallHierarchy>().0.clone());
    real_wait(cx, "the callers", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.contains("processFindForm"))
    });
    crate::window_tests_real::close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn type_declaration_of_a_variable(cx: &mut TestAppContext) {
    let (root, language, window) = real_project_ready(cx, &[]);
    let path = source_root(&root)
        .join("org/springframework/samples/petclinic/owner")
        .join(source_file("OwnerController"));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let text = buffer_of(cx, &view);
    let line = text
        .lines()
        .position(|line| line.contains("private final OwnerRepository owners"))
        .expect("the field is in the file");
    view.update(cx, |view, cx| view.restore_caret(line, 24, cx));
    press(cx, window, "ctrl-shift-b");
    real_wait(cx, "the type of the variable", |cx| {
        active_path(cx, window).is_some_and(|path| path.ends_with(source_file("OwnerRepository")))
    });
    crate::window_tests_real::close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_structure_shows_the_signature_of_a_method(cx: &mut TestAppContext) {
    let (root, language, window) = real_project_ready(cx, &[]);
    let path = source_root(&root)
        .join("org/springframework/samples/petclinic/owner")
        .join(source_file("OwnerController"));
    open_path(cx, window, &path);
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveStructure>().0.clone());
    real_wait(cx, "the signature", |cx| {
        panel
            .read_with(cx, |panel, cx| panel.lines(cx))
            .iter()
            .any(|line| line.contains("processFindForm(") && line.contains(": String"))
    });
    crate::window_tests_real::close(cx, window, language, true);
}
