//! Closing the window while the terminal runs a program: the cross has to close the
//! window and end the program, as File > Exit already does.

use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{TERMINAL_TOOL, Workspace};

use crate::terminal_panel::ActiveTerminal;
use crate::window_tests::{open_sample_window, wait_for_process, wait_for_signal};

/// The line that starts a program which prints or sleeps forever: nothing in it ends
/// until the terminal goes away.
fn endless_program() -> &'static str {
    if cfg!(windows) {
        "ping -t 127.0.0.1"
    } else {
        "sleep 600"
    }
}

/// Opens the terminal of a window and waits until the shell is up.
fn running_terminal(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Arc<terminal::TerminalSession> {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TERMINAL_TOOL, window, cx)
        })
        .unwrap();
    let terminal = cx.update(|cx| cx.global::<ActiveTerminal>().0.clone());
    wait_for_signal(cx, "the shell", |cx| {
        terminal.read_with(cx, |panel, _| panel.session().is_some())
    });
    terminal
        .read_with(cx, |panel, _| panel.session().cloned())
        .expect("the terminal has a session")
}

/// Starts the endless program and returns the shell with the programs it started.
fn run_endless_program(
    cx: &mut TestAppContext,
    session: &Arc<terminal::TerminalSession>,
) -> Vec<u32> {
    session
        .send(format!("{}\r", endless_program()).into_bytes())
        .expect("input");
    // The shell starts the program as its child, which is what the close has to end.
    wait_for_signal(cx, "the program of the terminal", |_| {
        subtree(session.pid()).len() > 1
    });
    subtree(session.pid())
}

/// Closes the window the way the cross does: the platform asks the workspace whether it
/// may close, and the window goes away as the application removes it.
fn close_by_the_cross(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let closed = visual.simulate_close();
    assert!(closed, "the cross asks to close the window");
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert!(
        cx.windows().is_empty(),
        "the window of the test is still open"
    );
}

/// Waits until the programs of `running` are gone, giving a real program its time.
fn wait_until_gone(cx: &mut TestAppContext, running: &[u32]) {
    let mut left = running.to_vec();
    wait_for_process(cx, |_| {
        let alive = platform::process_tree();
        left.retain(|pid| alive.iter().any(|process| process.pid == *pid));
        left.is_empty()
    });
    assert!(left.is_empty(), "the terminal still runs {left:?}");
}

/// The ids of the shell and of every program it started.
fn subtree(shell: u32) -> Vec<u32> {
    let mut ids = vec![shell];
    let mut grew = true;
    while grew {
        grew = false;
        for process in platform::process_tree() {
            if process.parent.is_some_and(|parent| ids.contains(&parent))
                && !ids.contains(&process.pid)
            {
                ids.push(process.pid);
                grew = true;
            }
        }
    }
    ids
}

#[ui_kit::gpui::test]
fn the_cross_closes_the_window_while_a_program_runs(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "close-terminal");
    let session = running_terminal(cx, window);
    let running = run_endless_program(cx, &session);
    assert!(running.len() > 1, "the program runs: {running:?}");
    // The panel alone has to keep the session: a copy would hold the program up.
    drop(session);

    close_by_the_cross(cx, window);

    wait_until_gone(cx, &running);
}

#[ui_kit::gpui::test]
fn a_session_kept_by_a_frame_does_not_outlive_the_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "close-terminal-frame");
    let session = running_terminal(cx, window);
    let running = run_endless_program(cx, &session);
    // A frame of the grid holds the session while the window goes away, so the panel is
    // not the last owner: the window still has to end the program.
    let kept = session.clone();
    drop(session);

    close_by_the_cross(cx, window);

    wait_until_gone(cx, &running);
    drop(kept);
}
