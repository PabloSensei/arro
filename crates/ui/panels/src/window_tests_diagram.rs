//! The Diagram tab: the types around a place and a folder, the sections of the toolbar,
//! adding and deleting a type, dragging a node and the SVG export.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentEdit, GraphEdge, GraphEdgeKind, GraphMember, GraphMemberKind,
    GraphType, GraphTypeKind, GraphVisibility, LanguageFn, LanguageId, LanguageSupport, Provider,
    Queries, SemanticServer, SourceId, SourceSpan, Symbol, SymbolKind, SymbolProvider,
    SyntaxProvider, TypeGraph, TypeGraphProvider, TypeGraphQuery,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::diagram::DiagramView;
use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, click, draw, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-diagram");

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

/// A language that answers the diagram from the graph the test sets.
struct DiagramLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    graph: Mutex<TypeGraph>,
    added: Mutex<TypeGraph>,
}

impl DiagramLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            graph: Mutex::new(TypeGraph::default()),
            added: Mutex::new(TypeGraph::default()),
        })
    }
}

impl SemanticServer for DiagramLanguage {
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

impl Provider<TypeGraphQuery, TypeGraph> for DiagramLanguage {
    fn start(&self, query: TypeGraphQuery, _: base::Cancel, mut reply: lang_api::Reply<TypeGraph>) {
        let graph = match query {
            TypeGraphQuery::Type(_) => self.added.lock().unwrap().clone(),
            _ => self.graph.lock().unwrap().clone(),
        };
        reply
            .send(Answer::new(graph, Confidence::Certain, SOURCE))
            .ok();
    }
}

impl LanguageSupport for DiagramLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-diagram")
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

    fn type_graph(&self) -> Option<&TypeGraphProvider> {
        Some(self)
    }
}

/// The symbols of the project for Add Class: one type per source file.
struct DiagramSymbols;

impl SymbolProvider for DiagramSymbols {
    fn file_symbols(&self, _: &Path, text: &str) -> Vec<Symbol> {
        let name = text
            .split_whitespace()
            .nth(1)
            .unwrap_or("Circle")
            .trim_end_matches(|character: char| !character.is_alphanumeric())
            .to_owned();
        vec![Symbol {
            name,
            kind: SymbolKind::Class,
            container: Some("demo".to_owned()),
            depth: 1,
            range: 0..text.len(),
            name_range: 0..text.len().min(6),
        }]
    }
}

static SYMBOLS: DiagramSymbols = DiagramSymbols;

