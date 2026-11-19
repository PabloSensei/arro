//! F6 on a member without a real window: a stand-in language says what the place is,
//! fills the Move Members form, and answers the move. The test checks what the window
//! shows, what it sends back and how it applies the answer.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionProvider, ActionQuery, Answer, Confidence, LanguageFn, LanguageId,
    LanguageSupport, MoveDestination, MoveDestinationProvider, MoveKind, MoveMember,
    MoveMembersForm, MoveMembersFormProvider, MoveMembersQuery, MoveOutcome, MovePlace,
    MoveProvider, MoveQuery, MoveRequest, Provider, Queries, SourceEdit, SourceId, SyntaxProvider,
    TextPoint, TextSpan, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::{edit_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("move-server");

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
struct MoveLanguage {
    syntax: NoSyntax,
    place: Mutex<MovePlace>,
    form: Mutex<Option<MoveMembersForm>>,
    destinations: Mutex<Vec<MoveDestination>>,
    member_outcome: Mutex<MoveOutcome>,
    file_target: Mutex<Option<PathBuf>>,
    applied: Mutex<Vec<MoveRequest>>,
}

impl MoveLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            place: Mutex::new(MovePlace::File),
            form: Mutex::new(None),
            destinations: Mutex::new(Vec::new()),
            member_outcome: Mutex::new(MoveOutcome::default()),
            file_target: Mutex::new(None),
            applied: Mutex::new(Vec::new()),
        })
    }

    fn set_place(&self, place: MovePlace) {
        *self.place.lock().unwrap() = place;
    }

    fn set_form(&self, form: MoveMembersForm) {
        *self.form.lock().unwrap() = Some(form);
    }

    fn set_destinations(&self, destinations: Vec<MoveDestination>) {
        *self.destinations.lock().unwrap() = destinations;
    }

    fn set_member_outcome(&self, outcome: MoveOutcome) {
        *self.member_outcome.lock().unwrap() = outcome;
    }

    fn set_file_target(&self, target: PathBuf) {
        *self.file_target.lock().unwrap() = Some(target);
    }

    fn applied(&self) -> Vec<MoveRequest> {
        self.applied.lock().unwrap().clone()
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for MoveLanguage {
    fn start(&self, _: ActionQuery, _: base::Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        let _ = reply.send(self.answer(Vec::new()));
    }
}

impl Provider<MoveMembersQuery, Option<MoveMembersForm>> for MoveLanguage {
    fn start(
        &self,
        _: MoveMembersQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Option<MoveMembersForm>>,
    ) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(self.answer(form));
    }
}

impl Provider<MoveQuery, Vec<MoveDestination>> for MoveLanguage {
    fn start(
        &self,
        _: MoveQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<MoveDestination>>,
    ) {
        let destinations = self.destinations.lock().unwrap().clone();
        let _ = reply.send(self.answer(destinations));
    }
}

impl Provider<MoveRequest, MoveOutcome> for MoveLanguage {
    fn start(&self, query: MoveRequest, _: base::Cancel, mut reply: lang_api::Reply<MoveOutcome>) {
        self.applied.lock().unwrap().push(query.clone());
        match query.kind {
            MoveKind::Member => {
                let outcome = self.member_outcome.lock().unwrap().clone();
                let _ = reply.send(self.answer(outcome));
            }
            MoveKind::File => {
                let target = self.file_target.lock().unwrap().clone();
                let outcome = target.map_or_else(MoveOutcome::default, |target| MoveOutcome {
                    change: WorkspaceChange {
                        edits: vec![SourceEdit {
                            uri: query.spot.uri.clone(),
                            edits: vec![edit_in("", 0, 0, "// moved")],
                        }],
                        renames: vec![lang_api::FileRename {
                            from: query.spot.uri.clone(),
                            to: uri_of(&target),
                        }],
                    },
                    error: None,
                });
                let _ = reply.send(self.answer(outcome));
            }
        }
    }
}

