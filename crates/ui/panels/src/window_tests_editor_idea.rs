//! The editor as IDEA does it: a drag after a multiple click selects words or lines,
//! Alt+Enter groups the quick fixes before the intentions and previews the change, the
//! window of a Git change leaves the keys to the editor, and the file types carry their
//! own icons in the tree and in the tabs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::Languages;
use lang_api::{ActionEntry, ActionKind, SourceEdit};
use ui_kit::gpui;
use ui_kit::gpui::{
    MouseButton, MouseDownEvent, MouseUpEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{Modifiers, Theme, point};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, git_repository, open_path, open_window, press, project_setup,
    scratch, type_text,
};
use crate::window_tests_edit::{open_editor, selected_text, text_point};
use crate::window_tests_editor_popups::{PopupLanguage, open_window_with};
use crate::window_tests_git::wait_for;
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

/// Whether the popup with the debug selector is on screen.
fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// A press of the second or the third click of a series, held while the pointer moves,
/// so the editor grows the selection by the unit of that click.
fn drag_after_clicks(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: (usize, usize),
    to: (usize, usize),
    clicks: usize,
) {
    let from = text_point(cx, window, from.0, from.1);
    let to = text_point(cx, window, to.0, to.1);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for click in 1..=clicks {
        visual.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: from,
            modifiers: Modifiers::default(),
            click_count: click,
            first_mouse: false,
        });
        if click < clicks {
            visual.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: Modifiers::default(),
                click_count: click,
            });
        }
    }
    for step in 1..=8 {
        let fraction = step as f32 / 8.;
        visual.simulate_mouse_move(
            point(
                from.x + (to.x - from.x) * fraction,
                from.y + (to.y - from.y) * fraction,
            ),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
    }
    visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// The entries of the fake language: a quick fix and an intention, both with the edit
/// they would make, so the list can show it without applying.
fn actions_of(path: &Path, text: &str) -> Vec<ActionEntry> {
    let uri = uri_of(path);
    let entry = |title: &str, kind: ActionKind, code: &str, start: usize, end: usize, new: &str| {
        ActionEntry {
            title: title.to_owned(),
            kind: Some(kind),
            code: Some(code.to_owned()),
            preferred: kind == ActionKind::QuickFix,
            edits: vec![SourceEdit {
                uri: uri.clone(),
                edits: vec![edit_in(text, start, end, new)],
            }],
            renames: Vec::new(),
            handle: None,
        }
    };
    vec![
        // The intention comes first in the answer on purpose: the order of the list is
        // the section, not the order of the server.
        entry(
            "Rewrite the text",
            ActionKind::Refactor,
            "refactor.rewrite",
            0,
            0,
            "rewritten ",
        ),
        entry(
            "Fix the problem",
            ActionKind::QuickFix,
            "quickfix.fix",
            0,
            7,
            "fixed",
        ),
    ]
}

/// A window whose file icons know the languages of the editor.
fn open_icon_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
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
        }
        .with_languages(),
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
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

#[ui_kit::gpui::test]
fn dragging_after_a_double_click_selects_whole_words(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "idea-words", "alpha beta\ngamma delta\n");

    drag_after_clicks(cx, window, (0, 7), (1, 3), 2);
    assert_eq!(
        selected_text(cx, &view),
        "beta\ngamma",
        "the drag after a double click takes the words of both ends"
    );
}

#[ui_kit::gpui::test]
fn a_triple_click_and_its_drag_select_whole_lines(cx: &mut TestAppContext) {
    let (_root, window, view) = open_editor(cx, "idea-lines", "alpha beta\ngamma delta\nthird");

    drag_after_clicks(cx, window, (1, 3), (1, 3), 3);
    assert_eq!(
        selected_text(cx, &view),
        "gamma delta\n",
        "a triple click takes the line with its line end"
    );

    drag_after_clicks(cx, window, (0, 2), (1, 3), 3);
    assert_eq!(
        selected_text(cx, &view),
        "alpha beta\ngamma delta\n",
        "the drag after a triple click takes whole lines with their line ends"
    );

    drag_after_clicks(cx, window, (2, 1), (2, 2), 3);
    assert_eq!(
        selected_text(cx, &view),
        "third",
        "a triple click on the last line takes it without a line end"
    );
}

