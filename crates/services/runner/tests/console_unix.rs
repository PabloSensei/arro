//! A console over a real Unix shell: output, the order of the steps and a stop.
#![cfg(unix)]

use std::time::Duration;

use proc_sup::ProcessSpec;
use runner::{Console, ConsoleEvent, RunPlan, Step};

const PATIENCE: Duration = Duration::from_secs(10);

fn shell(line: &str) -> ProcessSpec {
    ProcessSpec::new("sh").args(["-c", line])
}

fn plan(before: Option<ProcessSpec>, program: ProcessSpec) -> RunPlan {
    RunPlan {
        before,
        program,
        grace: Duration::ZERO,
    }
}

/// The output of the whole plan and the event that ended it.
fn to_end(console: &Console) -> (String, Option<i32>, Step) {
    let mut text = String::new();
    loop {
        match console.events().recv_timeout(PATIENCE).unwrap() {
            ConsoleEvent::Output { text: part, .. } => text.push_str(&part),
            ConsoleEvent::Exited { code, step } => return (text, code, step),
            ConsoleEvent::Started { .. } | ConsoleEvent::Failed { .. } => {}
            ConsoleEvent::Closed => panic!("the console closed before the program ended"),
        }
    }
}

#[test]
fn output_and_exit_code_arrive_in_order() {
    let console = Console::start(plan(None, shell("echo hello; exit 3")));

    let (text, code, step) = to_end(&console);

    assert_eq!(text.trim(), "hello");
    assert_eq!((code, step), (Some(3), Step::Program));
}

#[test]
fn the_program_follows_a_step_that_succeeded() {
    let console = Console::start(plan(Some(shell("echo first")), shell("echo second")));

    let (text, code, _) = to_end(&console);

    assert_eq!(
        text.split_whitespace().collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert_eq!(code, Some(0));
}

#[test]
fn a_step_that_failed_keeps_the_program_from_starting() {
    let console = Console::start(plan(Some(shell("exit 7")), shell("echo second")));

    let (text, code, step) = to_end(&console);

    assert_eq!(text, "");
    assert_eq!((code, step), (Some(7), Step::Before));
}

#[test]
fn a_marker_in_the_output_is_seen_without_taking_events() {
    let console = Console::start(plan(None, shell("echo ready-marker")));

    assert!(console.wait_for_output("ready-marker", PATIENCE, &base::Cancel::new()));

    let (text, code, _) = to_end(&console);
    assert!(text.contains("ready-marker"), "{text}");
    assert_eq!(code, Some(0));
}

#[test]
fn stop_ends_a_long_program() {
    let console = Console::start(plan(None, shell("sleep 60")));
    std::thread::sleep(Duration::from_millis(200));

    console.stop();

    let (_, code, step) = to_end(&console);
    assert_eq!(step, Step::Program);
    assert_ne!(code, Some(0));
}
