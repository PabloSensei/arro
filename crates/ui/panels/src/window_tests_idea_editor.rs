//! The editing habits of IDEA: Soft-Wrap and Show Whitespaces reach every editor and
//! are saved, Paste from History survives a restart, Ctrl+F7 lists the uses in the file,
//! Backspace takes a step of the indentation, and Ctrl+W grows a selection without a
//! parse.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use lang_api::SourceSpan;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, SETTINGS_FILE, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    buffer_of, draw, editor_of, open_path, press, project_setup, sample_project, scratch,
    wait_for_signal,
};
use crate::window_tests_edit::{open_editor, selected_text};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

/// The window of a test on a folder of its state that the test keeps: the second window
/// of the test reads what the first one wrote.
fn open_window_at(
    cx: &mut TestAppContext,
    root: PathBuf,
    state_dir: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(editor_view::Languages {
            resolve: |_| None,
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
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    });
    guard_window(cx);
    window
}

/// Closes what a window of the test still holds when the application goes away, as the
/// window of the window tests does.
fn guard_window(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.on_app_quit(|cx| {
            for handle in cx.windows() {
                let Some(handle) = handle.downcast::<Workspace>() else {
                    continue;
                };
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
            async {}
        })
        .detach();
    });
}

fn close(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn soft_wrap_and_whitespaces_reach_every_editor_and_are_saved(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "wrap-every-editor");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    let main_view = editor_of(cx, window, &main);
    let notes_view = editor_of(cx, window, &notes);

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keymap::editor::TOGGLE_SOFT_WRAP, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        cx.update(|cx| cx.global::<Settings>().soft_wrap),
        "the menu row changes the setting"
    );
    assert!(
        main_view.read_with(cx, |view, _| view.soft_wrap()),
        "the open editor follows the setting"
    );
    assert!(
        notes_view.read_with(cx, |view, _| view.soft_wrap()),
        "every open editor follows it"
    );

    let settings_file = window
        .update(cx, |workspace, _, _| {
            workspace.settings_dir().join(SETTINGS_FILE)
        })
        .unwrap();
    wait_for_signal(cx, "the saved Soft-Wrap", |_| {
        Settings::load(&settings_file).soft_wrap
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keymap::editor::TOGGLE_WHITESPACE, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        main_view.read_with(cx, |view, _| view.show_whitespace()),
        "Show Whitespaces reaches the open editors"
    );
    assert!(
        notes_view.read_with(cx, |view, _| view.show_whitespace()),
        "Show Whitespaces reaches every open editor"
    );

    wait_for_signal(cx, "the settings file", |_| {
        let saved = Settings::load(&settings_file);
        saved.soft_wrap && saved.show_whitespace
    });

    let third = root.join("src").join("third.txt");
    fs::write(&third, "third\n").unwrap();
    open_path(cx, window, &third);
    draw(cx, window);
    let third_view = editor_of(cx, window, &third);
    assert!(
        third_view.read_with(cx, |view, _| view.soft_wrap()),
        "a new tab opens with the saved Soft-Wrap"
    );
    assert!(
        third_view.read_with(cx, |view, _| view.show_whitespace()),
        "a new tab opens with the saved Show Whitespaces"
    );
}

#[ui_kit::gpui::test]
fn paste_history_survives_a_restarted_window(cx: &mut TestAppContext) {
    let state_dir = scratch("clipboard-state");
    let root = sample_project("clipboard-restart");
    let path = root.join("src").join("main.txt");

    let window = open_window_at(cx, root.clone(), state_dir.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    press(cx, window, "ctrl-a ctrl-c");
    let copied = buffer_of(cx, &editor_of(cx, window, &path));
    assert!(
        !copied.is_empty(),
        "the copied text is the text of the file"
    );
    cx.run_until_parked();
    assert!(
        cx.update(|cx| editor_view::clipboard_history(cx))
            .contains(&copied),
        "the copy reaches the history at once"
    );
    close(cx, window);
    cx.update(|cx| editor_view::set_clipboard_history(cx, Vec::new()));

    let window = open_window_at(cx, root, state_dir);
    cx.run_until_parked();
    wait_for_signal(cx, "the history saved by the first window", |cx| {
        cx.update(|cx| editor_view::clipboard_history(cx))
            .contains(&copied)
    });
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn ctrl_f7_lists_the_usages_of_the_current_file(cx: &mut TestAppContext) {
    let root = project(
        "usages-in-file",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&main),
            span: span_in("greet\n", 0, 5),
        },
        SourceSpan {
            uri: uri_of(&other),
            span: span_in("greet\n", 0, 5),
        },
    ]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-f7");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap(),
        "Ctrl+F7 opens the Usages window"
    );
    let usages = cx.update(|cx| cx.global::<ActiveUsages>().0.clone());
    let items = usages.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(items.len(), 1, "only the places in the current file stay");
    assert_eq!(
        platform::path_key(&items[0].path),
        platform::path_key(&main),
        "the place is in the current file"
    );
}

#[ui_kit::gpui::test]
fn backspace_in_the_indent_takes_a_step(cx: &mut TestAppContext) {
    let (_, window, view) = open_editor(cx, "smart-backspace", "    first\n        value\n");
    view.update(cx, |view, cx| view.restore_caret(1, 8, cx));
    draw(cx, window);
    press(cx, window, "backspace");
    assert_eq!(
        buffer_of(cx, &view),
        "    first\n    value\n",
        "Backspace takes the four spaces of one step"
    );
}

#[ui_kit::gpui::test]
fn ctrl_w_without_a_parse_walks_words_lines_paragraphs_and_the_file(cx: &mut TestAppContext) {
    let text = "alpha beta gamma\nsecond line\n\nanother paragraph\nlast\n";
    let (_, window, view) = open_editor(cx, "selection-ladder", text);
    view.update(cx, |view, cx| view.restore_caret(0, 6, cx));
    draw(cx, window);

    press(cx, window, "ctrl-w");
    assert_eq!(
        selected_text(cx, &view),
        "beta",
        "the first press takes the word"
    );
    press(cx, window, "ctrl-w");
    assert_eq!(
        selected_text(cx, &view),
        "alpha beta gamma\n",
        "the second press takes the line"
    );
    press(cx, window, "ctrl-w");
    assert_eq!(
        selected_text(cx, &view),
        "alpha beta gamma\nsecond line\n",
        "the third press takes the paragraph"
    );
    press(cx, window, "ctrl-w");
    assert_eq!(
        selected_text(cx, &view),
        text,
        "the fourth press takes the file"
    );

    press(cx, window, "ctrl-shift-w");
    assert_eq!(
        selected_text(cx, &view),
        "alpha beta gamma\nsecond line\n",
        "Ctrl+Shift+W goes back to the paragraph"
    );
    press(cx, window, "ctrl-shift-w");
    assert_eq!(
        selected_text(cx, &view),
        "alpha beta gamma\n",
        "and on to the line"
    );
    press(cx, window, "ctrl-shift-w");
    assert_eq!(selected_text(cx, &view), "beta", "and on to the word");
    press(cx, window, "ctrl-shift-w");
    assert_eq!(selected_text(cx, &view), "", "and back to the caret");
}
