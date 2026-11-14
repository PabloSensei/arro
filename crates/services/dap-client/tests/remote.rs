//! Stopping a session that attached to a program somebody else runs disconnects the
//! debugger and leaves that program alive.

use std::sync::Arc;
use std::time::Duration;

use dap_client::{Command, DebugState, Debugger, Launch, Snapshot};
use proc_sup::ProcessSpec;

async fn wait_stopped(debugger: &Debugger) -> Arc<Snapshot> {
    for _ in 0..400 {
        if let Some(snapshot) = debugger.poll()
            && matches!(snapshot.state, DebugState::Stopped { .. })
        {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the session did not stop");
}

/// A program of this test that must outlive the session.
#[cfg(windows)]
fn stand_in() -> ProcessSpec {
    ProcessSpec::new("cmd").args(["/d", "/c", "ping", "-n", "60", "127.0.0.1"])
}

#[cfg(unix)]
fn stand_in() -> ProcessSpec {
    ProcessSpec::new("sh").args(["-c", "sleep 60"])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_end_of_an_attached_session_keeps_the_program_alive() {
    let server = lang_fixture::MockDap::start().unwrap();
    let mut remote = stand_in().start().expect("the stand-in program starts");
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = std::sync::mpsc::channel();
    let launch = Launch {
        address: server.address().to_owned(),
        adapter_id: "fixture".into(),
        attach_host: Some("127.0.0.1".into()),
        attach_port: Some(1),
        name: "remote".into(),
        ..Launch::default()
    };
    let running = tokio::spawn(dap_client::run(launch, receiver, snapshots));
    let debugger = Debugger::new(commands, incoming);
    wait_stopped(&debugger).await;

    debugger.send(Command::Disconnect);
    running.await.unwrap().unwrap();

    let disconnect = server
        .requests()
        .into_iter()
        .find(|request| request["command"] == "disconnect")
        .expect("the session disconnected");
    assert_eq!(
        disconnect["arguments"]["terminateDebuggee"],
        serde_json::json!(false),
        "a program this process did not start is not ended with the session"
    );
    assert!(
        remote
            .try_wait()
            .expect("the state of the program")
            .is_none(),
        "the program of somebody else ended with the session"
    );

    let _ = remote.stop();
    server.join();
}
