//! Completion as IDEA does it: a name is found by its humps and with one typo, the
//! second Ctrl+Space shows every entry of the answer, the third opens the documentation
//! of the selected row, and the entries the user took before rise above their equals.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{CompletionUsage, EditorView, Languages, Spot};
use lang_api::{CompletionEntry, CompletionKind};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
    wait_for_signal,
};
use crate::window_tests_speed::SpeedLanguage;
use crate::{AgentOptions, LanguageServer, connect, install};

/// The language of the editor itself, so the keys of a text file have their rules.
fn plain_language(path: &Path) -> Option<editor_view::LanguageConfig> {
    let is_text = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"));
    if !is_text {
        return None;
    }
    let mut config = dockerfile_syntax::config();
    config.name = "Plain";
    config.statements = Some(editor_view::StatementRules {
        terminator: ";",
        block_words: &[],
    });
    Some(config)
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

fn class(label: &str, sort: &str) -> CompletionEntry {
    CompletionEntry {
        label: label.to_owned(),
        insert_text: Some(label.to_owned()),
        sort_text: Some(sort.to_owned()),
        kind: CompletionKind::Class,
        ..CompletionEntry::default()
    }
}

/// A window over `state_dir`, so that a test opens two windows on one state folder.
fn window_with_state(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<SpeedLanguage>,
    state_dir: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: plain_language,
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
        support: Some(language as Arc<dyn lang_api::LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("completion-idea-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn focus_editor(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
}

fn rows(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<Vec<String>> {
    view.read_with(cx, |view, _| view.completion_rows())
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

#[ui_kit::gpui::test]
fn humps_find_a_type_and_the_matched_letters_stand_out(cx: &mut TestAppContext) {
    let root = project("completion-idea-humps", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    language.set_entries(vec![
        class("StringBuffer", "0002"),
        class("ArrayList", "0003"),
        class("StringBuilder", "0001"),
    ]);
    let window = crate::window_tests_speed::open_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src/A.txt");
    open_path(cx, window, &path);
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "StrB");
    assert_eq!(
        rows(cx, &view).and_then(|rows| rows.first().cloned()),
        Some("StringBuilder".to_owned()),
        "StrB finds StringBuilder by its humps"
    );
    let highlighted = view
        .read_with(cx, |view, _| view.completion_highlighted())
        .expect("the list is open");
    let letters = highlighted
        .into_iter()
        .find(|(label, _)| label == "StringBuilder")
        .map(|(_, letters)| letters)
        .expect("StringBuilder is in the list");
    assert_eq!(letters, ["S", "t", "r", "B"]);
}

#[ui_kit::gpui::test]
fn a_second_ctrl_space_shows_every_entry_and_a_third_opens_the_documentation(
    cx: &mut TestAppContext,
) {
    let root = project("completion-idea-show-all", &[("src/A.txt", "\n")]);
    let language = SpeedLanguage::new();
    let mut builder = class("StringBuilder", "0001");
    builder.resolve = Some("StringBuilder".to_owned());
    language.set_entries(vec![builder, class("ArrayList", "0002")]);
    let window = crate::window_tests_speed::open_window(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src/A.txt");
    open_path(cx, window, &path);
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "StrB");
    assert_eq!(
        rows(cx, &view),
        Some(vec!["StringBuilder".to_owned()]),
        "the prefix filters the answer"
    );
    assert_eq!(language.completions(), 1);

    // The list opened by typing is not a Ctrl+Space yet: the first press asks again and
    // marks the list as the one the key opened.
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert_eq!(language.completions(), 2);
    wait_for_signal(cx, "the documentation of the row", |cx| {
        view.read_with(cx, |view, _| view.completion_documentation())
            .is_some()
    });
    draw(cx, window);
    let resolves = language.resolves();

    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert_eq!(
        rows(cx, &view).map(|rows| rows.len()),
        Some(2),
        "the second Ctrl+Space shows the whole answer"
    );
    assert_eq!(
        language.completions(),
        2,
        "the second Ctrl+Space asks the server nothing"
    );

    press(cx, window, "ctrl-space");
    draw(cx, window);
    let doc = view.read_with(cx, |view, _| view.quick_doc_text());
    assert!(
        shown(cx, window, "quick-doc"),
        "the third Ctrl+Space opens the documentation of the selected row: {doc:?}"
    );
    assert_eq!(
        language.resolves(),
        resolves,
        "the documentation already here is not asked for again"
    );
}

#[ui_kit::gpui::test]
fn a_choice_taken_twice_rises_after_the_window_opens_again(cx: &mut TestAppContext) {
    let root = project("completion-idea-usage", &[("src/A.txt", "\n")]);
    let state = scratch("completion-idea-usage-state");
    let language = SpeedLanguage::new();
    language.set_entries(vec![
        class("AbstractList", "0001"),
        class("ArrayList", "0002"),
    ]);
    let window = window_with_state(cx, root.clone(), language.clone(), state.clone());
    cx.run_until_parked();
    draw(cx, window);
    let path = root.join("src/A.txt");
    open_path(cx, window, &path);
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);

    type_text(cx, window, "A");
    assert_eq!(
        rows(cx, &view),
        Some(vec!["AbstractList".to_owned(), "ArrayList".to_owned()]),
        "without a choice the server order stands"
    );
    press(cx, window, "down enter");
    assert_eq!(buffer_of(cx, &view), "ArrayList\n");

    let text = buffer_of(cx, &view);
    view.update(cx, |view, cx| {
        view.replace_text(0..text.len(), "", cx);
        view.go_to(Spot::Offset(0), cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    type_text(cx, window, "A");
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "ArrayList");

    wait_for_signal(cx, "the table of the chosen entries", |_| {
        fs::read_to_string(state.join("completion-usage.tsv"))
            .is_ok_and(|body| body.contains("A\tArrayList"))
    });

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| cx.remove_global::<CompletionUsage>());

    let window = window_with_state(cx, root.clone(), language, state);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    focus_editor(cx, window);
    let view = editor_of(cx, window, &path);
    type_text(cx, window, "A");
    let shown = rows(cx, &view).expect("the list is open");
    assert_eq!(
        shown.first().map(String::as_str),
        Some("ArrayList"),
        "the entry taken twice comes before its equal"
    );
    assert!(
        shown.iter().any(|label| label == "AbstractList"),
        "the other entry stays in the list: {shown:?}"
    );
}
