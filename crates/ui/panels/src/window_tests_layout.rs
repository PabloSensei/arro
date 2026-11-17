//! The live-check findings of dff1701 without a window: keys of the Russian layout,
//! signatures in the File Structure popup and the pick popups at the caret.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::{LanguageConfig, Languages};
use keymap::workspace as keys;
use lang_api::{
    Answer, Confidence, DocumentEdit, FileSymbol, FileSymbolsProvider, FileSymbolsQuery,
    GenerateApplyProvider, GenerateChoice, GenerateProvider, GenerateQuery, Generation, LanguageFn,
    LanguageId, LanguageSupport, OverridableMethod, OverridableMethods, OverridableMethodsProvider,
    OverridableMethodsQuery, Provider, Queries, SemanticServer, SourceId, SymbolKind,
    SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::palette::Palette;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_palette, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::span_in;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-layout");

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

/// A language that answers the outline, Override and Generate requests from values the
/// test sets.
struct LayoutLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    symbols: Mutex<Vec<FileSymbol>>,
    overridable: Mutex<Vec<OverridableMethod>>,
    generation: Mutex<Option<Generation>>,
}

impl LayoutLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            symbols: Mutex::new(Vec::new()),
            overridable: Mutex::new(Vec::new()),
            generation: Mutex::new(None),
        })
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for LayoutLanguage {
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

impl Provider<FileSymbolsQuery, Vec<FileSymbol>> for LayoutLanguage {
    fn start(
        &self,
        _: FileSymbolsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<FileSymbol>>,
    ) {
        let symbols = self.symbols.lock().unwrap().clone();
        let _ = reply.send(self.answer(symbols));
    }
}

impl Provider<OverridableMethodsQuery, OverridableMethods> for LayoutLanguage {
    fn start(
        &self,
        query: OverridableMethodsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<OverridableMethods>,
    ) {
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

impl Provider<GenerateQuery, Generation> for LayoutLanguage {
    fn start(&self, _: GenerateQuery, _: base::Cancel, mut reply: lang_api::Reply<Generation>) {
        let generation = self.generation.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(generation));
    }
}

impl LanguageSupport for LayoutLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-layout")
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

    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        Some(self)
    }

    fn overridable_methods(&self) -> Option<&OverridableMethodsProvider> {
        Some(self)
    }

    fn generate(&self) -> Option<&GenerateProvider> {
        Some(self)
    }

    fn generate_apply(&self) -> Option<&GenerateApplyProvider> {
        Some(self)
    }
}

/// The provider of an apply request, only so that the language claims the capability.
impl Provider<lang_api::GenerateApplyRequest, Generation> for LayoutLanguage {
    fn start(
        &self,
        _: lang_api::GenerateApplyRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<Generation>,
    ) {
        let generation = self.generation.lock().unwrap().clone().unwrap_or_default();
        let _ = reply.send(self.answer(generation));
    }
}

