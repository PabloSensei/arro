#![cfg(windows)]

use std::io::{Read, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use base::Cancel;
use platform::{Job, ProcessWaiter};
use proc_sup::{CaptureError, Process, ProcessSpec, capture};

const PATIENCE: Duration = Duration::from_secs(10);

fn shell(command: &str) -> ProcessSpec {
    ProcessSpec::new("cmd.exe").args(["/d", "/c", command])
}

/// A shell that starts `ping` at once and waits for it: a tree of two.
fn tree_of_two() -> Process {
    let process = shell("ping -n 60 127.0.0.1").start().unwrap();
    wait_until(|| process.tree().unwrap().len() >= 2);
    process
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < PATIENCE, "condition not met in time");
        sleep(Duration::from_millis(10));
    }
}

/// Opens the processes of a tree for waiting. A process that is gone already has
/// no waiter, which is what the caller wants to see.
fn waiters_of(tree: &[u32]) -> Vec<ProcessWaiter> {
    tree.iter()
        .filter_map(|&pid| ProcessWaiter::open(pid).unwrap())
        .collect()
}

/// Waits for every process to end within one patience. Waiting on handles rather
/// than on ids leaves no room for a process that took a freed id in the meantime.
fn wait_for_all(waiters: &[ProcessWaiter]) {
    let start = Instant::now();
    for waiter in waiters {
        let left = PATIENCE.saturating_sub(start.elapsed());
        assert!(
            waiter.wait(left).unwrap(),
            "process {} outlived the tree",
            waiter.id()
        );
    }
}

#[test]
fn output_and_exit_code_reach_the_owner() {
    let mut process = shell("echo hello& exit /b 7").start().unwrap();
    let mut output = String::new();

    process
        .take_stdout()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();

    assert_eq!(output.trim(), "hello");
    assert_eq!(process.wait().unwrap().code(), Some(7));
}

#[test]
fn stdin_reaches_the_child() {
    let mut process = ProcessSpec::new("sort.exe").start().unwrap();
    let mut stdin = process.take_stdin().unwrap();
    let mut stdout = process.take_stdout().unwrap();

    stdin.write_all(b"b\r\na\r\n").unwrap();
    drop(stdin);

    let mut output = String::new();
    stdout.read_to_string(&mut output).unwrap();
    assert_eq!(output, "a\r\nb\r\n");
}

#[test]
fn working_directory_and_environment_are_passed() {
    let directory = test_support::scratch("proc-sup-cwd");
    let mut process = shell("echo %CD%^|%PROC_SUP_TEST%")
        .cwd(&directory)
        .env("PROC_SUP_TEST", "value")
        .env_remove("PATHEXT")
        .start()
        .unwrap();
    let mut output = String::new();

    process
        .take_stdout()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();

    let expected = directory.to_string_lossy();
    let (cwd, value) = output.trim().split_once('|').unwrap();
    assert_eq!(cwd.trim_end_matches('\\'), expected.trim_end_matches('\\'));
    assert_eq!(value, "value");
}

#[test]
fn a_program_that_is_missing_is_an_error() {
    assert!(
        ProcessSpec::new("no-such-program-here.exe")
            .start()
            .is_err()
    );
}

#[test]
fn stop_ends_the_whole_tree() {
    let mut process = tree_of_two();
    let tree = process.tree().unwrap();
    let waiters = waiters_of(&tree);

    process.stop().unwrap();

    wait_until(|| process.tree().unwrap().is_empty());
    wait_for_all(&waiters);
}

#[test]
fn stop_after_the_process_exited_is_harmless() {
    let mut process = shell("exit /b 0").start().unwrap();
    process.wait().unwrap();

    assert_eq!(process.stop().unwrap().code(), Some(0));
}

#[test]
fn dropping_the_owner_ends_the_whole_tree() {
    let process = tree_of_two();
    let tree = process.tree().unwrap();
    let waiters = waiters_of(&tree);

    drop(process);

    wait_for_all(&waiters);
}

#[test]
fn a_grandchild_outlives_its_parent_but_not_the_job() {
    // The shell starts `ping` in the background and exits at once.
    let mut process = shell("start /b ping -n 60 127.0.0.1").start().unwrap();
    process.wait().unwrap();
    wait_until(|| !process.tree().unwrap().is_empty());
    let tree = process.tree().unwrap();
    let waiters = waiters_of(&tree);

    drop(process);

    wait_for_all(&waiters);
}

#[test]
fn capture_returns_output_and_status() {
    let captured = capture(&shell("echo out& echo err 1>&2& exit /b 3"), &Cancel::new()).unwrap();

    assert_eq!(captured.status.code(), Some(3));
    assert_eq!(captured.stdout.trim(), "out");
    assert_eq!(captured.stderr.trim(), "err");
}

#[test]
fn capture_does_not_wait_for_a_pipe_held_by_a_grandchild() {
    let start = Instant::now();

    capture(&shell("start /b ping -n 30 127.0.0.1"), &Cancel::new()).unwrap();

    assert!(start.elapsed() < Duration::from_secs(20));
}

#[test]
fn capture_ends_on_cancellation() {
    let cancel = Cancel::new();
    let later = cancel.clone();
    let canceller = std::thread::spawn(move || {
        sleep(Duration::from_millis(100));
        later.cancel();
    });
    let spec = ProcessSpec::new("ping.exe").args(["-n", "120", "127.0.0.1"]);
    // No helper in this test binary, so the process goes straight to force.
    let result = capture(&spec.ctrl_helper("no-such-helper.exe"), &cancel);

    canceller.join().unwrap();
    assert!(matches!(result, Err(CaptureError::Cancelled)));
}

#[test]
fn a_job_lists_and_ends_a_process_given_to_it() {
    let job = Job::new().unwrap();
    let mut child = std::process::Command::new("ping.exe")
        .args(["-n", "60", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();

    job.assign(&child).unwrap();

    assert_eq!(job.process_ids().unwrap(), [child.id()]);
    job.terminate(1).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(1));
}
