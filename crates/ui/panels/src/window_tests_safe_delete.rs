//! Safe Delete, Unwrap/Remove, Method Hierarchy and the full Generate list without a
//! window. The language answers from memory; the parsing of the real constructs is
//! checked by the unit tests of the language crate.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, ChangeSignatureInfo, ChangeSignatureRequest, Confidence, Deletion, DeletionKind,
    DocumentEdit, DocumentSpot, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateQuery,
    GeneratedFile, Generation, LanguageFn, LanguageId, LanguageSupport, MethodHierarchyEntry,
    MethodHierarchyQuery, MethodRelation, MethodSignature, Provider, Queries, SemanticServer,
    SignatureParameter, SourceEdit, SourceId, SourceSpan, SyntaxProvider, TextPoint, TextSpan,
    UnwrapEdit, WorkspaceChange,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::method_hierarchy::ActiveMethodHierarchy;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch,
};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-edit");

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct TestSyntax {
    queries: Queries,
    unwrap: Mutex<Vec<UnwrapEdit>>,
    deletion: Mutex<Option<Deletion>>,
    first_type: Mutex<Option<usize>>,
}

impl SyntaxProvider for TestSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.queries
    }

    fn unwrap_edits(&self, _: &str, _: usize) -> Vec<UnwrapEdit> {
        self.unwrap.lock().unwrap().clone()
    }

    fn deletion(&self, _: &str, _: usize) -> Option<Deletion> {
        self.deletion.lock().unwrap().clone()
    }

    fn first_type_offset(&self, _: &str) -> Option<usize> {
        *self.first_type.lock().unwrap()
    }
}

pub(crate) struct FakeEditLanguage {
    syntax: TestSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    references: Mutex<Vec<SourceSpan>>,
    generation: Mutex<Option<Generation>>,
    applied: Mutex<Option<Generation>>,
    kinds: Mutex<Vec<GenerateKind>>,
    pub(crate) hierarchy: Mutex<Vec<MethodHierarchyEntry>>,
    pub(crate) hierarchy_requests: Mutex<usize>,
    signature: Mutex<Option<ChangeSignatureInfo>>,
    signature_change: Mutex<Option<WorkspaceChange>>,
}

impl FakeEditLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: TestSyntax {
                queries: Queries::default(),
                unwrap: Mutex::new(Vec::new()),
                deletion: Mutex::new(None),
                first_type: Mutex::new(None),
            },
            documents: Mutex::new(HashMap::new()),
            references: Mutex::new(Vec::new()),
            generation: Mutex::new(None),
            applied: Mutex::new(None),
            kinds: Mutex::new(Vec::new()),
            hierarchy: Mutex::new(Vec::new()),
            hierarchy_requests: Mutex::new(0),
            signature: Mutex::new(None),
            signature_change: Mutex::new(None),
        })
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for FakeEditLanguage {
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

impl Provider<DocumentSpot, Vec<SourceSpan>> for FakeEditLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<Vec<SourceSpan>>) {
        let spans = self.references.lock().unwrap().clone();
        let _ = reply.send(self.answer(spans));
    }
}

impl Provider<GenerateQuery, Generation> for FakeEditLanguage {
    fn start(&self, query: GenerateQuery, _: base::Cancel, mut reply: lang_api::Reply<Generation>) {
        self.kinds.lock().unwrap().push(query.kind);
        let generation = self.generation.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(generation));
    }
}

impl Provider<GenerateApplyRequest, Generation> for FakeEditLanguage {
    fn start(
        &self,
        _: GenerateApplyRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<Generation>,
    ) {
        let generation = self.applied.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(generation));
    }
}

impl Provider<MethodHierarchyQuery, Vec<MethodHierarchyEntry>> for FakeEditLanguage {
    fn start(
        &self,
        _: MethodHierarchyQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<MethodHierarchyEntry>>,
    ) {
        *self.hierarchy_requests.lock().unwrap() += 1;
        let entries = self.hierarchy.lock().unwrap().clone();
        let _ = reply.send(self.answer(entries));
    }
}

impl Provider<DocumentSpot, ChangeSignatureInfo> for FakeEditLanguage {
    fn start(
        &self,
        _: DocumentSpot,
        _: base::Cancel,
        mut reply: lang_api::Reply<ChangeSignatureInfo>,
    ) {
        let info = self.signature.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(info));
    }
}

