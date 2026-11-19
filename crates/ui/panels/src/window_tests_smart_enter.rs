//! Smart Enter and the context menu of the editor on a source file whose real grammar
//! reads its tree: the test support hands it over from the language crate, and a
//! stand-in answers the one reference the menu copies. No language server runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Languages};
use lang_api::{
    Answer, Confidence, LanguageId, LanguageSupport, Provider, ReferenceQuery,
    ReferenceStringProvider, SourceId, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, Theme, point, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, line_height, open_path, press, project_setup, scratch,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("plain-reference");

/// The tree of the language, with the reference of one place as the server of a project
/// would answer it.
struct SourceLanguage(Arc<dyn LanguageSupport>);

impl LanguageSupport for SourceLanguage {
    fn id(&self) -> LanguageId {
        self.0.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.0.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.0.syntax()
    }

    fn reference_string(&self) -> Option<&ReferenceStringProvider> {
        Some(self)
    }
}

impl Provider<ReferenceQuery, String> for SourceLanguage {
    fn start(&self, _: ReferenceQuery, _: base::Cancel, mut reply: lang_api::Reply<String>) {
        let answer = Answer::new("pkg.Class#method".to_owned(), Confidence::Certain, SOURCE);
        let _ = reply.send(answer);
    }
}

/// The window of these tests: the language of a plain source and no server.
fn open_window(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("smart-enter-state");
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
    let support = SourceLanguage(window_language::plain_source());
    let server = LanguageServer {
        support: Some(Arc::new(support) as Arc<dyn LanguageSupport>),
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("smart-enter-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    });
    // The window of a test is closed before the test ends, as the close button of a real
    // one does: the workspace, its panels and the watchers of its index go with it.
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
    window
}

fn open_source(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (PathBuf, WindowHandle<Workspace>, gpui::Entity<EditorView>) {
    let root = scratch(name);
    let path = window_language::write_test_source(&root, "Main", text);
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    (path, window, view)
}

/// Puts the text of the case into the open file and the caret on `line` and `column`.
fn reset(
    cx: &mut TestAppContext,
    view: &gpui::Entity<EditorView>,
    text: &str,
    line: usize,
    column: usize,
) {
    let length = buffer_of(cx, view).len();
    view.update(cx, |view, cx| {
        view.replace_text(0..length, text, cx);
        view.restore_caret(line, column, cx);
    });
}

fn text_point(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) -> gpui::Point<gpui::Pixels> {
    let height = line_height(cx);
    let bounds = bounds_of(cx, window, "editor-body-0");
    point(
        bounds.left() + px(67. + column as f32 * 8.),
        bounds.top() + px(height * line as f32 + height / 2.),
    )
}

fn right_click_text(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    line: usize,
    column: usize,
) {
    let position = text_point(cx, window, line, column);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

/// Clicks the row of the open menu whose label is `label`.
fn click_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let index = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .and_then(|entries| entries.iter().position(|entry| entry.label == label))
        })
        .unwrap()
        .unwrap_or_else(|| panic!("the menu has no row {label}"));
    // The test platform looks its selectors up by a static name.
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"))
        .center();
    visual.simulate_click(bounds, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

#[ui_kit::gpui::test]
fn smart_enter_closes_a_call_and_starts_the_next_line(cx: &mut TestAppContext) {
    const TEXT: &str = "class Main {\n    void run() {\n        helper(a, b\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-call", TEXT);

    reset(cx, &view, TEXT, 2, "        helper(a, b".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        helper(a, b);\n        \n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 8));
}

#[ui_kit::gpui::test]
fn smart_enter_opens_the_body_of_a_condition_and_of_a_method(cx: &mut TestAppContext) {
    const CONDITION: &str = "class Main {\n    void run() {\n        if (x\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-condition", CONDITION);

    reset(cx, &view, CONDITION, 2, "        if (x".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        if (x) {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 12));

    // A loop takes the same shape.
    const LOOP: &str = "class Main {\n    void run() {\n        for (int i = 0; i < n\n    }\n}\n";
    reset(cx, &view, LOOP, 2, "        for (int i = 0; i < n".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        for (int i = 0; i < n) {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 12));

    // The header of a method takes a body of its own.
    const HEADER: &str = "class Main {\n    void run(\n}\n";
    reset(cx, &view, HEADER, 1, "    void run(".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        \n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (2, 8));
}

#[ui_kit::gpui::test]
fn smart_enter_finishes_else_catch_and_try(cx: &mut TestAppContext) {
    const ELSE: &str =
        "class Main {\n    void run() {\n        if (a) {\n        } else\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-else", ELSE);

    reset(cx, &view, ELSE, 3, "        } else".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        if (a) {\n        } else {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (4, 12));

    // `catch` alone gets an empty parameter list with the caret between its brackets.
    const CATCH: &str =
        "class Main {\n    void run() {\n        try {\n        } catch\n    }\n}\n";
    reset(cx, &view, CATCH, 3, "        } catch".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        try {\n        } catch () {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 17));

    const TRY: &str = "class Main {\n    void run() {\n        try\n    }\n}\n";
    reset(cx, &view, TRY, 2, "        try".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        try {\n            \n        }\n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 12));
}

