//! The wheel and the arrow keys in a pseudoconsole: what a full-screen program receives, and
//! where the wheel turns the history instead.
#![cfg(windows)]

use std::path::Path;
use std::thread;
use std::time::Duration;

use terminal::{ShiftEnter, SpawnSpec, TerminalSession, allowlisted, command_line};

const WAIT: Duration = Duration::from_secs(15);
/// Long enough for the program to print what it received, and for the console to settle.
const SETTLE: Duration = Duration::from_millis(1500);

fn spawn(command_line: String) -> TerminalSession {
    TerminalSession::spawn(&SpawnSpec {
        command_line,
        cwd: None,
        env: allowlisted(),
        cols: 80,
        rows: 24,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn")
}

/// The probe program with the modes its arguments name, once its screen shows READY.
fn tui(modes: &[&str]) -> TerminalSession {
    let program = Path::new(env!("CARGO_BIN_EXE_term-probe"));
    let mut arguments = vec!["tui".to_string()];
    arguments.extend(modes.iter().map(|mode| mode.to_string()));
    let session = spawn(command_line(program, &arguments));
    assert!(session.wait_for(WAIT, |screen| screen.contains("READY")));
    session
}

/// Everything the program printed about its input, one `GOT` line per read joined in order, with
/// the escapes the program shows as `^[` turned back into the bytes they are.
fn received(session: &TerminalSession) -> String {
    thread::sleep(SETTLE);
    session
        .screen()
        .lines()
        .iter()
        .filter_map(|line| line.strip_prefix("GOT "))
        .collect::<String>()
        .replace("^[", "\x1b")
}

#[test]
fn a_wheel_turn_reaches_a_program_that_asked_for_sgr_mouse_reports() {
    let session = tui(&["alt", "mouse", "sgr"]);
    session.wheel(3, (4, 2), false).expect("wheel");
    let input = received(&session);
    assert_eq!(input.matches("\x1b[<64;5;3M").count(), 1, "{input:?}");
    assert!(
        !input.contains("\x1b[A"),
        "a mouse report is not an arrow: {input:?}"
    );
}

#[test]
fn a_wheel_turn_downwards_is_button_65_in_x10_reports() {
    let session = tui(&["alt", "mouse"]);
    // Six lines are two notches; the report of button 65 carries the cell after a 32 offset.
    session.wheel(-6, (4, 2), false).expect("wheel");
    let input = received(&session);
    assert_eq!(input.matches("\x1b[Ma%#").count(), 2, "{input:?}");
}

#[test]
fn the_alternate_screen_without_mouse_reports_gets_the_arrow_keys() {
    let session = tui(&["alt"]);
    session.wheel(3, (4, 2), false).expect("wheel");
    let input = received(&session);
    assert_eq!(input.matches("\x1b[A").count(), 3, "{input:?}");
    assert!(
        !input.contains("\x1b[<"),
        "no mouse report without mouse reports: {input:?}"
    );
}

#[test]
fn the_arrow_keys_of_the_wheel_follow_the_cursor_mode() {
    let session = tui(&["alt", "cursor"]);
    session.wheel(-3, (4, 2), false).expect("wheel");
    let input = received(&session);
    assert_eq!(input.matches("\x1bOB").count(), 3, "{input:?}");
}

#[test]
fn shift_turns_the_history_of_a_program_that_asked_for_the_mouse() {
    // The lines are printed on the main screen, where the history is, before the mouse is taken.
    let session = tui(&["lines", "mouse", "sgr"]);
    assert!(session.wait_for(WAIT, |screen| screen.history > 0));
    assert_eq!(session.screen().display_offset, 0);
    session.wheel(3, (4, 2), true).expect("wheel");
    assert!(session.wait_for(WAIT, |screen| screen.display_offset == 3));
    // The program's lines show only at the bottom of the screen, which the view has left.
    session.scroll_to_bottom();
    assert!(
        !received(&session).contains("\x1b[<"),
        "Shift keeps the wheel for the history"
    );
    session.wheel(3, (4, 2), false).expect("wheel");
    assert!(received(&session).contains("\x1b[<64;5;3M"));
}

#[test]
fn the_wheel_of_the_main_screen_turns_the_history() {
    let session = tui(&["lines"]);
    assert!(session.wait_for(WAIT, |screen| screen.history > 0));
    session.wheel(3, (4, 2), false).expect("wheel");
    assert!(session.wait_for(WAIT, |screen| screen.display_offset == 3));
}

#[test]
fn an_erase_of_the_scrollback_drops_the_history() {
    // What a repaint that erases the scrollback does: the view keeps none of the lines it had.
    // READY comes after the erase, so the history is already gone when tui() returns.
    let session = tui(&["lines", "erase"]);
    assert_eq!(session.screen().history, 0);
}

#[test]
fn the_alternate_screen_keeps_no_history_to_scroll() {
    // The lines of the main screen stay in its history; the alternate screen has none of its own.
    let session = tui(&["lines", "alt"]);
    assert!(session.wait_for(WAIT, |screen| screen.contains("READY")));
    session.scroll(3);
    thread::sleep(SETTLE);
    assert_eq!(session.screen().display_offset, 0);
}

#[test]
fn new_output_does_not_pull_a_scrolled_view_down() {
    let session = tui(&["lines"]);
    assert!(session.wait_for(WAIT, |screen| screen.history > 0));
    session.scroll(3);
    assert!(session.wait_for(WAIT, |screen| screen.display_offset == 3));
    // Each read of the probe prints a line, and the view stays where the user left it.
    session.send(b"x\r".to_vec()).expect("send");
    assert!(session.wait_for(WAIT, |screen| screen.display_offset > 3));
}
