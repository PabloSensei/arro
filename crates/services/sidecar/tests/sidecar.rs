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
    Launcher, Limits, NotTrusted, Plan, ProcessHandle, ServerProcess, ServerSpec, Sidecar, State,
    Status, Trust, WaitFuture,
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
        ..Limits::default()
    }
}

/// How one launch of the fake server behaves.
#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Good,
    CrashAfterReady,
    NoInitializeReply,
    IgnoresShutdown,
    FailsToSpawn,
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
    methods: Mutex<Vec<String>>,
}

impl ServerEvents for Consumer {
    fn notification(&self, method: &str, _params: Value) {
        self.methods.lock().unwrap().push(method.to_owned());
    }
}

/// The server side of one fake process.
struct FakeServer {
    behaviour: Behaviour,
    client: OnceLock<Client>,
    exit: Mutex<Option<oneshot::Sender<()>>>,
    received: Arc<Mutex<Vec<String>>>,
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
        self.received.lock().unwrap().push(method.to_owned());
        match method {
            "initialized" => {
                let client = self.client.get().unwrap();
                client.raw_notify("hello", Value::Null).unwrap();
                client.raw_notify("ready", Value::Null).unwrap();
                if self.behaviour == Behaviour::CrashAfterReady {
                    self.end();
                }
            }
            "exit" => self.end(),
            _ => {}
        }
    }

    fn request(&self, method: &str, _params: Value) -> Result<Value, ResponseError> {
        self.received.lock().unwrap().push(method.to_owned());
        match method {
            "initialize" if self.behaviour == Behaviour::NoInitializeReply => Err(ResponseError {
                code: -1,
                message: "busy".into(),
                data: None,
            }),
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
    current: Mutex<Option<Arc<FakeServer>>>,
}

impl FakeLauncher {
    fn scripted(behaviours: &[Behaviour]) -> Arc<Self> {
        let launcher = Self::default();
        *launcher.script.lock().unwrap() = behaviours.iter().copied().collect();
        Arc::new(launcher)
    }

    fn crash_current(&self) {
        self.current.lock().unwrap().as_ref().unwrap().end();
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
        if behaviour == Behaviour::FailsToSpawn {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let (client_end, server_end) = duplex(4096);
        let (stdout, stdin) = split(client_end);
        let (server_input, server_output) = split(server_end);
        let (exit, exited) = oneshot::channel();
        let server = Arc::new(FakeServer {
            behaviour,
            client: OnceLock::new(),
            exit: Mutex::new(Some(exit)),
            received: self.received.clone(),
        });
        let client = Client::start(server_input, server_output, server.clone());
        server.client.set(client).ok().unwrap();
        *self.current.lock().unwrap() = Some(server.clone());
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

fn start(launcher: &Arc<FakeLauncher>) -> Sidecar {
    Sidecar::start(
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer::default()),
        limits(),
        Trust::Trusted,
    )
    .ok()
    .unwrap()
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    let current = status.clone();
    let waited = timeout(LIMIT, status.wait_for(test)).await;
    let Ok(found) = waited else {
        let last = current.borrow();
        panic!(
            "stuck in {:?}, crashes {}, generation {}, {:?}",
            last.state, last.crashes, last.generation, last.last_error
        );
    };
    found.unwrap().clone()
}

#[tokio::test]
async fn an_untrusted_project_launches_nothing() {
    let launcher = FakeLauncher::scripted(&[]);
    let result = Sidecar::start(
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer::default()),
        limits(),
        Trust::Untrusted,
    );
    assert_eq!(result.err(), Some(NotTrusted));
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn reaches_ready_and_exposes_the_client() {
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);
    assert_eq!(ready.process_id, Some(4242));
    assert!(sidecar.client().is_some());
    let received = launcher.received.lock().unwrap().clone();
    assert_eq!(received, ["initialize", "initialized"]);
}

#[tokio::test]
async fn the_consumer_sees_server_notifications() {
    let launcher = FakeLauncher::scripted(&[]);
    let consumer = Arc::new(Consumer::default());
    let sidecar = Sidecar::start(
        Arc::new(Spec),
        launcher,
        consumer.clone(),
        limits(),
        Trust::Trusted,
    )
    .ok()
    .unwrap();
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;
    assert!(
        consumer
            .methods
            .lock()
            .unwrap()
            .contains(&"hello".to_owned())
    );
}

#[tokio::test]
async fn a_crash_restarts_with_a_new_generation() {
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    let first = until(&mut status, |status| status.state == State::Ready).await;
    let old_client = first.client.unwrap();

    launcher.crash_current();
    let second = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert_eq!(second.crashes, 1);
    assert_eq!(second.last_error.as_deref(), Some("process ended"));
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert!(second.client.is_some());
    drop(old_client);
}

#[tokio::test]
async fn a_failed_spawn_is_retried() {
    let launcher = FakeLauncher::scripted(&[Behaviour::FailsToSpawn]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.crashes, 1);
    assert!(ready.last_error.unwrap().contains("launch failed"));
}

#[tokio::test]
async fn an_initialize_failure_kills_and_restarts() {
    let launcher = FakeLauncher::scripted(&[Behaviour::NoInitializeReply]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 2);
    assert_eq!(launcher.killed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_crash_loop_ends_in_degraded_and_launches_no_more() {
    let launcher = FakeLauncher::scripted(&[Behaviour::CrashAfterReady; 10]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    let degraded = until(&mut status, |status| status.state == State::Degraded).await;
    assert_eq!(degraded.crashes, 3);
    assert!(degraded.client.is_none());
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 3);

    sidecar.stop().await;
    assert_eq!(status.borrow().state, State::Stopped);
}

#[tokio::test]
async fn stop_is_polite_and_does_not_restart() {
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    timeout(LIMIT, sidecar.stop()).await.unwrap();
    let received = launcher.received.lock().unwrap().clone();
    assert_eq!(received, ["initialize", "initialized", "shutdown", "exit"]);
    assert_eq!(launcher.killed.load(Ordering::SeqCst), 0);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    let last = status.borrow().clone();
    assert_eq!(last.state, State::Stopped);
    assert!(last.client.is_none());
}

#[tokio::test]
async fn stop_kills_a_server_that_ignores_shutdown() {
    let launcher = FakeLauncher::scripted(&[Behaviour::IgnoresShutdown]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    timeout(LIMIT, sidecar.stop()).await.unwrap();
    assert_eq!(launcher.killed.load(Ordering::SeqCst), 1);
    assert_eq!(status.borrow().state, State::Stopped);
}

#[tokio::test]
async fn stop_during_backoff_launches_no_more() {
    let launcher = FakeLauncher::scripted(&[Behaviour::FailsToSpawn; 5]);
    let mut slow = limits();
    slow.backoff = Duration::from_secs(60);
    let sidecar = Sidecar::start(
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer::default()),
        slow,
        Trust::Trusted,
    )
    .ok()
    .unwrap();
    let mut status = sidecar.status();
    until(&mut status, |status| status.crashes == 1).await;
    timeout(LIMIT, sidecar.stop()).await.unwrap();
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn low_memory_stops_a_running_server_and_it_starts_again() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let sidecar = Sidecar::start_with_memory(
        memory,
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer::default()),
        limits(),
        Trust::Trusted,
    )
    .ok()
    .unwrap();
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    let stopped = until(&mut status, |status| status.state == State::Stopped).await;
    assert!(
        stopped
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("memory")),
        "{:?}",
        stopped.last_error
    );
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    assert!(sidecar.restart());
    let second = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert_eq!(second.generation, 2);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test]
async fn low_memory_at_start_does_not_stop_the_server() {
    let launcher = FakeLauncher::scripted(&[]);
    let (_signal, memory) = watch::channel(true);
    let sidecar = Sidecar::start_with_memory(
        memory,
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer::default()),
        limits(),
        Trust::Trusted,
    )
    .ok()
    .unwrap();
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    sleep(Duration::from_millis(50)).await;
    assert_eq!(status.borrow().state, State::Ready);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test]
async fn dropping_the_sidecar_ends_the_process() {
    let launcher = FakeLauncher::scripted(&[]);
    let sidecar = start(&launcher);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(launcher.dropped.load(Ordering::SeqCst), 0);

    drop(sidecar);
    timeout(LIMIT, async {
        while launcher.dropped.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
