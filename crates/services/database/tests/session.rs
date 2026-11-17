//! A helper is a process of the IDE: it must not outlive the session that started it.
//!
//! The stand-in is built with this crate, so the test needs no driver and no database.

use std::time::{Duration, Instant};

use database::{Launch, Session};

const FAKE: &str = env!("CARGO_BIN_EXE_database-fake-helper");

fn running(pid: u32) -> bool {
    platform::process_tree()
        .iter()
        .any(|process| process.pid == pid)
}

#[test]
fn a_dropped_session_stops_the_helper() {
    let session = Session::start(Launch::new(FAKE)).expect("the stand-in helper starts");
    let pid = session.process_id().expect("a helper has a process id");
    assert!(running(pid), "the helper runs");

    drop(session);

    let started = Instant::now();
    while running(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the helper outlives its session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
