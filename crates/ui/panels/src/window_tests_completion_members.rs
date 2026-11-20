//! The members of the expression before the caret in the completion list: they come
//! before the postfix templates, no template is offered when an identifier stands right
//! after the caret, and Tab replaces the name after the caret while Enter inserts before
//! it. The real server check needs `IDE_CORPUS` and a filled package cache:
//!
//! `cargo test -p panels window_tests_completion_members -- --ignored --test-threads=1`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use lang_api::{
    CompletionEntry, CompletionKind, CompletionProvider, CompletionResolveProvider, LanguageId,
    LanguageSupport, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_complete::CompleteLanguage;
use crate::window_tests_real::{build_window, close, project, wait_for, wait_ready};
use crate::{AgentOptions, LanguageServer, connect, install};

/// The members the stub server answers for the expression, in the order it sends them.
const MEMBERS: [&str; 4] = ["getName", "getId", "getBirthDate", "getType"];

/// The rows the list shows at once; the members must be among the first of them.
const ROWS_ON_SCREEN: usize = 12;

/// The server answers a real project within this long.
const LIMIT: Duration = Duration::from_secs(120);

/// The column after the dot in `String compName = pet.`, on the line of the probe.
const DOT_COLUMN: usize = "        String compName = pet.".len();

/// A source with the caret after the dot of `pet.`; `tail` is the text that follows it.
fn source(tail: &str) -> String {
    format!(
        "class Probe {{\n    void run(Pet pet) {{\n        String compName = pet.{tail}\n    }}\n}}\n"
    )
}

/// A member of the expression as a server sends it: the label with its parameters, the
/// sort text that ranks it, and the name that replaces the typed one.
fn member(name: &str, rank: usize) -> CompletionEntry {
    CompletionEntry {
        label: format!("{name}()"),
        insert_text: Some(name.to_owned()),
        sort_text: Some(format!("{rank:04}")),
        kind: CompletionKind::Method,
        ..CompletionEntry::default()
    }
}

/// The labels the list shows for the members, in the order the stub sends them.
fn member_labels() -> Vec<String> {
    MEMBERS.iter().map(|name| format!("{name}()")).collect()
}

/// The language of the source files, with the completion answers of the stub. The
/// syntax and the postfix templates are the real ones of the window, as in a project.
struct MemberCompletion(Arc<CompleteLanguage>);

impl LanguageSupport for MemberCompletion {
    fn id(&self) -> LanguageId {
        LanguageId("members")
    }

    fn matches(&self, path: &Path) -> bool {
        window_language::language_for(path).is_some()
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.0.syntax()
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        self.0.completion()
    }

    fn completion_resolve(&self) -> Option<&CompletionResolveProvider> {
        self.0.completion_resolve()
    }
}

fn members_language() -> Arc<CompleteLanguage> {
    let language = CompleteLanguage::new();
    language.set_entries(
        MEMBERS
            .iter()
            .enumerate()
            .map(|(rank, name)| member(name, rank + 1))
            .collect(),
    );
    language
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<CompleteLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("completion-members-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: window_language::language_for,
            templates: window_language::templates_for,
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
        support: Some(Arc::new(MemberCompletion(language)) as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("completion-members-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// Opens the probe with the caret after the dot of `pet.`.
fn open_probe(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    tail: &str,
) -> (PathBuf, gpui::Entity<EditorView>) {
    let path = window_language::write_test_source(root, "Probe", &source(tail));
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: 2,
                column: DOT_COLUMN,
            },
            cx,
        )
    });
    cx.run_until_parked();
    (path, view)
}

/// The labels of the open completion list, or `None` when no list is open.
fn list_rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<Vec<String>> {
    view.read_with(cx, |view, _| view.completion_rows())
}

#[ui_kit::gpui::test]
fn the_members_of_an_expression_come_before_its_postfix_templates(cx: &mut TestAppContext) {
    let root = scratch("completion-members-order");
    let window = open_window(cx, root.clone(), members_language());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root, ";");

    press(cx, window, "ctrl-space");
    draw(cx, window);

    let rows = list_rows(cx, &view).expect("the list is open");
    let members = member_labels();
    assert_eq!(
        rows[..members.len()],
        members[..],
        "the members come first: {rows:?}"
    );
    assert!(
        rows.len() > members.len(),
        "the postfix templates follow the members: {rows:?}"
    );
    assert!(
        rows[members.len()..].iter().all(|row| !row.ends_with("()")),
        "only templates follow the members: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn no_postfix_template_is_offered_when_an_identifier_follows_the_caret(cx: &mut TestAppContext) {
    let root = scratch("completion-members-identifier");
    let window = open_window(cx, root.clone(), members_language());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root, "getName();");

    press(cx, window, "ctrl-space");
    draw(cx, window);

    assert_eq!(
        list_rows(cx, &view),
        Some(member_labels()),
        "the list holds the members and no postfix template"
    );
}

#[ui_kit::gpui::test]
fn tab_replaces_the_identifier_after_the_caret_with_the_chosen_member(cx: &mut TestAppContext) {
    let root = scratch("completion-members-tab");
    let window = open_window(cx, root.clone(), members_language());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root, "getName();");

    press(cx, window, "ctrl-space");
    press(cx, window, "down");
    press(cx, window, "tab");
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(
        text.contains("String compName = pet.getId();"),
        "Tab replaces the name after the caret: {text:?}"
    );
}

#[ui_kit::gpui::test]
fn enter_inserts_the_chosen_member_before_the_identifier_after_the_caret(cx: &mut TestAppContext) {
    let root = scratch("completion-members-enter");
    let window = open_window(cx, root.clone(), members_language());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root, "getName();");

    press(cx, window, "ctrl-space");
    press(cx, window, "down");
    press(cx, window, "enter");
    draw(cx, window);

    let text = buffer_of(cx, &view);
    assert!(
        text.contains("String compName = pet.getIdgetName();"),
        "Enter inserts before the name after the caret: {text:?}"
    );
}

/// The real server: the members of `pet.` come first in the list, and `getName` and
/// `getId` are among the first rows.
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn the_real_server_lists_the_members_of_the_expression_first(cx: &mut TestAppContext) {
    let (root, language) = project("completion-members-real");
    let path = window_language::main_class(
        &root,
        "org.springframework.samples.petclinic.owner",
        "Owner",
    );
    let window = build_window(
        cx,
        root.clone(),
        &language,
        project_setup(scratch("completion-members-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let text = fs::read_to_string(&path).unwrap();
    let line = text
        .lines()
        .position(|line| line.contains("String compName = pet.getName();"))
        .expect("the line of the test");
    let column = text.lines().nth(line).unwrap().find("pet.").unwrap() + "pet.".len();
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-space");
    draw(cx, window);
    wait_for(cx, "the completion list", LIMIT, |cx| {
        list_rows(cx, &view).is_some()
    });
    let rows = list_rows(cx, &view).unwrap();
    let first: Vec<String> = rows.iter().take(ROWS_ON_SCREEN).cloned().collect();
    assert!(
        first.iter().all(|row| row.contains('(')),
        "the first rows are members, not templates: {first:?}"
    );
    assert!(
        first.iter().any(|row| row.starts_with("getName(")),
        "getName is on the first rows: {first:?}"
    );
    assert!(
        first.iter().any(|row| row.starts_with("getId(")),
        "getId is on the first rows: {first:?}"
    );

    close(cx, window, language, false);
}
