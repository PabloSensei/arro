//! Fix with AI and Explain with AI in a drawn window: the row of Alt+Enter on a
//! diagnostic, the menu of a failed test and of an exception line of the console, and
//! the session of the mock agent that receives the message. There is no network and no
//! process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lang_api::{DiagnosticEntry, Severity};
use ui_kit::Modifiers;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{AI_TOOL, RUN_TOOL, Settings, Workspace};

use crate::agents::{ActiveAgents, AgentKind, AgentsPanel};
use crate::problems::ActiveProblems;
use crate::script::ActiveRunPanel;
use crate::transcript::Item;
use crate::window_tests::{
    bounds_of, draw, editor_of, open_path, press, wait_for_file_index, wait_for_signal,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in};

const TEXT: &str = "class Main {\n    void run() { boom(); }\n}\n";

/// The line and the column, from zero, of the `boom` call the language reports.
fn on_boom() -> (usize, usize) {
    let start = TEXT.find("boom").unwrap();
    let line = TEXT[..start].matches('\n').count();
    let column = start - TEXT[..start].rfind('\n').map_or(0, |index| index + 1);
    (line, column)
}

fn panel(cx: &TestAppContext) -> Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

fn run_panel(cx: &TestAppContext) -> Entity<crate::run::RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

/// The window of a project that trusts the folder, so a session starts at once.
fn trust(cx: &mut TestAppContext, window: WindowHandle<Workspace>, root: &Path) {
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(root).unwrap();
}

