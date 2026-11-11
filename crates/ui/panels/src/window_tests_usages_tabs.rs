//! Find Usages: the flags of the dialog, the tabs of the Usages window and the
//! declaration left out of the list, driven by a language of the test.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, FileSymbol, FileSymbolsProvider, FileSymbolsQuery,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SourceId, SourceSpan,
    SyntaxProvider, TextPoint, UsageEntry, UsageKind,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::usages::{ActiveShowUsages, ActiveUsages, PendingFindUsages};
use crate::window_tests::{click, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{project, span_in, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("usages-tabs-server");

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

/// The language of these tests: the uses of each point and the declarations of the
/// file are set by the test.
pub(crate) struct UsageLanguage {
    syntax: NoSyntax,
    usages: Mutex<Vec<UsageEntry>>,
    /// The uses of one place, by the line and column of the query.
    at: Mutex<HashMap<(u32, u32), Vec<UsageEntry>>>,
    symbols: Mutex<Vec<FileSymbol>>,
}

impl UsageLanguage {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            usages: Mutex::new(Vec::new()),
            at: Mutex::new(HashMap::new()),
            symbols: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn set_usages(&self, entries: Vec<UsageEntry>) {
        *self.usages.lock().unwrap() = entries;
    }

    pub(crate) fn set_usages_at(&self, point: TextPoint, entries: Vec<UsageEntry>) {
        self.at
            .lock()
            .unwrap()
            .insert((point.line, point.character), entries);
    }

    pub(crate) fn set_symbols(&self, symbols: Vec<FileSymbol>) {
        *self.symbols.lock().unwrap() = symbols;
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for UsageLanguage {
    fn start(
        &self,
        spot: DocumentSpot,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<UsageEntry>>,
    ) {
        let at = self
            .at
            .lock()
            .unwrap()
            .get(&(spot.point.line, spot.point.character))
            .cloned()
            .unwrap_or_else(|| self.usages.lock().unwrap().clone());
        let _ = reply.send(Answer::new(at, Confidence::Certain, SOURCE));
    }
}

impl Provider<FileSymbolsQuery, Vec<FileSymbol>> for UsageLanguage {
    fn start(
        &self,
        _: FileSymbolsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<FileSymbol>>,
    ) {
        let symbols = self.symbols.lock().unwrap().clone();
        let _ = reply.send(Answer::new(symbols, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for UsageLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("usages-tabs")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        Some(self)
    }

    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        Some(self)
    }
}

pub(crate) fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<UsageLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
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
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

pub(crate) fn usage(
    path: &Path,
    text: &str,
    start: usize,
    end: usize,
    kind: UsageKind,
) -> UsageEntry {
    UsageEntry {
        target: SourceSpan {
            uri: uri_of(path),
            span: span_in(text, start, end),
        },
        kind,
        container: Some("Main.run".to_owned()),
    }
}

pub(crate) fn hub(cx: &TestAppContext) -> gpui::Entity<crate::usages::UsagesHub> {
    cx.read(|cx| cx.global::<ActiveUsages>().0.clone())
}

pub(crate) fn counts(cx: &TestAppContext) -> usize {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().len())
}

pub(crate) fn labels(cx: &TestAppContext) -> Vec<String> {
    hub(cx)
        .read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec())
        .into_iter()
        .map(|item| item.label)
        .collect()
}

pub(crate) fn title(cx: &TestAppContext) -> String {
    hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).title().to_owned())
}

pub(crate) fn tabs(cx: &TestAppContext) -> Vec<String> {
    hub(cx).read_with(cx, |hub, _| hub.tab_names())
}

fn popup(cx: &TestAppContext) -> Option<gpui::Entity<crate::usages::ShowUsagesPopup>> {
    cx.read(|cx| {
        cx.try_global::<ActiveShowUsages>()
            .and_then(|active| active.0.clone())
    })
}

fn popup_rows(cx: &TestAppContext) -> usize {
    popup(cx)
        .map(|popup| popup.read_with(cx, |popup, _| popup.visible_rows()))
        .unwrap_or(0)
}

fn popup_done(cx: &TestAppContext) -> bool {
    popup(cx)
        .map(|popup| popup.read_with(cx, |popup, _| popup.is_done()))
        .unwrap_or(false)
}

pub(crate) fn field(name: &str, text: &str, start: usize, end: usize) -> FileSymbol {
    FileSymbol {
        name: name.to_owned(),
        detail: None,
        kind: lang_api::SymbolKind::Field,
        range: span_in(text, start, end),
        name_range: span_in(text, start, end),
        children: Vec::new(),
    }
}

pub(crate) fn method(name: &str, text: &str, start: usize, end: usize) -> FileSymbol {
    FileSymbol {
        name: name.to_owned(),
        detail: None,
        kind: lang_api::SymbolKind::Method,
        range: span_in(text, start, end),
        name_range: span_in(text, start, end),
        children: Vec::new(),
    }
}

pub(crate) fn class(name: &str, text: &str, children: Vec<FileSymbol>) -> FileSymbol {
    FileSymbol {
        name: name.to_owned(),
        detail: None,
        kind: lang_api::SymbolKind::Class,
        range: span_in(text, 0, text.len()),
        name_range: span_in(
            text,
            text.find(name).unwrap_or(0),
            text.find(name).unwrap_or(0) + name.len(),
        ),
        children,
    }
}

pub(crate) fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn the_dialog_shows_the_flags_of_a_class_and_find_searches_at_once(cx: &mut TestAppContext) {
    let text = "class Owner {\n  void save() {}\n  String name;\n}\n";
    let root = project(
        "usages-flags-class",
        &[("src/Main.txt", text), ("src/Other.txt", "save\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    language.set_symbols(vec![class(
        "Owner",
        text,
        vec![
            method(
                "save",
                text,
                text.find("save").unwrap(),
                text.find("save").unwrap() + 4,
            ),
            field(
                "name",
                text,
                text.find("name").unwrap(),
                text.find("name").unwrap() + 4,
            ),
        ],
    )]);
    let save = span_in(
        text,
        text.find("save").unwrap(),
        text.find("save").unwrap() + 4,
    );
    language.set_usages_at(
        save.start,
        vec![usage(&other, "save\n", 0, 4, UsageKind::Call)],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 7, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    assert!(
        visible(cx, window, "find-usages-settings"),
        "Ctrl+Alt+Shift+F7 opens the dialog"
    );
    wait_for(cx, "the flags of the class", |cx| {
        visible(cx, window, "find-usages-methods")
    });
    assert!(visible(cx, window, "find-usages-check-usages"));
    assert!(visible(cx, window, "find-usages-check-methods"));
    assert!(visible(cx, window, "find-usages-check-fields"));
    assert!(visible(cx, window, "find-usages-check-derived"));
    assert!(
        !visible(cx, window, "find-usages-overriding"),
        "a class has no overriding or implementing methods"
    );

    click(cx, window, "find-usages-usages");
    click(cx, window, "find-usages-fields");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the usages of the methods", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && counts(cx) == 1
    });
    draw(cx, window);
    assert_eq!(labels(cx), vec!["save".to_owned()]);
    assert!(title(cx).contains("methods"), "{}", title(cx));
    assert_eq!(tabs(cx), vec!["Owner".to_owned()]);
    assert!(
        cx.read(|cx| cx.global::<PendingFindUsages>().0.clone())
            .is_some_and(|options| !options.usages && !options.fields),
        "Find keeps its flags for the next Alt+F7"
    );
}

