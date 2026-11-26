//! The start of a session against a stand-in adapter of this test: one that refuses
//! `attach`, one that accepts it and never sends `initialized`, and one that sends
//! `initialized` before the attach is asked for. No debugger is needed.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use dap_client::{Command, DebugState, Launch, Snapshot};
use serde_json::{Value, json};

/// A session that does not end in this time is stuck.
const LIMIT: Duration = Duration::from_secs(10);

/// What the stand-in adapter does with the requests it reads.
#[derive(Clone, Copy)]
enum Adapter {
    /// `attach` fails with the reason of the adapter.
    RefusesAttach,
    /// `attach` succeeds, `initialized` never comes, and `disconnect` is never answered.
    NeverInitialized,
    /// `initialized` comes with the answer to `initialize`, before the attach is asked for.
    /// The adapter reports the attach it receives, so the session shows it.
    InitializedFirst,
}

/// Reads one message of the protocol, or `None` at the end of the connection.
fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some(value) = header.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn write_message(writer: &mut impl Write, message: &Value) {
    let body = message.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
    writer.flush().unwrap();
}

/// The answer to a request: a success, or a failure with the reason of the adapter.
fn answer(seq: i64, request_seq: i64, command: &str, failure: Option<&str>) -> Value {
    let mut message = json!({
        "seq": seq,
        "type": "response",
        "request_seq": request_seq,
        "command": command,
        "success": failure.is_none(),
        "body": {},
    });
    if let Some(reason) = failure {
        message["message"] = json!(reason);
    }
    message
}

/// An event of the protocol.
fn event(seq: i64, name: &str, body: Value) -> Value {
    json!({"seq": seq, "type": "event", "event": name, "body": body})
}

/// Starts the stand-in adapter on a free port and returns its address.
fn adapter(behaviour: Adapter) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        let mut seq = 1;
        while let Some(request) = read_message(&mut reader) {
            let command = request["command"].as_str().unwrap_or_default().to_owned();
            let request_seq = request["seq"].as_i64().unwrap_or_default();
            let mut messages = Vec::new();
            match (command.as_str(), behaviour) {
                ("attach", Adapter::RefusesAttach) => {
                    seq += 1;
                    let reason = "no port 5005 is open";
                    messages.push(answer(seq, request_seq, &command, Some(reason)));
                }
                ("initialize", Adapter::InitializedFirst) => {
                    seq += 1;
                    messages.push(answer(seq, request_seq, &command, None));
                    seq += 1;
                    messages.push(event(seq, "initialized", json!({})));
                }
                ("attach", Adapter::InitializedFirst) => {
                    seq += 1;
                    messages.push(answer(seq, request_seq, &command, None));
                    seq += 1;
                    let body = json!({"category": "console", "output": "attach seen\n"});
                    messages.push(event(seq, "output", body));
                }
                // This adapter answers every other request, as the configuration of a session
                // expects; only the wait for `initialized` is what the test looks at.
                (_, Adapter::InitializedFirst) => {
                    seq += 1;
                    messages.push(answer(seq, request_seq, &command, None));
                }
                ("initialize" | "attach" | "configurationDone", _) => {
                    seq += 1;
                    messages.push(answer(seq, request_seq, &command, None));
                }
                ("disconnect", Adapter::RefusesAttach) => {
                    seq += 1;
                    messages.push(answer(seq, request_seq, &command, None));
                }
                // Anything else gets no reply: a stuck adapter answers nothing.
                _ => {}
            }
            for message in &messages {
                write_message(&mut writer, message);
            }
        }
    });
    address
}

fn launch_at(address: String) -> Launch {
    Launch {
        address,
        adapter_id: "fixture".into(),
        name: "fixture".into(),
        attach_port: Some(5005),
        ..Launch::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_attach_ends_the_session_with_the_reason_of_the_adapter() {
    let address = adapter(Adapter::RefusesAttach);
    // Kept alive: a closed command channel would be a stop of its own.
    let (_commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = channel();

    let ended = tokio::time::timeout(
        LIMIT,
        dap_client::run(launch_at(address), receiver, snapshots),
    )
    .await
    .expect("the session did not end");

    let error = ended.expect_err("a refused attach is an error of the session");
    assert!(
        error.to_string().contains("no port 5005 is open"),
        "{error}"
    );
    let last: Snapshot = incoming
        .try_iter()
        .last()
        .map(|snapshot| (*snapshot).clone())
        .expect("the session published its end");
    assert_eq!(last.state, DebugState::Terminated);
    assert!(
        last.console
            .last()
            .is_some_and(|line| line.error && line.text.contains("no port 5005 is open")),
        "{:?}",
        last.console
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_the_session_waits_for_initialized_ends_it() {
    let address = adapter(Adapter::NeverInitialized);
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = channel();
    let running = tokio::spawn(dap_client::run(launch_at(address), receiver, snapshots));

    // The stop comes before the adapter could say `initialized`; the session ends by
    // itself after the stop, though the adapter never answers the disconnect.
    commands.send(Command::Disconnect).unwrap();
    let started = Instant::now();
    let ended = tokio::time::timeout(LIMIT, running)
        .await
        .expect("the stop did not end the session")
        .expect("the session task");
    assert!(ended.is_ok(), "{ended:?}");
    assert!(
        started.elapsed() < LIMIT,
        "the stop waited {:?} for the adapter",
        started.elapsed()
    );
    let last = incoming
        .try_iter()
        .last()
        .expect("the session published its end");
    assert_eq!(last.state, DebugState::Terminated);
}

/// `initialized` can be queued before the start asks for the attach. The attach still goes
/// out: the adapter reports it, and the session then ends with the stop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_initialized_event_that_comes_first_still_gets_the_attach() {
    let address = adapter(Adapter::InitializedFirst);
    let (commands, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (snapshots, incoming) = channel();
    let running = tokio::spawn(dap_client::run(launch_at(address), receiver, snapshots));

    let started = Instant::now();
    loop {
        let seen = incoming.try_iter().any(|snapshot| {
            snapshot
                .console
                .iter()
                .any(|line| line.text == "attach seen")
        });
        if seen {
            break;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the attach never reached the adapter"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    commands.send(Command::Disconnect).unwrap();
    let ended = tokio::time::timeout(LIMIT, running)
        .await
        .expect("the stop did not end the session")
        .expect("the session task");
    assert!(ended.is_ok(), "{ended:?}");
}
