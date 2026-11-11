use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{
    ActionKind, ActionProvider, ActionQuery, Confidence, DocumentSpot, MoveDestinationProvider,
    MoveKind, MoveProvider, MoveQuery, MoveRequest, NavigationKind, NavigationProvider,
    NavigationQuery, SourceId, SymbolQuery, TextPoint, WorkspaceSymbolProvider, ask,
};
use lsp_client::lsp_types::notification::{Notification, PublishDiagnostics};
use lsp_client::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Uri};
use lsp_client::{Client, ServerEvents};
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

    /// Whatever the client sent, the marker arrives next.
    async fn assert_silent(&mut self, client: &Client) {
        client.raw_notify("marker", Value::Null).unwrap();
        assert_eq!(self.read().await["method"], "marker");
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

fn uri(name: &str) -> Uri {
    Uri::from_str(&format!("file:///C:/project/{name}")).unwrap()
}

fn place(version: i32) -> Place {
    Place {
        uri: uri("A"),
        version,
        position: Position::new(2, 3),
    }
}

fn live() -> (Arc<Router>, watch::Sender<Status>, Client, Server) {
    let (client, server) = server();
    let (sender, receiver) = watch::channel(status(Some(client.clone()), 1));
    let router = Router::new(receiver);
    (router, sender, client, server)
}

async fn opened(server: &mut Server) -> Value {
    let message = server.read().await;
    assert_eq!(message["method"], "textDocument/didOpen", "{message}");
    message["params"]["textDocument"].clone()
}

#[tokio::test]
async fn documents_wait_for_the_server_and_arrive_with_their_latest_text() {
    let (sender, receiver) = watch::channel(status(None, 0));
    let router = Router::new(receiver);
    router.open(uri("A"), "text", 1, "one".into());
    router.change(&uri("A"), 2, "two".into(), Vec::new());

    let (client, mut server) = server();
    sender.send_replace(status(Some(client), 1));
    let document = opened(&mut server).await;
    assert_eq!(document["text"], "two");
    assert_eq!(document["version"], 1);
    assert_eq!(document["languageId"], "text");
}

#[tokio::test]
async fn a_new_server_generation_receives_every_open_document() {
    let (router, sender, _old, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    router.open(uri("B"), "text", 1, "b".into());
    router.change(&uri("A"), 2, "a2".into(), Vec::new());
    router.close(&uri("B"));
    opened(&mut server).await;
    opened(&mut server).await;
    assert_eq!(server.read().await["method"], "textDocument/didChange");
    assert_eq!(server.read().await["method"], "textDocument/didClose");

    let (client, mut next) = self::server();
    sender.send_replace(status(Some(client.clone()), 2));
    let document = opened(&mut next).await;
    assert_eq!(document["uri"], uri("A").as_str());
    assert_eq!(document["text"], "a2");
    next.assert_silent(&client).await;
}

#[tokio::test]
async fn nothing_is_replayed_twice_for_one_generation() {
    let (router, sender, client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    opened(&mut server).await;
    let mut same = status(Some(client.clone()), 1);
    same.state = State::Importing;
    sender.send_replace(same);
    tokio::task::yield_now().await;
    server.assert_silent(&client).await;
}

#[tokio::test]
async fn a_question_for_an_old_version_is_not_sent() {
    let (router, _sender, client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 5, "a".into());
    opened(&mut server).await;
    assert!(matches!(
        router.definition(place(4)).await,
        Err(RouteError::Stale)
    ));
    server.assert_silent(&client).await;
}

#[tokio::test]
async fn an_answer_to_a_document_that_changed_meanwhile_is_dropped() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    opened(&mut server).await;

    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.hover(place(1)).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/hover");
    router.change(&uri("A"), 2, "b".into(), Vec::new());
    server.reply(&request, json!({"contents": "old"})).await;
    assert!(matches!(ask.await.unwrap(), Err(RouteError::Stale)));
}

#[tokio::test]
async fn an_answer_for_the_current_version_arrives() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    opened(&mut server).await;
    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.hover(place(1)).await }
    });
    let request = server.read().await;
    assert_eq!(
        request["params"]["position"],
        json!({"line": 2, "character": 3})
    );
    server.reply(&request, json!({"contents": "docs"})).await;
    assert!(ask.await.unwrap().unwrap().is_some());
}

#[tokio::test]
async fn without_a_server_questions_fail_at_once() {
    let (_sender, receiver) = watch::channel(status(None, 0));
    let router = Router::new(receiver);
    router.open(uri("A"), "text", 1, "a".into());
    assert!(matches!(
        router.definition(place(1)).await,
        Err(RouteError::NotReady)
    ));
    assert!(matches!(
        router.workspace_symbol("x").await,
        Err(RouteError::NotReady)
    ));
}

