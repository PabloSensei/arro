//! The Hierarchy window in its Type view: the type under the caret, its supertypes and
//! its subtypes, one request per expanded row.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, HierarchyDirection, LanguageFn, LanguageId, LanguageSupport, Provider,
    Queries, SourceId, SourceSpan, SyntaxProvider, TypeHierarchyItem, TypeHierarchyQuery, TypeKind,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::outline::SymbolsFor;
use crate::type_hierarchy::{ActiveTypeHierarchy, HierarchyView};
use crate::window_tests::{
    active_path, bounds_of, click, draw, open_path, press, project_setup, scratch,
};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-type-hierarchy");

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
    calls: Mutex<Vec<(String, Option<HierarchyDirection>)>>,
    /// The requests that were left without an answer.
    pending: Mutex<Vec<lang_api::Reply<Vec<TypeHierarchyItem>>>>,
    silent: Mutex<bool>,
}

impl TypeLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            roots: Mutex::new(Vec::new()),
            neighbours: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            silent: Mutex::new(false),
        })
    }

    fn root(&self, item: TypeHierarchyItem) {
        *self.roots.lock().unwrap() = vec![item];
    }

    fn supertypes(&self, name: &str, items: Vec<TypeHierarchyItem>) {
        self.neighbours.lock().unwrap().push((
            name.to_owned(),
            HierarchyDirection::Supertypes,
            items,
        ));
    }

    fn subtypes(&self, name: &str, items: Vec<TypeHierarchyItem>) {
        self.neighbours.lock().unwrap().push((
            name.to_owned(),
            HierarchyDirection::Subtypes,
            items,
        ));
    }

    fn calls(&self) -> Vec<(String, Option<HierarchyDirection>)> {
        self.calls.lock().unwrap().clone()
    }
}

impl Provider<TypeHierarchyQuery, Vec<TypeHierarchyItem>> for TypeLanguage {
    fn start(
        &self,
        query: TypeHierarchyQuery,
        cancel: base::Cancel,
        mut reply: lang_api::Reply<Vec<TypeHierarchyItem>>,
    ) {
        let call = match &query {
            TypeHierarchyQuery::Root(_) => ("root".to_owned(), None),
            TypeHierarchyQuery::Neighbours { item, direction } => {
                (item.name.clone(), Some(*direction))
            }
        };
        self.calls.lock().unwrap().push(call);
        if *self.silent.lock().unwrap() && matches!(query, TypeHierarchyQuery::Root(_)) {
            // The reply stays open, as a server that is still thinking keeps it.
            let _ = cancel;
            self.pending.lock().unwrap().push(reply);
            return;
        }
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
        LanguageId("fake-type-hierarchy")
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
    language: Arc<TypeLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("type-hierarchy-state");
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
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let setup = project_setup(scratch("type-hierarchy-cache"));
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

fn item(
    name: &str,
    kind: TypeKind,
    path: &Path,
    text: &str,
    detail: Option<&str>,
) -> TypeHierarchyItem {
    let offset = text
        .find("class ")
        .map_or(0, |start| start + "class ".len());
    TypeHierarchyItem {
        name: name.to_owned(),
        detail: detail.map(str::to_owned),
        kind,
        abstract_type: false,
        place: Some(SourceSpan {
            uri: uri_of(path),
            span: span_in(text, offset, offset + name.len()),
        }),
        handle: name.to_owned(),
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<crate::type_hierarchy::TypeHierarchyPanel> {
    cx.update(|cx| cx.global::<ActiveTypeHierarchy>().0.clone())
}

#[ui_kit::gpui::test]
fn ctrl_h_opens_the_window_with_the_root(cx: &mut TestAppContext) {
    let text = "class Circle {\n}\n";
    let root = project("type-hierarchy-open", &[("src/Circle.txt", text)]);
    let path = root.join("src").join("Circle.txt");
    let language = TypeLanguage::new();
    language.root(item(
        "Circle",
        TypeKind::Class,
        &path,
        text,
        Some("com.example"),
    ));
    language.subtypes(
        "Circle",
        vec![item("Round", TypeKind::Class, &path, text, None)],
    );
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-h");
    draw(cx, window);

    assert!(
        bounds_of(cx, window, "type-hierarchy-panel").size.height > px(0.),
        "Ctrl+H shows the Hierarchy window"
    );
    let panel = panel(cx);
    wait_for(cx, "the root of the type hierarchy", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.contains("Circle"))
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.view()),
        HierarchyView::Type
    );
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert!(
        lines
            .iter()
            .any(|line| line.trim() == "Circle  com.example"),
        "{lines:?}"
    );
}

#[ui_kit::gpui::test]
fn expanding_a_row_asks_the_language_for_its_subtypes(cx: &mut TestAppContext) {
    let text = "class Circle {\n}\n";
    let root = project("type-hierarchy-expand", &[("src/Circle.txt", text)]);
    let path = root.join("src").join("Circle.txt");
    let language = TypeLanguage::new();
    language.root(item("Circle", TypeKind::Class, &path, text, None));
    language.subtypes(
        "Circle",
        vec![item("Round", TypeKind::Class, &path, text, None)],
    );
    language.subtypes(
        "Round",
        vec![item("SmallRound", TypeKind::Class, &path, text, None)],
    );
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-h");
    draw(cx, window);
    let panel = panel(cx);

    click(cx, window, "type-hierarchy-view-subtypes");
    draw(cx, window);
    wait_for(cx, "the subtypes of the root", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.trim() == "Round")
    });