#[ui_kit::gpui::test]
fn alt_enter_puts_quick_fixes_before_intentions_and_previews_the_change(cx: &mut TestAppContext) {
    let text = "problem\n";
    let root = project("idea-sections", &[("src/Main.txt", text)]);
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    language.set_actions(actions_of(&path, text));
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(shown(cx, window, "actions-popup"), "Alt+Enter shows a list");
    assert!(
        shown(cx, window, "actions-separator"),
        "the two sections are divided"
    );
    let visible = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("the list is open");
    assert_eq!(
        visible.rows,
        vec![
            (true, "Fix the problem".to_owned()),
            (false, "Rewrite the text".to_owned()),
        ],
        "the quick fix comes first, then the intention"
    );
    assert_eq!(
        visible.preview,
        Some(vec!["- problem".to_owned(), "+ fixed".to_owned()]),
        "the language gave the edit, so the selected row has a preview"
    );
    assert!(
        shown(cx, window, "actions-preview"),
        "the preview stands beside the list"
    );
}

#[ui_kit::gpui::test]
fn an_empty_alt_enter_says_so_and_goes_away_by_itself(cx: &mut TestAppContext) {
    let root = project("idea-no-actions", &[("src/Main.txt", "plain\n")]);
    let path = root.join("src").join("Main.txt");
    let language = PopupLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);

    press(cx, window, "alt-enter");
    draw(cx, window);
    assert!(
        !shown(cx, window, "actions-popup"),
        "an empty answer opens no list"
    );
    assert!(
        shown(cx, window, "lsp-note"),
        "an empty answer says so under the caret"
    );

    cx.executor().advance_clock(Duration::from_millis(3000));
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        !shown(cx, window, "lsp-note"),
        "the message goes away without a key"
    );
}

#[ui_kit::gpui::test]
fn typing_reaches_the_editor_while_a_change_window_is_open(cx: &mut TestAppContext) {
    let root = git_repository("idea-hunk-typing");
    let path = root.join("main.txt");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    wait_for(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });

    view.update(cx, |view, cx| view.replace_text(0..5, "ALPHA", cx));
    wait_for(cx, "the change marks", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| {
            view.gutter_marks()
                .iter()
                .any(|mark| mark.lines.contains(&0))
        })
    });
    let center = view
        .read_with(cx, |view, _| view.change_mark_bounds(0))
        .expect("the change mark has its place")
        .center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(center, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
    assert!(
        shown(cx, window, "hunk-popup"),
        "a click on the change mark opens the window"
    );

    type_text(cx, window, "z");
    draw(cx, window);
    assert!(
        !shown(cx, window, "hunk-popup"),
        "typing closes the window of the change"
    );
    assert!(
        buffer_of(cx, &view).contains('z'),
        "the typed letter reaches the text behind the window"
    );
}

#[ui_kit::gpui::test]
fn python_and_rust_icons_are_not_the_text_icon_in_the_tree_and_the_tab(cx: &mut TestAppContext) {
    let root = scratch("idea-icons");
    fs::create_dir_all(root.join("src")).unwrap();
    let python = root.join("src").join("main.py");
    let rust = root.join("src").join("lib.rs");
    fs::write(&python, "print('hello')\n").unwrap();
    fs::write(&rust, "fn main() {}\n").unwrap();
    let window = open_icon_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);

    let (python_icon, rust_icon) = window
        .read_with(cx, |workspace, _| {
            (
                workspace.icons().icon_for(&python),
                workspace.icons().icon_for(&rust),
            )
        })
        .unwrap();
    assert_eq!(
        python_icon, "fileTypes/python",
        "the tree gives a Python file its own icon"
    );
    assert_eq!(
        rust_icon, "fileTypes/rust",
        "the tree gives a Rust file its own icon"
    );

    open_path(cx, window, &python);
    open_path(cx, window, &rust);
    let (python_tab, rust_tab) = window
        .read_with(cx, |workspace, _| {
            (workspace.tab_icon(&python), workspace.tab_icon(&rust))
        })
        .unwrap();
    assert_eq!(
        python_tab,
        Some("fileTypes/python"),
        "the tab of a Python file carries its own icon"
    );
    assert_eq!(
        rust_tab,
        Some("fileTypes/rust"),
        "the tab of a Rust file carries its own icon"
    );
    assert_ne!(
        python_tab,
        Some("fileTypes/text"),
        "the Python tab does not fall back to the text icon"
    );
}