impl LanguageSupport for MoveLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-move")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn move_place(&self, _: &str, _: usize) -> MovePlace {
        self.place.lock().unwrap().clone()
    }

    fn move_members_form(&self) -> Option<&MoveMembersFormProvider> {
        Some(self)
    }

    fn move_destinations(&self) -> Option<&MoveDestinationProvider> {
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
    language: Arc<MoveLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("move-state");
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
    let setup = project_setup(scratch("move-cache"));
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

fn span(text: &str, needle: &str) -> TextSpan {
    let start = text.find(needle).unwrap();
    let point = |offset: usize| {
        let before = &text[..offset];
        let line = before.matches('\n').count() as u32;
        let character = before
            .rsplit_once('\n')
            .map_or(before.len(), |(_, last)| last.len()) as u32;
        TextPoint { line, character }
    };
    TextSpan {
        start: point(start),
        end: point(start + needle.len()),
    }
}

fn move_form(text: &str, checked: &str) -> MoveMembersForm {
    let member = |name: &str, detail: &str| MoveMember {
        name: name.to_owned(),
        detail: Some(detail.to_owned()),
        checked: name == checked,
        span: span(text, name),
    };
    MoveMembersForm {
        title: "Move Members".to_owned(),
        members_label: "Members".to_owned(),
        members: vec![
            member("save", "void save()"),
            member("load", "String load()"),
        ],
        target_label: "To class".to_owned(),
        targets: vec![
            MoveDestination {
                name: "Storage".to_owned(),
                detail: Some("probe".to_owned()),
                folder: None,
                handle: "probe.Storage".to_owned(),
            },
            MoveDestination {
                name: "Warehouse".to_owned(),
                detail: Some("probe".to_owned()),
                folder: None,
                handle: "probe.Warehouse".to_owned(),
            },
        ],
    }
}

const STORAGE: &str = "void save() {}\nString load() { return \"\"; }\n";
const OTHER: &str = "other\n";

#[ui_kit::gpui::test]
fn f6_on_a_member_opens_the_form_with_the_member_ticked(cx: &mut TestAppContext) {
    let root = project("move-members-open", &[("src/Storage.txt", STORAGE)]);
    let storage = root.join("src").join("Storage.txt");
    let language = MoveLanguage::new();
    language.set_place(MovePlace::Member);
    // The second member is the one the form marks, so the test tells it from the first.
    language.set_form(move_form(STORAGE, "load"));
    language.set_member_outcome(MoveOutcome {
        change: WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri_of(&storage),
                edits: vec![edit_in(STORAGE, 0, 0, "// save moved\n")],
            }],
            renames: Vec::new(),
        },
        error: None,
    });

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &storage);
    let view = editor_of(cx, window, &storage);

    press(cx, window, "f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-members").size.height > px(0.),
        "F6 on a member opens the form"
    );
    assert!(
        bounds_of(cx, window, "move-members-member-0").size.height > px(0.),
        "the form lists the members"
    );
    assert!(
        bounds_of(cx, window, "move-members-field").size.height > px(0.),
        "the form has the target field"
    );
    assert!(
        bounds_of(cx, window, "move-members-target-0").size.height > px(0.),
        "the field hints at the types of the project"
    );
    assert!(
        bounds_of(cx, window, "move-members-cancel").size.height > px(0.),
        "the form has Cancel"
    );
    assert!(
        bounds_of(cx, window, "move-members-refactor").size.height > px(0.),
        "the form has Refactor"
    );

    // The ticked member is the one the form sends, and typing reaches the field.
    type_text(cx, window, "Warehouse");
    press(cx, window, "enter");
    draw(cx, window);
    let applied = language.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].kind, MoveKind::Member);
    assert_eq!(applied[0].destination.handle, "probe.Warehouse");
    assert_eq!(
        applied[0].spot.point,
        span(STORAGE, "load").start,
        "the ticked member is the one that moves"
    );
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "// save moved\nvoid save() {}\nString load() { return \"\"; }\n"
    );
}

