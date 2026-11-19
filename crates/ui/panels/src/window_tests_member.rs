//! Pull Members Up and the other member refactorings without a real window: a stand-in
//! language fills the form, the test moves through it and checks what the window sends
//! back and applies.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use keymap::editor as editor_keys;
use lang_api::{
    ActionEntry, ActionOutcome, ActionProvider, ActionQuery, Answer, Confidence, LanguageFn,
    LanguageId, LanguageSupport, MemberActionForm, MemberActionParametersProvider,
    MemberActionProvider, MemberActionQuery, MemberActionRequest, MemberChoice, MemberRefactoring,
    MemberTarget, Provider, Queries, SourceEdit, SourceId, SyntaxProvider, WorkspaceChange,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{bounds_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{edit_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("member-server");

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

/// The language of the test: it offers no server actions and fills the member form.
struct MemberLanguage {
    syntax: NoSyntax,
    form: Mutex<Option<MemberActionForm>>,
    change: Mutex<WorkspaceChange>,
    applied: Mutex<Vec<MemberActionRequest>>,
}

impl MemberLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            form: Mutex::new(None),
            change: Mutex::new(WorkspaceChange::default()),
            applied: Mutex::new(Vec::new()),
        })
    }

    fn set_form(&self, form: MemberActionForm) {
        *self.form.lock().unwrap() = Some(form);
    }

    fn set_change(&self, change: WorkspaceChange) {
        *self.change.lock().unwrap() = change;
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for MemberLanguage {
    fn start(&self, _: ActionQuery, _: base::Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        let _ = reply.send(self.answer(Vec::new()));
    }
}

impl Provider<MemberActionQuery, Option<MemberActionForm>> for MemberLanguage {
    fn start(
        &self,
        _: MemberActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Option<MemberActionForm>>,
    ) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(self.answer(form));
    }
}

impl Provider<MemberActionRequest, ActionOutcome> for MemberLanguage {
    fn start(
        &self,
        query: MemberActionRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<ActionOutcome>,
    ) {
        self.applied.lock().unwrap().push(query);
        let change = self.change.lock().unwrap().clone();
        let _ = reply.send(self.answer(ActionOutcome {
            change,
            error: None,
        }));
    }
}

impl LanguageSupport for MemberLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-member")
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

    fn member_action_form(&self) -> Option<&MemberActionParametersProvider> {
        Some(self)
    }

    fn apply_member_action(&self) -> Option<&MemberActionProvider> {
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
    language: Arc<MemberLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("member-state");
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
    let setup = project_setup(scratch("member-cache"));
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

fn pull_up_form() -> MemberActionForm {
    MemberActionForm {
        title: "Pull Members Up".to_owned(),
        targets: vec![MemberTarget {
            name: "Base".to_owned(),
            detail: Some("a".to_owned()),
            value: "file:///a/Base.txt".to_owned(),
        }],
        target_label: "Supertype".to_owned(),
        targets_selectable: true,
        members: vec![MemberChoice {
            name: "value".to_owned(),
            detail: Some("public int value()".to_owned()),
            checked: true,
            can_be_abstract: true,
            abstract_checked: false,
            value: "value-handle".to_owned(),
        }],
        members_label: "Members to pull up".to_owned(),
        fields: Vec::new(),
        warning: None,
    }
}

#[ui_kit::gpui::test]
fn refactor_this_lists_pull_members_up_and_applies_its_form(cx: &mut TestAppContext) {
    let root = project("member-form", &[("src/Shape.txt", "shape\n")]);
    let shape = root.join("src").join("Shape.txt");
    let language = MemberLanguage::new();
    language.set_form(pull_up_form());
    let mut change = WorkspaceChange::default();
    change.edits.push(SourceEdit {
        uri: uri_of(&shape),
        edits: vec![edit_in("shape\n", 5, 5, " lifted")],
    });
    language.set_change(change);

    let window = open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &shape);
    let view = editor_of(cx, window, &shape);

    // The four rows are commands of the window even without a key in the IDEA table.
    let commands = window
        .read_with(cx, |workspace, _| {
            workspace
                .commands()
                .all()
                .filter(|command| {
                    command.id == editor_keys::PULL_MEMBERS_UP
                        || command.id == editor_keys::PUSH_MEMBERS_DOWN
                        || command.id == editor_keys::EXTRACT_SUPERCLASS
                        || command.id == editor_keys::INTRODUCE_PARAMETER_OBJECT
                })
                .count()
        })
        .unwrap();
    assert_eq!(commands, 4, "all four rows are commands");

    // Refactor This lists the rows of the editor; Pull Members Up is after the other
    // eleven own rows.
    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Refactor This lists the refactorings"
    );
    for _ in 0..10 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "member-form").size.height > px(0.),
        "the row opens the form"
    );
    assert!(
        bounds_of(cx, window, "member-form-target-0").size.height > px(0.),
        "the form lists the supertype"
    );
    assert!(
        bounds_of(cx, window, "member-form-member-0").size.height > px(0.),
        "the form lists the members"
    );
    assert!(
        bounds_of(cx, window, "member-form-abstract-0").size.height > px(0.),
        "a method has the make abstract option"
    );
    assert!(
        bounds_of(cx, window, "member-form-preview").size.height > px(0.),
        "the form has Preview"
    );

    // Down to the abstract check, Space ticks it, Enter applies the form.
    press(cx, window, "down");
    press(cx, window, "down");
    press(cx, window, "space");
    press(cx, window, "enter");
    draw(cx, window);

    let applied = language.applied.lock().unwrap().clone();
    assert_eq!(applied.len(), 1, "{applied:?}");
    assert_eq!(applied[0].refactoring, MemberRefactoring::PullUp);
    assert_eq!(applied[0].target.as_deref(), Some("file:///a/Base.txt"));
    assert_eq!(applied[0].members.len(), 1);
    assert_eq!(applied[0].members[0].value, "value-handle");
    assert!(applied[0].members[0].is_abstract);
    assert_eq!(crate::window_tests::buffer_of(cx, &view), "shape lifted\n");
}

#[ui_kit::gpui::test]
fn the_form_of_a_member_refactoring_shows_its_warning(cx: &mut TestAppContext) {
    let root = project("member-warning", &[("src/Owner.txt", "owner\n")]);
    let owner = root.join("src").join("Owner.txt");
    let language = MemberLanguage::new();
    let mut form = pull_up_form();
    form.warning = Some("value uses the private count".to_owned());
    language.set_form(form);
    let window = open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    for _ in 0..10 {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "member-form-warning").size.height > px(0.),
        "the conflict warning is visible before the refactoring applies"
    );
}
