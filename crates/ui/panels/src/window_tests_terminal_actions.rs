//! Run Selection in Terminal and the links of the terminal in a window: the text of the
//! editor reaches the shell of the tab, and a printed path opens at its line and column.
//! A real shell runs behind every tab, as in the other tests of the terminal.

use std::fs;
use std::path::{Path, PathBuf};

use ui_kit::MouseButton;
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use workspace::{RUN_TOOL, TERMINAL_TOOL, Workspace};

use crate::terminal_panel::{ActiveTerminal, TerminalPanel};
use crate::window_tests::{
    active_path, draw, editor_of, open_path, open_sample_window, press, type_text, wait_for_signal,
};
use crate::window_tests_terminal_tabs::{
    cell_position, open_terminal, print_line, row_of, session_of, tick,
};

fn terminal_of(cx: &mut TestAppContext) -> gpui::Entity<TerminalPanel> {
    cx.read(|cx| cx.global::<ActiveTerminal>().0.clone())
}

/// The command Run Selection in Terminal runs, as the menu and Find Action do.
fn run_in_terminal(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command(keymap::workspace::RUN_IN_TERMINAL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

/// A file of the sample project that holds shell commands of the tests.
fn command_file(root: &Path, name: &str, lines: &str) -> PathBuf {
    let path = root.join("src").join(name);
    fs::write(&path, lines).unwrap();
    path
}

#[ui_kit::gpui::test]
fn the_selection_of_the_editor_runs_in_the_terminal(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-run-selection");
    let path = command_file(
        &root,
        "run-me.txt",
        "echo term-from-selection\necho term-from-caret\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    // The first line with its line break: the second line stays out of the selection.
    press(cx, window, "shift-down");
    let selected = view
        .read_with(cx, |view, _| view.selected_text())
        .expect("the first line is selected");
    assert_eq!(selected, "echo term-from-selection\n");

    run_in_terminal(cx, window);
    let session = session_of(cx, &terminal);
    wait_for_signal(cx, "the selected line", |_| {
        session.screen().contains("term-from-selection")
    });
    assert!(
        !session.screen().contains("term-from-caret"),
        "the line of the caret is not in the selection"
    );
}

#[ui_kit::gpui::test]
fn without_a_selection_the_line_of_the_caret_runs(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-run-line");
    let path = command_file(
        &root,
        "run-line.txt",
        "echo term-from-selection\necho term-from-caret\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    assert!(
        view.read_with(cx, |view, _| view.selected_text()).is_none(),
        "the caret covers nothing"
    );

    run_in_terminal(cx, window);
    let session = session_of(cx, &terminal);
    wait_for_signal(cx, "the line of the caret", |_| {
        session.screen().contains("term-from-caret")
    });
    assert!(
        !session.screen().contains("term-from-selection"),
        "the line above the caret does not run"
    );
}

/// A multi-line selection is a set of lines the shell takes one after another, as if the
/// user typed each of them.
#[ui_kit::gpui::test]
fn a_multi_line_selection_runs_its_lines_one_after_another(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-run-multi");
    let path = command_file(
        &root,
        "run-multi.txt",
        "echo term-first\necho term-second\necho term-third\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    press(cx, window, "shift-down");
    press(cx, window, "shift-down");
    let selected = view
        .read_with(cx, |view, _| view.selected_text())
        .expect("two lines are selected");
    assert_eq!(selected, "echo term-first\necho term-second\n");

    run_in_terminal(cx, window);
    let session = session_of(cx, &terminal);
    wait_for_signal(cx, "both lines of the selection", |_| {
        let screen = session.screen();
        screen.contains("term-first") && screen.contains("term-second")
    });
    assert!(
        !session.screen().contains("term-third"),
        "the line below the selection does not run"
    );
}

#[ui_kit::gpui::test]
fn a_closed_terminal_opens_with_a_new_tab_in_the_project_root(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "terminal-run-closed");
    let path = command_file(
        &root,
        "run-closed.txt",
        "echo term-from-selection\necho term-from-caret\n",
    );
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, window);
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "the terminal is closed"
    );

    run_in_terminal(cx, window);
    wait_for_signal(cx, "the open terminal", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap()
    });
    let terminal = terminal_of(cx);
    wait_for_signal(cx, "the shell of the new tab", |cx| {
        terminal.read_with(cx, |panel, _| {
            panel.tabs_len() == 2 && panel.session().is_some()
        })
    });
    assert_eq!(
        terminal.read_with(cx, |panel, _| panel.tab_directories()),
        [root.clone(), root.clone()],
        "the new tab starts in the root of the project"
    );
    let session = session_of(cx, &terminal);
    wait_for_signal(cx, "the line of the caret", |_| {
        session.screen().contains("term-from-caret")
    });
}

/// Ctrl+Enter (Command+Enter on macOS) runs a build line of the prompt in the Run
/// window: the sample project has no build tool, so the configuration waits for a project
/// instead of starting anything.
#[ui_kit::gpui::test]
fn ctrl_enter_runs_a_build_line_of_the_prompt_in_the_run_window(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-run-line-key");
    draw(cx, window);
    type_text(cx, window, "gradle --version");
    let session = session_of(cx, &terminal);
    // The caret of the drawn prompt is the signal that the shell finished rendering the
    // line: a key in the middle of a redraw would read a half-drawn prompt.
    wait_for_signal(cx, "the typed line in the shell", |_| {
        let screen = session.screen();
        screen.cursor.is_some() && screen.contains("gradle --version")
    });

    press(
        cx,
        window,
        if keymap::platform().uses_command() {
            "cmd-enter"
        } else {
            "ctrl-enter"
        },
    );
    wait_for_signal(cx, "the Run window", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(RUN_TOOL))
            .unwrap()
    });
    let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
    wait_for_signal(cx, "the configuration of the line", |cx| {
        run.read_with(cx, |panel, _| {
            panel.run_config_name() == Some("gradle --version")
        })
    });
}

#[ui_kit::gpui::test]
fn control_click_on_a_printed_path_opens_it_at_the_line_and_column(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-link-place");
    let path = root.join("src/main.txt");
    let session = print_line(cx, &terminal, "src/main.txt:2:2");
    let row = row_of(&session, "src/main.txt:2:2");
    let at = cell_position(cx, window, row);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(
        at,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    visual.simulate_mouse_up(
        at,
        MouseButton::Left,
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    drop(visual);
    cx.run_until_parked();

    wait_for_signal(cx, "the file of the link", |cx| {
        active_path(cx, window).as_deref() == Some(path.as_path())
    });
    let view = editor_of(cx, window, &path);
    let caret = view
        .read_with(cx, |view, _| view.head_offset())
        .unwrap_or(0);
    assert_eq!(
        caret, 7,
        "the second line of the file starts after `alpha`, the column after `b`"
    );
    tick(cx);
}
