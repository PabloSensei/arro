//! The end of a session is a fact of the last snapshot, not of a flag beside it: a panel
//! that saw the final version has also seen that the program ended.
#![cfg(windows)]

use std::time::Duration;

use terminal::{ShiftEnter, SpawnSpec, TerminalSession, allowlisted};

const WAIT: Duration = Duration::from_secs(15);

#[test]
fn the_last_snapshot_of_a_finished_program_says_it_is_closed() {
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

    let started = session.screen().version;
    assert!(!session.is_closed(), "a running shell is not closed");

    session.send(b"exit 3\r".to_vec()).expect("input");
    assert!(session.wait_closed(WAIT));

    let last = session.screen();
    assert!(
        last.closed,
        "the last published frame carries the end of the program"
    );
    assert!(
        last.version > started,
        "closing is a new version: {} after {started}",
        last.version
    );
    assert_eq!(session.exit_code(), Some(3));
}
