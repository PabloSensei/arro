//! A CLI call that times out ends the process, not only the wait: a stand-in that never
//! answers must not outlive the test and hold its executable against the next build.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use container::{DaemonState, DockerCli};
use platform::ProcessWaiter;
use serde_json::{Value, json};

const FAKE: &str = env!("CARGO_BIN_EXE_container-fake-docker");
const PATIENCE: Duration = Duration::from_secs(20);

/// A state file for a stand-in that never answers, as a Docker Desktop that is stuck does.
fn hanging_state() -> std::path::PathBuf {
    let file = test_support::scratch("fake-docker-timeout").join("state.json");
    let state = json!({"up": true, "hang": true, "containers": [], "images": [], "logs": {}});
    fs::write(&file, state.to_string()).unwrap();
    // The fake reads its daemon from here, the CLI passes the environment on.
    unsafe { std::env::set_var("FAKE_DOCKER_STATE", &file) };
    file
}

/// The pid the stand-in writes before it starts hanging.
fn wait_for_pid(file: &Path) -> u32 {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < PATIENCE, "the stand-in wrote no pid");
        if let Ok(text) = fs::read_to_string(file)
            && let Ok(state) = serde_json::from_str::<Value>(&text)
            && let Some(pid) = state["pid"].as_u64()
        {
            return pid as u32;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_call_that_times_out_ends_the_stand_in() {
    let file = hanging_state();
    let cli = DockerCli::new(FAKE)
        .with_context(Some("desktop-linux".into()))
        .with_timeout(Duration::from_secs(5));

    // The call blocks by design, so it runs on its own thread while the test waits for the
    // pid the stand-in writes at its start. A timeout shorter than that start would test
    // nothing: there would be no process to look for.
    let called = std::thread::spawn(move || cli.daemon());
    let pid = wait_for_pid(&file);
    let daemon = called.join().expect("the call thread");

    let DaemonState::Down { reason } = daemon else {
        panic!("a CLI that does not answer is down: {daemon:?}");
    };
    assert!(reason.contains("did not answer"), "{reason}");

    let waiter = ProcessWaiter::open(pid).expect("the process table");
    if let Some(waiter) = waiter {
        assert!(
            waiter.wait(Duration::from_secs(10)).expect("a wait"),
            "the stand-in {pid} outlived the timeout"
        );
    }
}