impl Provider<ChangeSignatureRequest, lang_api::ChangeSignatureOutcome> for FakeEditLanguage {
    fn start(
        &self,
        _: ChangeSignatureRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<lang_api::ChangeSignatureOutcome>,
    ) {
        let change = self.signature_change.lock().unwrap().clone();
        let outcome = lang_api::ChangeSignatureOutcome {
            change: change.unwrap_or_default(),
            error: None,
        };
        let _ = reply.send(self.answer(outcome));
    }
}

impl LanguageSupport for FakeEditLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-edit")
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

    fn references(&self) -> Option<&lang_api::ReferencesProvider> {
        Some(self)
    }

    fn generate(&self) -> Option<&lang_api::GenerateProvider> {
        Some(self)
    }

    fn generate_apply(&self) -> Option<&lang_api::GenerateApplyProvider> {
        Some(self)
    }

    fn method_hierarchy(&self) -> Option<&lang_api::MethodHierarchyProvider> {
        Some(self)
    }

    fn change_signature_info(&self) -> Option<&lang_api::ChangeSignatureInfoProvider> {
        Some(self)
    }

    fn change_signature(&self) -> Option<&lang_api::ChangeSignatureProvider> {
        Some(self)
    }
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

pub(crate) fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

pub(crate) fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
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
    language: Arc<FakeEditLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("edit-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
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
    let setup = project_setup(scratch("edit-cache"));
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

fn caret(
    view: &gpui::Entity<editor_view::EditorView>,
    cx: &mut TestAppContext,
    line: usize,
    column: usize,
) {
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
}

#[ui_kit::gpui::test]
fn safe_delete_of_an_unused_method_is_one_step_of_undo(cx: &mut TestAppContext) {
    let text = "class A {\n    int f() {\n        return 1;\n    }\n}\n";
    let root = project("safe-delete-unused", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    *language.syntax.deletion.lock().unwrap() = Some(Deletion {
        kind: Some(DeletionKind::Method),
        name: "f".to_owned(),
        range: text.find("    int f()").unwrap()
            ..text.find("    }\n}\n").unwrap() + "    }\n".len(),
        name_range: text.find("f()").unwrap()..text.find("f()").unwrap() + 1,
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    });
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 8);
    press(cx, window, "alt-delete");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "class A {\n}\n"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &editor_of(cx, window, &path)), text);
}

#[ui_kit::gpui::test]
fn safe_delete_with_usages_shows_view_usages_and_delete_anyway(cx: &mut TestAppContext) {
    let text =
        "class A {\n    void used() {\n    }\n    void caller() {\n        used();\n    }\n}\n";
    let root = project("safe-delete-used", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    *language.syntax.deletion.lock().unwrap() = Some(Deletion {
        kind: Some(DeletionKind::Method),
        name: "used".to_owned(),
        range: text.find("    void used()").unwrap()..text.find("    void caller").unwrap(),
        name_range: text.find("used").unwrap()..text.find("used").unwrap() + 4,
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    });
    *language.references.lock().unwrap() = vec![SourceSpan {
        uri: uri_of(&path),
        span: span_in(
            text,
            text.rfind("used").unwrap(),
            text.rfind("used").unwrap() + 4,
        ),
    }];
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 10);
    press(cx, window, "alt-delete");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "safe-delete-popup").size.height > px(0.),
        "the dialog shows the number of usages"
    );
    press(cx, window, "escape");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "Cancel changes nothing"
    );
    press(cx, window, "alt-delete");
    press(cx, window, "right enter");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "class A {\n    void caller() {\n        used();\n    }\n}\n"
    );
}

