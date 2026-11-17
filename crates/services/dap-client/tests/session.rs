//! The session model against the mock adapter of `lang-fixture`: a stop on a
//! breakpoint, two frames, variables with lazy children, steps, continue and the end.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use dap_client::{BreakpointSpec, Command, DebugState, Launch, Snapshot};

const LIMIT: Duration = Duration::from_secs(10);

fn wait_for(
    snapshots: &Receiver<Arc<Snapshot>>,
    mut done: impl FnMut(&Snapshot) -> bool,
) -> Arc<Snapshot> {
    let started = std::time::Instant::now();
    loop {
        let left = LIMIT.checked_sub(started.elapsed()).unwrap_or_default();
        match snapshots.recv_timeout(left) {
            Ok(snapshot) => {
                if done(&snapshot) {
                    return snapshot;
                }
            }
            Err(RecvTimeoutError::Timeout) => panic!("the session did not get there in time"),
            Err(RecvTimeoutError::Disconnected) => panic!("the session ended too early"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_follows_the_mock_scenario() {
    let server = lang_fixture::MockDap::start().unwrap();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = channel();
    let launch = Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        breakpoints: vec![BreakpointSpec {
            file: PathBuf::from("C:/fixture/demo.sample"),
            line: 42,
            ..BreakpointSpec::default()
        }],
        watch: vec!["1+1".into()],
        ..Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));

    let stopped = wait_for(&incoming, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
    });
    assert_eq!(stopped.frames.len(), 2);
    // The adapter sent no source and line zero for the first frame; both stay usable.
    assert_eq!(stopped.frames[0].source, None);
    assert_eq!(stopped.frames[0].line, 0);
    assert_eq!(
        stopped.frames[1].source,
        Some(PathBuf::from("C:/fixture/demo.sample"))
    );
    assert_eq!(stopped.frames[1].line, 42);
    assert_eq!(stopped.threads.len(), 2);
    assert_eq!(stopped.scopes.len(), 2);
    assert_eq!(stopped.variables[&100][1].name, "items");
    assert_eq!(stopped.watch[0].value, "42");

    commands.send(Command::Expand(101)).unwrap();
    let expanded = wait_for(&incoming, |snapshot| {
        snapshot
            .variables
            .get(&101)
            .is_some_and(|items| items.len() == 2)
    });
    assert_eq!(expanded.variables[&101][1].value, "\"b\"");

    commands.send(Command::Next).unwrap();
    let stepped = wait_for(
        &incoming,
        |snapshot| matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "step"),
    );
    assert_eq!(stepped.frames[1].line, 42);

    commands.send(Command::Continue).unwrap();
    wait_for(
        &incoming,
        |snapshot| matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "breakpoint"),
    );

    commands.send(Command::Disconnect).unwrap();
    wait_for(&incoming, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    running.await.unwrap().unwrap();
    server.join();
}

/// An adapter-specific request goes through the session as it is and brings its answer
/// back. The mock advertises two replaced classes, the shape the language then reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_extension_request_reaches_the_adapter() {
    let server = lang_fixture::MockDap::start().unwrap();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, _incoming) = channel();
    let launch = Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        ..Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));
    let debugger = dap_client::Debugger::new(commands, _incoming);

    let answer = debugger
        .request("redefineClasses", serde_json::json!({}))
        .await
        .expect("the adapter answers");
    assert_eq!(answer["changedClasses"][0], "demo.Probe");
    assert_eq!(answer["changedClasses"][1], "demo.Probe$1");

    assert!(debugger.send(Command::Disconnect));
    running.await.unwrap().unwrap();
    server.join();
}

/// A condition that does not compile is reported at its breakpoint and in the console,
/// and the exception filter of the capabilities stops the session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_condition_and_an_exception_filter_reach_the_window() {
    let server = lang_fixture::MockDap::start().unwrap();
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = channel();
    let launch = Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        exception_filters: vec!["uncaught".into()],
        ..Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));

    let stopped = wait_for(&incoming, |snapshot| {
        matches!(snapshot.state, DebugState::Stopped { .. })
    });
    assert_eq!(stopped.exception_filters.len(), 2);

    commands
        .send(Command::SetBreakpoint(BreakpointSpec {
            file: PathBuf::from("C:/fixture/demo.sample"),
            line: 42,
            condition: Some("broken".into()),
            ..BreakpointSpec::default()
        }))
        .unwrap();
    let failed = wait_for(&incoming, |snapshot| {
        snapshot.breakpoints.iter().any(|breakpoint| {
            breakpoint
                .message
                .as_deref()
                .is_some_and(|message| message.contains("broken"))
        })
    });
    assert!(
        failed
            .console
            .iter()
            .any(|line| line.error && line.text.contains("broken"))
    );

    commands.send(Command::Continue).unwrap();
    let exception = wait_for(
        &incoming,
        |snapshot| matches!(&snapshot.state, DebugState::Stopped { reason } if reason == "exception"),
    );
    assert_eq!(
        exception.state,
        DebugState::Stopped {
            reason: "exception".into()
        }
    );

    commands.send(Command::Disconnect).unwrap();
    wait_for(&incoming, |snapshot| {
        snapshot.state == DebugState::Terminated
    });
    running.await.unwrap().unwrap();
    server.join();
}
