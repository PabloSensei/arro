//! Alt+Insert while the language server imports or has failed. The Generate list still opens
//! with the rows that the symbols of the class give, and choosing a row says in the status bar
//! that the server is not ready, without changing the text. The server never answers the
//! questions it is asked, as a server that is still importing does not answer them, so a list
//! that waits for an answer would never open.
//!
//! cargo test -p panels window_tests_generate_server

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    GenerateApplyProvider, GenerateApplyRequest, GenerateProvider, GenerateQuery, Generation,
    LanguageId, LanguageSupport, OverridableMethods, OverridableMethodsProvider,
    OverridableMethodsQuery, Provider, Reply, Symbol, SymbolKind, SymbolProvider, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, press, project_setup, scratch};
use crate::window_tests_generate_list::{GenerateLanguage, alt_insert, project};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

/// The status bar line of a row that asks the server while the server is not ready.
const NOT_READY: &str = "The language server is not ready yet";

/// The rows the list keeps in a class that has a field.
const USUAL_ROWS: [&str; 8] = [
    "Constructor",
    "Getter",
    "Setter",
    "Getter and Setter",
    "equals() and hashCode()",
    "toString()",
    "Override Methods",
    "Implement Methods",
];

/// A language whose server is not ready. Its questions are held and never answered, and its
/// symbols are those of the test text: the class `A` with the field `name`.
struct NotReady {
    /// The syntax comes from the fake of the Generate list.
    language: Arc<GenerateLanguage>,
    symbols: Vec<Symbol>,
    generate: Mutex<Vec<Reply<Generation>>>,
    apply: Mutex<Vec<Reply<Generation>>>,
    overrides: Mutex<Vec<Reply<OverridableMethods>>>,
}

impl NotReady {
    fn new(text: &str) -> Arc<Self> {
        let field = text.find("name").expect("the field is in the text");
        Arc::new(Self {
            language: GenerateLanguage::new(),
            symbols: vec![
                Symbol {
                    name: "A".to_owned(),
                    kind: SymbolKind::Class,
                    container: None,
                    depth: 0,
                    range: 0..text.len() - 1,
                    name_range: 6..7,
                },
                Symbol {
                    name: "name".to_owned(),
                    kind: SymbolKind::Field,
                    container: Some("A".to_owned()),
                    depth: 1,
                    range: field..field + 4,
                    name_range: field..field + 4,
                },
            ],
            generate: Mutex::new(Vec::new()),
            apply: Mutex::new(Vec::new()),
            overrides: Mutex::new(Vec::new()),
        })
    }
}

impl Provider<GenerateQuery, Generation> for NotReady {
    fn start(&self, _: GenerateQuery, _: base::Cancel, reply: Reply<Generation>) {
        self.generate.lock().unwrap().push(reply);
    }
}

impl Provider<GenerateApplyRequest, Generation> for NotReady {
    fn start(&self, _: GenerateApplyRequest, _: base::Cancel, reply: Reply<Generation>) {
        self.apply.lock().unwrap().push(reply);
    }
}

impl Provider<OverridableMethodsQuery, OverridableMethods> for NotReady {
    fn start(&self, _: OverridableMethodsQuery, _: base::Cancel, reply: Reply<OverridableMethods>) {
        self.overrides.lock().unwrap().push(reply);
    }
}

impl SymbolProvider for NotReady {
    fn file_symbols(&self, _: &Path, _: &str) -> Vec<Symbol> {
        self.symbols.clone()
    }
}

impl LanguageSupport for NotReady {
    fn id(&self) -> LanguageId {
        LanguageId("fake-generate-not-ready")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.language.syntax()
    }

    fn symbols(&self) -> Option<&dyn SymbolProvider> {
        Some(self)
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

    fn server_ready(&self) -> bool {
        false
    }
}

/// The window of the project `root`, whose server is `language` and says `status` on its line.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<NotReady>,
    status: &str,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("generate-server-state");
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
    let setup = project_setup(scratch("generate-server-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status_lines) = futures_channel::mpsc::unbounded();
    status_sender.unbounded_send(status.to_string()).ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status: status_lines,
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

/// The message of the status bar, once a row wrote it.
fn status_message(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.status_message().map(|text| text.to_string())
        })
        .unwrap()
}

/// Chooses the row `label` of the open list `rows`, moving down from its first row.
fn choose(cx: &mut TestAppContext, window: WindowHandle<Workspace>, rows: &[String], label: &str) {
    let index = rows
        .iter()
        .position(|row| row == label)
        .unwrap_or_else(|| panic!("no {label} row in {rows:?}"));
    for _ in 0..index {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
}

#[ui_kit::gpui::test]
fn alt_insert_lists_the_usual_rows_while_the_server_imports(cx: &mut TestAppContext) {
    let text = "class A {\n    private String name;\n}\n";
    let (root, path) = project("generate-server-importing", text);
    let window = open_window(cx, root, NotReady::new(text), "Language server importing");
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    for row in USUAL_ROWS {
        assert!(rows.contains(&row.to_owned()), "{row} is listed: {rows:?}");
    }
    choose(cx, window, &rows, "Getter");
    wait_for(cx, "the status message", |cx| {
        status_message(cx, window).as_deref() == Some(NOT_READY)
    });
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "a row chosen while the server imports changes no text"
    );
}

#[ui_kit::gpui::test]
fn alt_insert_lists_the_usual_rows_when_the_server_has_failed(cx: &mut TestAppContext) {
    let text = "class A {\n    private String name;\n}\n";
    let (root, path) = project("generate-server-failed", text);
    let window = open_window(cx, root, NotReady::new(text), "Language server failed");
    cx.run_until_parked();
    draw(cx, window);

    let rows = alt_insert(cx, window, &path, 0, 9).expect("the list opens");
    for row in USUAL_ROWS {
        assert!(rows.contains(&row.to_owned()), "{row} is listed: {rows:?}");
    }
    choose(cx, window, &rows, "Implement Methods");
    wait_for(cx, "the status message", |cx| {
        status_message(cx, window).as_deref() == Some(NOT_READY)
    });
    draw(cx, window);
    assert_eq!(
        buffer_of(cx, &editor_of(cx, window, &path)),
        text,
        "Implement Methods chosen after a failed import changes no text"
    );
}