fn start_mock(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let panel = panel(cx);
    window
        .update(cx, |_, window, cx| {
            panel.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the mock session", |cx| {
        panel.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
}

/// The messages of the user in the chat of the active session.
fn user_texts(cx: &TestAppContext) -> Vec<String> {
    let panel = panel(cx);
    panel
        .read_with(cx, |panel, _| {
            panel
                .active_entry()
                .map(|entry| entry.transcript.items.clone())
                .unwrap_or_default()
        })
        .into_iter()
        .filter_map(|item| match item {
            Item::User(text) => Some(text.to_string()),
            _ => None,
        })
        .collect()
}

/// The rows of an open menu of the window, as their labels.
fn menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// Right-clicks the middle of the element with a debug selector.
fn right_click(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let bounds = bounds_of(cx, window, selector);
    right_click_at(cx, window, bounds.center());
}

/// Clicks the row of the open menu whose label is `label`.
fn click_menu_row(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    let selector: &'static str = Box::leak(format!("menu-{index}").into_boxed_str());
    crate::window_tests::click(cx, window, selector);
    draw(cx, window);
}

/// Lets the answer of the stand-in language and the timers of the window run.
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
}

/// The project of these tests with a diagnostic on the call of `boom`, and a session.
fn open_with_error(
    cx: &mut TestAppContext,
    name: &str,
) -> (
    WindowHandle<Workspace>,
    PathBuf,
    Entity<editor_view::EditorView>,
) {
    let root = project(name, &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let start = TEXT.find("boom").unwrap();
    language.set_diagnostics(vec![DiagnosticEntry {
        span: span_in(TEXT, start, start + "boom".len()),
        severity: Severity::Error,
        message: "cannot find symbol boom".to_owned(),
        unnecessary: false,
        code: Some("compiler.err.cant.resolve".to_owned()),
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    trust(cx, window, &root);
    start_mock(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    wait_for_signal(cx, "the diagnostic", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });
    (window, path, view)
}

#[ui_kit::gpui::test]
fn alt_enter_on_an_error_offers_fix_with_ai_and_sends_it_to_the_session(cx: &mut TestAppContext) {
    let (window, _path, view) = open_with_error(cx, "fix-ai-error");

    // The caret stands on the call that the language reports.
    let (line, column) = on_boom();
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);
    press(cx, window, "alt-enter");
    draw(cx, window);

    let rows = view
        .read_with(cx, |view, _| view.actions_visible())
        .expect("Alt+Enter shows the list")
        .rows;
    assert_eq!(
        rows.last().map(|(_, label)| label.as_str()),
        Some("Fix with AI"),
        "the row is the last of the list: {rows:?}"
    );

    // The last row is the one the user picks.
    for _ in 1..rows.len() {
        press(cx, window, "down");
    }
    press(cx, window, "enter");
    draw(cx, window);

    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(AI_TOOL))
        .unwrap();
    assert!(open, "the AI Sessions window shows the session");
    wait_for_signal(cx, "the message of the user", |cx| {
        !user_texts(cx).is_empty()
    });
    let shown = user_texts(cx);
    let message = &shown[0];
    assert!(
        message.contains("Fix this error in src/Main.txt:2."),
        "{message}"
    );
    assert!(message.contains("cannot find symbol boom"), "{message}");
    assert!(
        message.contains("2:     void run() { boom(); }"),
        "the code around the error is in the message: {message}"
    );
    assert!(
        message.contains("Use the Arro MCP tools to read more."),
        "{message}"
    );
}

#[ui_kit::gpui::test]
fn alt_enter_without_ai_features_has_no_fix_row(cx: &mut TestAppContext) {
    let root = project("fix-ai-disabled", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let start = TEXT.find("boom").unwrap();
    language.set_diagnostics(vec![DiagnosticEntry {
        span: span_in(TEXT, start, start + "boom".len()),
        severity: Severity::Error,
        message: "cannot find symbol boom".to_owned(),
        unnecessary: false,
        code: None,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    trust(cx, window, &root);
    start_mock(cx, window);
    // The setting is off before the file opens, as a user turns it off in the settings.
    cx.update(|cx| {
        let mut settings = cx.global::<Settings>().clone();
        settings.ai_enabled = false;
        cx.set_global(settings);
    });
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    wait_for_signal(cx, "the diagnostic", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });

    let (line, column) = on_boom();
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);
    press(cx, window, "alt-enter");
    draw(cx, window);

    let rows = view
        .read_with(cx, |view, _| view.actions_visible())
        .map(|visible| visible.rows)
        .unwrap_or_default();
    assert!(
        !rows.iter().any(|(_, label)| label == "Fix with AI"),
        "no row of the AI is offered: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn fix_with_ai_in_the_problems_menu_sends_the_problem(cx: &mut TestAppContext) {
    let root = project("fix-ai-problems", &[("src/Main.txt", TEXT)]);
    let path = root.join("src").join("Main.txt");
    let language = FakeLanguage::new();
    let start = TEXT.find("boom").unwrap();
    language.set_diagnostics(vec![DiagnosticEntry {
        span: span_in(TEXT, start, start + "boom".len()),
        severity: Severity::Error,
        message: "cannot find symbol boom".to_owned(),
        unnecessary: false,
        code: None,
    }]);
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    trust(cx, window, &root);
    start_mock(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    settle(cx);
    wait_for_signal(cx, "the diagnostic", |cx| {
        !view
            .read_with(cx, |view, _| view.diagnostic_marks())
            .is_empty()
    });

    press(cx, window, "alt-6");
    draw(cx, window);
    let problems = cx.read(|cx| cx.global::<ActiveProblems>().0.clone());
    let items = problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert_eq!(
        items.len(),
        1,
        "the one diagnostic is in the window: {items:?}"
    );

    right_click(cx, window, "item-row-0");
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Fix with AI"),
        "the menu of a problem offers the row: {labels:?}"
    );
    click_menu_row(cx, window, "Fix with AI");

    wait_for_signal(cx, "the message of the user", |cx| {
        !user_texts(cx).is_empty()
    });
    let shown = user_texts(cx);
    let message = &shown[0];
    assert!(
        message.contains("Fix this error in src/Main.txt:2."),
        "{message}"
    );
    assert!(message.contains("cannot find symbol boom"), "{message}");
}

#[ui_kit::gpui::test]
fn fix_with_ai_on_a_failed_test_sends_its_name_message_stack_and_file(cx: &mut TestAppContext) {
    let root = project(
        "fix-ai-test",
        &[(
            "src/demo/Vitest.code",
            "class Vitest {\n    void fails() {\n    }\n}\n",
        )],
    );
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    trust(cx, window, &root);
    start_mock(cx, window);
    wait_for_file_index(cx);

    let panel = run_panel(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx);
            panel.update(cx, |panel, cx| {
                panel.seed_test_failure(
                    "demo.Vitest",
                    "fails",
                    "expected 1, got 2",
                    "AssertionFailedError: expected\n\tat demo.Vitest.fails(Vitest.code:2)",
                );
                cx.notify();
            });
        })
        .unwrap();
    draw(cx, window);

    let selector: &'static str = Box::leak("test-row-seed/c/f".to_string().into_boxed_str());
    right_click(cx, window, selector);
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Fix with AI"),
        "the menu of the failed test offers the row: {labels:?}"
    );
    click_menu_row(cx, window, "Fix with AI");

    wait_for_signal(cx, "the message of the user", |cx| {
        !user_texts(cx).is_empty()
    });
    let shown = user_texts(cx);
    let message = &shown[0];
    assert!(message.contains("demo.Vitest#fails"), "{message}");
    assert!(message.contains("src/demo/Vitest.code"), "{message}");
    assert!(message.contains("expected 1, got 2"), "{message}");
    assert!(
        message.contains("at demo.Vitest.fails(Vitest.code:2)"),
        "{message}"
    );
}

#[ui_kit::gpui::test]
fn explain_with_ai_on_an_exception_line_of_the_console_sends_the_stack(cx: &mut TestAppContext) {
    let root = project("fix-ai-console", &[("src/Main.txt", TEXT)]);
    let window = open_window_with(cx, root.clone(), FakeLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    trust(cx, window, &root);
    start_mock(cx, window);

    let panel = run_panel(cx);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(RUN_TOOL, window, cx);
            panel.update(cx, |panel, cx| {
                panel.seed_console_lines(&[
                    "demo.IllegalStateException: boom",
                    "    at demo.Main.run(Main.txt:2)",
                    "Process finished with exit code 1",
                ]);
                cx.notify();
            });
        })
        .unwrap();
    draw(cx, window);

    // A click on the frame finds the exception above it.
    right_click(cx, window, "run-console-line-1");
    let labels = menu_labels(cx, window);
    assert!(
        labels.iter().any(|label| label == "Explain with AI"),
        "the menu of an exception line offers the row: {labels:?}"
    );
    click_menu_row(cx, window, "Explain with AI");

    wait_for_signal(cx, "the message of the user", |cx| {
        !user_texts(cx).is_empty()
    });
    let shown = user_texts(cx);
    let message = &shown[0];
    assert!(
        message.contains("Explain the cause of this exception."),
        "{message}"
    );
    assert!(
        message.contains("demo.IllegalStateException: boom"),
        "{message}"
    );
    assert!(
        message.contains("at demo.Main.run(Main.txt:2)"),
        "{message}"
    );
    assert!(
        !message.contains("Process finished with exit code 1"),
        "the stack stops at the first line that is not a frame: {message}"
    );
}