#[ui_kit::gpui::test]
fn safe_delete_of_a_parameter_uses_the_signature(cx: &mut TestAppContext) {
    let text = "class A {\n    void f(int a, int b) {\n    }\n    void caller() {\n        f(1, 2);\n    }\n}\n";
    let root = project("safe-delete-parameter", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    let parameter = text.find(", int b").unwrap();
    let method = text.find("    void f").unwrap();
    let method_name = text.find("f(int").unwrap();
    *language.syntax.deletion.lock().unwrap() = Some(Deletion {
        kind: Some(DeletionKind::Parameter),
        name: "b".to_owned(),
        range: parameter..parameter + ", int b".len(),
        name_range: text.find("int b").unwrap() + 4..text.find("int b").unwrap() + 5,
        method: Some(method..text.find("    }\n    void caller").unwrap() + 5),
        method_name: Some(method_name..method_name + 1),
        parameter_index: Some(1),
        sole_type: false,
    });
    *language.references.lock().unwrap() = vec![SourceSpan {
        uri: uri_of(&path),
        span: span_in(
            text,
            text.find("f(1, 2)").unwrap(),
            text.find("f(1, 2)").unwrap() + 7,
        ),
    }];
    *language.signature.lock().unwrap() = Some(ChangeSignatureInfo {
        signature: Some(MethodSignature {
            name: "f".to_owned(),
            visibility: "private".to_owned(),
            return_type: "void".to_owned(),
            parameters: vec![
                SignatureParameter {
                    type_name: "int".to_owned(),
                    name: "a".to_owned(),
                    default_value: String::new(),
                    original_index: Some(0),
                },
                SignatureParameter {
                    type_name: "int".to_owned(),
                    name: "b".to_owned(),
                    default_value: String::new(),
                    original_index: Some(1),
                },
            ],
            exceptions: Vec::new(),
        }),
        error: None,
    });
    let declaration_start = text.find(", int b").unwrap();
    let call_start = text.find("f(1, 2)").unwrap();
    *language.signature_change.lock().unwrap() = Some(WorkspaceChange {
        edits: vec![SourceEdit {
            uri: uri_of(&path),
            edits: vec![
                DocumentEdit {
                    span: span_in(text, declaration_start, declaration_start + ", int b".len()),
                    text: String::new(),
                },
                DocumentEdit {
                    span: span_in(text, call_start, call_start + "f(1, 2)".len()),
                    text: "f(1)".to_owned(),
                },
            ],
        }],
        renames: Vec::new(),
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 20);
    press(cx, window, "alt-delete");
    press(cx, window, "right enter");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "class A {\n    void f(int a) {\n    }\n    void caller() {\n        f(1);\n    }\n}\n"
    );
}

#[ui_kit::gpui::test]
fn safe_delete_shows_the_usages_window(cx: &mut TestAppContext) {
    let text =
        "class A {\n    void used() {\n    }\n    void caller() {\n        used();\n    }\n}\n";
    let root = project("safe-delete-view", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    *language.syntax.deletion.lock().unwrap() = Some(Deletion {
        kind: Some(DeletionKind::Method),
        name: "used".to_owned(),
        range: text.find("    void used()").unwrap()..text.find("    void caller").unwrap(),
        name_range: text.find("used").unwrap()..text.find("used").unwrap() + 4,
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: false,
    });
    *language.references.lock().unwrap() = vec![SourceSpan {
        uri: uri_of(&path),
        span: span_in(
            text,
            text.rfind("used").unwrap(),
            text.rfind("used").unwrap() + 4,
        ),
    }];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 10);
    press(cx, window, "alt-delete");
    press(cx, window, "enter");
    draw(cx, window);
    let count = cx.read(|cx| {
        let hub = cx.global::<ActiveUsages>().0.clone();
        hub.read(cx).panel().read(cx).items().len()
    });
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
        .unwrap();
    assert!(open, "View Usages opens the Usages window");
    assert_eq!(count, 1);
}

#[ui_kit::gpui::test]
fn safe_delete_of_a_sole_type_removes_the_file(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let root = project("safe-delete-file", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    crate::tree_ops::use_temporary_delete();
    let language = FakeEditLanguage::new();
    *language.syntax.deletion.lock().unwrap() = Some(Deletion {
        kind: Some(DeletionKind::Type),
        name: "A".to_owned(),
        range: 0..text.len(),
        name_range: text.find('A').unwrap()..text.find('A').unwrap() + 1,
        method: None,
        method_name: None,
        parameter_index: None,
        sole_type: true,
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 0, 6);
    press(cx, window, "alt-delete");
    wait_for(cx, "the file to be deleted", |_| !path.exists());
}

#[ui_kit::gpui::test]
fn unwrap_lists_the_constructs_and_applies_one_edit(cx: &mut TestAppContext) {
    let text = "class A {\n    void f() {\n        if (ready) {\n            work();\n        }\n    }\n}\n";
    let root = project("unwrap", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    let start = text.find("if (ready)").unwrap();
    let end = text.find("work();\n        }").unwrap() + "work();\n        }".len();
    *language.syntax.unwrap.lock().unwrap() = vec![UnwrapEdit {
        title: "Unwrap 'if'".to_owned(),
        start,
        end,
        replacement: "work();".to_owned(),
    }];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 2, 12);
    press(cx, window, "ctrl-shift-delete");
    draw(cx, window);
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        "class A {\n    void f() {\n        work();\n    }\n}\n"
    );
    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &editor_of(cx, window, &path)), text);
}

