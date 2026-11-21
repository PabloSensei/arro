//! What `prepareRename` answers when the server refuses the name, and when it cannot answer.
//! A refusal is a null answer or an error whose text says the name cannot be renamed. Any
//! other error leaves the question unknown, so the editor keeps the name in the file.
//!
//! cargo test -p semantic-router --test prepare_rename prepare_

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{Confidence, DocumentSpot, PrepareRenameProvider, SourceId, TextPoint, ask};
use lsp_client::lsp_types::Uri;
use lsp_client::{Client, ServerEvents};
use semantic_router::{LspProviders, Published, Router};
use serde_json::{Value, json};
use sidecar::{State, Status};
use tokio::io::{
    AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf,
    duplex, split,
};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::timeout;

const LIMIT: Duration = Duration::from_secs(5);

/// How the server answers the question about the name.
enum Given {
    Result(Value),
    Error(i64, &'static str),
}

struct Quiet;

impl ServerEvents for Quiet {
    fn notification(&self, _method: &str, _params: Value) {}
}

struct Server {
    input: BufReader<ReadHalf<DuplexStream>>,
    output: WriteHalf<DuplexStream>,
}

impl Server {
    async fn read(&mut self) -> Value {
        timeout(LIMIT, self.read_any())
            .await
            .expect("nothing arrived")
            .expect("stream ended")
    }

    async fn read_any(&mut self) -> Option<Value> {
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

    async fn reply(&mut self, request: &Value, result: Value) {
        self.send(json!({"id": request["id"], "result": result}))
            .await;
    }

    async fn fail(&mut self, request: &Value, code: i64, message: &str) {
        let error = json!({"code": code, "message": message});
        self.send(json!({"id": request["id"], "error": error}))
            .await;
    }

    async fn send(&mut self, body: Value) {
        let body = body.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        self.output.write_all(frame.as_bytes()).await.unwrap();
    }
}

fn status(client: Option<Client>, generation: u64) -> Status {
    Status {
        state: State::Ready,
        generation,
        alive: client.is_some(),
        client,
        process_id: None,
        crashes: 0,
        last_error: None,
    }
}

fn server() -> (Client, Server) {
    let (client_end, server_end) = duplex(4096);
    let (stdout, stdin) = split(client_end);
    let (input, output) = split(server_end);
    let client = Client::start(stdout, stdin, Arc::new(Quiet));
    (
        client,
        Server {
            input: BufReader::new(input),
            output,
        },
    )
}

fn uri() -> Uri {
    Uri::from_str("file:///C:/project/A").unwrap()
}

fn live() -> (Arc<Router>, watch::Sender<Status>, Client, Server) {
    let (client, server) = server();
    let (sender, receiver) = watch::channel(status(Some(client.clone()), 1));
    let router = Router::new(receiver);
    (router, sender, client, server)
}

async fn opened(server: &mut Server) {
    let message = server.read().await;
    assert_eq!(message["method"], "textDocument/didOpen", "{message}");
}

fn providers_for(router: &Arc<Router>) -> LspProviders {
    let (published, revisions) = Published::new();
    LspProviders::new(
        router.clone(),
        published,
        revisions,
        Handle::current(),
        "text",
        SourceId::new("test"),
    )
}

/// Asks about the name at the start of the open document and answers the question the way
/// `given` says. Returns whether the language accepted the name, and how sure its answer is.
async fn prepare(
    providers: &LspProviders,
    server: &mut Server,
    given: Given,
) -> (bool, Confidence) {
    let prepare: &PrepareRenameProvider = providers;
    let spot = DocumentSpot {
        uri: uri().as_str().to_owned(),
        version: 1,
        point: TextPoint::default(),
    };
    let mut replies = ask(prepare, spot);
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/prepareRename", "{request}");
    match given {
        Given::Result(result) => server.reply(&request, result).await,
        Given::Error(code, message) => server.fail(&request, code, message).await,
    }
    let answer = timeout(LIMIT, replies.next())
        .await
        .expect("the question is answered")
        .expect("one answer");
    (answer.value.is_some(), answer.confidence)
}

#[tokio::test]
async fn prepare_an_error_that_says_the_name_cannot_be_renamed_refuses_it() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "pet".into());
    opened(&mut server).await;
    let providers = providers_for(&router);

    for message in [
        "Renaming this element is not supported.",
        "The element cannot be renamed.",
        "This name can't be renamed here",
    ] {
        let answer = prepare(&providers, &mut server, Given::Error(-32600, message)).await;
        assert_eq!(answer, (false, Confidence::Certain), "{message}");
    }
}

#[tokio::test]
async fn prepare_a_null_answer_refuses_the_name() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "pet".into());
    opened(&mut server).await;
    let providers = providers_for(&router);

    let answer = prepare(&providers, &mut server, Given::Result(Value::Null)).await;
    assert_eq!(answer, (false, Confidence::Certain));
}

#[tokio::test]
async fn prepare_a_busy_or_failing_server_leaves_the_name_unknown() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "pet".into());
    opened(&mut server).await;
    let providers = providers_for(&router);

    for (code, message) in [
        (-32603, "The language server is not ready"),
        (-32603, "Internal error while resolving the name"),
        (-32801, "The workspace is busy, try again"),
    ] {
        let answer = prepare(&providers, &mut server, Given::Error(code, message)).await;
        assert_eq!(answer, (false, Confidence::Unknown), "{message}");
    }
}

#[tokio::test]
async fn prepare_a_range_accepts_the_name() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "pet".into());
    opened(&mut server).await;
    let providers = providers_for(&router);

    let range = json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}});
    let answer = prepare(&providers, &mut server, Given::Result(range)).await;
    assert_eq!(answer, (true, Confidence::Certain));
}