fn symbols_for(path: &Path) -> Option<&'static dyn SymbolProvider> {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
        .then_some(&SYMBOLS as &'static dyn SymbolProvider)
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
    language: Arc<DiagramLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("diagram-state");
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
    let setup = project_setup(scratch("diagram-cache"));
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
    let search = SearchSetup {
        excludes: Vec::new(),
        symbols_for,
        extra_sources: Vec::new(),
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(&mut workspace, search, setup, agents, window, cx);
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    for _ in 0..300 {
        if check(cx) {
            return;
        }
        cx.run_until_parked();
    }
    panic!("the test waited for {what} in vain");
}

fn diagram(cx: &TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<DiagramView> {
    window
        .read_with(cx, |workspace, _| workspace.active_view())
        .unwrap()
        .and_then(|view| view.downcast::<DiagramView>().ok())
        .expect("the active tab shows a diagram")
}

fn member(name: &str, kind: GraphMemberKind, type_name: &str) -> GraphMember {
    GraphMember {
        name: name.to_owned(),
        type_name: type_name.to_owned(),
        parameters: Vec::new(),
        kind,
        visibility: GraphVisibility::Public,
        static_member: false,
        abstract_member: false,
        place: None,
    }
}

fn node(qualified: &str, members: Vec<GraphMember>, place: Option<SourceSpan>) -> GraphType {
    GraphType {
        name: qualified.rsplit('.').next().unwrap_or(qualified).to_owned(),
        qualified: qualified.to_owned(),
        kind: GraphTypeKind::Class,
        abstract_type: false,
        external: false,
        members,
        place,
    }
}

/// The graph of a class with an ancestor, a subtype and two member sections.
fn class_graph(path: &Path) -> TypeGraph {
    let text = "class Owner {\n  String name;\n  String getName() {}\n}\n";
    let owner = node(
        "demo.Owner",
        vec![
            member("name", GraphMemberKind::Field, "String"),
            member("getName", GraphMemberKind::Method, "String"),
        ],
        Some(SourceSpan {
            uri: uri_of(path),
            span: span_in(text, 6, 11),
        }),
    );
    let person = node("demo.Person", Vec::new(), None);
    let address = node("demo.Address", Vec::new(), None);
    let serializable = GraphType {
        name: "Serializable".to_owned(),
        qualified: "Serializable".to_owned(),
        kind: GraphTypeKind::Interface,
        abstract_type: false,
        external: true,
        members: Vec::new(),
        place: None,
    };
    TypeGraph {
        nodes: vec![owner, person, address, serializable],
        edges: vec![
            GraphEdge {
                from: "demo.Owner".to_owned(),
                to: "demo.Person".to_owned(),
                kind: GraphEdgeKind::Inheritance,
                multiplicity: None,
            },
            GraphEdge {
                from: "demo.Address".to_owned(),
                to: "demo.Owner".to_owned(),
                kind: GraphEdgeKind::Inheritance,
                multiplicity: None,
            },
            GraphEdge {
                from: "demo.Owner".to_owned(),
                to: "Serializable".to_owned(),
                kind: GraphEdgeKind::Implementation,
                multiplicity: None,
            },
        ],
    }
}

fn double_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    for clicks in 1..=2 {
        visual.simulate_event(gpui::MouseDownEvent {
            button: gpui::MouseButton::Left,
            position,
            modifiers: gpui::Modifiers::default(),
            click_count: clicks,
            first_mouse: false,
        });
        visual.simulate_event(gpui::MouseUpEvent {
            button: gpui::MouseButton::Left,
            position,
            modifiers: gpui::Modifiers::default(),
            click_count: clicks,
        });
    }
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn the_diagram_shows_the_ancestors_and_the_subtype_of_a_class(cx: &mut TestAppContext) {
    let root = project("diagram-class", &[("src/Owner.txt", "class Owner {}\n")]);
    let path = root.join("src").join("Owner.txt");
    let language = DiagramLanguage::new();
    *language.graph.lock().unwrap() = class_graph(&path);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the layout of the diagram", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 4 && view.position_of("demo.Owner").is_some()
        })
    });
    let names = view.read_with(cx, |view, _| view.node_names());
    for wanted in ["demo.Owner", "demo.Person", "demo.Address", "Serializable"] {
        assert!(
            names.iter().any(|name| name == wanted),
            "{wanted}: {names:?}"
        );
    }
    draw(cx, window);
    for selector in [
        "diagram-node-demo.Owner",
        "diagram-node-demo.Person",
        "diagram-node-demo.Address",
    ] {
        assert!(
            bounds_of(cx, window, selector).size.width > px(0.),
            "{selector} is on screen"
        );
    }
}

#[ui_kit::gpui::test]
fn the_sections_follow_the_fields_and_methods_buttons(cx: &mut TestAppContext) {
    let root = project("diagram-sections", &[("src/Owner.txt", "class Owner {}\n")]);
    let path = root.join("src").join("Owner.txt");
    let language = DiagramLanguage::new();
    *language.graph.lock().unwrap() = class_graph(&path);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the members of the class", |cx| {
        view.read_with(cx, |view, _| view.member_names("demo.Owner").len() == 2)
    });
    draw(cx, window);
    click(cx, window, "diagram-fields");
    draw(cx, window);
    let members = view.read_with(cx, |view, _| view.member_names("demo.Owner"));
    assert_eq!(members, ["getName"], "Fields takes the field out");
    click(cx, window, "diagram-methods");
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.member_names("demo.Owner"))
            .is_empty(),
        "both sections are off"
    );
    click(cx, window, "diagram-fields");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.member_names("demo.Owner")),
        ["name"],
        "the field comes back alone"
    );
}

#[ui_kit::gpui::test]
fn a_double_click_on_a_node_opens_its_class(cx: &mut TestAppContext) {
    let root = project(
        "diagram-open",
        &[
            ("src/Owner.txt", "class Owner {}\n"),
            ("src/Person.txt", "class Person {}\n"),
        ],
    );
    let path = root.join("src").join("Owner.txt");
    let person = root.join("src").join("Person.txt");
    let language = DiagramLanguage::new();
    let mut graph = class_graph(&path);
    let text = "class Person {}\n";
    graph.nodes[1].place = Some(SourceSpan {
        uri: uri_of(&person),
        span: span_in(text, 6, 12),
    });
    *language.graph.lock().unwrap() = graph;
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the layout of the diagram", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 4 && view.position_of("demo.Owner").is_some()
        })
    });
    double_click(cx, window, "diagram-node-demo.Person");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap(),
        Some(person),
        "the double click opens the class of the node"
    );
}

