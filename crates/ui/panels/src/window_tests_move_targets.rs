//! F6 on an instance method without a real window: a stand-in language says what the
//! place is, fills the form with the places around it, and answers the move. The test
//! checks what the window shows, what it sends back and how it applies the answer.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, LanguageFn, LanguageId, LanguageSupport, MoveKind, MoveOutcome, MovePlace,
    MoveProvider, MoveRequest, MoveTargetsForm, MoveTargetsFormProvider, MoveTargetsQuery,
    Provider, Queries, SourceEdit, SourceId, SyntaxProvider, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{bounds_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{edit_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("move-targets-server");

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

/// The language of the test: the place, the form and the outcome are set by the test.
struct TargetsLanguage {
    syntax: NoSyntax,
    place: Mutex<MovePlace>,
    form: Mutex<Option<MoveTargetsForm>>,
    outcome: Mutex<MoveOutcome>,
    applied: Mutex<Vec<MoveRequest>>,
}

impl TargetsLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            place: Mutex::new(MovePlace::File),
            form: Mutex::new(None),
            outcome: Mutex::new(MoveOutcome::default()),
            applied: Mutex::new(Vec::new()),
        })
    }

    fn set_place(&self, place: MovePlace) {
        *self.place.lock().unwrap() = place;
    }

    fn set_form(&self, form: Option<MoveTargetsForm>) {
        *self.form.lock().unwrap() = form;
    }

    fn set_outcome(&self, outcome: MoveOutcome) {
        *self.outcome.lock().unwrap() = outcome;
    }

    fn applied(&self) -> Vec<MoveRequest> {
        self.applied.lock().unwrap().clone()
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<MoveTargetsQuery, Option<MoveTargetsForm>> for TargetsLanguage {
    fn start(
        &self,
        _: MoveTargetsQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Option<MoveTargetsForm>>,
    ) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(self.answer(form));
    }
}

impl Provider<MoveRequest, MoveOutcome> for TargetsLanguage {
    fn start(&self, query: MoveRequest, _: base::Cancel, mut reply: lang_api::Reply<MoveOutcome>) {
        self.applied.lock().unwrap().push(query);
        let outcome = self.outcome.lock().unwrap().clone();
        let _ = reply.send(self.answer(outcome));
    }
}

impl LanguageSupport for TargetsLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-move-targets")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn move_place(&self, _: &str, _: usize) -> MovePlace {
        self.place.lock().unwrap().clone()
    }

    fn move_targets_form(&self) -> Option<&MoveTargetsFormProvider> {
        Some(self)
    }

    fn move_file(&self) -> Option<&MoveProvider> {
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
    language: Arc<TargetsLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("move-targets-state");
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
    let setup = project_setup(scratch("move-targets-cache"));
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

fn targets_form(title: &str) -> MoveTargetsForm {
    MoveTargetsForm {
        title: title.to_owned(),
        targets: vec![
            lang_api::MoveDestination {
                name: "receiver".to_owned(),
                detail: Some("InstanceTarget".to_owned()),
                folder: None,
                handle: r#"{"name":"receiver","type":"InstanceTarget"}"#.to_owned(),
            },
            lang_api::MoveDestination {
                name: "prefix".to_owned(),
                detail: Some("String".to_owned()),
                folder: None,
                handle: r#"{"name":"prefix","type":"String"}"#.to_owned(),
            },
        ],
    }
}

const MOVER: &str = "String describe(InstanceTarget target) {}\n";
const OTHER: &str = "other\n";

#[ui_kit::gpui::test]
fn f6_on_an_instance_method_opens_the_form_with_the_targets(cx: &mut TestAppContext) {
    let root = project("move-targets-open", &[("src/Mover.txt", MOVER)]);
    let mover = root.join("src").join("Mover.txt");
    let language = TargetsLanguage::new();
    language.set_place(MovePlace::InstanceMethod);
    language.set_form(Some(targets_form("Move Instance Method: describe")));
    language.set_outcome(MoveOutcome {
        change: WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri_of(&mover),
                edits: vec![edit_in(MOVER, 0, 0, "// moved\n")],
            }],
            renames: Vec::new(),
        },
        error: None,
    });

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &mover);
    let view = editor_of(cx, window, &mover);

    press(cx, window, "f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-targets").size.height > px(0.),
        "F6 on an instance method opens the form"
    );
    assert!(
        bounds_of(cx, window, "move-targets-title").size.height > px(0.),
        "the form has a title"
    );
    assert!(
        bounds_of(cx, window, "move-targets-target-0").size.height > px(0.),
        "the form lists the first place"
    );
    assert!(
        bounds_of(cx, window, "move-targets-target-1").size.height > px(0.),
        "the form lists every place"
    );
    assert!(
        bounds_of(cx, window, "move-targets-cancel").size.height > px(0.),
        "the form has Cancel"
    );
    assert!(
        bounds_of(cx, window, "move-targets-refactor").size.height > px(0.),
        "the form has Refactor"
    );

    // The first place is chosen, and Enter moves the method there.
    press(cx, window, "enter");
    draw(cx, window);
    let applied = language.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].kind, MoveKind::InstanceMethod);
    assert_eq!(
        applied[0].destination.name, "receiver",
        "the first place is the one that moves"
    );
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "// moved\nString describe(InstanceTarget target) {}\n"
    );
}