/// The comment tokens of the plain text files of these tests.
fn text_language(path: &Path) -> Option<LanguageConfig> {
    let extension = path.extension()?.to_str()?;
    if !extension.eq_ignore_ascii_case("txt") {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Sample";
    config.line_comment = Some("//");
    config.block_comment = Some(("/*", "*/"));
    config.indent_unit = "    ";
    Some(config)
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
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

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<LayoutLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("layout-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: text_language,
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
    let setup = project_setup(scratch("layout-cache"));
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

fn method(
    owner: &str,
    name: &str,
    parameters: &[&str],
    return_type: Option<&str>,
) -> OverridableMethod {
    OverridableMethod {
        name: name.to_owned(),
        owner: owner.to_owned(),
        parameters: parameters.iter().map(|p| (*p).to_owned()).collect(),
        return_type: return_type.map(str::to_owned),
        abstract_method: false,
        handle: format!("{owner}#{name}"),
    }
}

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<Palette> {
    let view = window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("a popup is open");
    view.downcast::<Palette>()
        .expect("the popup is the palette")
}

fn inside(cx: &mut TestAppContext, window: WindowHandle<Workspace>, popup: &'static str) {
    let editor = bounds_of(cx, window, "editor-body-0");
    let bounds = bounds_of(cx, window, popup);
    assert!(
        bounds.top() >= editor.top(),
        "{popup} does not leave the top of the editor: {bounds:?} in {editor:?}"
    );
    assert!(
        bounds.bottom() <= editor.bottom(),
        "{popup} does not leave the bottom of the editor: {bounds:?} in {editor:?}"
    );
    assert!(
        bounds.left() >= editor.left() && bounds.right() <= editor.right(),
        "{popup} stays inside the editor: {bounds:?} in {editor:?}"
    );
}

const BRACES: &str = "void m() {\n    if (x) {\n        body();\n    }\n}\n";

#[ui_kit::gpui::test]
fn russian_punctuation_keys_run_the_actions_of_their_latin_keys(cx: &mut TestAppContext) {
    let root = project("layout-russian", &[("src/A.txt", BRACES)]);
    let window = open_window(cx, root.clone(), LayoutLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("A.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(2, 8, cx));
    press(cx, window, "ctrl-х");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (1, 11));
    press(cx, window, "ctrl-ъ");
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 4));

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    press(cx, window, "ctrl-?");
    assert_eq!(
        view.read_with(cx, |view, _| view.line_text(0)),
        Some("/*void m() {*/".to_owned())
    );
    press(cx, window, "ctrl-я");
    assert_eq!(
        view.read_with(cx, |view, _| view.line_text(0)),
        Some(BRACES.lines().next().unwrap().to_owned())
    );
}

#[ui_kit::gpui::test]
fn file_structure_popup_shows_the_signatures_and_filters_by_name(cx: &mut TestAppContext) {
    let text = "class OwnerController {\n  String processFindForm(int page, Owner owner) {\n    return \"\";\n  }\n  OwnerRepository owners;\n}\n";
    let root = project("layout-structure", &[("src/OwnerController.txt", text)]);
    let language = LayoutLanguage::new();
    let method_at = text.find("String processFindForm").unwrap();
    let method_end = text.find("  OwnerRepository").unwrap();
    let field_at = text.find("  OwnerRepository").unwrap();
    let field_end = field_at + "  OwnerRepository owners;".len();
    *language.symbols.lock().unwrap() = vec![FileSymbol {
        name: "OwnerController".to_owned(),
        detail: None,
        kind: SymbolKind::Class,
        range: span_in(text, 0, text.len()),
        name_range: span_in(text, 6, 21),
        children: vec![
            FileSymbol {
                name: "processFindForm".to_owned(),
                detail: Some("(int, Owner) : String".to_owned()),
                kind: SymbolKind::Method,
                range: span_in(text, method_at, method_end),
                name_range: span_in(text, method_at + 7, method_at + 22),
                children: Vec::new(),
            },
            FileSymbol {
                name: "owners".to_owned(),
                detail: Some(": OwnerRepository".to_owned()),
                kind: SymbolKind::Field,
                range: span_in(text, field_at, field_end),
                name_range: span_in(text, field_at + 20, field_at + 26),
                children: Vec::new(),
            },
        ],
    }];
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("OwnerController.txt");
    open_path(cx, window, &path);
    cx.run_until_parked();
    open_palette(cx, window, keys::FILE_STRUCTURE);
    draw(cx, window);

    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert!(
        rows.contains(&(
            "processFindForm".to_owned(),
            "(int, Owner): String".to_owned()
        )),
        "{rows:?}"
    );
    assert!(
        rows.contains(&("owners".to_owned(), ": OwnerRepository".to_owned())),
        "{rows:?}"
    );
    assert!(
        bounds_of(cx, window, "palette-signature-1").size.height > px(0.),
        "the signature is a part of the row"
    );

    type_text(cx, window, "proc");
    draw(cx, window);
    let rows = palette(cx, window).read_with(cx, |palette, _| palette.rows_text());
    assert_eq!(rows.len(), 1, "the filter still matches the name: {rows:?}");
    assert_eq!(rows[0].0, "processFindForm");
}

#[ui_kit::gpui::test]
fn pick_popups_at_the_caret_stay_inside_the_editor(cx: &mut TestAppContext) {
    let mut text = String::from("class A {\n");
    for index in 0..24 {
        text.push_str(&format!("  int field{index};\n"));
    }
    text.push_str("}\n");
    let root = project("layout-popups", &[("src/A.txt", &text)]);
    let language = LayoutLanguage::new();
    *language.overridable.lock().unwrap() = vec![
        method("Object", "toString", &[], Some("String")),
        method("Object", "hashCode", &[], Some("int")),
    ];
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
                checked: false,
                handle: "age".to_owned(),
            },
        ],
        change: None,
        files: Vec::new(),
        error: None,
    });
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src").join("A.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(24, 0, cx));
    press(cx, window, "ctrl-o");
    draw(cx, window);
    inside(cx, window, "override-popup");
    assert!(
        bounds_of(cx, window, "override-checkbox-0").size.height > px(0.),
        "a real checkbox is in the row"
    );
    assert!(
        bounds_of(cx, window, "override-return-type-0").size.height > px(0.),
        "the return type is in the row"
    );
    press(cx, window, "escape");

    press(cx, window, "alt-insert");
    draw(cx, window);
    inside(cx, window, "actions-popup");
    press(cx, window, "enter");
    draw(cx, window);
    inside(cx, window, "generate-popup");
    assert!(
        bounds_of(cx, window, "generate-checkbox-0").size.height > px(0.),
        "a real checkbox is in the row"
    );
}
