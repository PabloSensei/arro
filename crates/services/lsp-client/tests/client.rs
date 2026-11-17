use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lsp_client::lsp_types::request::{HoverRequest, Shutdown};
use lsp_client::lsp_types::{
    HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    WorkDoneProgressParams,
};
use lsp_client::{Client, Error, ResponseError, ServerEvents};
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
    duplex, split,
};
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(5);

/// A pipe as small as a Windows pipe: a blocked side really blocks.
const PIPE: usize = 4096;

#[derive(Default)]
struct Events {
    notifications: Mutex<Vec<(String, Value)>>,
    requests: Mutex<Vec<String>>,
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
        self.requests.lock().unwrap().push(method.to_owned());
        if method == "workspace/configuration" {
            return Ok(json!([{"answer": 42}]));
        }
        Err(ResponseError::method_not_found(method))
    }

    fn closed(&self) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

/// The far end of the connection, driven by each test.
struct Server {
    input: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
}

impl Server {
    async fn read(&mut self) -> Option<Value> {
        let mut length = 0;
        let mut line = String::new();
        loop {
            line.clear();
            if self.input.read_line(&mut line).await.ok()? == 0 {
                return None;
            }
            if line.trim().is_empty() {
                break;
            }
            length = line.trim().strip_prefix("Content-Length: ")?.parse().ok()?;
        }
        let mut body = vec![0; length];
        self.input.read_exact(&mut body).await.ok()?;
        serde_json::from_slice(&body).ok()
    }

    async fn write(&mut self, message: Value) {
        let body = message.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.output.write_all(frame.as_bytes()).await.unwrap();
    }
}

fn connect() -> (Client, Server, Arc<Events>) {
    let (client_end, server_end) = duplex(PIPE);
    let (client_output, client_input) = split(client_end);
    let (server_input, server_output) = split(server_end);
    let events = Arc::new(Events::default());
    let client = Client::start(client_output, client_input, events.clone());
    let server = Server {
        input: BufReader::new(server_input),
        output: server_output,
    };
    (client, server, events)
}

fn uri() -> Uri {
    Uri::from_str("file:///c:/project/Main.txt").unwrap()
}

fn hover_params() -> HoverParams {
    HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier::new(uri()),
            position: Position::new(1, 2),
        },
        work_done_progress_params: WorkDoneProgressParams::default(),
    }
}

#[tokio::test]
async fn typed_request_gets_its_reply() {
    let (client, mut server, _) = connect();
    let reply = tokio::spawn(async move { client.request::<HoverRequest>(hover_params()).await });
    let request = server.read().await.unwrap();
    assert_eq!(request["method"], "textDocument/hover");
    assert_eq!(request["params"]["position"]["character"], 2);
    let id = request["id"].clone();
    server
        .write(json!({"jsonrpc": "2.0", "id": id, "result": {"contents": "docs"}}))
        .await;
    let hover = reply.await.unwrap().unwrap().unwrap();
    assert_eq!(serde_json::to_value(hover.contents).unwrap(), json!("docs"));
}

#[tokio::test]
async fn replies_find_their_requests_out_of_order() {
    let (client, mut server, _) = connect();
    let (first, second) = (client.clone(), client.clone());
    let first = tokio::spawn(async move { first.raw_request("a", json!(1)).await });
    let second = tokio::spawn(async move { second.raw_request("b", json!(2)).await });
    let one = server.read().await.unwrap();
    let two = server.read().await.unwrap();
    for request in [two, one] {
        let echo = request["method"].clone();
        server
            .write(json!({"id": request["id"], "result": echo}))
            .await;
    }
    assert_eq!(first.await.unwrap().unwrap(), "a");
    assert_eq!(second.await.unwrap().unwrap(), "b");
}

#[tokio::test]
async fn request_without_params_omits_the_field() {
    let (client, mut server, _) = connect();
    let shutdown = tokio::spawn(async move { client.request::<Shutdown>(()).await });
    let request = server.read().await.unwrap();
    assert!(request.get("params").is_none());
    server
        .write(json!({"id": request["id"], "result": null}))
        .await;
    shutdown.await.unwrap().unwrap();
}

