//! Alt+Insert lists only what applies at the caret, without a window. The language answers
//! each kind from the test; the class symbols answer when the server does not know.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentEdit, GenerateApplyProvider, GenerateApplyRequest, GenerateChoice,
    GenerateKind, GenerateProvider, GenerateQuery, Generation, LanguageFn, LanguageId,
    LanguageSupport, OverridableMethods, OverridableMethodsProvider, OverridableMethodsQuery,
    Provider, Queries, SourceEdit, SourceId, Symbol, SymbolKind, SymbolProvider, SyntaxProvider,
    WorkspaceChange,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_safe_delete::{span_in, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-generate-list");

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

/// A language whose answers the test sets. A kind without an answer has nothing to offer;
/// `unknown` makes every answer say the server does not know; `symbols` set, the language
/// has symbols and the caret is inside the classes they name; `applied` is the answer to
/// an apply request, the edit of a chosen part or of the parts none of which was chosen.
struct GenerateLanguage {
    syntax: NoSyntax,
    answers: Mutex<Vec<(GenerateKind, Generation)>>,
    unknown: Mutex<bool>,
    symbols: Mutex<Option<Vec<Symbol>>>,
    applied: Mutex<Option<Generation>>,
}

impl GenerateLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            answers: Mutex::new(Vec::new()),
            unknown: Mutex::new(false),
            symbols: Mutex::new(None),
            applied: Mutex::new(None),
        })
    }

    fn answer_kind(&self, kind: GenerateKind, generation: Generation) {
        self.answers.lock().unwrap().push((kind, generation));
    }

    fn set_unknown(&self) {
        *self.unknown.lock().unwrap() = true;
    }

    fn set_symbols(&self, symbols: Vec<Symbol>) {
        *self.symbols.lock().unwrap() = Some(symbols);
    }

    fn set_applied(&self, generation: Generation) {
        *self.applied.lock().unwrap() = Some(generation);
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        let confidence = if *self.unknown.lock().unwrap() {
            Confidence::Unknown
        } else {
            Confidence::Certain
        };
        Answer::new(value, confidence, SOURCE)
    }
}

impl Provider<GenerateQuery, Generation> for GenerateLanguage {
    fn start(&self, query: GenerateQuery, _: base::Cancel, mut reply: lang_api::Reply<Generation>) {
        let generation = self
            .answers
            .lock()
            .unwrap()
            .iter()
            .find(|(kind, _)| *kind == query.kind)
            .map(|(_, generation)| generation.clone())
            .unwrap_or_default();
        let _ = reply.send(self.answer(generation));
    }
}

impl Provider<OverridableMethodsQuery, OverridableMethods> for GenerateLanguage {
    fn start(
        &self,
        _: OverridableMethodsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<OverridableMethods>,
    ) {
        // No method to implement: the Implement Methods row leaves the list.
        let methods = OverridableMethods {
            owner_type: "A".to_owned(),
            methods: Vec::new(),
        };
        let _ = reply.send(self.answer(methods));
    }
}

impl Provider<GenerateApplyRequest, Generation> for GenerateLanguage {
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

impl SymbolProvider for GenerateLanguage {
    fn file_symbols(&self, _: &Path, _: &str) -> Vec<Symbol> {
        self.symbols.lock().unwrap().clone().unwrap_or_default()
    }
}

impl LanguageSupport for GenerateLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-generate-list")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        self.symbols
            .lock()
            .unwrap()
            .is_some()
            .then_some(self as &dyn SymbolProvider)
    }

    fn generate(&self) -> Option<&GenerateProvider> {
        Some(self)
    }

    fn overridable_methods(&self) -> Option<&OverridableMethodsProvider> {
        Some(self)
    }

    fn generate_apply(&self) -> Option<&GenerateApplyProvider> {
        Some(self)
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<GenerateLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("generate-list-state");
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
    let setup = project_setup(scratch("generate-list-cache"));
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

/// A file `src/A.txt` with `text` in the project `name`.
fn project(name: &str, text: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let path = root.join("src").join("A.txt");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    (root, path)
}

/// A server answer that offers the fields named.
fn fields(names: &[&str]) -> Generation {
    Generation {
        choices: names
            .iter()
            .map(|name| GenerateChoice {
                label: (*name).to_owned(),
                detail: Some("String".to_owned()),
                checked: true,
                handle: (*name).to_owned(),
            })
            .collect(),
        ..Generation::default()
    }
}

/// The class `A` of a file whose text is `text`, from its first byte to its last.
fn class_symbol(text: &str, kind: SymbolKind) -> Symbol {
    Symbol {
        name: "A".to_owned(),
        kind,
        container: None,
        depth: 0,
        range: 0..text.len() - 1,
        name_range: 6..7,
    }
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The labels of the Alt+Insert list, once it is open.
fn list_rows(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Option<Vec<String>> {
    let view = editor_of(cx, window, path);
    let rows = view.read_with(cx, |view, _| view.actions_visible())?;
    Some(rows.rows.into_iter().map(|(_, label)| label).collect())
}

/// Alt+Insert with the caret at `line`, `column`. Waits until the list opens or its note
/// shows, and gives the labels of the list, or `None` when the list stayed closed.
fn alt_insert(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    line: usize,
    column: usize,
) -> Option<Vec<String>> {
    open_path(cx, window, path);
    editor_of(cx, window, path).update(cx, |view, cx| view.restore_caret(line, column, cx));
    press(cx, window, "alt-insert");
    wait_for(cx, "the Generate list or its note", |cx| {
        draw(cx, window);
        list_rows(cx, window, path).is_some() || has_selector(cx, window, "lsp-note")
    });
    list_rows(cx, window, path)
}

#[ui_kit::gpui::test]
fn a_class_without_fields_has_no_getter_or_setter(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("generate-list-no-fields", text);
    let language = GenerateLanguage::new();
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    assert_eq!(
        rows,
        ["Constructor", "Override Methods"],
        "a class without fields offers its constructor and its overrides only"
    );
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "lsp-note"),
        "a class without fields shows no Nothing to generate notice"
    );
}

#[ui_kit::gpui::test]
fn a_class_without_fields_gets_its_constructor_without_a_notice(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("generate-list-constructor", text);
    let language = GenerateLanguage::new();
    // No field: the server's answer for the constructor has nothing to choose and no edit.
    // Asked for the edit with no part chosen, the server writes the no-argument constructor.
    let insert = text.find('}').unwrap();
    language.set_applied(Generation {
        change: Some(WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri_of(&path),
                edits: vec![DocumentEdit {
                    span: span_in(text, insert, insert),
                    text: "    public A() {\n    }\n".to_owned(),
                }],
            }],
            renames: Vec::new(),
        }),
        ..Generation::default()
    });
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    assert_eq!(
        rows.first().map(String::as_str),
        Some("Constructor"),
        "the constructor is offered first: {rows:?}"
    );
    press(cx, window, "enter");
    wait_for(cx, "the constructor in the text", |cx| {
        buffer_of(cx, &editor_of(cx, window, &path)).contains("public A()")
    });
    draw(cx, window);
    assert!(
        !has_selector(cx, window, "lsp-note"),
        "picking the constructor shows no Nothing to generate notice"
    );
}

