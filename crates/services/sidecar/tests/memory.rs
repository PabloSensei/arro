//! Stops for low memory: the server waits for memory to stay free and then
//! starts again, unless short stops repeat too often.

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
use tokio::time::{Instant, advance, timeout};

/// Long enough that a broken wait reaches it only after the virtual clock jumps
/// to the next timer, which makes a stuck test fail instead of hang.
const LIMIT: Duration = Duration::from_secs(3600);

const AUTOMATIC: &str = "memory is low, it starts again when memory frees";
const MANUAL: &str = "memory is low, restart it manually";

fn limits() -> Limits {
    Limits {
        initialize: Duration::from_secs(5),
        shutdown: Duration::from_millis(200),
        ..Limits::default()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    Good,
    CrashAfterReady,
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
struct Consumer;

impl ServerEvents for Consumer {
    fn notification(&self, _method: &str, _params: Value) {}
}

/// The server side of one fake process.
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
        match method {
            "initialized" => {
                let client = self.client.get().unwrap();
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
        match method {
            "initialize" => Ok(json!({"capabilities": {}})),
            _ => Ok(Value::Null),
        }
    }
}

struct FakeHandle {
    exit: Option<oneshot::Receiver<()>>,
    server: Arc<FakeServer>,
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
        self.server.end();
    }
}

#[derive(Default)]
struct FakeLauncher {
    script: Mutex<VecDeque<Behaviour>>,
    launches: AtomicUsize,
}

impl FakeLauncher {
    fn scripted(behaviours: &[Behaviour]) -> Arc<Self> {
        let launcher = Self::default();
        *launcher.script.lock().unwrap() = behaviours.iter().copied().collect();
        Arc::new(launcher)
    }

    fn launches(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
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
        let server = Arc::new(FakeServer {
            behaviour,
            client: OnceLock::new(),
            exit: Mutex::new(Some(exit)),
        });
        let client = Client::start(server_input, server_output, server.clone());
        server.client.set(client).ok().unwrap();
        let handle = FakeHandle {
            exit: Some(exited),
            server,
        };
        Ok(ServerProcess {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            handle: Box::new(handle),
        })
    }
}

fn start(launcher: &Arc<FakeLauncher>, memory: watch::Receiver<bool>, limits: Limits) -> Sidecar {
    Sidecar::start_with_memory(
        memory,
        Arc::new(Spec),
        launcher.clone(),
        Arc::new(Consumer),
        limits,
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

/// A wait without timers, for checks that run while the clock is frozen.
async fn until_now(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    for _ in 0..10_000 {
        if test(&status.borrow()) {
            return status.borrow().clone();
        }
        tokio::task::yield_now().await;
    }
    let last = status.borrow();
    panic!(
        "stuck in {:?}, crashes {}, generation {}, {:?}",
        last.state, last.crashes, last.generation, last.last_error
    );
}

/// Lets the supervisor and the fake server run without moving the clock.
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Keeps the paused clock from jumping to the next timer while a test places an
/// event in the middle of a hold.
struct Frozen {
    release: Option<std::sync::mpsc::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Frozen {
    fn new() -> Self {
        let (release, held) = std::sync::mpsc::channel();
        let task = tokio::task::spawn_blocking(move || {
            let _ = held.recv();
        });
        Self {
            release: Some(release),
            task: Some(task),
        }
    }

    async fn end(mut self) {
        drop(self.release.take());
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        drop(self.release.take());
    }
}

#[tokio::test(start_paused = true)]
async fn it_waits_for_memory_to_stay_high_before_starting_again() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    let hold = Duration::from_secs(30);
    limits.memory_wait = hold;
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    let stopped = until(&mut status, |status| status.state == State::Stopped).await;
    assert_eq!(stopped.last_error.as_deref(), Some(AUTOMATIC));
    assert_eq!(launcher.launches(), 1);

    // While memory is low nothing starts, however long the sidecar waits.
    advance(Duration::from_secs(600)).await;
    assert_eq!(launcher.launches(), 1);
    assert_eq!(status.borrow().state, State::Stopped);

    let freed = Instant::now();
    signal.send(false).unwrap();
    let ready = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert!(
        freed.elapsed() >= hold,
        "started after {:?}",
        freed.elapsed()
    );
    assert_eq!(ready.generation, 2);
    assert_eq!(launcher.launches(), 2);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_new_low_reading_starts_the_hold_over() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;

    let frozen = Frozen::new();
    signal.send(false).unwrap();
    settle().await;
    advance(Duration::from_secs(10)).await;
    settle().await;

    // Memory drops again in the middle of the hold.
    signal.send(true).unwrap();
    settle().await;
    advance(Duration::from_secs(5)).await;
    settle().await;
    signal.send(false).unwrap();
    settle().await;

    // The hold began again, so the first timer must not start the server.
    advance(Duration::from_secs(20)).await;
    settle().await;
    assert_eq!(status.borrow().state, State::Stopped);
    assert_eq!(launcher.launches(), 1);

    advance(Duration::from_secs(11)).await;
    let ready = until_now(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert_eq!(ready.generation, 2);
    assert_eq!(launcher.launches(), 2);

    frozen.end().await;
    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn short_stops_double_the_hold_and_turn_starts_off() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    limits.memory_wait_max = Duration::from_secs(300);
    limits.memory_attempts = 2;
    limits.stable = Duration::from_secs(60);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    // The first stop is not after an automatic start: the base hold applies.
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    let freed = Instant::now();
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert!(freed.elapsed() >= Duration::from_secs(30));
    assert_eq!(launcher.launches(), 2);

    // A short life after an automatic start doubles the next hold.
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    let freed = Instant::now();
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 3
    })
    .await;
    assert!(
        freed.elapsed() >= Duration::from_secs(60),
        "started after {:?}",
        freed.elapsed()
    );
    assert_eq!(launcher.launches(), 3);

    // The second short stop turns automatic starts off.
    signal.send(true).unwrap();
    let stopped = until(&mut status, |status| status.state == State::Stopped).await;
    assert_eq!(stopped.last_error.as_deref(), Some(MANUAL));

    advance(Duration::from_secs(600)).await;
    assert_eq!(status.borrow().state, State::Stopped);
    assert_eq!(launcher.launches(), 3);

    assert!(sidecar.restart());
    let ready = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 4
    })
    .await;
    assert_eq!(ready.generation, 4);
    assert_eq!(launcher.launches(), 4);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_hold_stops_growing_at_the_cap() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    limits.memory_wait_max = Duration::from_secs(45);
    limits.memory_attempts = 3;
    limits.stable = Duration::from_secs(60);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;

    // The doubled hold would be 60 seconds, but the cap is 45.
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    let freed = Instant::now();
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 3
    })
    .await;
    assert!(
        freed.elapsed() >= Duration::from_secs(45),
        "started after {:?}",
        freed.elapsed()
    );
    assert!(
        freed.elapsed() < Duration::from_secs(60),
        "started after {:?}",
        freed.elapsed()
    );

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_long_life_resets_the_short_stop_series() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    limits.memory_wait_max = Duration::from_secs(300);
    limits.memory_attempts = 3;
    limits.stable = Duration::from_secs(60);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    // The first automatic start stops soon, which doubles the next hold.
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 3
    })
    .await;

    // A life that reaches `stable` starts the series over: the hold is the base.
    advance(Duration::from_secs(60)).await;
    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    let freed = Instant::now();
    signal.send(false).unwrap();
    until(&mut status, |status| {
        status.state == State::Ready && status.generation == 4
    })
    .await;
    assert!(
        freed.elapsed() >= Duration::from_secs(30),
        "started after {:?}",
        freed.elapsed()
    );
    assert!(
        freed.elapsed() < Duration::from_secs(60),
        "started after {:?}",
        freed.elapsed()
    );

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_restart_during_the_wait_starts_at_once() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;

    let asked = Instant::now();
    assert!(sidecar.restart());
    let ready = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert!(
        asked.elapsed() < Duration::from_secs(30),
        "started after {:?}",
        asked.elapsed()
    );
    assert_eq!(ready.generation, 2);
    assert_eq!(launcher.launches(), 2);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_stop_during_the_wait_ends_the_sidecar() {
    let launcher = FakeLauncher::scripted(&[]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    limits.memory_wait = Duration::from_secs(30);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.state == State::Ready).await;

    signal.send(true).unwrap();
    until(&mut status, |status| status.state == State::Stopped).await;
    signal.send(false).unwrap();

    timeout(LIMIT, sidecar.stop()).await.unwrap();
    assert_eq!(status.borrow().state, State::Stopped);
    assert_eq!(launcher.launches(), 1);

    advance(Duration::from_secs(600)).await;
    assert_eq!(status.borrow().state, State::Stopped);
    assert_eq!(launcher.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn low_memory_during_backoff_also_waits_for_memory() {
    let launcher = FakeLauncher::scripted(&[Behaviour::CrashAfterReady]);
    let (signal, memory) = watch::channel(false);
    let mut limits = limits();
    let hold = Duration::from_secs(30);
    limits.memory_wait = hold;
    limits.backoff = Duration::from_secs(600);
    let sidecar = start(&launcher, memory, limits);
    let mut status = sidecar.status();
    until(&mut status, |status| status.crashes == 1).await;

    signal.send(true).unwrap();
    let stopped = until(&mut status, |status| status.state == State::Stopped).await;
    assert_eq!(stopped.last_error.as_deref(), Some(AUTOMATIC));
    assert_eq!(launcher.launches(), 1);

    let freed = Instant::now();
    signal.send(false).unwrap();
    let ready = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert!(freed.elapsed() >= hold);
    assert_eq!(ready.generation, 2);
    assert_eq!(launcher.launches(), 2);

    timeout(LIMIT, sidecar.stop()).await.unwrap();
}
