//! `agent-try --mock` inside a terminal of ours, driven like a person at a keyboard. Off by
//! default: it needs the installed harnesses.

use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use agent_core::Harness;
use agent_pty::input_box_visible;
use agent_testkit::MOCK_MODEL;
use terminal::{
    Key, KeyEvent, Screen, ShiftEnter, SpawnSpec, TerminalSession, allowlisted, command_line,
};

const WAIT: Duration = Duration::from_secs(60);

fn start(name: &str) -> TerminalSession {
    let program = Path::new(env!("CARGO_BIN_EXE_agent-try"));
    let arguments = [name.to_string(), "--mock".to_string()];
    TerminalSession::spawn(&SpawnSpec {
        command_line: command_line(program, &arguments),
        cwd: None,
        env: allowlisted(),
        cols: 120,
        rows: 40,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn")
}

fn submit(session: &TerminalSession, text: &str, enter_delay: Duration) {
    for ch in text.chars() {
        session.key(KeyEvent::plain(Key::Char(ch))).expect("key");
    }
    sleep(enter_delay);
    session.key(KeyEvent::plain(Key::Enter)).expect("enter");
}

fn wait_screen(session: &TerminalSession, check: impl Fn(&Screen) -> bool) {
    let seen = session.wait_for(WAIT, check);
    assert!(seen, "{}", session.screen().lines().join("\n"));
}

fn turn_and_exit(name: &str, harness: Harness, marker: Option<&str>, quit: &str, delay: Duration) {
    let session = start(name);
    wait_screen(&session, |screen| {
        input_box_visible(harness, &screen.lines(), true, marker)
    });
    // A TUI drops keys for a moment after it first shows the box.
    sleep(Duration::from_secs(2));

    submit(&session, "hello", delay);
    wait_screen(&session, |screen| screen.contains("MOCK_REPLY"));
    sleep(Duration::from_secs(1));
    submit(&session, quit, delay);

    wait_screen(&session, |screen| screen.contains("tool calls: 0"));
    assert!(session.wait_closed(WAIT));
    assert_eq!(session.exit_code(), Some(0));
}

#[test]
#[ignore = "needs an installed claude"]
fn claude_runs_in_the_console_host() {
    let delay = Duration::from_millis(300);

    turn_and_exit("claude", Harness::Claude, None, "/exit", delay);
}

#[test]
#[ignore = "needs an installed codex"]
fn codex_runs_in_the_console_host() {
    let delay = Duration::from_millis(1500);

    turn_and_exit("codex", Harness::Codex, Some(MOCK_MODEL), "/quit", delay);
}
