//! The wheel over the terminal of a window and over the terminal of an AI session, without a
//! window of the desktop. The program in the terminal is a stand-in: a PowerShell script that sets
//! the modes of a full-screen program, prints READY and then prints what its input brings. The
//! wheel reaches the program as mouse reports or arrow keys, or it turns the history of the screen.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use agent_core::{Harness, Installed};
use terminal::TerminalSession;
use ui_kit::gpui::{
    self, Modifiers, ScrollDelta, ScrollEvent, TestAppContext, VisualTestContext, WindowHandle,
};
use ui_kit::{MouseButton, point, px};
use workspace::{AI_TOOL, Workspace};

use crate::agents::{ActiveAgents, AgentKind};
use crate::window_tests::{
    bounds_of, click, draw, open_window, press, sample_project, scratch, wait_for_settled_bounds,
    wait_for_signal,
};
use crate::window_tests_cached_views::show_tool;
use crate::window_tests_terminal_tabs::{cell_position, open_terminal, session_of};

/// Long enough for the stand-in to print what it received.
const SETTLE: Duration = Duration::from_millis(1500);

/// Loads the console functions the stand-in needs: a read of its input that returns the bytes as
/// they come, without line editing, and output that takes escapes.
const CONSOLE: &str = "Add-Type -TypeDefinition @'\r\n\
using System;\r\n\
using System.Runtime.InteropServices;\r\n\
public static class ConsoleIo {\r\n\
    [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern IntPtr GetStdHandle(int which);\r\n\
    [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool GetConsoleMode(IntPtr handle, out uint mode);\r\n\
    [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool SetConsoleMode(IntPtr handle, uint mode);\r\n\
    [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool ReadFile(IntPtr handle, byte[] buffer, uint count, out uint read, IntPtr overlapped);\r\n\
}\r\n\
'@\r\n\
$esc = [char]27\r\n\
$stdin = [ConsoleIo]::GetStdHandle(-10)\r\n\
$stdout = [ConsoleIo]::GetStdHandle(-11)\r\n\
$mode = [uint32]0\r\n\
[void][ConsoleIo]::GetConsoleMode($stdout, [ref]$mode)\r\n\
[void][ConsoleIo]::SetConsoleMode($stdout, $mode -bor 4)\r\n\
[void][ConsoleIo]::SetConsoleMode($stdin, 512)\r\n";

/// The reads of the stand-in: every read of its input is printed as one `GOT` line, with the
/// escape shown as `^[`.
const READS: &str = "$buffer = New-Object byte[] 256\r\n\
$read = [uint32]0\r\n\
while ([ConsoleIo]::ReadFile($stdin, $buffer, 256, [ref]$read, [IntPtr]::Zero) -and $read -gt 0) {\r\n\
    $text = [Text.Encoding]::ASCII.GetString($buffer, 0, [int]$read).Replace(\"$esc\", '^[')\r\n\
    [Console]::Write(\"`r`nGOT \" + $text)\r\n\
}\r\n";

/// The stand-in script that sets `modes` in their order and then prints READY. The modes are
/// `lines` (200 lines on the main screen), `alt` (the alternate screen), `mouse` and `sgr` (mouse
/// reports and their encoding) and `cursor` (the application cursor keys).
fn script(modes: &[&str]) -> String {
    let mut text = String::from(CONSOLE);
    for mode in modes {
        let line = match *mode {
            "lines" => {
                "for ($number = 1; $number -le 200; $number++) { [Console]::Write(('line {0:000}' -f $number) + \"`r`n\") }\r\n"
            }
            "alt" => "[Console]::Write(\"$esc[?1049h$esc[2J$esc[H\")\r\n",
            "mouse" => "[Console]::Write(\"$esc[?1000h\")\r\n",
            "sgr" => "[Console]::Write(\"$esc[?1006h\")\r\n",
            "cursor" => "[Console]::Write(\"$esc[?1h\")\r\n",
            other => panic!("the stand-in has no mode {other}"),
        };
        text.push_str(line);
    }
    // No line break after READY: a line break there would scroll the screen after the test sees
    // READY, and a wheel turn sent in between would then land one line off.
    text.push_str("[Console]::Write(\"READY\")\r\n");
    text.push_str(READS);
    text
}

