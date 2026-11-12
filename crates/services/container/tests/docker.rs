//! The panel's view of Docker on a fake CLI: a container started, seen, its log live,
//! stopped, the daemon going down and coming back. No daemon is needed.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use container::{ContainerState, DaemonState, DockerCli, DockerSnapshot, DockerWatch};
use runner::{Console, ConsoleEvent, RunPlan};
use serde_json::{Value, json};

const FAKE: &str = env!("CARGO_BIN_EXE_container-fake-docker");
const PATIENCE: Duration = Duration::from_secs(20);

fn state_file() -> PathBuf {
    let file = test_support::scratch("fake-docker").join("state.json");
    let state = json!({
        "up": true,
        "containers": [],
        "images": [{"id": "sha256:74cc", "repository": "hello-world", "tag": "latest"}],
        "logs": {"fake0001": ["/docker-entrypoint.sh: Configuration complete; ready for start up"]},
    });
    fs::write(&file, state.to_string()).unwrap();
    // The fake reads its daemon from here, the CLI passes the environment on.
    unsafe { std::env::set_var("FAKE_DOCKER_STATE", &file) };
    file
}

fn set_up(file: &PathBuf, up: bool) {
    let mut state: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    state["up"] = json!(up);
    fs::write(file, state.to_string()).unwrap();
}

/// The next snapshot that `wanted` accepts.
fn wait_for(watch: &DockerWatch, wanted: impl Fn(&DockerSnapshot) -> bool) -> DockerSnapshot {
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < PATIENCE, "no such snapshot came");
        if let Ok(snapshot) = watch.snapshots().recv_timeout(Duration::from_millis(100))
            && wanted(&snapshot)
        {
            return snapshot;
        }
    }
}

fn read_until(console: &Console, log: &mut String, done: impl Fn(&str) -> bool) {
    let start = Instant::now();
    while !done(log) {
        assert!(start.elapsed() < PATIENCE, "the log stopped at:\n{log}");
        match console.events().recv_timeout(Duration::from_millis(100)) {
            Ok(ConsoleEvent::Output { text, .. }) => log.push_str(&text),
            Ok(ConsoleEvent::Exited { .. }) => log.push_str("<exited>"),
            _ => {}
        }
    }
}

#[test]
fn a_container_runs_logs_live_stops_and_survives_the_daemon_going_away() {
    let file = state_file();
    let cli = DockerCli::new(FAKE).with_context(Some("desktop-linux".into()));
    let watch = DockerWatch::start(cli.clone(), Duration::from_millis(100));

    let first = wait_for(&watch, |snapshot| {
        matches!(snapshot.daemon, DaemonState::Up { .. })
    });
    assert_eq!(first.contexts.len(), 2);
    assert!(
        first
            .contexts
            .iter()
            .any(|context| context.current && context.name == "desktop-linux")
    );
    assert!(first.containers.is_empty());
    assert_eq!(first.images[0].repository, "hello-world");

    // Started from the panel, seen in the tree.
    let id = cli
        .run_detached("nginx", &["--publish", "8080:80"])
        .unwrap();
    watch.refresh();
    let running = wait_for(&watch, |snapshot| !snapshot.containers.is_empty());
    assert_eq!(running.containers[0].id, id);
    assert_eq!(running.containers[0].state, ContainerState::Running);

    // The log: the lines so far, then live ones.
    let logs = Console::start(RunPlan {
        before: None,
        program: cli.logs(&id),
        grace: Duration::from_secs(1),
    });
    let mut log = String::new();
    read_until(&logs, &mut log, |log| log.contains("tick 3"));
    assert!(
        log.starts_with("/docker-entrypoint.sh: Configuration complete"),
        "{log}"
    );

    // Stop from the panel: the container exits and its log ends with it.
    cli.stop(&id).unwrap();
    read_until(&logs, &mut log, |log| log.contains("<exited>"));
    watch.refresh();
    wait_for(&watch, |snapshot| {
        snapshot
            .containers
            .first()
            .is_some_and(|container| container.state == ContainerState::Exited)
    });

    // Docker Desktop goes off: a state, not an error, and the contexts stay.
    set_up(&file, false);
    let down = wait_for(&watch, |snapshot| {
        matches!(snapshot.daemon, DaemonState::Down { .. })
    });
    let DaemonState::Down { reason } = &down.daemon else {
        unreachable!()
    };
    assert!(reason.contains("failed to connect"), "{reason}");
    assert!(down.containers.is_empty());
    assert_eq!(down.problem, None);
    assert_eq!(down.contexts.len(), 2);

    // And comes back: everything is there again.
    set_up(&file, true);
    let back = wait_for(&watch, |snapshot| !snapshot.containers.is_empty());
    assert_eq!(back.containers[0].id, id);

    cli.remove(&id).unwrap();
    watch.refresh();
    wait_for(&watch, |snapshot| {
        matches!(snapshot.daemon, DaemonState::Up { .. }) && snapshot.containers.is_empty()
    });
    drop(watch);
    let _ = fs::remove_file(file);
}

#[test]
fn no_cli_is_a_state_too() {
    let cli = DockerCli::new("C:/nowhere/docker.exe");

    assert_eq!(cli.daemon(), DaemonState::NoCli);
}

/// The CLI of this machine, whatever its daemon does. Not in CI: needs Docker installed.
#[test]
#[ignore = "needs the docker CLI"]
fn the_real_cli_reports_its_state() {
    let cli = DockerCli::find().expect("docker CLI");

    let snapshot = container::take(&cli);

    eprintln!("{:?}, contexts {:?}", snapshot.daemon, snapshot.contexts);
    assert_ne!(snapshot.daemon, DaemonState::NoCli);
    assert!(!snapshot.contexts.is_empty());
    assert_eq!(snapshot.problem, None);
}