#[ui_kit::gpui::test]
fn the_dialog_of_a_field_filters_readers_and_writers(cx: &mut TestAppContext) {
    let text = "class Thing {\n  int value;\n}\n";
    let root = project(
        "usages-flags-field",
        &[
            ("src/Main.txt", text),
            ("src/Read.txt", "value\n"),
            ("src/Write.txt", "value\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let reader = root.join("src/Read.txt");
    let writer = root.join("src/Write.txt");
    let language = UsageLanguage::new();
    language.set_symbols(vec![class(
        "Thing",
        text,
        vec![field(
            "value",
            text,
            text.find("value").unwrap(),
            text.find("value").unwrap() + 5,
        )],
    )]);
    language.set_usages(vec![
        usage(&reader, "value\n", 0, 5, UsageKind::Read),
        usage(&writer, "value\n", 0, 5, UsageKind::Write),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(1, 6, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the flags of the field", |cx| {
        visible(cx, window, "find-usages-readers")
    });
    assert!(visible(cx, window, "find-usages-check-readers"));
    assert!(visible(cx, window, "find-usages-check-writers"));
    assert!(!visible(cx, window, "find-usages-methods"));

    click(cx, window, "find-usages-writers");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the readers of the field", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && counts(cx) == 1
    });
    draw(cx, window);
    let items = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].path, reader, "the writers are filtered out");
    assert!(title(cx).contains("fields"), "{}", title(cx));
}