#[ui_kit::gpui::test]
fn smart_enter_only_starts_a_line_after_a_finished_statement(cx: &mut TestAppContext) {
    const TEXT: &str = "class Main {\n    void run() {\n        helper(a, b);\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-done", TEXT);

    reset(cx, &view, TEXT, 2, "        helper(a, b);".len());
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        helper(a, b);\n        \n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (3, 8));
}

#[ui_kit::gpui::test]
fn smart_enter_finishes_every_caret_in_one_undo(cx: &mut TestAppContext) {
    const TEXT: &str =
        "class Main {\n    void run() {\n        one(a, b\n        one(c, d\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-carets", TEXT);

    reset(cx, &view, TEXT, 2, 8);
    // The first Alt+J takes the word, the second adds its neighbour as a caret.
    press(cx, window, "alt-j alt-j");
    press(cx, window, "ctrl-shift-enter");
    assert_eq!(
        buffer_of(cx, &view),
        "class Main {\n    void run() {\n        one(a, b);\n        \n        one(c, d);\n        \n    }\n}\n"
    );
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (5, 8));

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        TEXT,
        "one undo takes the edit at both carets"
    );
}

#[ui_kit::gpui::test]
fn the_editor_menu_offers_folding_and_copy_reference(cx: &mut TestAppContext) {
    const TEXT: &str = "package pkg;\n\nclass Main {\n    void run() {\n    }\n}\n";
    let (_path, window, view) = open_source(cx, "smart-enter-menu", TEXT);

    right_click_text(cx, window, 2, 0);
    let entries = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().map(<[_]>::to_vec)
        })
        .unwrap()
        .expect("the right click opens the menu");

    let folding = entries
        .iter()
        .find(|entry| entry.label == "Folding")
        .expect("the menu has Folding");
    let labels: Vec<&str> = folding
        .submenu
        .iter()
        .filter(|entry| !entry.label.is_empty())
        .map(|entry| entry.label.as_ref())
        .collect();
    assert_eq!(labels, ["Expand", "Collapse", "Expand All", "Collapse All"]);

    let reference = entries
        .iter()
        .find(|entry| entry.label == "Copy Reference")
        .expect("the menu has Copy Reference");
    assert!(reference.on_click.is_some(), "Copy Reference is enabled");
    assert_eq!(
        reference.shortcut.as_deref(),
        Some("Ctrl+Alt+Shift+C"),
        "the row names its key"
    );

    for label in ["Analyze Stack Trace...", "Column Selection Mode"] {
        assert!(
            entries.iter().any(|entry| entry.label == label),
            "the menu has {label}"
        );
    }

    // Copy Reference puts the reference of the place under the caret on the clipboard.
    click_menu_row(cx, window, "Copy Reference");
    cx.run_until_parked();
    assert_eq!(clipboard(cx).as_deref(), Some("pkg.Class#method"));
    assert_eq!(buffer_of(cx, &view), TEXT, "the menu changes no text");
}