/// Writes the stand-in script of `modes` into a folder of the test and returns its path.
fn stand_in(name: &str, modes: &[&str]) -> PathBuf {
    let file = scratch(name).join("stand-in.ps1");
    fs::write(&file, script(modes)).expect("the script of the stand-in");
    file
}

/// What the stand-in printed about its input, with the escapes the screen shows as `^[` turned
/// back into the bytes they are.
fn received(session: &TerminalSession) -> String {
    session
        .screen()
        .lines()
        .iter()
        .filter_map(|line| line.strip_prefix("GOT "))
        .collect::<String>()
        .replace("^[", "\x1b")
}

/// One turn of the wheel over grid row `row`, as Windows sends it: a notch is three lines up. Shift turns
/// the wheel into a horizontal delta, which the window reads as the lines of the turn.
fn wheel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: usize,
    lines: f32,
    shift: bool,
) {
    let position = cell_position(cx, window, row);
    let delta = if shift {
        gpui::point(lines, 0.)
    } else {
        gpui::point(0., lines)
    };
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(ScrollEvent {
        position,
        delta: ScrollDelta::Lines(delta),
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        ..Default::default()
    });
    cx.run_until_parked();
    draw(cx, window);
}

/// Waits until what the stand-in printed holds `needle`, then lets the reads that follow arrive.
fn expect_input(cx: &mut TestAppContext, session: &TerminalSession, what: &str, needle: &str) {
    wait_for_signal(cx, what, |_| received(session).contains(needle));
    thread::sleep(SETTLE);
}

/// The terminal of the window, with the stand-in started in its shell.
fn stand_in_in_terminal(
    cx: &mut TestAppContext,
    name: &str,
    modes: &[&str],
) -> (WindowHandle<Workspace>, Arc<TerminalSession>) {
    let (_, window, terminal) = open_terminal(cx, name);
    let session = session_of(cx, &terminal);
    let file = stand_in(name, modes);
    let command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File \"{}\"\r",
        file.display()
    );
    session.send(command.into_bytes()).expect("input");
    wait_for_signal(cx, "the stand-in", |_| session.screen().contains("READY"));
    draw(cx, window);
    wait_for_settled_bounds(cx, window, "terminal-grid");
    (window, session)
}

/// A window of the sample project that trusts its folder, so an AI session starts at once.
fn open_trusted(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let root = sample_project(name);
    let window = open_window(cx, root.clone());
    draw(cx, window);
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    store::TrustList::new(&state_dir).trust(&root).unwrap();
    window
}

/// The AI session of the window: the stand-in is its harness, and the session shows its READY.
fn stand_in_in_ai_session(
    cx: &mut TestAppContext,
    name: &str,
    modes: &[&str],
) -> (WindowHandle<Workspace>, Arc<TerminalSession>) {
    let window = open_trusted(cx, name);
    let file = stand_in(name, modes);
    let installed = Installed {
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
    };
    let agents = cx.read(|cx| cx.global::<ActiveAgents>().0.clone());
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
    let mut found = None;
    wait_for_signal(cx, "the terminal of the session", |cx| {
        found = agents.read_with(cx, |panel, _| {
            panel
                .active_entry()
                .and_then(|entry| entry.terminal.clone())
        });
        found.is_some()
    });
    let session = found.expect("the session has a terminal");
    wait_for_signal(cx, "the stand-in of the session", |cx| {
        draw(cx, window);
        session.screen().contains("READY")
    });
    wait_for_settled_bounds(cx, window, "terminal-grid");
    (window, session)
}

#[ui_kit::gpui::test]
fn a_long_output_turns_back_to_its_first_lines(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-history", &["lines"]);
    assert!(
        !session
            .screen()
            .lines()
            .iter()
            .any(|line| line == "line 001")
    );

    // The history is shorter than two hundred lines, so the view reaches its top. The session
    // turns the view on its own thread, so the screen shows the line a little later.
    wheel(cx, window, 2, 200., false);

    wait_for_signal(cx, "the first line", |_| {
        session
            .screen()
            .lines()
            .iter()
            .any(|line| line == "line 001")
    });
}

#[ui_kit::gpui::test]
fn the_wheel_reaches_a_full_screen_program_as_a_mouse_report(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-mouse", &["alt", "mouse", "sgr"]);

    // Row two of the grid is the third row, and column zero the first.
    wheel(cx, window, 2, 3., false);

    expect_input(cx, &session, "the mouse report", "\x1b[<64;1;3M");
    assert_eq!(received(&session).matches("\x1b[A").count(), 0);
}

