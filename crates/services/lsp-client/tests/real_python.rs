//! A real language server that is not the one the product ships: `pylsp` over this
//! neutral client, from `initialize` to `exit`.
//!
//! Ignored: the server is an external program. Run it with `--ignored` and
//! `ARRO_PYLSP` pointing at the executable; without the variable the test only says
//! that it was skipped.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_client::lsp_types::Uri;
use lsp_client::{Client, ResponseError, ServerEvents};
use serde_json::{Value, json};
use tokio::process::{Child, Command};
use tokio::time::{Instant, timeout};

const LIMIT: Duration = Duration::from_secs(60);
const EXIT_LIMIT: Duration = Duration::from_secs(30);

const HELLO: &str = "def greet(name):\n    return \"Hello, \" + name\n\n\ngreeting = greet(\"world\")\nprint(greeting)\n";

const BROKEN: &str = "def broken(:\n    return 1\n";

#[derive(Default)]
struct Events {
    notifications: Mutex<Vec<(String, Value)>>,
    closed: AtomicUsize,
}

impl ServerEvents for Events {
    fn notification(&self, method: &str, params: Value) {
        self.notifications
            .lock()
            .unwrap()
            .push((method.to_owned(), params));
    }

    fn request(&self, method: &str, _params: Value) -> Result<Value, ResponseError> {
        match method {
            "workspace/configuration" => Ok(json!([null])),
            "workspace/workspaceFolders" => Ok(json!([])),
            "client/registerCapability"
            | "client/unregisterCapability"
            | "window/workDoneProgress/create"
            | "window/showMessageRequest" => Ok(Value::Null),
            other => Err(ResponseError::method_not_found(other)),
        }
    }

