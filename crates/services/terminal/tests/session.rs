//! Real programs in a real pseudoconsole.

use std::path::Path;
use std::time::Duration;

use terminal::{
    Key, KeyEvent, Mods, ShiftEnter, SpawnSpec, TerminalEvent, TerminalSession, allowlisted,
    command_line,
};

const WAIT: Duration = Duration::from_secs(15);

fn spawn(command_line: String) -> TerminalSession {
    TerminalSession::spawn(&SpawnSpec {
        command_line,
        cwd: None,
        env: allowlisted(),
        cols: 100,
        rows: 30,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn")
}

fn cmd() -> TerminalSession {
    let session = spawn("cmd.exe /q".to_string());
    assert!(session.wait_for(WAIT, |screen| screen.contains(">")));
    session
}

fn probe_with(arguments: &[String]) -> TerminalSession {
    let program = Path::new(env!("CARGO_BIN_EXE_term-probe"));
    spawn(command_line(program, arguments))
}

fn probe(mode: &str) -> TerminalSession {
    probe_with(&[mode.to_string()])
}

fn type_text(session: &TerminalSession, text: &str) {
    for ch in text.chars() {
        session.key(KeyEvent::plain(Key::Char(ch))).expect("key");
    }
}

fn run_command(session: &TerminalSession, text: &str) {
    type_text(session, text);
    session.key(KeyEvent::plain(Key::Enter)).expect("enter");
}

#[test]
fn a_session_can_be_hosted_in_a_console() {
    let session = probe_with(&["attach".to_string(), "cmd.exe /q".to_string()]);
    assert!(session.wait_for(WAIT, |screen| screen.contains(">")));

    run_command(&session, "echo nested-output");
    let nested = session.wait_for(WAIT, |screen| {
        screen.lines().iter().any(|line| line == "nested-output")
    });
    assert!(nested, "{:?}", session.screen().lines());

    run_command(&session, "exit");
    assert!(session.wait_closed(WAIT));
    assert_eq!(session.exit_code(), Some(0));
}

#[test]
fn input_reaches_the_program_and_output_the_screen() {
    let session = cmd();

    run_command(&session, "echo привет-terminal");

    let printed = session.wait_for(WAIT, |screen| {
        screen.lines().iter().any(|line| line == "привет-terminal")
    });
    assert!(printed, "{:?}", session.screen().lines());
}

#[test]
fn the_size_of_the_program_follows_a_resize() {
    let session = cmd();

    session.resize(70, 20);
    run_command(&session, "mode con");

    let seen = session.wait_for(WAIT, |screen| screen.cols == 70 && screen.contains("70"));
    assert!(seen, "{:?}", session.screen().lines());
}

#[test]
fn ctrl_c_stops_the_running_program_and_not_the_shell() {
    let session = cmd();
    run_command(&session, "ping -n 300 127.0.0.1");
    std::thread::sleep(Duration::from_secs(1));

    let ctrl_c = KeyEvent::new(Key::Char('c'), Mods::CTRL);
    session.key(ctrl_c).expect("key");
    run_command(&session, "echo after-ctrl-c");

    let printed = session.wait_for(WAIT, |screen| {
        screen.lines().iter().any(|line| line == "after-ctrl-c")
    });
    assert!(printed, "{:?}", session.screen().lines());
}

#[test]
fn exit_code_and_end_of_stream_arrive() {
    let session = cmd();
    let events = session.events();

    run_command(&session, "exit 7");

    assert!(session.wait_closed(WAIT));
    assert_eq!(session.exit_code(), Some(7));
    let exited = events
        .try_iter()
        .any(|event| event == TerminalEvent::Exited(Some(7)));
    assert!(exited);
    assert_eq!(session.screen().exit_code, Some(7));
}

#[test]
fn a_megabyte_paste_arrives_intact() {
    let session = probe("lines");
    let line = format!("{}привет\n", "x".repeat(88));
    let count = 1024 * 1024 / line.len();
    let text = line.repeat(count) + "END\n";
    let (mut bytes, mut hash) = (0u64, 0xcbf29ce484222325u64);
    for line in text.lines().filter(|line| *line != "END") {
        bytes += line.len() as u64;
        for byte in line.bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }

    session.paste(&text).expect("paste");

    let expected = format!("TOTAL {bytes} {hash:016x}");
    let arrived = session.wait_for(Duration::from_secs(120), |screen| {
        screen.contains(&expected)
    });
    assert!(arrived, "{:?}", session.screen().lines());
}

#[test]
fn modifiers_reach_a_console_program() {
    let session = probe("keys");
    assert!(session.wait_for(WAIT, |screen| screen.contains("READY")));
    assert!(session.wait_for(WAIT, |_| session.modes().win32_input));

    let presses = [
        (KeyEvent::new(Key::Enter, Mods::SHIFT), "KEY 13 13 0x10"),
        (KeyEvent::new(Key::Backspace, Mods::CTRL), "KEY 8 127 0x8"),
        (KeyEvent::plain(Key::Backspace), "KEY 8 8 0x0"),
        (KeyEvent::new(Key::Left, Mods::ALT), "KEY 37 0 0x102"),
        (KeyEvent::plain(Key::F(10)), "KEY 121 0 0x0"),
    ];
    for (event, expected) in presses {
        session.key(event).expect("key");
        let seen = session.wait_for(WAIT, |screen| screen.contains(expected));
        assert!(seen, "{expected}: {:?}", session.screen().lines());
    }
}

#[test]
fn characters_without_a_key_arrive_as_themselves() {
    let session = probe("keys");
    assert!(session.wait_for(WAIT, |screen| screen.contains("READY")));
    assert!(session.wait_for(WAIT, |_| session.modes().win32_input));

    session.key(KeyEvent::plain(Key::Char('€'))).expect("key");
    session.paste_lines("{").expect("paste");

    for expected in ["KEY 0 8364 0x0", "KEY 0 123 0x0"] {
        let seen = session.wait_for(WAIT, |screen| screen.contains(expected));
        assert!(seen, "{expected}: {:?}", session.screen().lines());
    }
}