#[ui_kit::gpui::test]
fn the_wheel_of_a_full_screen_program_without_mouse_reports_is_arrow_keys(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-arrows", &["alt"]);

    wheel(cx, window, 2, 3., false);

    expect_input(cx, &session, "the arrow keys", "\x1b[A\x1b[A\x1b[A");
    assert_eq!(received(&session).matches("\x1b[A").count(), 3);
    assert_eq!(received(&session).matches("\x1b[<").count(), 0);
}

#[ui_kit::gpui::test]
fn shift_turns_the_history_while_the_program_has_the_mouse(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-shift", &["lines", "mouse", "sgr"]);
    wait_for_signal(cx, "the history", |_| session.screen().history > 0);

    wheel(cx, window, 2, 3., true);

    wait_for_signal(cx, "the view to leave the bottom", |_| {
        session.screen().display_offset == 3
    });
    // The program's lines show at the bottom of the screen, which the view has left.
    session.scroll_to_bottom();
    thread::sleep(SETTLE);
    assert_eq!(received(&session).matches("\x1b[<").count(), 0);
}

#[ui_kit::gpui::test]
fn the_wheel_of_an_ai_session_reaches_its_program_as_a_mouse_report(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_ai_session(cx, "wheel-ai-mouse", &["alt", "mouse", "sgr"]);

    wheel(cx, window, 2, 3., false);

    expect_input(
        cx,
        &session,
        "the mouse report of the session",
        "\x1b[<64;1;3M",
    );
}

#[ui_kit::gpui::test]
fn shift_turns_the_history_of_an_ai_session(cx: &mut TestAppContext) {
    let (window, session) =
        stand_in_in_ai_session(cx, "wheel-ai-shift", &["lines", "mouse", "sgr"]);
    wait_for_signal(cx, "the history of the session", |_| {
        session.screen().history > 0
    });

    wheel(cx, window, 2, 3., true);

    wait_for_signal(cx, "the view of the session to leave the bottom", |_| {
        session.screen().display_offset == 3
    });
}

#[ui_kit::gpui::test]
fn the_thumb_of_the_bar_drags_the_history(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-thumb", &["lines"]);
    wait_for_signal(cx, "the history", |_| session.screen().history > 0);
    draw(cx, window);

    // The view is at the bottom, so the thumb of the bar is at the foot of its track, in the right
    // inset of the grid. A press four pixels above the foot takes it, and a move up pulls it back.
    let bounds = bounds_of(cx, window, "terminal-grid");
    let rows = f32::from(session.size().1);
    let track_foot = bounds.top() + px(6. + 18. * rows);
    let grab = point(bounds.right() - px(3.), track_foot - px(4.));
    let pulled = point(grab.x, grab.y - px(150.));
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(grab, MouseButton::Left, gpui::Modifiers::default());
    visual.simulate_mouse_move(pulled, Some(MouseButton::Left), gpui::Modifiers::default());
    visual.simulate_mouse_up(pulled, MouseButton::Left, gpui::Modifiers::default());
    drop(visual);
    cx.run_until_parked();

    wait_for_signal(cx, "the view to leave the bottom", |_| {
        session.screen().display_offset > 0
    });
    let screen = session.screen();
    assert!(
        screen.display_offset < screen.history,
        "the view stays in the history: {} of {}",
        screen.display_offset,
        screen.history
    );
}

#[ui_kit::gpui::test]
fn shift_page_up_and_down_turn_the_history_by_a_page(cx: &mut TestAppContext) {
    let (window, session) = stand_in_in_terminal(cx, "wheel-page", &["lines"]);
    wait_for_signal(cx, "the history", |_| session.screen().history > 0);
    // A click on the grid gives the terminal the focus that the keys go to.
    click(cx, window, "terminal-grid");
    let rows = usize::from(session.size().1);

    press(cx, window, "shift-pageup");
    wait_for_signal(cx, "the page up", |_| {
        session.screen().display_offset == rows
    });
    press(cx, window, "shift-pagedown");
    wait_for_signal(cx, "the page down", |_| {
        session.screen().display_offset == 0
    });
}