#[ui_kit::gpui::test]
fn generate_lists_the_full_menu_and_applies_the_marked_parts(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let root = project("generate", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let language = FakeEditLanguage::new();
    *language.generation.lock().unwrap() = Some(Generation {
        choices: vec![
            GenerateChoice {
                label: "name".to_owned(),
                detail: Some("String".to_owned()),
                checked: true,
                handle: "name".to_owned(),
            },
            GenerateChoice {
                label: "age".to_owned(),
                detail: Some("int".to_owned()),
                checked: true,
                handle: "age".to_owned(),
            },
        ],
        ..Generation::default()
    });
    let insert = text.find("}\n").unwrap();
    *language.applied.lock().unwrap() = Some(Generation {
        change: Some(WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri_of(&path),
                edits: vec![DocumentEdit {
                    span: span_in(text, insert, insert),
                    text: "\n    public String toString() {\n    }\n".to_owned(),
                }],
            }],
            renames: Vec::new(),
        }),
        ..Generation::default()
    });
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 0);
    press(cx, window, "alt-insert");
    draw(cx, window);
    for _ in 0..5 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "generate-popup").size.height > px(0.),
        "toString() asks which fields to include"
    );
    assert_eq!(
        *language.kinds.lock().unwrap().last().unwrap(),
        GenerateKind::ToString
    );
    press(cx, window, "ctrl-a enter");
    assert!(
        buffer_of(cx, &editor_of(cx, window, &path)).contains("toString"),
        "the chosen generation is applied"
    );
}