#[ui_kit::gpui::test]
fn a_new_search_replaces_an_unpinned_tab_and_a_pinned_one_opens_another(cx: &mut TestAppContext) {
    let text = "alpha beta\n";
    let root = project(
        "usages-tabs",
        &[("src/Main.txt", text), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = UsageLanguage::new();
    let alpha = usage(&other, "alpha\n", 0, 5, UsageKind::Read);
    let beta = usage(&other, "alpha\n", 0, 5, UsageKind::Read);
    language.set_usages_at(
        TextPoint {
            line: 0,
            character: 0,
        },
        vec![alpha],
    );
    language.set_usages_at(
        TextPoint {
            line: 0,
            character: 6,
        },
        vec![beta],
    );
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);

    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    press(cx, window, "alt-f7");
    wait_for(cx, "the first tab", |cx| !tabs(cx).is_empty());
    assert_eq!(tabs(cx), vec!["alpha".to_owned()]);
    draw(cx, window);
    assert!(
        visible(cx, window, "usage-tabs"),
        "the window shows the tabs"
    );

    view.update(cx, |view, cx| view.restore_caret(0, 6, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "alt-f7");
    wait_for(cx, "the replaced tab", |cx| {
        tabs(cx) == vec!["beta".to_owned()]
    });
    assert_eq!(tabs(cx).len(), 1, "an unpinned tab is replaced");

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the dialog", |cx| {
        visible(cx, window, "find-usages-new-tab")
    });
    click(cx, window, "find-usages-new-tab");
    click(cx, window, "find-usages-find");
    wait_for(cx, "the second tab", |cx| tabs(cx).len() == 2);
    assert_eq!(tabs(cx), vec!["beta".to_owned(), "beta".to_owned()]);

    let pinned = hub(cx);
    pinned.update(cx, |hub, cx| hub.set_pinned(1, true, cx));
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);
    press(cx, window, "alt-f7");
    wait_for(cx, "the search of a pinned tab", |cx| tabs(cx).len() == 3);
    assert_eq!(
        tabs(cx),
        vec!["beta".to_owned(), "beta".to_owned(), "alpha".to_owned()]
    );

    pinned.update(cx, |hub, cx| hub.close_tab(0, cx));
    assert_eq!(tabs(cx).len(), 2, "a tab closes");
    assert_eq!(hub(cx).read_with(cx, |hub, _| hub.active_tab()), 1);
}

