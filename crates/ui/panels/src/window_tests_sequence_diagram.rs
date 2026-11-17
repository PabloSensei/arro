//! The Sequence Diagram tab: the participants and the messages of a method, the frame
//! of a fragment, the double click on a message, hiding a participant, a rebuild with
//! other settings and the copy of the PlantUML text.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentEdit, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    SemanticServer, SequenceDiagram, SequenceFragment, SequenceFragmentKind, SequenceMessage,
    SequenceMessageKind, SequenceParticipant, SequenceParticipantKind, SequenceQuery,
    SequenceSection, SequenceSettings, SourceId, SourceSpan, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::sequence_diagram::SequenceDiagramView;
use crate::window_tests::{bounds_of, click, draw, open_path, project_setup, scratch};
use crate::window_tests_lang::{span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-sequence");

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

/// A language that answers the diagram from the model the test sets.
struct SequenceLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    base: Mutex<SequenceDiagram>,
    deep: Mutex<SequenceDiagram>,
    queries: Mutex<Vec<SequenceSettings>>,
}

impl SequenceLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            base: Mutex::new(SequenceDiagram::default()),
            deep: Mutex::new(SequenceDiagram::default()),
            queries: Mutex::new(Vec::new()),
        })
    }
}

impl SemanticServer for SequenceLanguage {
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

impl Provider<SequenceQuery, SequenceDiagram> for SequenceLanguage {
    fn start(
        &self,
        query: SequenceQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<SequenceDiagram>,
    ) {
        self.queries.lock().unwrap().push(query.settings);
        let diagram = if query.settings.depth <= 2 {
            self.base.lock().unwrap().clone()
        } else {
            self.deep.lock().unwrap().clone()
        };
        reply
            .send(Answer::new(diagram, Confidence::Certain, SOURCE))
            .ok();
    }
}

impl LanguageSupport for SequenceLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-sequence")
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

    fn sequence_diagram(&self) -> Option<&lang_api::SequenceDiagramProvider> {
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
    language: Arc<SequenceLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("sequence-state");
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
    let setup = project_setup(scratch("sequence-cache"));
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

fn sequence(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> gpui::Entity<SequenceDiagramView> {
    window
        .read_with(cx, |workspace, _| workspace.active_view())
        .unwrap()
        .and_then(|view| view.downcast::<SequenceDiagramView>().ok())
        .expect("the active tab shows a sequence diagram")
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
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

fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    let mut visual = gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Right,
        position,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    visual.simulate_event(gpui::MouseUpEvent {
        button: gpui::MouseButton::Right,
        position,
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
    });
    cx.run_until_parked();
    draw(cx, window);
}

fn participant(name: &str, kind: SequenceParticipantKind) -> SequenceParticipant {
    SequenceParticipant {
        name: name.to_owned(),
        qualified: format!("demo.{name}"),
        kind,
        place: None,
    }
}

fn message(
    from: usize,
    to: usize,
    kind: SequenceMessageKind,
    label: &str,
    place: Option<SourceSpan>,
) -> SequenceMessage {
    SequenceMessage {
        from,
        to,
        kind,
        label: label.to_owned(),
        returns: (kind == SequenceMessageKind::Return).then(|| label.to_owned()),
        place,
        fragments: Vec::new(),
        depth: 1,
    }
}

/// The diagram the fake language answers with.
fn diagrams(call_site: &Path) -> (SequenceDiagram, SequenceDiagram) {
    let text = "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n";
    let call = text.find("repo.findById").unwrap();
    let place = Some(SourceSpan {
        uri: uri_of(call_site),
        span: span_in(text, call, call + "repo.findById(1)".len()),
    });
    let mut call_message = message(
        0,
        1,
        SequenceMessageKind::Call,
        "findById(id)",
        place.clone(),
    );
    call_message.returns = Some("Owner".to_owned());
    let return_message = message(1, 0, SequenceMessageKind::Return, "Owner", None);
    let log = message(0, 2, SequenceMessageKind::Call, "log(x)", place.clone());
    let mut self_call = message(
        0,
        0,
        SequenceMessageKind::SelfCall,
        "helper()",
        place.clone(),
    );
    self_call.fragments = vec![0];
    let alt = SequenceFragment {
        kind: SequenceFragmentKind::Alt,
        sections: vec![
            SequenceSection {
                condition: "found".to_owned(),
                messages: 3..4,
            },
            SequenceSection {
                condition: "else".to_owned(),
                messages: 4..5,
            },
        ],
        depth: 0,
    };
    let mut second = message(0, 1, SequenceMessageKind::Call, "save(name)", place.clone());
    second.fragments = vec![0];
    let base = SequenceDiagram {
        participants: vec![
            participant("Controller", SequenceParticipantKind::Class),
            participant("Repository", SequenceParticipantKind::Interface),
            participant("Logger", SequenceParticipantKind::External),
        ],
        messages: vec![call_message, return_message, log, self_call, second],
        fragments: vec![alt],
        title: "Controller.run()".to_owned(),
    };
    let mut deep = base.clone();
    let loop_call = message(0, 1, SequenceMessageKind::Call, "save(name)", place.clone());
    deep.messages.push(loop_call);
    deep.title = "Controller.run(deep)".to_owned();
    (base, deep)
}

fn open_tab(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
) -> (
    PathBuf,
    WindowHandle<Workspace>,
    Arc<SequenceLanguage>,
    PathBuf,
) {
    let root = project(name, files);
    let call_site = root.join("src").join("CallSite.txt");
    let editor = root.join("src").join("Controller.txt");
    let language = SequenceLanguage::new();
    let (base, deep) = diagrams(&call_site);
    *language.base.lock().unwrap() = base;
    *language.deep.lock().unwrap() = deep;
    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &editor);
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("window.show_sequence_diagram", window, cx)
        })
        .unwrap();
    assert!(ran, "Generate Sequence Diagram is a registered command");
    cx.run_until_parked();
    draw(cx, window);
    (root, window, language, call_site)
}

