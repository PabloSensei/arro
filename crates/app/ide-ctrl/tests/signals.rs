#![cfg(windows)]

//! Stopping real processes through the helper. Lives here because a test of this package is
//! the one place cargo builds the helper for.

use std::io::Read;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use proc_sup::{ProcessClass, ProcessSpec};

const GRACE: Duration = Duration::from_secs(5);

fn ping() -> ProcessSpec {
    ProcessSpec::new("ping.exe")
        .args(["-n", "120", "127.0.0.1"])
        .ctrl_helper(env!("CARGO_BIN_EXE_ide-ctrl"))
}

/// Ping exits by itself on Ctrl+C, so an exit inside the grace period proves the signal
/// arrived: a process ended by force would have waited out the whole of it.
#[test]
fn ctrl_c_stops_a_process_without_force() {
    let mut process = ping().start().unwrap();
    let start = Instant::now();

    process.shut_down(GRACE).unwrap();

    assert!(start.elapsed() < GRACE);
    assert!(process.tree().unwrap().is_empty());
}

/// Ping prints its statistics on Ctrl+Break and goes on, much like a JVM with a thread dump.
#[test]
fn ctrl_break_reports_and_leaves_the_process_running() {
    let mut process = ping().start().unwrap();
    let mut stdout = process.take_stdout().unwrap();
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).unwrap();
        output.len()
    });
    sleep(Duration::from_millis(1500));

    process.thread_dump().unwrap();

    sleep(Duration::from_millis(200));
    assert!(process.try_wait().unwrap().is_none());
    process.stop().unwrap();
    // Two reply lines are about 100 bytes, the statistics take more than twice that.
    assert!(reader.join().unwrap() > 300);
}

#[test]
fn a_client_is_not_in_a_job_and_ends_alone() {
    let mut client = ping().class(ProcessClass::Client).start().unwrap();

    assert_eq!(client.tree().unwrap(), [client.pid()]);
    client.stop().unwrap();
    assert!(client.try_wait().unwrap().is_some());
}

#[test]
fn a_process_that_cannot_be_asked_is_ended_by_force() {
    let mut process = ping().ctrl_helper("no-such-helper.exe").start().unwrap();
    assert!(process.interrupt().is_err());

    process.shut_down(GRACE).unwrap();

    assert!(process.tree().unwrap().is_empty());
}
