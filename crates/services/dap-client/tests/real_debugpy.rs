//! A real debug adapter that is not the one the product ships: `debugpy` over this
//! neutral client, from `initialize` to the end of the program.
//!
//! Ignored: the adapter is an external program. Run it with `--ignored` and
//! `ARRO_DEBUGPY_PYTHON` pointing at the interpreter that has `debugpy`; without the
//! variable the test only says that it was skipped.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use dap_client::{Client, Event};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(60);
const EXIT_LIMIT: Duration = Duration::from_secs(30);

/// The line with the call of `print`, the first place the program may stop.
const BREAKPOINT: u32 = 3;

const SCRIPT: &str = "def main():\n    total = 40 + 2\n    print(total)\n\n\nmain()\n";

fn python() -> Option<PathBuf> {
    std::env::var_os("ARRO_DEBUGPY_PYTHON")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn start(python: &Path, folder: &Path) -> (Client, UnboundedReceiver<Event>, Child) {
    let mut child = Command::new(python)
        .args(["-m", "debugpy.adapter"])
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", python.display()));
    let input = child.stdin.take().expect("the adapter standard input");
    let output = child.stdout.take().expect("the adapter standard output");
    let (client, events) = Client::start(output, input);
    (client, events, child)
}

async fn request(client: &Client, command: &str, arguments: Value) -> Value {
    timeout(LIMIT, client.request(command, arguments))
        .await
        .unwrap_or_else(|_| panic!("no answer to {command}"))
        .unwrap_or_else(|error| panic!("{command} failed: {error}"))
}

async fn wait_for(
    events: &mut UnboundedReceiver<Event>,
    what: &str,
    mut matches: impl FnMut(&Event) -> bool,
) -> Event {
    // The events passed over on the way, so that a failure says what came instead.
    let mut seen = Vec::new();
    let found = timeout(LIMIT, async {
        loop {
            match events.recv().await {
                Some(event) if matches(&event) => return Some(event),
                Some(Event::Closed) | None => return None,
                Some(event) => seen.push(format!("{event:?}")),
            }
        }
    })
    .await;
    match found {
        Ok(Some(event)) => event,
        Ok(None) => panic!("the adapter closed before {what}; events: {seen:#?}"),
        Err(_) => panic!("the adapter sent no {what}; events: {seen:#?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs debugpy in ARRO_DEBUGPY_PYTHON"]
async fn a_real_python_adapter_drives_the_neutral_client() {
    let Some(python) = python() else {
        eprintln!("ARRO_DEBUGPY_PYTHON is not set, skipped the real debug adapter");
        return;
    };
    let folder = test_support::TempDir::new("real-debugpy");
    let script = folder.join("probe.py");
    std::fs::write(&script, SCRIPT).expect("the source of the test");

    let (client, mut events, mut adapter) = start(&python, folder.path());
    let capabilities = request(
        &client,
        "initialize",
        json!({
            "clientID": "arro",
            "clientName": "Arro",
            "adapterID": "python",
            "locale": "en-US",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            "supportsVariableType": true,
            "supportsRunInTerminalRequest": false
        }),
    )
    .await;
    assert_eq!(
        capabilities["supportsConfigurationDoneRequest"],
        json!(true),
        "the adapter cannot start the program: {capabilities}"
    );

    // The adapter answers `launch` only after `configurationDone`; a wait here would
    // stop the session before it starts.
    let launch = {
        let client = client.clone();
        let arguments = json!({
            "name": "arro boundary",
            "type": "python",
            "request": "launch",
            "program": script.to_string_lossy(),
            "cwd": folder.path().to_string_lossy(),
            "python": [python.to_string_lossy()],
            "console": "internalConsole",
            "justMyCode": false,
            "stopOnEntry": false
        });
        tokio::spawn(async move { client.request("launch", arguments).await })
    };
    wait_for(&mut events, "the initialized event", |event| {
        matches!(event, Event::Initialized)
    })
    .await;

    let breakpoints = request(
        &client,
        "setBreakpoints",
        json!({
            "source": {"path": script.to_string_lossy()},
            "breakpoints": [{"line": BREAKPOINT}],
            "sourceModified": false
        }),
    )
    .await;
    assert_eq!(
        breakpoints["breakpoints"][0]["line"].as_u64(),
        Some(u64::from(BREAKPOINT)),
        "the adapter moved the breakpoint: {breakpoints}"
    );
    request(&client, "configurationDone", json!({})).await;
    timeout(LIMIT, launch)
        .await
        .expect("the adapter did not answer launch")
        .expect("the launch task")
        .unwrap_or_else(|error| panic!("launch failed: {error}"));

    let stopped = wait_for(&mut events, "a stop on the breakpoint", |event| {
        matches!(event, Event::Stopped { .. })
    })
    .await;
    let Event::Stopped {
        reason, thread_id, ..
    } = stopped
    else {
        unreachable!()
    };
    assert_eq!(reason, "breakpoint");
    let thread = match thread_id {
        Some(thread) => thread,
        None => {
            let answer = request(&client, "threads", json!({})).await;
            answer["threads"][0]["id"]
                .as_i64()
                .unwrap_or_else(|| panic!("the adapter named no thread: {answer}"))
        }
    };

    let stack = request(
        &client,
        "stackTrace",
        json!({"threadId": thread, "startFrame": 0, "levels": 20}),
    )
    .await;
    let frame = &stack["stackFrames"][0];
    assert_eq!(
        frame["line"].as_u64(),
        Some(u64::from(BREAKPOINT)),
        "the stop is not on the breakpoint: {stack}"
    );
    let frame_path = frame["source"]["path"]
        .as_str()
        .unwrap_or_else(|| panic!("the frame names no source: {frame}"));
    assert_eq!(
        std::path::Path::new(frame_path).file_name(),
        script.file_name(),
        "the frame is in another file: {stack}"
    );
    let frame_id = frame["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("the frame has no id: {frame}"));

    let scopes = request(&client, "scopes", json!({"frameId": frame_id})).await;
    let locals = scopes["scopes"]
        .as_array()
        .and_then(|scopes| {
            scopes
                .iter()
                .find(|scope| scope["name"].as_str() == Some("Locals"))
        })
        .unwrap_or_else(|| panic!("the adapter has no locals: {scopes}"));
    let reference = locals["variablesReference"]
        .as_i64()
        .unwrap_or_else(|| panic!("the locals have no reference: {locals}"));
    let variables = request(
        &client,
        "variables",
        json!({"variablesReference": reference}),
    )
    .await;
    let total = variables["variables"]
        .as_array()
        .and_then(|variables| {
            variables
                .iter()
                .find(|variable| variable["name"].as_str() == Some("total"))
        })
        .unwrap_or_else(|| panic!("the locals have no `total`: {variables}"));
    assert_eq!(total["value"].as_str(), Some("42"));

    request(&client, "continue", json!({"threadId": thread})).await;
    wait_for(&mut events, "the end of the program", |event| {
        matches!(event, Event::Exited { .. } | Event::Terminated)
    })
    .await;

    let _ = timeout(
        EXIT_LIMIT,
        client.request("disconnect", json!({"terminateDebuggee": false})),
    )
    .await;
    drop(client);
    drop(events);
    let status = timeout(EXIT_LIMIT, adapter.wait())
        .await
        .expect("the adapter did not exit")
        .expect("the adapter process");
    // A debug server and the client that disconnect at the same moment make debugpy
    // report a fatal error while finalizing and leave with 1, after every protocol step
    // above has answered; a signal is still a failure.
    assert!(status.code().is_some(), "the adapter was killed: {status}");
}
