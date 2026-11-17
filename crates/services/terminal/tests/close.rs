//! Alone in its test binary: it counts the children of this process, conhost among them.
#![cfg(windows)]

use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use terminal::{Key, KeyEvent, ShiftEnter, SpawnSpec, TerminalSession, allowlisted};

const PROBE: &str = env!("CARGO_BIN_EXE_term-probe");
const WAIT: Duration = Duration::from_secs(15);

fn children() -> Vec<String> {
    let parent = std::process::id().to_string();
    let output = Command::new(PROBE).args(["children", &parent]).output();
    let output = output.expect("probe").stdout;
    let lines = String::from_utf8(output).expect("utf-8");
    lines.lines().map(str::to_string).collect()
}

fn wait_for(check: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while !check() {
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(50));
    }
    true
}

#[test]
fn closing_a_session_leaves_no_process_behind() {
    let ready = test_support::scratch("term-close").join("ready");
    let session = TerminalSession::spawn(&SpawnSpec {
        command_line: "cmd.exe /q".to_string(),
        cwd: None,
        env: allowlisted(),
        cols: 100,
        rows: 30,
        shift_enter: ShiftEnter::default(),
    })
    .expect("spawn");
    assert!(session.wait_for(WAIT, |screen| screen.contains(">")));
    let command = format!("start /b \"\" \"{PROBE}\" sleep \"{}\"\r", ready.display());
    session.send(command.into_bytes()).expect("input");
    session.key(KeyEvent::plain(Key::Enter)).expect("enter");
    assert!(
        wait_for(|| ready.exists()),
        "{:?}",
        session.screen().lines()
    );
    assert!(!children().is_empty());

    drop(session);

    let left = wait_for(|| children().is_empty());
    assert!(left, "still running: {:?}", children());
}