    panel.update(cx, |panel, cx| panel.expand(1, cx));
    wait_for(cx, "the subtypes of a row", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.trim() == "SmallRound")
    });
    let calls = language.calls();
    assert!(
        calls
            .iter()
            .any(|(name, direction)| name == "Round"
                && *direction == Some(HierarchyDirection::Subtypes)),
        "the window asked for the subtypes of the expanded row: {calls:?}"
    );
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert_eq!(lines, ["Circle", "  Round", "    SmallRound"], "{lines:?}");
}

#[ui_kit::gpui::test]
fn switching_to_supertypes_shows_the_types_above(cx: &mut TestAppContext) {
    let text = "class Circle {\n}\n";
    let root = project("type-hierarchy-supertypes", &[("src/Circle.txt", text)]);
    let path = root.join("src").join("Circle.txt");
    let language = TypeLanguage::new();
    language.root(item("Circle", TypeKind::Class, &path, text, None));
    language.supertypes(
        "Circle",
        vec![item("Shape", TypeKind::Interface, &path, text, None)],
    );
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-h");
    draw(cx, window);

    click(cx, window, "type-hierarchy-view-supertypes");
    draw(cx, window);
    let panel = panel(cx);
    wait_for(cx, "the supertypes of the type", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.trim() == "Shape")
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.view()),
        HierarchyView::Supertypes
    );
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert_eq!(lines, ["Circle", "  Shape"], "{lines:?}");
}

#[ui_kit::gpui::test]
fn enter_opens_the_file_at_the_declaration(cx: &mut TestAppContext) {
    let circle_text = "class Circle {\n}\n";
    let base_text = "class Base {\n}\n";
    let root = project(
        "type-hierarchy-enter",
        &[("src/Circle.txt", circle_text), ("src/Base.txt", base_text)],
    );
    let circle = root.join("src").join("Circle.txt");
    let base = root.join("src").join("Base.txt");
    let language = TypeLanguage::new();
    language.root(item("Circle", TypeKind::Class, &circle, circle_text, None));
    language.supertypes(
        "Circle",
        vec![item("Base", TypeKind::Class, &base, base_text, None)],
    );
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &circle);
    press(cx, window, "ctrl-h");
    draw(cx, window);
    let panel = panel(cx);

    wait_for(cx, "the chain of supertypes", |cx| {
        panel
            .read_with(cx, |panel, _| panel.lines())
            .iter()
            .any(|line| line.trim() == "Base")
    });
    // The selected type is the one under the caret; `Base` is one row above it.
    press(cx, window, "up");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_line()),
        Some("Base".to_owned())
    );
    press(cx, window, "enter");
    draw(cx, window);
    wait_for(cx, "the declaration of the type", |cx| {
        active_path(cx, window) == Some(base.clone())
    });
}

#[ui_kit::gpui::test]
fn closing_the_window_cancels_the_requests_in_flight(cx: &mut TestAppContext) {
    let text = "class Circle {\n}\n";
    let root = project("type-hierarchy-cancel", &[("src/Circle.txt", text)]);
    let path = root.join("src").join("Circle.txt");
    let language = TypeLanguage::new();
    *language.silent.lock().unwrap() = true;
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-h");
    draw(cx, window);

    wait_for(cx, "the request of the window", |_| {
        !language.pending.lock().unwrap().is_empty()
    });
    assert!(
        !language.pending.lock().unwrap()[0].is_cancelled(),
        "the request is in flight"
    );

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        cx.shutdown();
        cx.clear_globals();
    });
    cx.run_until_parked();
    assert!(
        language.pending.lock().unwrap()[0].is_cancelled(),
        "closing the window drops the reply and cancels the request"
    );
}