#[ui_kit::gpui::test]
fn a_final_field_has_a_getter_and_no_setter(cx: &mut TestAppContext) {
    let text = "class A {\n    private final String name;\n}\n";
    let (root, path) = project("generate-list-final-field", text);
    let language = GenerateLanguage::new();
    language.answer_kind(GenerateKind::Getter, fields(&["name"]));
    language.answer_kind(GenerateKind::GetterAndSetter, fields(&["name"]));
    language.answer_kind(GenerateKind::EqualsAndHashCode, fields(&["name"]));
    // The server offers no Setter for a final field, so no answer for it is given here.
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    assert!(
        rows.contains(&"Getter".to_owned()),
        "the final field has a getter: {rows:?}"
    );
    assert!(
        !rows.contains(&"Setter".to_owned()),
        "the final field has no setter: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn a_field_with_a_getter_has_no_getter_row(cx: &mut TestAppContext) {
    let text = "class A {\n    private String name;\n    public String getName() {\n        return name;\n    }\n}\n";
    let (root, path) = project("generate-list-with-getter", text);
    let language = GenerateLanguage::new();
    // Every field has its getter, so the server has nothing for Getter.
    language.answer_kind(GenerateKind::Setter, fields(&["name"]));
    language.answer_kind(GenerateKind::GetterAndSetter, fields(&["name"]));
    language.answer_kind(GenerateKind::EqualsAndHashCode, fields(&["name"]));
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    assert!(
        !rows.contains(&"Getter".to_owned()),
        "the field has its getter: {rows:?}"
    );
    assert!(
        rows.contains(&"Setter".to_owned()),
        "the field has no setter: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn without_a_server_answer_the_class_symbols_decide(cx: &mut TestAppContext) {
    let text = "class A {\n}\n";
    let (root, path) = project("generate-list-symbols", text);
    let language = GenerateLanguage::new();
    language.set_unknown();
    language.set_symbols(vec![class_symbol(text, SymbolKind::Class)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    assert!(rows.contains(&"Constructor".to_owned()), "{rows:?}");
    for hidden in [
        "Getter",
        "Setter",
        "Getter and Setter",
        "equals() and hashCode()",
        "toString()",
    ] {
        assert!(
            !rows.contains(&hidden.to_owned()),
            "the symbols say the class has no fields, so no {hidden}: {rows:?}"
        );
    }
}

#[ui_kit::gpui::test]
fn an_empty_file_says_nothing_to_generate(cx: &mut TestAppContext) {
    let (root, path) = project("generate-list-empty", "");
    let language = GenerateLanguage::new();
    // A file without any type: no class, and no answer for Test.
    language.set_symbols(Vec::new());
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        alt_insert(cx, window, &path, 0, 0).is_none(),
        "an empty list does not open"
    );
    assert!(
        has_selector(cx, window, "lsp-note"),
        "the note says there is nothing to generate"
    );
}

#[ui_kit::gpui::test]
fn outside_a_class_only_test_is_offered(cx: &mut TestAppContext) {
    let text = "interface A {\n}\n";
    let (root, path) = project("generate-list-interface", text);
    let language = GenerateLanguage::new();
    // Rows of a class would apply to these answers, but not in an interface.
    language.answer_kind(GenerateKind::Getter, fields(&["name"]));
    language.answer_kind(
        GenerateKind::Test,
        Generation {
            choices: vec![GenerateChoice {
                label: "name()".to_owned(),
                detail: None,
                checked: false,
                handle: "name()".to_owned(),
            }],
            ..Generation::default()
        },
    );
    language.set_symbols(vec![class_symbol(text, SymbolKind::Interface)]);
    let window = open_window(cx, root, language);
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 13).expect("the list opens");
    assert_eq!(rows, ["Test"], "an interface has the short list");
}
