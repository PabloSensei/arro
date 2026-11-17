//! A helper is a process of the IDE: it must not outlive the session that started it,
//! and a completion of another thread must reach it through the same pipes.
//!
//! The stand-in is built with this crate, so the test needs no runtime.

use std::time::{Duration, Instant};

use repl::{COMPLETION_TIMEOUT, Launch, Session};

const FAKE: &str = env!("CARGO_BIN_EXE_repl-fake-helper");

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

#[test]
fn a_completion_reaches_the_helper_and_its_answer_comes_back() {
    let session = Session::start(Launch::new(FAKE)).expect("the stand-in helper starts");
    let source = session.completion_source();

    let items = source
        .complete("\"abc\".subs", 10, COMPLETION_TIMEOUT)
        .expect("the helper answers");

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text, "substring(");
    assert!(items[0].matches_type);
}
