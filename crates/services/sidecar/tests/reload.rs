//! Starting the server again: the status forgets the old connection, the server gets
//! the refresh requests of its consumer, and a dropped sidecar says goodbye first.

use std::collections::VecDeque;
use std::future::pending;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lsp_client::lsp_types::{ClientCapabilities, InitializeParams};
use lsp_client::{Client, ResponseError, ServerEvents};
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

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Good,
    IgnoresShutdown,
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

#[derive(Default)]
struct Consumer {
    requests: Mutex<Vec<String>>,
}

impl ServerEvents for Consumer {
    fn notification(&self, _method: &str, _params: Value) {}

    fn request(&self, method: &str, _params: Value) -> Result<Value, ResponseError> {
        self.requests.lock().unwrap().push(method.to_owned());
        Ok(Value::Null)
    }
}

struct FakeServer {
    behaviour: Behaviour,
    client: OnceLock<Client>,
    exit: Mutex<Option<oneshot::Sender<()>>>,
}

impl FakeServer {
    fn end(&self) {
        if let Some(exit) = self.exit.lock().unwrap().take() {
            let _ = exit.send(());
        }
    }
}

impl ServerEvents for FakeServer {
    fn notification(&self, method: &str, _params: Value) {
        if method != "initialized" {
            if method == "exit" {
                self.end();
            }
            return;
        }
        let client = self.client.get().unwrap();
        client.raw_notify("ready", Value::Null).unwrap();
        // The server asks the editor for new labels, as a language server does after a
        // change of the project.
        let client = client.clone();
        tokio::spawn(async move {
            let _ = client
                .raw_request("workspace/codeLens/refresh", Value::Null)
                .await;
        });
    }

    fn request(&self, method: &str, _params: Value) -> Result<Value, ResponseError> {
        match method {
            "initialize" => Ok(json!({"capabilities": {}})),
            "shutdown" if self.behaviour == Behaviour::IgnoresShutdown => Err(ResponseError {
                code: -1,
                message: "no".into(),
                data: None,
            }),
            _ => Ok(Value::Null),
        }
    }
}

struct FakeHandle {
    exit: Option<oneshot::Receiver<()>>,
    server: Arc<FakeServer>,
    killed: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
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

impl Drop for FakeHandle {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct FakeLauncher {
    script: Mutex<VecDeque<Behaviour>>,
    launches: AtomicUsize,
    killed: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
    received: Arc<Mutex<Vec<String>>>,
}

impl FakeLauncher {
    fn scripted(behaviours: &[Behaviour]) -> Arc<Self> {
        let launcher = Self::default();
        *launcher.script.lock().unwrap() = behaviours.iter().copied().collect();
        Arc::new(launcher)
    }
}

impl Launcher for FakeLauncher {
    fn launch(&self, _plan: &Plan) -> io::Result<ServerProcess> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let behaviour = self
            .script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Behaviour::Good);
        let (client_end, server_end) = duplex(4096);
        let (stdout, stdin) = split(client_end);
        let (server_input, server_output) = split(server_end);
        let (exit, exited) = oneshot::channel();
        let received = self.received.clone();
        let server = Arc::new(FakeServer {
            behaviour,
            client: OnceLock::new(),
            exit: Mutex::new(Some(exit)),
        });
        let events = Arc::new(Recording {
            methods: received,
            server: server.clone(),
        });
        let client = Client::start(server_input, server_output, events);
        server.client.set(client).ok().unwrap();
        let handle = FakeHandle {
            exit: Some(exited),
            server,
            killed: self.killed.clone(),
            dropped: self.dropped.clone(),
        };
        Ok(ServerProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            handle: Box::new(handle),
        })
    }
}

/// Records what the server received and lets the fake server answer it.
struct Recording {
    methods: Arc<Mutex<Vec<String>>>,
    server: Arc<FakeServer>,
}

impl ServerEvents for Recording {
    fn notification(&self, method: &str, params: Value) {
        self.methods.lock().unwrap().push(method.to_owned());
        self.server.notification(method, params);
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        self.methods.lock().unwrap().push(method.to_owned());
        self.server.request(method, params)
    }
}

fn start(launcher: &Arc<FakeLauncher>, consumer: Arc<Consumer>) -> Sidecar {
    Sidecar::start(
        Arc::new(Spec),
        launcher.clone(),
        consumer,
        limits(),
        Trust::Trusted,
    )
    .ok()
    .unwrap()
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    timeout(LIMIT, status.wait_for(test))
        .await
        .expect("the sidecar did not reach the state in time")
        .unwrap()
        .clone()
}

#[tokio::test]
async fn a_reload_forgets_the_old_client_before_the_next_generation() {
    let launcher = FakeLauncher::scripted(&[Behaviour::IgnoresShutdown, Behaviour::Good]);
    let sidecar = start(&launcher, Arc::new(Consumer::default()));
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    assert!(sidecar.reload());
    let stopping = until(&mut status, |status| status.state == State::Stopping).await;
    assert!(stopping.client.is_none(), "the old connection is gone");
    assert!(!stopping.alive, "the old generation cannot move the state");

    let next = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert!(next.client.is_some());
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test]
async fn a_code_lens_refresh_request_reaches_the_consumer() {
    let consumer = Arc::new(Consumer::default());
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher, consumer.clone());
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    timeout(LIMIT, async {
        while !consumer
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "workspace/codeLens/refresh")
        {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the refresh request did not reach the consumer");

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_sidecar_sends_shutdown_and_exit() {
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher, Arc::new(Consumer::default()));
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    drop(sidecar);

    let received = launcher.received.lock().unwrap().clone();
    assert!(
        received.iter().any(|method| method == "shutdown"),
        "{received:?}"
    );
    assert!(
        received.iter().any(|method| method == "exit"),
        "{received:?}"
    );
    assert_eq!(launcher.killed.load(Ordering::SeqCst), 0);
}
