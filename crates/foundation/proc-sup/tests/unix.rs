//! A process group of a real Unix, and waiting for a process by its id.
#![cfg(unix)]

use std::thread::sleep;
use std::time::{Duration, Instant};

use platform::ProcessWaiter;
use proc_sup::ProcessSpec;

const PATIENCE: Duration = Duration::from_secs(10);

fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < PATIENCE, "condition not met in time");
        sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_tree_of_two_is_stopped_as_one_group() {
    let mut process = ProcessSpec::new("sh")
        .args(["-c", "sleep 60 & wait"])
        .start()
        .unwrap();
    wait_until(|| process.tree().unwrap().len() >= 2);

    process.stop().unwrap();

    assert!(process.tree().unwrap().is_empty());
}

#[test]
fn a_process_is_waited_for_by_its_id() {
    let mut process = ProcessSpec::new("sleep").args(["60"]).start().unwrap();
    let waiter = ProcessWaiter::open(process.pid()).unwrap().unwrap();

    assert!(!waiter.wait(Duration::from_millis(50)).unwrap());
    process.stop().unwrap();

    assert!(waiter.wait(PATIENCE).unwrap());
}
