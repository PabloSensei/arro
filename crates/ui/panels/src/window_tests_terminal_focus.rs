//! The keys of the terminal with the focus in it: Escape gives the focus to the editor and
//! keeps the terminal open, or goes to the shell when the setting says so, and the keys of
//! the window reach the window instead of the shell. A real shell runs behind the terminal,
//! as in the other tests of the terminal.

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{PROJECT_TOOL, TERMINAL_TOOL, Workspace};

use crate::terminal_panel::TerminalPanel;
use crate::window_tests::{draw, open_path, press, wait_for_signal};
use crate::window_tests_git::has_selector;
use crate::window_tests_terminal_tabs::{open_terminal, session_of};

/// The key of a test on this platform: the keymap of macOS binds the windows to Command.
fn key_of(windows: &'static str, mac: &'static str) -> &'static str {
    if keymap::platform().uses_command() {
        mac
    } else {
        windows
    }
}

/// Gives the keyboard to the grid of the terminal, as a click on it does.
fn focus_terminal(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    terminal: &Entity<TerminalPanel>,
) {
    let focus = terminal.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    draw(cx, window);
}

fn terminal_focused(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    terminal: &Entity<TerminalPanel>,
) -> bool {
    let focus = terminal.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, _| focus.is_focused(window))
        .unwrap()
}

fn editor_focused(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .update(cx, |workspace, window, cx| {
            workspace.editor_has_focus(window, cx)
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn escape_in_the_terminal_moves_the_focus_to_the_editor_and_keeps_the_terminal(
    cx: &mut TestAppContext,
) {
    let (root, window, terminal) = open_terminal(cx, "terminal-escape-editor");
    open_path(cx, window, &root.join("src").join("main.txt"));
    focus_terminal(cx, window, &terminal);

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        editor_focused(cx, window),
        "Escape gives the focus to the editor"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "the terminal stays open"
    );
}

/// With the setting off, Escape is the key of the shell: the focus stays in the terminal.
/// Whether the shell reads the key is not seen here: PowerShell does not clear a typed line
/// on Escape, so the screen shows nothing of it.
#[ui_kit::gpui::test]
fn escape_with_the_setting_off_keeps_the_focus_in_the_terminal(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-escape-shell");
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.terminal_escape_focus = false, cx)
        })
        .unwrap();
    focus_terminal(cx, window, &terminal);

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        terminal_focused(cx, window, &terminal),
        "the focus stays in the terminal"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TERMINAL_TOOL))
            .unwrap(),
        "the terminal stays open"
    );
}

#[ui_kit::gpui::test]
fn alt_1_from_the_terminal_opens_the_project(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-keys-project");
    // The project window starts open, so the key would close it rather than open it.
    window
        .update(cx, |workspace, window, cx| {
            if workspace.tool_open(PROJECT_TOOL) {
                workspace.toggle_tool(PROJECT_TOOL, window, cx);
            }
        })
        .unwrap();
    cx.run_until_parked();
    focus_terminal(cx, window, &terminal);

    press(cx, window, key_of("alt-1", "cmd-1"));
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
            .unwrap(),
        "the key opens the Project window from the terminal"
    );
}

#[ui_kit::gpui::test]
fn ctrl_shift_a_from_the_terminal_opens_find_action(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-keys-find-action");
    focus_terminal(cx, window, &terminal);

    press(cx, window, key_of("ctrl-shift-a", "cmd-shift-a"));
    draw(cx, window);
    assert!(
        has_selector(cx, window, "palette"),
        "the key opens Find Action from the terminal"
    );
}

#[ui_kit::gpui::test]
fn ctrl_e_from_the_terminal_stays_with_the_shell(cx: &mut TestAppContext) {
    // Ctrl+E is the end of the line in readline, so the terminal keeps it; Recent Files
    // opens from the editor. On macOS Cmd+E is not a key of the shell and opens the list.
    let (_, window, terminal) = open_terminal(cx, "terminal-keys-recent");
    focus_terminal(cx, window, &terminal);

    let mac = key_of("ctrl-e", "cmd-e") == "cmd-e";
    press(cx, window, key_of("ctrl-e", "cmd-e"));
    draw(cx, window);
    assert_eq!(
        has_selector(cx, window, "palette"),
        mac,
        "Ctrl+E stays with the shell; Cmd+E opens Recent Files"
    );
}

#[ui_kit::gpui::test]
fn ctrl_c_from_the_terminal_goes_to_the_shell(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-keys-ctrl-c");
    focus_terminal(cx, window, &terminal);
    let session = session_of(cx, &terminal);
    // A typed line that is not run: the Ctrl+C of the shell cancels it, so it never prints.
    session.send(b"echo cancelled".to_vec()).expect("input");
    wait_for_signal(cx, "the typed line", |_| {
        session.screen().contains("echo cancelled")
    });

    press(cx, window, "ctrl-c");
    // A line typed while the shell still handles the interrupt can be dropped with the
    // cancelled one, so the marker line is typed again until the shell prints it.
    let printed = || {
        session
            .screen()
            .lines()
            .iter()
            .any(|line| line.trim() == "after-cancel")
    };
    for _ in 0..10 {
        session
            .send(b"echo after-cancel\r".to_vec())
            .expect("input");
        let started = std::time::Instant::now();
        while !printed() && started.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(50));
            cx.run_until_parked();
        }
        if printed() {
            break;
        }
    }
    assert!(printed(), "the shell prints the line after Ctrl+C");
    assert!(
        !session
            .screen()
            .lines()
            .iter()
            .any(|line| line.trim() == "cancelled"),
        "the cancelled line does not run"
    );
}

#[ui_kit::gpui::test]
fn shift_f11_from_the_terminal_shows_the_bookmarks(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "terminal-keys-bookmarks");
    focus_terminal(cx, window, &terminal);

    press(cx, window, key_of("shift-f11", "shift-f3"));
    draw(cx, window);
    assert!(
        has_selector(cx, window, "places-popup"),
        "the key shows the bookmarks from the terminal"
    );
}

/// A full-screen program (vim, less) on the alternate screen gets its Escape: the focus
/// stays in the terminal even with the setting on.
#[ui_kit::gpui::test]
fn escape_on_the_alternate_screen_stays_with_the_program(cx: &mut TestAppContext) {
    let (root, window, terminal) = open_terminal(cx, "terminal-escape-alt-screen");
    open_path(cx, window, &root.join("src").join("main.txt"));
    focus_terminal(cx, window, &terminal);
    let session = session_of(cx, &terminal);
    let enter = if cfg!(windows) {
        b"[Console]::Write([char]27 + '[?1049h')\r".to_vec()
    } else {
        b"printf '\\033[?1049h'\r".to_vec()
    };
    session.send(enter).expect("input");
    wait_for_signal(cx, "the alternate screen", |_| session.modes().alt_screen);

    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        terminal_focused(cx, window, &terminal),
        "Escape stays with the full-screen program"
    );
}