#[tokio::test]
async fn server_errors_keep_code_and_message() {
    let (client, mut server, _) = connect();
    let reply = tokio::spawn(async move { client.raw_request("x", Value::Null).await });
    let request = server.read().await.unwrap();
    let error = json!({"code": -32603, "message": "boom", "data": "stack"});
    server
        .write(json!({"id": request["id"], "error": error}))
        .await;
    match reply.await.unwrap() {
        Err(Error::Server(error)) => {
            assert_eq!((error.code, error.message.as_str()), (-32603, "boom"));
            assert_eq!(error.data, Some(json!("stack")));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn cancelled_reply_is_its_own_error() {
    let (client, mut server, _) = connect();
    let reply = tokio::spawn(async move { client.raw_request("x", Value::Null).await });
    let request = server.read().await.unwrap();
    server
        .write(json!({"id": request["id"], "error": {"code": -32800, "message": ""}}))
        .await;
    assert!(matches!(reply.await.unwrap(), Err(Error::Cancelled)));
}

#[tokio::test]
async fn dropping_a_request_sends_cancel_and_a_late_reply_is_ignored() {
    let (client, mut server, _) = connect();
    let request = client.raw_request("slow", Value::Null);
    let gave_up = timeout(Duration::from_millis(30), request).await;
    assert!(gave_up.is_err());

    let sent = server.read().await.unwrap();
    let cancel = server.read().await.unwrap();
    assert_eq!(cancel["method"], "$/cancelRequest");
    assert_eq!(cancel["params"]["id"], sent["id"]);

    server
        .write(json!({"id": sent["id"], "result": "late"}))
        .await;
    let next = tokio::spawn(async move { client.raw_request("next", Value::Null).await });
    let request = server.read().await.unwrap();
    server
        .write(json!({"id": request["id"], "result": "fresh"}))
        .await;
    assert_eq!(next.await.unwrap().unwrap(), "fresh");
}

#[tokio::test]
async fn a_finished_request_sends_no_cancel() {
    let (client, mut server, _) = connect();
    let reply = tokio::spawn({
        let client = client.clone();
        async move { client.raw_request("x", Value::Null).await }
    });
    let request = server.read().await.unwrap();
    server
        .write(json!({"id": request["id"], "result": 1}))
        .await;
    reply.await.unwrap().unwrap();
    client.raw_notify("marker", Value::Null).unwrap();
    assert_eq!(server.read().await.unwrap()["method"], "marker");
}

#[tokio::test]
async fn server_requests_are_answered() {
    let (_client, mut server, events) = connect();
    server
        .write(json!({"id": "ask", "method": "workspace/configuration"}))
        .await;
    let reply = server.read().await.unwrap();
    assert_eq!(reply["id"], "ask");
    assert_eq!(reply["result"], json!([{"answer": 42}]));

    server.write(json!({"id": 7, "method": "unknown"})).await;
    let reply = server.read().await.unwrap();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["error"]["code"], ResponseError::METHOD_NOT_FOUND);
    assert_eq!(
        *events.requests.lock().unwrap(),
        ["workspace/configuration", "unknown"]
    );
}

#[tokio::test]
async fn notifications_arrive_in_order() {
    let (client, mut server, events) = connect();
    for index in 0..50 {
        server.write(json!({"method": "n", "params": index})).await;
    }
    client.raw_notify("marker", Value::Null).unwrap();
    server.read().await.unwrap();
    server.write(json!({"method": "last"})).await;
    timeout(LIMIT, async {
        while events
            .notifications
            .lock()
            .unwrap()
            .last()
            .is_none_or(|n| n.0 != "last")
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let seen = events.notifications.lock().unwrap();
    let order: Vec<_> = seen
        .iter()
        .take(50)
        .map(|n| n.1.as_i64().unwrap())
        .collect();
    assert_eq!(order, (0..50).collect::<Vec<_>>());
}

#[tokio::test]
async fn end_of_stream_fails_pending_requests_and_later_sends() {
    let (client, server, events) = connect();
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.raw_request("x", Value::Null).await }
    });
    tokio::task::yield_now().await;
    drop(server);
    assert!(matches!(
        timeout(LIMIT, pending).await.unwrap().unwrap(),
        Err(Error::Closed)
    ));
    assert!(matches!(
        client.raw_request("y", Value::Null).await,
        Err(Error::Closed)
    ));
    assert_eq!(events.closed.load(Ordering::SeqCst), 1);
}

/// What froze the first S9 matrix: the server stops reading its input, asks a
/// question and fills its output, while the editor keeps sending. The client
/// must drain everything, then the server reads again and all of it arrives.
#[tokio::test]
async fn reader_never_waits_for_the_writer() {
    const COUNT: usize = 2000;
    let (client, mut server, events) = connect();
    let filler = "x".repeat(2000);

    // Goes well past both pipes while the server reads nothing.
    for _ in 0..COUNT {
        client.raw_notify("edit", json!(filler)).unwrap();
    }
    server
        .write(json!({"id": "ask", "method": "workspace/configuration"}))
        .await;
    let flood = timeout(LIMIT, async {
        for _ in 0..COUNT {
            server
                .write(json!({"method": "diagnostics", "params": filler}))
                .await;
        }
    });
    flood.await.expect("the client stopped reading the server");

    let drained = timeout(LIMIT, async {
        let mut edits = 0;
        let mut answers = 0;
        while edits < COUNT || answers < 1 {
            let message = server.read().await.unwrap();
            if message["method"] == "edit" {
                edits += 1;
            } else if message["id"] == "ask" {
                answers += 1;
            }
        }
    });
    drained.await.expect("the writer never delivered");
    assert_eq!(events.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn versions_grow_from_one_and_reach_the_server() {
    let (client, mut server, _) = connect();
    assert_eq!(client.open(uri(), "text", "ab".into()).unwrap(), 1);
    assert_eq!(client.change(&uri(), Vec::new()).unwrap(), 2);
    assert_eq!(client.change(&uri(), Vec::new()).unwrap(), 3);
    assert_eq!(client.version(&uri()), Some(3));

    let opened = server.read().await.unwrap();
    assert_eq!(opened["method"], "textDocument/didOpen");
    assert_eq!(opened["params"]["textDocument"]["version"], 1);
    assert_eq!(opened["params"]["textDocument"]["text"], "ab");
    let first = server.read().await.unwrap();
    assert_eq!(first["method"], "textDocument/didChange");
    assert_eq!(first["params"]["textDocument"]["version"], 2);
    assert_eq!(
        server.read().await.unwrap()["params"]["textDocument"]["version"],
        3
    );
}

#[tokio::test]
async fn a_closed_document_can_be_opened_again_from_version_one() {
    let (client, mut server, _) = connect();
    client.open(uri(), "text", String::new()).unwrap();
    client.change(&uri(), Vec::new()).unwrap();
    client.close(&uri()).unwrap();
    assert_eq!(client.version(&uri()), None);
    assert_eq!(client.open(uri(), "text", String::new()).unwrap(), 1);
    let mut methods = Vec::new();
    for _ in 0..4 {
        methods.push(
            server.read().await.unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(
        methods,
        [
            "textDocument/didOpen",
            "textDocument/didChange",
            "textDocument/didClose",
            "textDocument/didOpen",
        ]
    );
}

#[tokio::test]
async fn document_misuse_is_an_error_and_sends_nothing() {
    let (client, mut server, _) = connect();
    assert!(matches!(
        client.change(&uri(), Vec::new()),
        Err(Error::NotOpen(_))
    ));
    assert!(matches!(client.save(&uri(), None), Err(Error::NotOpen(_))));
    assert!(matches!(client.close(&uri()), Err(Error::NotOpen(_))));
    client.open(uri(), "text", String::new()).unwrap();
    assert!(matches!(
        client.open(uri(), "text", String::new()),
        Err(Error::AlreadyOpen(_))
    ));
    assert_eq!(
        server.read().await.unwrap()["method"],
        "textDocument/didOpen"
    );
    client.raw_notify("marker", Value::Null).unwrap();
    assert_eq!(server.read().await.unwrap()["method"], "marker");
}

#[tokio::test]
async fn shutdown_sends_exit_after_the_reply() {
    let (client, mut server, _) = connect();
    let done = tokio::spawn(async move { client.shutdown().await });
    let request = server.read().await.unwrap();
    assert_eq!(request["method"], "shutdown");
    server
        .write(json!({"id": request["id"], "result": null}))
        .await;
    assert_eq!(server.read().await.unwrap()["method"], "exit");
    done.await.unwrap().unwrap();
}