#[ui_kit::gpui::test]
fn the_command_opens_a_tab_with_the_participants_and_the_messages(cx: &mut TestAppContext) {
    let (_, window, _, _) = open_tab(
        cx,
        "sequence-open",
        &[
            (
                "src/Controller.txt",
                "class Controller {\n  void run() {}\n}\n",
            ),
            (
                "src/CallSite.txt",
                "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n",
            ),
        ],
    );
    let view = sequence(cx, window);
    wait_for(cx, "the diagram of the method", |cx| {
        view.read_with(cx, |view, _| {
            !view.participant_names().is_empty() && view.message_count() == 6
        })
    });
    draw(cx, window);
    let names = view.read_with(cx, |view, _| view.participant_names());
    assert_eq!(names, ["Controller", "Repository", "Logger"]);
    assert!(
        bounds_of(cx, window, "sequence-participant-Controller")
            .size
            .width
            > px(0.),
        "the participant is on screen"
    );
    for selector in [
        "sequence-message-0",
        "sequence-message-1",
        "sequence-message-3",
        "sequence-fragment-0",
    ] {
        assert!(
            has_selector(cx, window, selector),
            "{selector} is on screen"
        );
    }
}

#[ui_kit::gpui::test]
fn a_double_click_on_a_message_opens_the_line_of_the_call(cx: &mut TestAppContext) {
    let (_, window, _, call_site) = open_tab(
        cx,
        "sequence-open-call",
        &[
            (
                "src/Controller.txt",
                "class Controller {\n  void run() {}\n}\n",
            ),
            (
                "src/CallSite.txt",
                "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n",
            ),
        ],
    );
    let view = sequence(cx, window);
    wait_for(cx, "the diagram of the method", |cx| {
        view.read_with(cx, |view, _| view.message_count() == 6)
    });
    draw(cx, window);
    double_click(cx, window, "sequence-message-0");
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap(),
        Some(call_site),
        "the message opens its call site"
    );
    let editor = window
        .read_with(cx, |workspace, _| workspace.active_view())
        .unwrap()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
        .expect("the call site is an editor");
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret_line()),
        Some(2),
        "the caret is on the line of the call"
    );
}

#[ui_kit::gpui::test]
fn a_right_click_hides_a_participant_and_show_hidden_brings_it_back(cx: &mut TestAppContext) {
    let (_, window, _, _) = open_tab(
        cx,
        "sequence-hide",
        &[
            (
                "src/Controller.txt",
                "class Controller {\n  void run() {}\n}\n",
            ),
            (
                "src/CallSite.txt",
                "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n",
            ),
        ],
    );
    let view = sequence(cx, window);
    wait_for(cx, "the diagram of the method", |cx| {
        view.read_with(cx, |view, _| view.message_count() == 6)
    });
    draw(cx, window);
    right_click(cx, window, "sequence-participant-Repository");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.hidden_names()),
        ["Repository"],
        "the participant is hidden"
    );
    assert!(
        !has_selector(cx, window, "sequence-participant-Repository"),
        "the header is gone"
    );
    assert!(
        !has_selector(cx, window, "sequence-message-0"),
        "the message to the hidden participant is gone"
    );
    click(cx, window, "sequence-show-hidden");
    draw(cx, window);
    assert!(view.read_with(cx, |view, _| view.hidden_names().is_empty()));
    assert!(has_selector(cx, window, "sequence-participant-Repository"));
}