#[tokio::test]
async fn quick_completions_reach_the_server_once_and_the_last_one_wins() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    opened(&mut server).await;

    let first = tokio::spawn({
        let router = router.clone();
        async move { router.completion(place(1)).await }
    });
    tokio::task::yield_now().await;
    let mut last = place(1);
    last.position = Position::new(9, 9);
    let second = tokio::spawn({
        let router = router.clone();
        async move { router.completion(last).await }
    });

    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/completion");
    assert_eq!(request["params"]["position"]["line"], 9);
    server.reply(&request, json!([{"label": "x"}])).await;
    assert!(matches!(first.await.unwrap(), Err(RouteError::Superseded)));
    assert!(second.await.unwrap().unwrap().is_some());
}

#[tokio::test]
async fn a_newer_completion_cancels_the_one_in_flight() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    opened(&mut server).await;

    let first = tokio::spawn({
        let router = router.clone();
        async move { router.completion(place(1)).await }
    });
    let sent = server.read().await;
    let second = tokio::spawn({
        let router = router.clone();
        async move { router.completion(place(1)).await }
    });
    assert!(matches!(first.await.unwrap(), Err(RouteError::Superseded)));
    let cancel = server.read().await;
    assert_eq!(cancel["method"], "$/cancelRequest");
    assert_eq!(cancel["params"]["id"], sent["id"]);

    let request = server.read().await;
    server.reply(&request, json!([])).await;
    assert!(second.await.unwrap().is_ok());
}

#[tokio::test]
async fn completions_of_different_documents_do_not_replace_each_other() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "a".into());
    router.open(uri("B"), "text", 1, "b".into());
    opened(&mut server).await;
    opened(&mut server).await;
    let first = tokio::spawn({
        let router = router.clone();
        async move { router.completion(place(1)).await }
    });
    let mut other = place(1);
    other.uri = uri("B");
    let second = tokio::spawn({
        let router = router.clone();
        async move { router.completion(other).await }
    });
    for _ in 0..2 {
        let request = server.read().await;
        server.reply(&request, json!([])).await;
    }
    assert!(first.await.unwrap().is_ok());
    assert!(second.await.unwrap().is_ok());
}

#[tokio::test]
async fn extension_requests_pass_through() {
    let (router, _sender, _client, mut server) = live();
    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.extension("custom/thing", json!({"a": 1})).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "custom/thing");
    server.reply(&request, json!("result")).await;
    assert_eq!(ask.await.unwrap().unwrap(), "result");
}

#[tokio::test]
async fn published_diagnostics_replace_earlier_ones_and_raise_the_revision() {
    let (published, mut revision) = Published::new();
    let params = |message: &str| {
        json!({
            "uri": uri("A").as_str(),
            "diagnostics": [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 1},
                },
                "severity": 1,
                "message": message,
            }],
        })
    };
    published.notification(PublishDiagnostics::METHOD, params("one"));
    published.notification(PublishDiagnostics::METHOD, params("two"));
    published.notification("other", json!({}));
    timeout(LIMIT, revision.wait_for(|value| *value == 2))
        .await
        .unwrap()
        .unwrap();
    let found = published.diagnostics(&uri("A"));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].message, "two");
    assert!(published.diagnostics(&uri("B")).is_empty());
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
    .with_edit_command("lang.applyEdit")
    .with_refactor_action("lang.applyRefactor", "lang/getRefactorEdit")
    .with_move_requests("lang/getMoveDestinations", "lang/move")
}

#[tokio::test]
async fn move_lists_destinations_and_applies_the_edits_the_server_returns() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "class A {}".into());
    opened(&mut server).await;
    let providers = providers_for(&router);
    let spot = DocumentSpot {
        uri: uri("A").as_str().to_owned(),
        version: 1,
        point: TextPoint::default(),
    };

    let destinations: &MoveDestinationProvider = &providers;
    let mut replies = ask(
        destinations,
        MoveQuery {
            spot: spot.clone(),
            kind: MoveKind::File,
        },
    );
    let request = server.read().await;
    assert_eq!(request["method"], "lang/getMoveDestinations");
    assert_eq!(request["params"]["moveKind"], "moveResource");
    assert_eq!(request["params"]["sourceUris"][0], uri("A").as_str());
    server
        .reply(
            &request,
            json!({"destinations": [{
                "displayName": "org.example.other",
                "project": "demo",
                "path": "/src/main/source/org/example/other",
            }]}),
        )
        .await;
    let answer = timeout(LIMIT, replies.next()).await.unwrap().unwrap();
    assert_eq!(answer.value.len(), 1);
    assert_eq!(answer.value[0].name, "org.example.other");
    assert!(answer.value[0].detail.as_deref().unwrap().contains("demo"));

    let moving: &MoveProvider = &providers;
    let mut replies = ask(
        moving,
        MoveRequest {
            spot,
            kind: MoveKind::File,
            destination: answer.value[0].clone(),
            update_references: true,
        },
    );
    let request = server.read().await;
    assert_eq!(request["method"], "lang/move");
    assert_eq!(
        request["params"]["destination"]["displayName"],
        "org.example.other"
    );
    assert_eq!(request["params"]["updateReferences"], true);
    server
        .reply(
            &request,
            json!({"edit": {"changes": {uri("A").as_str(): [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 7},
                },
                "newText": "package org.example.other;",
            }]}}}),
        )
        .await;
    let outcome = timeout(LIMIT, replies.next()).await.unwrap().unwrap();
    assert!(outcome.value.error.is_none());
    assert_eq!(outcome.value.change.edits.len(), 1);
    assert_eq!(
        outcome.value.change.edits[0].edits[0].text,
        "package org.example.other;"
    );
}

