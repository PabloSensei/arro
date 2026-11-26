//! A restarted server receives the documents that stayed open in the editor.

use std::future::pending;
use std::io;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lsp_client::lsp_types::{ClientCapabilities, InitializeParams, Uri};
use lsp_client::{Client, ResponseError, ServerEvents};
use semantic_router::{Published, Router};
use serde_json::{Value, json};
use sidecar::{
    Launcher, Limits, Plan, ProcessHandle, ServerProcess, ServerSpec, Sidecar, State, Status,
    Trust, WaitFuture,
};
use tokio::io::{duplex, split};
use tokio::sync::{oneshot, watch};
use tokio::time::{sleep, timeout};

const LIMIT: Duration = Duration::from_secs(5);

fn limits() -> Limits {
    Limits {
        initialize: Duration::from_millis(200),
        shutdown: Duration::from_millis(200),
        restarts: 2,
        backoff: Duration::from_millis(5),
        stable: Duration::from_secs(60),
        memory_hold: Duration::ZERO,
        ..Limits::default()
    }
}

struct Spec;

impl ServerSpec for Spec {
    fn plan(&self) -> Plan {
        Plan::default()
    }

    fn initialize_params(&self) -> InitializeParams {
        InitializeParams {
            capabilities: ClientCapabilities::default(),
            ..Default::default()
        }
    }

    fn state_of(&self, method: &str, _params: &Value) -> Option<State> {
        (method == "ready").then_some(State::Ready)
    }
}

struct FakeServer {
    client: OnceLock<Client>,
    exit: Mutex<Option<oneshot::Sender<()>>>,
    received: Mutex<Vec<String>>,
}

impl FakeServer {
    fn end(&self) {
        if let Some(exit) = self.exit.lock().unwrap().take() {
            let _ = exit.send(());
        }
    }

    fn count(&self, method: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.as_str() == method)
            .count()
    }
}

impl ServerEvents for FakeServer {
    fn notification(&self, method: &str, _params: Value) {
        self.received.lock().unwrap().push(method.to_owned());
        match method {
            "initialized" => {
                let client = self.client.get().unwrap().clone();
                client.raw_notify("ready", Value::Null).unwrap();
                // The server asks the editor to draw its labels again, as a language
                // server does after the project changed.
                tokio::spawn(async move {
                    let _ = client
                        .raw_request("workspace/codeLens/refresh", Value::Null)
                        .await;
                });
            }
            "exit" => self.end(),
            _ => {}
        }
    }

    fn request(&self, method: &str, _params: Value) -> Result<Value, ResponseError> {
        self.received.lock().unwrap().push(method.to_owned());
        match method {
            "initialize" => Ok(json!({"capabilities": {}})),
            _ => Ok(Value::Null),
        }
    }
}

struct FakeHandle {
    exit: Option<oneshot::Receiver<()>>,
    server: Arc<FakeServer>,
    killed: Arc<AtomicUsize>,
}

impl ProcessHandle for FakeHandle {
    fn id(&self) -> Option<u32> {
        Some(4242)
    }

    fn wait(&mut self) -> WaitFuture<'_> {
        Box::pin(async {
            match self.exit.as_mut() {
                Some(exit) => {
                    let _ = exit.await;
                    self.exit = None;
                }
                None => pending::<()>().await,
            }
        })
    }

    fn kill(&mut self) {
        self.killed.fetch_add(1, Ordering::SeqCst);
        self.server.end();
    }
}

#[derive(Default)]
struct FakeLauncher {
    servers: Mutex<Vec<Arc<FakeServer>>>,
    killed: Arc<AtomicUsize>,
}

impl Launcher for FakeLauncher {
    fn launch(&self, _plan: &Plan) -> io::Result<ServerProcess> {
        let (client_end, server_end) = duplex(4096);
        let (stdout, stdin) = split(client_end);
        let (server_input, server_output) = split(server_end);
        let (exit, exited) = oneshot::channel();
        let server = Arc::new(FakeServer {
            client: OnceLock::new(),
            exit: Mutex::new(Some(exit)),
            received: Mutex::new(Vec::new()),
        });
        let client = Client::start(server_input, server_output, server.clone());
        server.client.set(client).ok().unwrap();
        self.servers.lock().unwrap().push(server.clone());
        let handle = FakeHandle {
            exit: Some(exited),
            server,
            killed: self.killed.clone(),
        };
        Ok(ServerProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            handle: Box::new(handle),
        })
    }
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    timeout(LIMIT, status.wait_for(test))
        .await
        .expect("the sidecar did not reach the state in time")
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reloaded_server_receives_the_documents_that_stayed_open() {
    let uri = Uri::from_str("file:///C:/project/A.txt").unwrap();
    let launcher = Arc::new(FakeLauncher::default());
    let (published, _revisions) = Published::new();
    let sidecar = Sidecar::start(
        Arc::new(Spec),
        launcher.clone(),
        published,
        limits(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    let router = Router::new(sidecar.status());
    until(&mut status, |status| status.state == State::Ready).await;

    router.open(uri.clone(), "text", 1, "class A {}".into());
    timeout(LIMIT, async {
        while launcher
            .servers
            .lock()
            .unwrap()
            .first()
            .map(|server| server.count("textDocument/didOpen"))
            .unwrap_or(0)
            == 0
        {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the first server did not receive the document");

    assert!(sidecar.reload());
    timeout(LIMIT, async {
        loop {
            let replayed = launcher
                .servers
                .lock()
                .unwrap()
                .get(1)
                .map(|server| server.count("textDocument/didOpen"))
                .unwrap_or(0);
            if replayed > 0 {
                return;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the second server did not receive the open document");

    let next = until(&mut status, |status| status.generation == 2).await;
    assert!(next.client.is_some());
    assert_eq!(launcher.killed.load(Ordering::SeqCst), 0);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_code_lens_refresh_request_bumps_the_publisher() {
    let launcher = Arc::new(FakeLauncher::default());
    let (published, _revisions) = Published::new();
    let mut refresh = published.code_lens_refresh();
    let sidecar = Sidecar::start(
        Arc::new(Spec),
        launcher,
        published,
        limits(),
        Trust::Trusted,
    )
    .unwrap();
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    timeout(LIMIT, refresh.wait_for(|revision| *revision > 0))
        .await
        .expect("the refresh request did not reach the publisher")
        .unwrap();

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}