#[ui_kit::gpui::test]
fn a_rebuild_asks_for_the_new_depth(cx: &mut TestAppContext) {
    let (_, window, language, _) = open_tab(
        cx,
        "sequence-rebuild",
        &[
            (
                "src/Controller.txt",
                "class Controller {\n  void run() {}\n}\n",
            ),
            (
                "src/CallSite.txt",
                "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n",
            ),
        ],
    );
    let view = sequence(cx, window);
    wait_for(cx, "the deep diagram", |cx| {
        view.read_with(cx, |view, _| view.message_count() == 6)
    });
    draw(cx, window);
    click(cx, window, "sequence-depth-down");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.settings().depth),
        2,
        "the toolbar changes the setting"
    );
    click(cx, window, "sequence-rebuild");
    wait_for(cx, "the shallow diagram", |cx| {
        view.read_with(cx, |view, _| view.message_count() == 5)
    });
    let last = language
        .queries
        .lock()
        .unwrap()
        .last()
        .copied()
        .expect("the provider was asked");
    assert_eq!(last.depth, 2);
    assert_eq!(
        view.read_with(cx, |view, _| view.settings().depth),
        2,
        "the tab keeps the setting of the rebuild"
    );
}

#[ui_kit::gpui::test]
fn copy_plantuml_puts_the_document_on_the_clipboard(cx: &mut TestAppContext) {
    let (_, window, _, _) = open_tab(
        cx,
        "sequence-copy",
        &[
            (
                "src/Controller.txt",
                "class Controller {\n  void run() {}\n}\n",
            ),
            (
                "src/CallSite.txt",
                "class Controller {\n  void run() {\n    repo.findById(1);\n  }\n}\n",
            ),
        ],
    );
    let view = sequence(cx, window);
    wait_for(cx, "the diagram of the method", |cx| {
        view.read_with(cx, |view, _| view.message_count() == 6)
    });
    draw(cx, window);
    click(cx, window, "sequence-export");
    draw(cx, window);
    click(cx, window, "sequence-copy-plantuml");
    draw(cx, window);
    let text = cx
        .read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
        .expect("Copy PlantUML puts the document on the clipboard");
    assert!(text.starts_with("@startuml\n"), "{text}");
    assert!(
        text.contains("Controller -> Repository : findById(id)"),
        "{text}"
    );
    assert!(text.contains("alt found\n"), "{text}");
    assert!(text.trim_end().ends_with("@enduml"), "{text}");
}

fn find_source(root: &Path, stem: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_owned()];
    while let Some(folder) = stack.pop() {
        let entries = fs::read_dir(folder).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.file_stem().is_some_and(|name| name == stem) {
                return Some(path);
            }
        }
    }
    None
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn petclinic_sequence_of_process_find_form_reaches_the_repository(cx: &mut TestAppContext) {
    use std::time::Duration;

    use crate::window_tests_real::{
        build_window, close, project as real_project, wait_for as wait_real, wait_ready,
    };

    let (root, language) = real_project("real-sequence");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    let path =
        find_source(&root, "OwnerController").expect("the owner controller is in the corpus");
    open_path(cx, window, &path);
    let text = fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .position(|line| line.contains("String processFindForm"))
        .expect("the controller declares processFindForm");
    let editor = crate::window_tests::editor_of(cx, window, &path);
    editor.update(cx, |editor, cx| editor.go_to_line(line, cx));
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("window.show_sequence_diagram", window, cx)
        })
        .unwrap();
    assert!(ran, "Generate Sequence Diagram is a registered command");
    cx.run_until_parked();
    draw(cx, window);
    let view = sequence(cx, window);
    wait_real(
        cx,
        "the sequence of processFindForm",
        Duration::from_secs(120),
        |cx| {
            view.read_with(cx, |view, _| {
                view.message_labels()
                    .iter()
                    .any(|label| label.starts_with("findByLastNameStartingWith("))
            })
        },
    );
    let names = view.read_with(cx, |view, _| view.participant_names());
    assert!(
        names.iter().any(|name| name == "OwnerRepository"),
        "{names:?}"
    );
    close(cx, window, language, true);
}
