//! Code lenses of a server: the labels of a document move with its edits, an old answer
//! is never given back, and the visible labels resolve their commands one by one.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{CodeLensKind, CodeLensProvider, CodeLensQuery, SourceId, ask};
use lsp_client::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Uri};
use lsp_client::{Client, CodeLensCapability, CodeLensSetup, ServerEvents};
use semantic_router::{LspProviders, Place, Published, RouteError, Router};
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
        let body = json!({"id": request["id"], "result": result}).to_string();
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

fn place(version: i32) -> Place {
    Place {
        uri: uri(),
        version,
        position: Position::new(0, 0),
    }
}

fn live() -> (Arc<Router>, watch::Sender<Status>, Client, Server) {
    let (client, server) = server();
    let (sender, receiver) = watch::channel(status(Some(client.clone()), 1));
    let router = Router::new(receiver);
    (router, sender, client, server)
}

async fn opened(server: &mut Server) {
    let message = server.read().await;
    assert_eq!(message["method"], "textDocument/didOpen");
}

fn lens(line: u32) -> Value {
    json!({
        "range": {
            "start": {"line": line, "character": 4},
            "end": {"line": line, "character": 8},
        },
        "command": {
            "title": "2 usages",
            "command": "lang.show.references",
        },
        "data": {"kind": "references"},
    })
}

#[tokio::test]
async fn a_label_follows_the_edits_and_an_old_answer_is_dropped() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "one\ntwo".into());
    opened(&mut server).await;

    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.code_lens(place(1)).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/codeLens");
    assert_eq!(request["params"]["textDocument"]["uri"], uri().as_str());
    server.reply(&request, json!([lens(1)])).await;
    let answer = ask.await.unwrap().unwrap().unwrap();
    assert_eq!(answer.len(), 1);
    assert_eq!(router.cached_code_lens(&uri(), 1).len(), 1);

    let insertion = TextDocumentContentChangeEvent {
        range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
        range_length: None,
        text: "x\n".to_owned(),
    };
    router.change(&uri(), 2, "x\none\ntwo".into(), vec![insertion]);
    let moved = router.cached_code_lens(&uri(), 2);
    assert_eq!(moved[0].range.start.line, 2, "the label follows the text");
    assert!(
        router.cached_code_lens(&uri(), 1).is_empty(),
        "an answer of an old version is never given back"
    );
    assert!(matches!(
        router.code_lens(place(1)).await,
        Err(RouteError::Stale)
    ));
}

#[tokio::test]
async fn a_label_resolves_into_its_command() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "one\ntwo".into());
    opened(&mut server).await;

    let unresolved: lsp_client::lsp_types::CodeLens = serde_json::from_value(json!({
        "range": {
            "start": {"line": 1, "character": 4},
            "end": {"line": 1, "character": 8},
        },
        "data": {"kind": "implementations"},
    }))
    .unwrap();
    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.code_lens_resolve(place(1), unresolved).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "codeLens/resolve");
    assert_eq!(request["params"]["data"]["kind"], "implementations");
    server
        .reply(
            &request,
            json!({
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 8},
                },
                "command": {
                    "title": "1 implementation",
                    "command": "lang.show.implementations",
                },
                "data": {"kind": "implementations"},
            }),
        )
        .await;
    let resolved = ask.await.unwrap().unwrap();
    let command = resolved.command.expect("the command of the label");
    assert_eq!(command.title, "1 implementation");
    assert_eq!(command.command, "lang.show.implementations");
}

#[tokio::test]
async fn the_provider_turns_answers_into_labels_of_the_editor() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri(), "text", 1, "one\ntwo".into());
    opened(&mut server).await;

    let setup = CodeLensSetup::new();
    setup.set(Some(CodeLensCapability { resolve: false }));
    let (published, revisions) = Published::new();
    let providers = LspProviders::new(
        router.clone(),
        published,
        revisions,
        Handle::current(),
        "text",
        SourceId::new("test"),
    )
    .with_code_lens(setup);
    let provider: &CodeLensProvider = &providers;
    let mut replies = ask(
        provider,
        CodeLensQuery {
            uri: uri().as_str().to_owned(),
            version: 1,
        },
    );
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/codeLens");
    server
        .reply(
            &request,
            json!([
                lens(1),
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 3},
                    },
                    "command": {
                        "title": "1 implementation",
                        "command": "lang.show.implementations",
                    },
                },
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 3},
                    },
                    "command": {"title": "Run", "command": "lang.run"},
                },
            ]),
        )
        .await;
    let answer = timeout(LIMIT, replies.next()).await.unwrap().unwrap();
    assert_eq!(answer.value.len(), 3);
    assert_eq!(answer.value[0].kind, CodeLensKind::Usages);
    assert_eq!(answer.value[0].title, "2 usages");
    assert_eq!(answer.value[1].kind, CodeLensKind::Implementations);
    assert_eq!(answer.value[2].kind, CodeLensKind::Other);
    assert!(
        answer.value[0].resolve.is_some(),
        "the label goes back on resolve"
    );
    assert!(
        answer.value[0].command.is_some(),
        "the command travels with the label"
    );
}

#[tokio::test]
async fn a_refresh_of_the_server_bumps_the_revision() {
    let (published, _diagnostics) = Published::new();
    let mut refresh = published.code_lens_refresh();
    published.notification("workspace/codeLens/refresh", json!({}));
    timeout(LIMIT, refresh.wait_for(|revision| *revision == 1))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_request_for_a_closed_document_gets_no_answer() {
    let (_sender, receiver) = watch::channel(status(None, 0));
    let router = Router::new(receiver);
    assert!(matches!(
        router.code_lens(place(1)).await,
        Err(RouteError::Stale)
    ));
}