#[ui_kit::gpui::test]
fn the_dialog_of_a_method_shows_the_hierarchy_flags(cx: &mut TestAppContext) {
    let text = "void run() {}\n";
    let root = project("usages-flags-method", &[("src/Main.txt", text)]);
    let main = root.join("src/Main.txt");
    let language = UsageLanguage::new();
    let run = text.find("run").unwrap();
    language.set_symbols(vec![method("run", text, run, run + 3)]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_for(cx, "the flags of the method", |cx| {
        visible(cx, window, "find-usages-overriding")
    });
    assert!(visible(cx, window, "find-usages-check-usages"));
    assert!(visible(cx, window, "find-usages-check-overriding"));
    assert!(visible(cx, window, "find-usages-check-implementing"));
    assert!(
        !visible(cx, window, "find-usages-methods"),
        "a method has no usages of its methods"
    );
    assert!(!visible(cx, window, "find-usages-readers"));
}

#[ui_kit::gpui::test]
fn show_usages_leaves_the_declaration_out(cx: &mut TestAppContext) {
    let text = "greet\n";
    let root = project(
        "usages-declaration",
        &[
            ("src/Main.txt", text),
            ("src/Other.txt", text),
            ("src/Third.txt", text),
        ],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let third = root.join("src/Third.txt");
    let language = UsageLanguage::new();
    language.set_usages(vec![
        usage(&main, text, 0, 5, UsageKind::Declaration),
        usage(&other, text, 0, 5, UsageKind::Read),
        usage(&third, text, 0, 5, UsageKind::Read),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-f7");
    draw(cx, window);
    wait_for(cx, "the popup", |cx| popup_done(cx));
    assert_eq!(popup_rows(cx), 2, "the declaration is not a usage");
}

/// The first path of the uses of the methods of the real Owner class, for the test.
fn owner_method_name(label: &str) -> bool {
    let name = label.split('(').next().unwrap_or(label);
    matches!(
        name,
        "getAddress"
            | "setAddress"
            | "getCity"
            | "setCity"
            | "getTelephone"
            | "setTelephone"
            | "getPets"
            | "addPet"
            | "getPet"
            | "toString"
            | "addVisit"
    )
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn readers_and_writers_of_a_field_in_a_closed_file_are_known(cx: &mut TestAppContext) {
    use crate::window_tests_real::{build_window, close, wait_for as wait_long, wait_ready};

    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("usages-closed-field").join("petclinic");
    window_language::prepare_project(&source, &root);
    let source = "package probe;\npublic class FieldHolder {\n    public int value;\n}\n";
    let reader = "package probe;\npublic class FieldReader {\n    int read(FieldHolder holder) {\n        return holder.value;\n    }\n}\n";
    let writer = "package probe;\npublic class FieldWriter {\n    void write(FieldHolder holder) {\n        holder.value = 1;\n    }\n}\n";
    let holder = window_language::write_probe(&root, "probe", "FieldHolder", source);
    window_language::write_probe(&root, "probe", "FieldReader", reader);
    window_language::write_probe(&root, "probe", "FieldWriter", writer);
    let language =
        window_language::RealLanguage::open(root.clone(), scratch("usages-closed-field-state"))
            .expect("the language server starts");

    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    open_path(cx, window, &holder);
    let view = editor_of(cx, window, &holder);
    let text = crate::window_tests::buffer_of(cx, &view);
    let line = text
        .lines()
        .position(|line| line.contains("public int value;"))
        .expect("the field is in the file");
    let column = text
        .lines()
        .nth(line)
        .and_then(|line| line.find("value"))
        .expect("the field has a name");
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_long(
        cx,
        "the usages of the field",
        Duration::from_secs(120),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
                .unwrap()
                && counts(cx) > 0
        },
    );
    draw(cx, window);
    click(cx, window, "item-button-usage-group-type");
    draw(cx, window);
    let groups = hub(cx).read_with(cx, |hub, cx| hub.group_labels(cx));
    assert!(groups.iter().any(|label| label == "Read"), "{groups:?}");
    assert!(groups.iter().any(|label| label == "Write"), "{groups:?}");
    let paths: Vec<PathBuf> = hub(cx)
        .read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec())
        .into_iter()
        .map(|item| item.path)
        .collect();
    let named = |stem: &str| {
        paths
            .iter()
            .any(|path| path.file_stem().is_some_and(|name| name == stem))
    };
    assert!(
        named("FieldReader"),
        "a use in a closed file is found: {paths:?}"
    );
    assert!(
        named("FieldWriter"),
        "a write in a closed file is found: {paths:?}"
    );

    close(cx, window, language, true);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn usages_of_the_methods_of_the_owner_class_are_found(cx: &mut TestAppContext) {
    use crate::window_tests_real::{
        build_window, close, project as real_project, wait_for as wait_long, wait_ready,
    };

    let (root, language) = real_project("usages-methods-owner");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let owner = window_language::main_class(
        &root,
        "org.springframework.samples.petclinic.owner",
        "Owner",
    );
    open_path(cx, window, &owner);
    let view = editor_of(cx, window, &owner);
    let text = crate::window_tests::buffer_of(cx, &view);
    let line = text
        .lines()
        .position(|line| line.contains("public class Owner"))
        .expect("the file declares Owner");
    let column = text
        .lines()
        .nth(line)
        .and_then(|line| line.find("Owner"))
        .expect("the class has a name");
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    wait_long(cx, "the flags of a class", Duration::from_secs(120), |cx| {
        visible(cx, window, "find-usages-methods")
    });
    click(cx, window, "find-usages-usages");
    click(cx, window, "find-usages-fields");
    click(cx, window, "find-usages-derived");
    click(cx, window, "find-usages-find");
    wait_long(
        cx,
        "the usages of the methods",
        Duration::from_secs(600),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
                .unwrap()
                && labels(cx).iter().any(|label| owner_method_name(label))
        },
    );
    draw(cx, window);
    let found = labels(cx);
    assert!(
        found.iter().any(|label| owner_method_name(label)),
        "the methods of Owner have uses: {found:?}"
    );
    assert!(title(cx).contains("methods"), "{}", title(cx));
    assert_eq!(tabs(cx), vec!["Owner".to_owned()]);

    close(cx, window, language, true);
}