#[ui_kit::gpui::test]
fn generate_test_creates_the_file(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let root = project("generate-test", &[("src/A.txt", text)]);
    let path = root.join("src").join("A.txt");
    let target = root.join("src").join("ATest.txt");
    let language = FakeEditLanguage::new();
    *language.generation.lock().unwrap() = Some(Generation {
        files: vec![GeneratedFile {
            path: target.clone(),
            text: "class ATest {\n}\n".to_owned(),
        }],
        ..Generation::default()
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 0);
    press(cx, window, "alt-insert");
    for _ in 0..9 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    wait_for(cx, "the test file", |_| target.exists());
    assert_eq!(active_path(cx, window).as_deref(), Some(target.as_path()));
}

#[ui_kit::gpui::test]
fn method_hierarchy_marks_defines_inherits_and_must_define(cx: &mut TestAppContext) {
    let text = "interface Shape {\n    double area();\n}\n";
    let root = project("method-hierarchy", &[("src/Shape.txt", text)]);
    let path = root.join("src").join("Shape.txt");
    let language = FakeEditLanguage::new();
    *language.hierarchy.lock().unwrap() = vec![
        MethodHierarchyEntry {
            type_name: "Shape".to_owned(),
            method: "area".to_owned(),
            relation: MethodRelation::Defines,
            place: Some(SourceSpan {
                uri: uri_of(&path),
                span: span_in(
                    text,
                    text.find("Shape").unwrap(),
                    text.find("Shape").unwrap() + 5,
                ),
            }),
        },
        MethodHierarchyEntry {
            type_name: "Circle".to_owned(),
            method: "area".to_owned(),
            relation: MethodRelation::MustDefine,
            place: Some(SourceSpan {
                uri: uri_of(&path),
                span: span_in(text, 0, 1),
            }),
        },
        MethodHierarchyEntry {
            type_name: "Base".to_owned(),
            method: "area".to_owned(),
            relation: MethodRelation::Inherits,
            place: None,
        },
    ];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    caret(&editor_of(cx, window, &path), cx, 1, 10);
    press(cx, window, "ctrl-shift-h");
    draw(cx, window);
    let items = cx.read(|cx| {
        cx.global::<ActiveMethodHierarchy>()
            .0
            .read(cx)
            .panel()
            .read(cx)
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>()
    });
    assert!(
        items.iter().any(|label| label.contains("defines")),
        "{items:?}"
    );
    assert!(
        items.iter().any(|label| label.contains("must define")),
        "{items:?}"
    );
    assert!(
        items.iter().any(|label| label.contains("inherits")),
        "{items:?}"
    );
}

fn real_popup_shown(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn real_caret(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path, line: usize) {
    let view = editor_of(cx, window, path);
    view.update(cx, |view, cx| view.restore_caret(line, 10, cx));
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn safe_delete_of_an_unused_method_and_of_a_used_one_with_the_dialog(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("safe-delete-real");
    let probe = window_language::write_probe(
        &root,
        "probe",
        "SafeDeleteProbe",
        "package probe;\n\npublic class SafeDeleteProbe {\n    void unusedMethod() {\n    }\n\n    void usedMethod() {\n        helper();\n    }\n\n    void helper() {\n    }\n\n    void caller() {\n        usedMethod();\n    }\n}\n",
    );
    let setup = project_setup(scratch("safe-delete-real-cache"));
    let window = crate::window_tests_real::build_window(cx, root.clone(), &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &probe);
    real_caret(cx, window, &probe, 3);
    press(cx, window, "alt-delete");
    crate::window_tests_real::wait_for(
        cx,
        "the unused method to go",
        std::time::Duration::from_secs(120),
        |cx| !buffer_of(cx, &editor_of(cx, window, &probe)).contains("unusedMethod"),
    );
    press(cx, window, "ctrl-z");

    real_caret(cx, window, &probe, 6);
    press(cx, window, "alt-delete");
    crate::window_tests_real::wait_for(
        cx,
        "the usages dialog",
        std::time::Duration::from_secs(120),
        |cx| real_popup_shown(cx, window, "safe-delete-popup"),
    );
    assert!(buffer_of(cx, &editor_of(cx, window, &probe)).contains("usedMethod"));
    press(cx, window, "escape");
    crate::window_tests_real::close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn generate_to_string_and_getter_and_setter_on_petclinic(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("generate-real");
    let probe = window_language::write_probe(
        &root,
        "probe",
        "GenerateProbe",
        "package probe;\n\npublic class GenerateProbe {\n    private String name;\n    private int age;\n}\n",
    );
    let setup = project_setup(scratch("generate-real-cache"));
    let window = crate::window_tests_real::build_window(cx, root.clone(), &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &probe);
    caret(&editor_of(cx, window, &probe), cx, 2, 0);
    press(cx, window, "alt-insert");
    for _ in 0..5 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    crate::window_tests_real::wait_for(
        cx,
        "the toString fields",
        std::time::Duration::from_secs(120),
        |cx| real_popup_shown(cx, window, "generate-popup"),
    );
    press(cx, window, "ctrl-a enter");
    crate::window_tests_real::wait_for(
        cx,
        "toString in the text",
        std::time::Duration::from_secs(120),
        |cx| buffer_of(cx, &editor_of(cx, window, &probe)).contains("toString"),
    );

    caret(&editor_of(cx, window, &probe), cx, 2, 0);
    press(cx, window, "alt-insert");
    for _ in 0..3 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    crate::window_tests_real::wait_for(
        cx,
        "the accessor fields",
        std::time::Duration::from_secs(120),
        |cx| real_popup_shown(cx, window, "generate-popup"),
    );
    press(cx, window, "ctrl-a enter");
    crate::window_tests_real::wait_for(
        cx,
        "the getter and setter",
        std::time::Duration::from_secs(120),
        |cx| {
            let text = buffer_of(cx, &editor_of(cx, window, &probe));
            text.contains("getName") && text.contains("setName")
        },
    );
    crate::window_tests_real::close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn method_hierarchy_of_an_interface_method(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("method-hierarchy-real");
    let shape = window_language::write_probe(
        &root,
        "probe",
        "Shape",
        "package probe;\n\npublic interface Shape {\n    double area();\n}\n",
    );
    window_language::write_probe(
        &root,
        "probe",
        "Circle",
        "package probe;\n\npublic class Circle implements Shape {\n    public double area() {\n        return 0;\n    }\n}\n",
    );
    let setup = project_setup(scratch("method-hierarchy-real-cache"));
    let window = crate::window_tests_real::build_window(cx, root.clone(), &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &shape);
    caret(&editor_of(cx, window, &shape), cx, 3, 14);
    press(cx, window, "ctrl-shift-h");
    crate::window_tests_real::wait_for(
        cx,
        "the method hierarchy",
        std::time::Duration::from_secs(120),
        |cx| {
            cx.read(|cx| {
                cx.try_global::<ActiveMethodHierarchy>()
                    .map(|active| {
                        active
                            .0
                            .read(cx)
                            .panel()
                            .read(cx)
                            .items()
                            .iter()
                            .any(|item| item.label.contains("Shape"))
                    })
                    .unwrap_or(false)
            })
        },
    );
    let items = cx.read(|cx| {
        cx.global::<ActiveMethodHierarchy>()
            .0
            .read(cx)
            .panel()
            .read(cx)
            .items()
            .iter()
            .map(|item| item.label.clone())
            .collect::<Vec<_>>()
    });
    assert!(
        items.iter().any(|label| label.contains("Circle")),
        "{items:?}"
    );
    crate::window_tests_real::close(cx, window, language, true);
}