#[ui_kit::gpui::test]
fn refactor_applies_the_change_of_two_files_in_one_undo_step(cx: &mut TestAppContext) {
    let root = project(
        "move-members-apply",
        &[("src/Storage.txt", STORAGE), ("src/Other.txt", OTHER)],
    );
    let storage = root.join("src").join("Storage.txt");
    let other = root.join("src").join("Other.txt");
    let language = MoveLanguage::new();
    language.set_place(MovePlace::Member);
    language.set_form(move_form(STORAGE, "save"));
    language.set_member_outcome(MoveOutcome {
        change: WorkspaceChange {
            edits: vec![
                SourceEdit {
                    uri: uri_of(&storage),
                    edits: vec![edit_in(STORAGE, 0, 0, "// gone\n")],
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
    open_path(cx, window, &storage);
    let view = editor_of(cx, window, &storage);

    press(cx, window, "f6");
    draw(cx, window);
    type_text(cx, window, "Warehouse");
    draw(cx, window);
    // The typed name matches a type of the project, whose value the move carries.
    press(cx, window, "enter");
    draw(cx, window);

    let applied = language.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].kind, MoveKind::Member);
    assert_eq!(applied[0].destination.handle, "probe.Warehouse");
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        "// gone\nvoid save() {}\nString load() { return \"\"; }\n"
    );
    assert_eq!(fs::read_to_string(&other).unwrap(), "// arrived\nother\n");

    // The whole change goes back with one Ctrl+Z, the closed file too.
    press(cx, window, "ctrl-z");
    assert_eq!(
        crate::window_tests::buffer_of(cx, &view),
        STORAGE,
        "Ctrl+Z takes the open file back"
    );
    assert_eq!(
        fs::read_to_string(&other).unwrap(),
        OTHER,
        "Ctrl+Z takes the closed file back in the same step"
    );
}

#[ui_kit::gpui::test]
fn the_error_of_the_server_stays_in_the_form_and_changes_no_file(cx: &mut TestAppContext) {
    let root = project(
        "move-members-error",
        &[("src/Storage.txt", STORAGE), ("src/Other.txt", OTHER)],
    );
    let storage = root.join("src").join("Storage.txt");
    let other = root.join("src").join("Other.txt");
    let language = MoveLanguage::new();
    language.set_place(MovePlace::Member);
    language.set_form(move_form(STORAGE, "save"));
    language.set_member_outcome(MoveOutcome {
        change: WorkspaceChange::default(),
        error: Some("save uses a private field of Storage".to_owned()),
    });

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &storage);
    let view = editor_of(cx, window, &storage);

    press(cx, window, "f6");
    draw(cx, window);
    type_text(cx, window, "Warehouse");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-members").size.height > px(0.),
        "the form stays open"
    );
    assert!(
        bounds_of(cx, window, "move-members-error").size.height > px(0.),
        "the form shows the text of the server"
    );
    assert_eq!(crate::window_tests::buffer_of(cx, &view), STORAGE);
    assert_eq!(fs::read_to_string(&other).unwrap(), OTHER);
}

#[ui_kit::gpui::test]
fn f6_on_a_file_still_offers_the_packages_of_the_server(cx: &mut TestAppContext) {
    let root = project("move-members-file", &[("src/Main.txt", "class Main\n")]);
    let from = root.join("src").join("Main.txt");
    let to = root.join("src").join("newpkg").join("Main.txt");
    fs::create_dir_all(root.join("src").join("newpkg")).unwrap();
    let language = MoveLanguage::new();
    language.set_destinations(vec![MoveDestination {
        name: "newpkg".to_owned(),
        detail: Some("probe".to_owned()),
        folder: Some(to.parent().unwrap().to_path_buf()),
        handle: "pkg".to_owned(),
    }]);
    language.set_file_target(to.clone());

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &from);

    press(cx, window, "f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-popup").size.height > px(0.),
        "F6 on a file lists the packages of the server"
    );
    type_text(cx, window, "new");
    press(cx, window, "enter");
    cx.run_until_parked();
    draw(cx, window);

    let applied = language.applied();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].kind, MoveKind::File);
    assert!(!from.exists(), "the file left the old package");
    assert!(to.is_file());
}

#[ui_kit::gpui::test]
fn refactor_this_opens_the_same_form_for_a_member(cx: &mut TestAppContext) {
    let root = project("move-members-this", &[("src/Storage.txt", STORAGE)]);
    let storage = root.join("src").join("Storage.txt");
    let language = MoveLanguage::new();
    language.set_place(MovePlace::Member);
    language.set_form(move_form(STORAGE, "save"));

    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &storage);

    // Rename, Change Signature and then Move are the rows of the editor.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Refactor This lists the refactorings"
    );
    press(cx, window, "down");
    press(cx, window, "down");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "move-members").size.height > px(0.),
        "the Move row opens the form"
    );
    assert!(
        bounds_of(cx, window, "move-members-member-0").size.height > px(0.),
        "the form lists the members"
    );
}