#[ui_kit::gpui::test]
fn add_class_puts_a_type_on_the_diagram_and_delete_takes_it_out(cx: &mut TestAppContext) {
    let root = project(
        "diagram-add",
        &[
            ("src/Owner.txt", "class Owner {}\n"),
            ("src/Circle.txt", "class Circle {}\n"),
        ],
    );
    let path = root.join("src").join("Owner.txt");
    let language = DiagramLanguage::new();
    *language.graph.lock().unwrap() = class_graph(&path);
    *language.added.lock().unwrap() = TypeGraph {
        nodes: vec![node("demo.Circle", Vec::new(), None)],
        edges: Vec::new(),
    };
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests::wait_for_project_index(cx);
    crate::window_tests::wait_for_file_index(cx);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the layout of the diagram", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 4 && view.position_of("demo.Owner").is_some()
        })
    });
    draw(cx, window);
    click(cx, window, "diagram-add");
    draw(cx, window);
    type_text(cx, window, "Circle");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "diagram-add-popup").size.height > px(0.),
        "Add Class lists the types of the index"
    );
    press(cx, window, "enter");
    draw(cx, window);
    wait_for(cx, "the added class", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 5 && view.position_of("demo.Circle").is_some()
        })
    });
    let names = view.read_with(cx, |view, _| view.node_names());
    assert!(names.iter().any(|name| name == "demo.Circle"), "{names:?}");
    draw(cx, window);
    click(cx, window, "diagram-node-demo.Circle");
    draw(cx, window);
    press(cx, window, "delete");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.node_count()),
        4,
        "Delete takes the selected node out"
    );
}

#[ui_kit::gpui::test]
fn dragging_a_node_moves_it_and_the_position_stays(cx: &mut TestAppContext) {
    let root = project("diagram-drag", &[("src/Owner.txt", "class Owner {}\n")]);
    let path = root.join("src").join("Owner.txt");
    let language = DiagramLanguage::new();
    *language.graph.lock().unwrap() = class_graph(&path);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the layout of the diagram", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 4 && view.position_of("demo.Owner").is_some()
        })
    });
    draw(cx, window);
    let before = view
        .read_with(cx, |view, _| view.position_of("demo.Owner"))
        .expect("the node has a place");
    let bounds = bounds_of(cx, window, "diagram-node-demo.Owner");
    let from = bounds.center();
    let to = point(from.x + px(80.), from.y + px(40.));
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(from, None, gpui::Modifiers::default());
    visual.simulate_mouse_down(from, gpui::MouseButton::Left, gpui::Modifiers::default());
    for step in 1..=4 {
        let fraction = step as f32 / 4.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
    let after = view
        .read_with(cx, |view, _| view.position_of("demo.Owner"))
        .expect("the node keeps its place");
    assert!(
        after.x > before.x && after.y > before.y,
        "the node follows the pointer: {before:?} to {after:?}"
    );
    draw(cx, window);
    let moved = bounds_of(cx, window, "diagram-node-demo.Owner");
    assert!(moved.origin.x > bounds.origin.x);
}

#[ui_kit::gpui::test]
fn the_export_writes_the_svg_of_the_diagram(cx: &mut TestAppContext) {
    let root = project("diagram-export", &[("src/Owner.txt", "class Owner {}\n")]);
    let path = root.join("src").join("Owner.txt");
    let language = DiagramLanguage::new();
    *language.graph.lock().unwrap() = class_graph(&path);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-alt-shift-u");
    draw(cx, window);
    let view = diagram(cx, window);
    wait_for(cx, "the layout of the diagram", |cx| {
        view.read_with(cx, |view, _| {
            view.node_count() == 4 && view.position_of("demo.Owner").is_some()
        })
    });
    let target = root.join("owner.svg");
    view.read_with(cx, |view, _| view.export_to(&target))
        .expect("the export writes the file");
    let svg = fs::read_to_string(&target).unwrap();
    assert!(svg.contains("class=\"node\""), "the nodes are in the file");
    assert!(svg.contains("demo.Owner"), "the names are in the file");
    assert!(
        svg.contains("class=\"edge inheritance\""),
        "the edges are in the file"
    );
}
