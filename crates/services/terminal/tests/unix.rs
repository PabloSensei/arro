//! A Unix pseudoterminal with a real program inside it.
#![cfg(unix)]

use std::time::Duration;

use terminal::{ShiftEnter, SpawnSpec, TerminalSession, allowlisted};

const WAIT: Duration = Duration::from_secs(15);

fn spawn(command_line: &str) -> TerminalSession {
    TerminalSession::spawn(&SpawnSpec {
        command_line: command_line.to_string(),
        cwd: None,
        env: allowlisted(),
        cols: 80,
        rows: 24,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn")
}

#[test]
fn output_reaches_the_screen_and_the_exit_code_the_owner() {
    let session = spawn("printf 'hello-from-pty\\n'; exit 3");

    let printed = session.wait_for(WAIT, |screen| screen.contains("hello-from-pty"));
    assert!(printed, "{:?}", session.screen().lines());
    assert!(session.wait_closed(WAIT));
    assert_eq!(session.exit_code(), Some(3));
}

#[test]
fn the_program_sees_the_size_of_the_terminal_and_a_resize() {
    let session = spawn("PS1='' sh -i");

    session
        .send(b"stty size\n".to_vec())
        .expect("input reaches the shell");
    let seen = session.wait_for(WAIT, |screen| screen.contains("24 80"));
    assert!(seen, "{:?}", session.screen().lines());

    session.resize(100, 30);
    session
        .send(b"stty size\n".to_vec())
        .expect("input reaches the shell");
    let resized = session.wait_for(WAIT, |screen| screen.contains("30 100"));
    assert!(resized, "{:?}", session.screen().lines());
}
