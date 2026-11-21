//! Ctrl+Alt+B and Ctrl+G in the window without a server. A declaration is no
//! implementation: Ctrl+Alt+B on a name whose only place is its declaration says so and
//! stays where it is. The Go to field opens with the caret's place selected, so a number
//! typed at once replaces it. The real server check is not part of a normal run:
//!
//! `cargo test -p panels window_tests_goto -- --ignored --test-threads=1`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use lang_api::{
    Answer, Confidence, LanguageId, LanguageSupport, NavigationKind, NavigationProvider,
    NavigationQuery, Provider, SemanticServer, SourceId, SourceSpan, SyntaxProvider, TextPoint,
    TextSpan,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    active_path, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::{FakeLanguage, project, uri_of};
use crate::window_tests_real::{build_window, project as real_project, wait_for, wait_ready};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-goto");

/// The fake language with a separate answer for implementations. The declaration answers
/// come from the fake language, as the other tests set them.
struct GotoLanguage {
    fake: Arc<FakeLanguage>,
    /// The places the implementation question answers, as the test set them.
    implementations: Mutex<Vec<SourceSpan>>,
}

impl GotoLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            fake: FakeLanguage::new(),
            implementations: Mutex::new(Vec::new()),
        })
    }

    fn set_implementations(&self, places: Vec<SourceSpan>) {
        *self.implementations.lock().unwrap() = places;
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for GotoLanguage {
    fn start(
        &self,
        query: NavigationQuery,
        cancel: base::Cancel,
        mut reply: lang_api::Reply<Vec<SourceSpan>>,
    ) {
        if query.kind != NavigationKind::Implementation {
            Provider::<NavigationQuery, Vec<SourceSpan>>::start(
                self.fake.as_ref(),
                query,
                cancel,
                reply,
            );
            return;
        }
        let spans = self.implementations.lock().unwrap().clone();
        let _ = reply.send(Answer::new(spans, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for GotoLanguage {
    fn id(&self) -> LanguageId {
        self.fake.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.fake.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.fake.syntax()
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        self.fake.server()
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }
}

/// The opener of the window of the other fake languages, for this one.
fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<GotoLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("goto-state");
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
    let setup = project_setup(scratch("goto-cache"));
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

/// A place of an answer: the start of a file, the same for the start and the end.
fn place(path: &Path, line: u32, character: u32) -> SourceSpan {
    SourceSpan {
        uri: uri_of(path),
        span: TextSpan {
            start: TextPoint { line, character },
            end: TextPoint { line, character },
        },
    }
}

fn caret(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> (usize, usize) {
    view.read_with(cx, |view, _| view.caret())
}

fn note_of(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<String> {
    view.read_with(cx, |view, _| view.note_text())
}

#[ui_kit::gpui::test]
fn ctrl_alt_b_on_a_name_whose_only_place_is_its_declaration_says_so(cx: &mut TestAppContext) {
    let root = project(
        "goto-none",
        &[("src/Main.txt", "call\n"), ("src/Other.txt", "place\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = GotoLanguage::new();
    language.fake.set_definition(&other, 0, 2);
    language.set_implementations(vec![place(&other, 0, 2)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.go_to(Spot::Offset(1), cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-b");
    draw(cx, window);
    assert_eq!(
        active_path(cx, window),
        Some(main),
        "the declaration is no implementation"
    );
    assert_eq!(
        note_of(cx, &view).as_deref(),
        Some("No implementations found")
    );
    assert_eq!(caret(cx, &view), (0, 1), "the caret stays on the name");
    assert_eq!(buffer_of(cx, &view), "call\n");
}

#[ui_kit::gpui::test]
fn ctrl_alt_b_on_the_declaration_itself_says_so_when_no_declaration_is_answered(
    cx: &mut TestAppContext,
) {
    let root = project("goto-own-name", &[("src/Other.txt", "place\n")]);
    let other = root.join("src").join("Other.txt");
    let language = GotoLanguage::new();
    // The declaration question answers nothing here, so only the place of the caret can
    // tell that the implementation answer names the declaration.
    language.set_implementations(vec![place(&other, 0, 2)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &other);
    let view = editor_of(cx, window, &other);
    view.update(cx, |view, cx| view.go_to(Spot::Offset(2), cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(other));
    assert_eq!(
        note_of(cx, &view).as_deref(),
        Some("No implementations found")
    );
    assert_eq!(caret(cx, &view), (0, 2), "the caret stays on the name");
}

#[ui_kit::gpui::test]
fn ctrl_alt_b_skips_the_declaration_and_goes_to_the_implementation(cx: &mut TestAppContext) {
    let root = project(
        "goto-implementation",
        &[
            ("src/Main.txt", "call\n"),
            ("src/Other.txt", "place\n"),
            ("src/Impl.txt", "work\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let implementation = root.join("src").join("Impl.txt");
    let language = GotoLanguage::new();
    language.fake.set_definition(&other, 0, 2);
    language.set_implementations(vec![place(&other, 0, 2), place(&implementation, 0, 2)]);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.go_to(Spot::Offset(1), cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-b");
    draw(cx, window);
    assert_eq!(active_path(cx, window), Some(implementation.clone()));
    let target = editor_of(cx, window, &implementation);
    assert_eq!(caret(cx, &target), (0, 2));
}

#[ui_kit::gpui::test]
fn ctrl_g_opens_with_the_caret_place_selected_and_a_typed_number_replaces_it(
    cx: &mut TestAppContext,
) {
    // Forty characters a line, so column 30 exists on line 141.
    let text: String = (1..=150)
        .map(|line| format!("{:<40}\n", format!("line {line}")))
        .collect();
    let root = project("goto-field", &[("src/Long.txt", text.as_str())]);
    let path = root.join("src").join("Long.txt");
    let window = open_window(cx, root.clone(), GotoLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: 140,
                column: 29,
            },
            cx,
        )
    });
    cx.run_until_parked();

    press(cx, window, "ctrl-g");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.goto_field()),
        Some(("141:30".to_owned(), 0..6)),
        "the field shows the caret as the status bar does, and all of it is selected"
    );

    press(cx, window, "enter");
    assert_eq!(
        caret(cx, &view),
        (140, 29),
        "Enter without an edit keeps the caret"
    );

    press(cx, window, "ctrl-g");
    type_text(cx, window, "20");
    assert_eq!(
        view.read_with(cx, |view, _| view.goto_field()),
        Some(("20".to_owned(), 2..2)),
        "the typed number replaces the selection"
    );
    press(cx, window, "enter");
    assert_eq!(caret(cx, &view), (19, 0), "Enter goes to line 20");
}

/// The server of PetClinic answers Ctrl+Alt+B on `findById` with the places the server
/// gives. The declaration in `OwnerRepository` is never opened: the answer is either
/// the note, or the list of places, or implementations the server found.
#[ignore = "needs IDE_CORPUS and a filled package cache"]
#[ui_kit::gpui::test]
fn a_real_server_does_not_open_the_declaration_of_find_by_id(cx: &mut TestAppContext) {
    let (root, language) = real_project("goto-real");
    let window = build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("goto-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let package = "org.springframework.samples.petclinic.owner";
    let controller = window_language::main_class(&root, package, "OwnerController");
    let repository = window_language::main_class(&root, package, "OwnerRepository");
    open_path(cx, window, &controller);
    let view = editor_of(cx, window, &controller);
    let text = buffer_of(cx, &view);
    let call = text
        .find("this.owners.findById(ownerId)")
        .expect("the controller calls findById");
    view.update(cx, |view, cx| {
        view.go_to(Spot::Offset(call + "this.owners.".len() + 1), cx)
    });
    cx.run_until_parked();

    press(cx, window, "ctrl-alt-b");
    wait_for(
        cx,
        "the answer to Ctrl+Alt+B on findById",
        Duration::from_secs(120),
        |cx| {
            note_of(cx, &view).is_some()
                || view
                    .read_with(cx, |view, _| view.navigation_targets())
                    .is_some()
                || active_path(cx, window) != Some(controller.clone())
        },
    );
    assert_ne!(active_path(cx, window), Some(repository));
}