    fn closed(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

impl Events {
    /// Waits for a notification the test must see, keeping every one that arrived while
    /// the test was busy with earlier requests.
    async fn wait_for(&self, what: &str, mut matches: impl FnMut(&str, &Value) -> bool) -> Value {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some((_, params)) = self
                .notifications
                .lock()
                .unwrap()
                .iter()
                .find(|(method, params)| matches(method, params))
            {
                return params.clone();
            }
            assert!(Instant::now() < deadline, "the server sent no {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn server() -> Option<PathBuf> {
    std::env::var_os("ARRO_PYLSP")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn start(server: &Path, folder: &Path) -> (Client, Arc<Events>, Child) {
    let mut child = Command::new(server)
        .current_dir(folder)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", server.display()));
    let input = child.stdin.take().expect("the server standard input");
    let output = child.stdout.take().expect("the server standard output");
    let events = Arc::new(Events::default());
    let client = Client::start(output, input, events.clone());
    (client, events, child)
}

/// A `file:` URI from a path of the test folder. Spaces and non-ASCII bytes are
/// percent-encoded, because a server echoes the spelling it was given.
fn file_uri(path: &Path) -> Uri {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'.' | b'-' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let uri = if encoded.starts_with('/') {
        format!("file://{encoded}")
    } else {
        format!("file:///{encoded}")
    };
    Uri::from_str(&uri).unwrap_or_else(|error| panic!("{uri}: {error}"))
}

async fn request(client: &Client, method: &str, params: Value) -> Value {
    timeout(LIMIT, client.raw_request(method, params))
        .await
        .unwrap_or_else(|_| panic!("no answer to {method}"))
        .unwrap_or_else(|error| panic!("{method} failed: {error}"))
}

fn completion_labels(answer: &Value) -> Vec<String> {
    let items = answer
        .get("items")
        .and_then(Value::as_array)
        .or_else(|| answer.as_array())
        .cloned()
        .unwrap_or_default();
    items
        .iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn markup_value(markup: &Value) -> &str {
    markup
        .get("value")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn hover_text(answer: &Value) -> String {
    let contents = answer.get("contents").unwrap_or(answer);
    match contents {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                other => markup_value(other).to_owned(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => markup_value(other).to_owned(),
    }
}

/// A location points at `file` on `line`, in either spelling the protocol has. Servers
/// spell a Windows drive as they like, so the path is compared, not the URI text.
fn definition_is_at(answer: &Value, file: &Path, line: u32) -> bool {
    let items: Vec<&Value> = match answer {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![answer],
        _ => Vec::new(),
    };
    items.iter().any(|item| {
        let target = item
            .get("targetUri")
            .and_then(Value::as_str)
            .or_else(|| item.get("uri").and_then(Value::as_str));
        let range = item
            .get("targetSelectionRange")
            .or_else(|| item.get("targetRange"))
            .or_else(|| item.get("range"));
        let start = range.and_then(|range| range.get("start"));
        target.and_then(path_of).as_deref() == Some(file)
            && start
                .and_then(|start| start.get("line"))
                .and_then(Value::as_u64)
                == Some(line as u64)
    })
}

fn path_of(uri: &str) -> Option<PathBuf> {
    lsp_client::uri::file_path(&Uri::from_str(uri).ok()?)
}

#[tokio::test]
#[ignore = "needs python-lsp-server in ARRO_PYLSP"]
async fn a_real_python_server_serves_the_neutral_client() {
    let Some(server) = server() else {
        eprintln!("ARRO_PYLSP is not set, skipped the real python language server");
        return;
    };
    let folder = test_support::TempDir::new("real-python");
    let hello = folder.join("hello.py");
    let broken = folder.join("broken.py");
    std::fs::write(&hello, HELLO).expect("the source of the test");
    std::fs::write(&broken, BROKEN).expect("the broken source of the test");
    let hello_uri = file_uri(&hello);
    let broken_uri = file_uri(&broken);
    let root = file_uri(folder.path());

    let (client, events, mut child) = start(&server, folder.path());
    let initialized = request(
        &client,
        "initialize",
        json!({
            "processId": std::process::id(),
            "rootUri": root.as_str(),
            "workspaceFolders": [{"uri": root.as_str(), "name": "boundary"}],
            "capabilities": {
                "textDocument": {
                    "completion": {"completionItem": {"snippetSupport": false}},
                    "hover": {"contentFormat": ["plaintext", "markdown"]},
                    "definition": {"linkSupport": false}
                },
                "workspace": {"workspaceFolders": true}
            },
            "clientInfo": {"name": "arro-boundary", "version": "1"}
        }),
    )
    .await;
    assert!(
        initialized["capabilities"]["completionProvider"].is_object(),
        "the server offers no completion: {initialized}"
    );
    client.raw_notify("initialized", json!({})).unwrap();

    client
        .open(hello_uri.clone(), "python", HELLO.to_owned())
        .expect("didOpen of the valid file");

    // `greeting = greet("world")`: the caret sits inside the call name, so the
    // completion is the function declared above.
    let completion = request(
        &client,
        "textDocument/completion",
        json!({
            "textDocument": {"uri": hello_uri.as_str()},
            "position": {"line": 4, "character": 14}
        }),
    )
    .await;
    let labels = completion_labels(&completion);
    assert!(
        labels.iter().any(|label| label == "greeting"),
        "completion offered {labels:?}"
    );

    let hover = request(
        &client,
        "textDocument/hover",
        json!({
            "textDocument": {"uri": hello_uri.as_str()},
            "position": {"line": 0, "character": 5}
        }),
    )
    .await;
    let hover = hover_text(&hover);
    assert!(hover.contains("greet"), "hover showed {hover:?}");

    let definition = request(
        &client,
        "textDocument/definition",
        json!({
            "textDocument": {"uri": hello_uri.as_str()},
            "position": {"line": 4, "character": 14}
        }),
    )
    .await;
    assert!(
        definition_is_at(&definition, &hello, 0),
        "definition answered {definition}"
    );

    client
        .open(broken_uri.clone(), "python", BROKEN.to_owned())
        .expect("didOpen of the broken file");
    let diagnostics = events
        .wait_for(
            "publishDiagnostics for the broken file",
            |method, params| {
                method == "textDocument/publishDiagnostics"
                    && params
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(path_of)
                        .as_deref()
                        == Some(broken.as_path())
                    && params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .is_some_and(|items| !items.is_empty())
            },
        )
        .await;
    assert!(
        diagnostics["diagnostics"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    timeout(LIMIT, client.shutdown())
        .await
        .expect("the server did not answer shutdown")
        .expect("shutdown");
    let status = timeout(EXIT_LIMIT, child.wait())
        .await
        .expect("the server did not exit")
        .expect("the server process");
    assert!(status.success(), "the server exited with {status}");
    timeout(EXIT_LIMIT, async {
        while events.closed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the client did not see the end of the connection");
}