#[tokio::test]
async fn an_introduce_parameter_action_brings_its_edits_and_code() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "class A {}".into());
    opened(&mut server).await;
    let providers = providers_for(&router);

    let actions: &ActionProvider = &providers;
    let mut replies = ask(
        actions,
        ActionQuery {
            uri: uri("A").as_str().to_owned(),
            version: 1,
            span: lang_api::TextSpan::default(),
            only: Some(ActionKind::Refactor),
        },
    );
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/codeAction");
    server
        .reply(
            &request,
            json!([{
                "title": "Introduce parameter",
                "kind": "refactor.introduce.parameter",
                "command": {
                    "title": "Introduce parameter",
                    "command": "lang.applyRefactor",
                    "arguments": ["introduceParameter", {
                        "textDocument": {"uri": uri("A").as_str()},
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 7},
                        },
                        "context": {"diagnostics": []},
                    }],
                },
            }]),
        )
        .await;
    let request = server.read().await;
    assert_eq!(request["method"], "lang/getRefactorEdit");
    assert_eq!(request["params"]["command"], "introduceParameter");
    server
        .reply(
            &request,
            json!({"edit": {"changes": {uri("A").as_str(): [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 7},
                },
                "newText": "class A { void f(String value) {} }",
            }]}}}),
        )
        .await;
    let answer = timeout(LIMIT, replies.next()).await.unwrap().unwrap();
    let entry = &answer.value[0];
    assert_eq!(entry.code.as_deref(), Some("refactor.introduce.parameter"));
    assert_eq!(
        entry.edits[0].edits[0].text,
        "class A { void f(String value) {} }"
    );
}

#[tokio::test]
async fn a_failed_search_answers_unknown_and_a_stale_question_stays_silent() {
    let (_sender, receiver) = watch::channel(status(None, 0));
    let router = Router::new(receiver);
    let (published, revisions) = Published::new();
    let providers = LspProviders::new(
        router.clone(),
        published,
        revisions,
        Handle::current(),
        "text",
        SourceId::new("test"),
    );

    let search: &WorkspaceSymbolProvider = &providers;
    let mut replies = ask(search, SymbolQuery("x".to_owned()));
    let answer = timeout(LIMIT, replies.next()).await.unwrap().unwrap();
    assert_eq!(answer.confidence, Confidence::Unknown);
    assert!(answer.value.is_empty());

    router.open(uri("A"), "text", 2, "a".into());
    let spot = DocumentSpot {
        uri: uri("A").as_str().to_owned(),
        version: 1,
        point: TextPoint::default(),
    };
    let query = NavigationQuery {
        kind: NavigationKind::Definition,
        spot,
    };
    let navigation: &NavigationProvider = &providers;
    let mut replies = ask(navigation, query);
    assert!(timeout(LIMIT, replies.next()).await.unwrap().is_none());
}

#[tokio::test]
async fn inlay_hints_cover_the_range_and_follow_the_edits_of_the_document() {
    let (router, _sender, _client, mut server) = live();
    tokio::task::yield_now().await;
    router.open(uri("A"), "text", 1, "call(true)".into());
    opened(&mut server).await;
    let range = Range::new(Position::new(0, 0), Position::new(0, 9));

    let ask = tokio::spawn({
        let router = router.clone();
        async move { router.inlay_hints(place(1), range).await }
    });
    let request = server.read().await;
    assert_eq!(request["method"], "textDocument/inlayHint");
    assert_eq!(
        request["params"]["range"],
        json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 9}})
    );
    server
        .reply(
            &request,
            json!([{
                "position": {"line": 0, "character": 5},
                "label": "flag:",
                "kind": 2,
                "paddingRight": true
            }]),
        )
        .await;
    let hints = ask.await.unwrap().unwrap().unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].position.character, 5);
    assert_eq!(
        router.cached_inlay_hints(&uri("A"), 1, &range).len(),
        1,
        "the answer is kept for the version it describes"
    );

    let insertion = TextDocumentContentChangeEvent {
        range: Some(Range::new(Position::new(0, 0), Position::new(0, 0))),
        range_length: None,
        text: "x".to_owned(),
    };
    router.change(&uri("A"), 2, "xcall(true)".into(), vec![insertion]);
    let moved = router.cached_inlay_hints(&uri("A"), 2, &range);
    assert_eq!(moved[0].position.character, 6, "the hint follows the text");
    assert!(
        router.cached_inlay_hints(&uri("A"), 1, &range).is_empty(),
        "an answer of an old version is never given back"
    );
    assert!(matches!(
        router.inlay_hints(place(1), range).await,
        Err(RouteError::Stale)
    ));
}