#[ui_kit::gpui::test]
fn refactor_applies_the_change_of_two_files_in_one_undo_step(cx: &mut TestAppContext) {
    let root = project(
        "move-targets-apply",
        &[("src/Mover.txt", MOVER), ("src/Other.txt", OTHER)],
    );
    let mover = root.join("src").join("Mover.txt");
    let other = root.join("src").join("Other.txt");
    let language = TargetsLanguage::new();
    language.set_place(MovePlace::InstanceMethod);
    language.set_form(Some(targets_form("Move Instance Method: describe")));
    language.set_outcome(MoveOutcome {
        change: WorkspaceChange {
            edits: vec![
                SourceEdit {
                    uri: uri_of(&mover),
                    edits: vec![edit_in(MOVER, 0, 0, "// gone\n")],
                },
                SourceEdit {
                    uri: uri_of(&other),
                    edits: vec![edit_in(OTHER, 0, 0, "// arrived\n")],
                },
            ],
            renames: Vec::new(),
        },
        error: None,
    });

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &mover);
    let view = editor_of(cx, window, &mover);

    press(cx, window, "f6");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);

    let applied = language.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].kind, MoveKind::InstanceMethod);
    assert_eq!(
        applied[0].destination.handle,
        r#"{"name":"receiver","type":"InstanceTarget"}"#
    );
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "// gone\nString describe(InstanceTarget target) {}\n"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "// arrived\nother\n");

    // The whole change goes back with one Ctrl+Z, the closed file too.
    press(cx, window, "ctrl-z");
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        MOVER,
        "Ctrl+Z takes the open file back"
    );
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        OTHER,
        "Ctrl+Z takes the closed file back in the same step"
    );
}

#[ui_kit::gpui::test]
fn a_method_without_places_shows_a_note_at_the_caret(cx: &mut TestAppContext) {
    let root = project("move-targets-empty", &[("src/Mover.txt", MOVER)]);
    let mover = root.join("src").join("Mover.txt");
    let language = TargetsLanguage::new();
    language.set_place(MovePlace::InstanceMethod);
    language.set_form(None);

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &mover);

    press(cx, window, "f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "lsp-note").size.height > px(0.),
        "the window says at the caret that no place takes the method"
    );
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("move-targets").is_none(),
        "the form goes away when the language names no place"
    );
}

#[ui_kit::gpui::test]
fn the_error_of_the_server_stays_in_the_form_and_changes_no_file(cx: &mut TestAppContext) {
    let root = project(
        "move-targets-error",
        &[("src/Mover.txt", MOVER), ("src/Other.txt", OTHER)],
    );
    let mover = root.join("src").join("Mover.txt");
    let other = root.join("src").join("Other.txt");
    let language = TargetsLanguage::new();
    language.set_place(MovePlace::InstanceMethod);
    language.set_form(Some(targets_form("Move Instance Method: describe")));
    language.set_outcome(MoveOutcome {
        change: WorkspaceChange::default(),
        error: Some("The method uses a private member of Mover".to_owned()),
    });

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &mover);
    let view = editor_of(cx, window, &mover);

    press(cx, window, "f6");
    draw(cx, window);
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-targets").size.height > px(0.),
        "the form stays open"
    );
    assert!(
        bounds_of(cx, window, "move-targets-error").size.height > px(0.),
        "the form shows the text of the server"
    );
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "String describe(InstanceTarget target) {}\n"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), OTHER);
}
