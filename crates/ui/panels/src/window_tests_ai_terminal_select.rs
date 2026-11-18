//! The terminal of an AI session and the Changes tab without a window: a drag selects the text of
//! the session, Ctrl+Shift+C and Ctrl+C copy it, Shift+drag selects while the program has the mouse
//! on, the lines of the Changes tab select and copy without their signs, New Session opens its menu
//! and the row starts the session, and the tabs switch on a click. The harness of these tests is a
//! PowerShell script that stands in for a TUI: it prints a screen and waits for input.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use agent_core::{Harness, Installed};
use change_tracker::TrackerState;
use terminal::TerminalSession;
use ui_kit::gpui::{self, Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{MouseButton, point, px};
use workspace::{AI_TOOL, Workspace};

use crate::agents::{ActiveAgents, AgentKind, AgentsPanel, Tab};
use crate::window_tests::{
    bounds_of, click, draw, open_window, press, sample_project, wait_for_settled_bounds,
    wait_for_signal,
};
use crate::window_tests_cached_views::show_tool;

/// The line the stand-in harness prints on its first row.
const LINE: &str = "alpha beta gamma";

fn panel(cx: &TestAppContext) -> Entity<AgentsPanel> {
    cx.read(|cx| cx.global::<ActiveAgents>().0.clone())
}

/// A window of the sample project that trusts the folder, so a session starts at once. Returns the
/// window and the folder of the project.
fn open(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(&root).unwrap();
    (window, root)
}

/// The stand-in harness: a PowerShell script that clears the screen, prints [`LINE`] on the
/// first row, switches the mouse on when `mouse` is set, and then waits as a TUI does.
fn stand_in(name: &str, mouse: bool) -> Installed {
    let file: PathBuf = test_support::scratch(name).join("stand-in.ps1");
    let mouse_line = if mouse {
        "[Console]::Write(\"$esc[?1000h$esc[?1006h\")"
    } else {
        ""
    };
    let script = format!(
        "$esc = [char]27\r\n\
         [Console]::Write(\"$esc[2J$esc[H\")\r\n\
         {mouse_line}\r\n\
         [Console]::Write(\"{LINE}`r`n\")\r\n\
         Start-Sleep -Seconds 300\r\n"
    );
    fs::write(&file, script).expect("the script of the stand-in");
    Installed {
        harness: Harness::OpenCode,
        program: "powershell.exe".into(),
        prefix: vec![
            "-NoProfile".to_string(),
            "-NoLogo".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            file.display().to_string(),
        ],
        through_shell: false,
    }
}

/// Starts the stand-in as the only harness of the window and waits until its screen shows
/// [`LINE`].
fn start_stand_in(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    installed: Installed,
) -> Entity<AgentsPanel> {
    let agents = panel(cx);
    cx.update(|cx| agents.update(cx, |panel, _| panel.use_installs_for_test(vec![installed])));
    window
        .update(cx, |_, window, cx| {
            agents.update(cx, |panel, cx| {
                panel.request(AgentKind::Tui(Harness::OpenCode), window, cx)
            })
        })
        .unwrap();
    draw(cx, window);
    wait_for_signal(cx, "the stand-in session", |cx| {
        agents.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
    show_tool(cx, window, AI_TOOL);
    wait_for_signal(cx, "the line of the stand-in", |cx| {
        draw(cx, window);
        active_terminal(cx, &agents).is_some_and(|terminal| terminal.screen().contains(LINE))
    });
    wait_for_settled_bounds(cx, window, "terminal-grid");
    agents
}

fn active_terminal(
    cx: &TestAppContext,
    agents: &Entity<AgentsPanel>,
) -> Option<Arc<TerminalSession>> {
    agents.read_with(cx, |panel, _| {
        panel
            .active_entry()
            .and_then(|entry| entry.terminal.clone())
    })
}

/// The places of the first cell and of the right edge of the row that shows [`LINE`].
fn line_ends(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    terminal: &TerminalSession,
) -> (gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>) {
    let lines = terminal.screen().lines();
    let row = lines
        .iter()
        .position(|line| line.trim() == LINE)
        .unwrap_or_else(|| panic!("{LINE} is on the screen: {lines:?}"));
    let bounds = bounds_of(cx, window, "terminal-grid");
    // The grid starts six pixels in and has rows of eighteen pixels: the middle of a row.
    let y = bounds.top() + px(6. + 18. * row as f32 + 9.);
    (
        point(bounds.left() + px(9.), y),
        point(bounds.right() - px(12.), y),
    )
}

/// Drags with the left button from `from` to `to`, with `modifiers` held.
fn drag(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    from: gpui::Point<gpui::Pixels>,
    to: gpui::Point<gpui::Pixels>,
    modifiers: Modifiers,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(from, MouseButton::Left, modifiers);
    visual.simulate_mouse_move(to, Some(MouseButton::Left), modifiers);
    visual.simulate_mouse_up(to, MouseButton::Left, modifiers);
    drop(visual);
    cx.run_until_parked();
    draw(cx, window);
}

/// The text the terminal of the session has selected.
fn selected(cx: &TestAppContext, agents: &Entity<AgentsPanel>) -> Option<String> {
    agents.read_with(cx, |panel, _| panel.grid_selection_text())
}

/// What the clipboard holds as text.
fn clipboard(cx: &TestAppContext) -> Option<String> {
    cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

#[gpui::test]
fn a_drag_selects_the_text_of_the_session_and_ctrl_shift_c_copies_it(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-terminal-drag");
    let agents = start_stand_in(cx, window, stand_in("ai-terminal-drag-harness", false));
    let terminal = active_terminal(cx, &agents).expect("the session has a terminal");
    let (start, end) = line_ends(cx, window, &terminal);

    drag(cx, window, start, end, Modifiers::default());
    assert_eq!(selected(cx, &agents).as_deref(), Some(LINE));

    press(cx, window, "ctrl-shift-c");
    assert_eq!(clipboard(cx).as_deref(), Some(LINE));

    // Ctrl+C copies too while a selection exists.
    cx.update(|cx| cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(String::new())));
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some(LINE));

    // A click clears the selection.
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_click(start, Modifiers::default());
    drop(visual);
    cx.run_until_parked();
    assert_eq!(selected(cx, &agents), None, "a click clears the selection");
}

/// The stand-in asks for the mouse. In this environment the terminal of the session never
/// reports that mode (the request does not reach its screen), so the test does not wait for it.
/// Arro sends no mouse events to the program, so a plain drag and a Shift+drag both select.
#[gpui::test]
fn a_drag_with_or_without_shift_selects_when_the_program_asks_for_the_mouse(
    cx: &mut TestAppContext,
) {
    let (window, _root) = open(cx, "ai-terminal-mouse");
    let agents = start_stand_in(cx, window, stand_in("ai-terminal-mouse-harness", true));
    let terminal = active_terminal(cx, &agents).expect("the session has a terminal");
    let (start, end) = line_ends(cx, window, &terminal);

    drag(cx, window, start, end, Modifiers::default());
    assert_eq!(selected(cx, &agents).as_deref(), Some(LINE));

    drag(
        cx,
        window,
        start,
        end,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    assert_eq!(selected(cx, &agents).as_deref(), Some(LINE));
}

#[gpui::test]
fn the_changes_tab_selects_lines_and_copies_them_without_their_signs(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "ai-terminal-changes");
    let agents = start_stand_in(cx, window, stand_in("ai-terminal-changes-harness", false));
    wait_for_signal(cx, "the first snapshot", |cx| {
        agents.read_with(cx, |panel, _| panel.changes.state == TrackerState::Ready)
    });

    // A new file of three lines: one hunk of three added lines, with a sign in front of each.
    fs::write(root.join("notes.txt"), "alpha\nbeta\ngamma\n").unwrap();
    let tracker = agents
        .read_with(cx, |panel, _| panel.tracker().cloned())
        .expect("the window tracks the changes");
    tracker.refresh();
    wait_for_signal(cx, "the new file in the changes", |cx| {
        draw(cx, window);
        agents.read_with(cx, |panel, _| {
            panel
                .changes
                .session
                .iter()
                .any(|change| change.path == "notes.txt")
        })
    });
    cx.update(|cx| agents.update(cx, |panel, cx| panel.show_tab(Tab::Changes, cx)));
    wait_for_signal(cx, "the review", |cx| {
        draw(cx, window);
        agents.read_with(cx, |panel, _| panel.review.file.is_some())
    });

    // A drag from the first line to the second copies two lines, without their signs.
    let first = bounds_of(cx, window, "review-line-0");
    let second = bounds_of(cx, window, "review-line-1");
    drag(
        cx,
        window,
        first.center(),
        second.center(),
        Modifiers::default(),
    );
    assert_eq!(
        agents.read_with(cx, |panel, _| panel.review.lines),
        Some((0, 1))
    );
    press(cx, window, "ctrl-c");
    assert_eq!(clipboard(cx).as_deref(), Some("alpha\nbeta"));
}

#[gpui::test]
fn new_session_opens_its_menu_and_the_row_starts_the_session(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-terminal-menu");
    show_tool(cx, window, AI_TOOL);
    let agents = panel(cx);
    cx.update(|cx| {
        agents.update(cx, |panel, _| {
            panel.use_installs_for_test(vec![stand_in("ai-terminal-menu-harness", false)])
        })
    });
    click(cx, window, "agent-new-session");
    draw(cx, window);

    // Row 0 is the harness on PATH: its row starts the session.
    click(cx, window, "menu-0");
    wait_for_signal(cx, "the session of the menu", |cx| {
        agents.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
    assert_eq!(
        agents.read_with(cx, |panel, _| panel.entries[0].kind.clone()),
        AgentKind::Tui(Harness::OpenCode)
    );
}

#[gpui::test]
fn the_tabs_of_the_session_switch_on_a_click(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "ai-terminal-tabs");
    show_tool(cx, window, AI_TOOL);
    let agents = panel(cx);
    window
        .update(cx, |_, window, cx| {
            agents.update(cx, |panel, cx| panel.request(AgentKind::Mock, window, cx))
        })
        .unwrap();
    wait_for_signal(cx, "the mock session", |cx| {
        agents.read_with(cx, |panel, _| panel.entries.len() == 1)
    });
    wait_for_settled_bounds(cx, window, "tab-changes");

    click(cx, window, "tab-changes");
    draw(cx, window);
    assert_eq!(agents.read_with(cx, |panel, _| panel.tab), Tab::Changes);

    click(cx, window, "tab-session");
    draw(cx, window);
    assert_eq!(agents.read_with(cx, |panel, _| panel.tab), Tab::Session);
}
