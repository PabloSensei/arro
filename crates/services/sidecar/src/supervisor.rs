use std::fmt;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use lsp_client::lsp_types::notification::Initialized;
use lsp_client::lsp_types::request::Initialize;
use lsp_client::lsp_types::{InitializeParams, InitializeResult, InitializedParams};
use lsp_client::{Client, Error as LspError, ResponseError, ServerEvents};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, timeout};

use crate::process::{Launcher, Plan, ProcessHandle, ServerProcess};

/// The states of `docs/ARCHITECTURE.md`, section 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Stopped,
    Starting,
    /// Started, answers may be incomplete until `Ready`.
    Importing,
    Ready,
    /// Gave up restarting. The editor keeps working without the server.
    Degraded,
    Stopping,
}

/// One server for one project.
pub trait ServerSpec: Send + Sync + 'static {
    /// Why the server cannot start at all. The supervisor does not launch it and
    /// stays `Degraded` with this text until the user asks for a restart.
    fn preflight(&self) -> Result<(), String> {
        Ok(())
    }

    /// Called before every launch, so that a spec can tell its fresh output from
    /// the output of an earlier start.
    fn starting(&self) {}

    /// The cause of a start that ended, read from the output of the server. The
    /// supervisor shows it instead of its own generic text.
    fn crash_reason(&self) -> Option<String> {
        None
    }

    fn plan(&self) -> Plan;

    /// The server ended on its own with `exit_code`. A spec that reads it as out of
    /// memory plans more for the next start.
    fn crashed(&self, _exit_code: Option<i32>) {}

    fn initialize_params(&self) -> InitializeParams;

    /// The answer of `initialize`: what the server said it can do. A spec reads the
    /// parts that later requests depend on.
    fn initialize_result(&self, result: &InitializeResult) {
        let _ = result;
    }

    /// Sent right after `initialized`, for what the server wants on top of it.
    fn initialized(&self, client: &Client) {
        let _ = client;
    }

    /// The state this notification means, if it moves the server forward.
    fn state_of(&self, method: &str, params: &Value) -> Option<State>;

    /// Answers a request from the server. Runs on the reader task, must not wait.
    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        let _ = params;
        Err(ResponseError::method_not_found(method))
    }
}

/// The user's decision about a project. Nothing runs in an untrusted one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    Trusted,
    Untrusted,
}

#[derive(Debug, PartialEq, Eq)]
pub struct NotTrusted;

impl fmt::Display for NotTrusted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the project is not trusted, the server stays off")
    }
}

impl std::error::Error for NotTrusted {}

#[derive(Clone, Debug)]
pub struct Limits {
    /// For `initialize`. An import happens after it, so this stays short.
    pub initialize: Duration,
    /// For `shutdown` and for the process to end after it.
    pub shutdown: Duration,
    /// Crashes in a row before `Degraded`.
    pub restarts: u32,
    /// Waited before restart `n`, multiplied by `n`.
    pub backoff: Duration,
    /// A server that lived this long starts a new series of crashes.
    pub stable: Duration,
    /// How long memory must stay high after a stop for low memory before the
    /// server starts again on its own.
    pub memory_wait: Duration,
    /// The longest hold after repeated short stops for memory.
    pub memory_wait_max: Duration,
    /// Short stops for memory in a row before automatic starts turn off until
    /// the user asks for one.
    pub memory_attempts: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            initialize: Duration::from_secs(60),
            shutdown: Duration::from_secs(5),
            restarts: 3,
            backoff: Duration::from_secs(1),
            stable: Duration::from_secs(60),
            memory_wait: Duration::from_secs(30),
            memory_wait_max: Duration::from_secs(300),
            memory_attempts: 3,
        }
    }
}

#[derive(Clone)]
pub struct Status {
    pub state: State,
    /// Grows with every launch, so that a consumer sees a new server and
    /// replays its open documents.
    pub generation: u64,
    /// False once this generation has ended: its last messages may still be in
    /// the pipe and must not move the state.
    pub alive: bool,
    /// Present from `initialized` until the server ends.
    pub client: Option<Client>,
    pub process_id: Option<u32>,
    /// Restarts since the last stable run.
    pub crashes: u32,
    pub last_error: Option<String>,
}

/// Passes server traffic to its consumer and moves the state.
struct Bridge {
    spec: Arc<dyn ServerSpec>,
    consumer: Arc<dyn ServerEvents>,
    status: watch::Sender<Status>,
    generation: u64,
}

impl ServerEvents for Bridge {
    fn notification(&self, method: &str, params: Value) {
        if let Some(state) = self.spec.state_of(method, &params) {
            self.status.send_if_modified(|status| {
                let current = status.generation == self.generation && status.alive;
                let forward = state == State::Ready || status.state == State::Starting;
                let changed = current && forward && status.state != state;
                if changed {
                    status.state = state;
                }
                changed
            });
        }
        self.consumer.notification(method, params);
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, ResponseError> {
        self.spec.request(method, params)
    }
}

/// Handle to the supervising task. Dropping it ends the server.
pub struct Sidecar {
    status: watch::Receiver<Status>,
    stop: mpsc::Sender<()>,
    restart: mpsc::Sender<()>,
    reload: mpsc::Sender<()>,
    task: JoinHandle<()>,
}

impl Sidecar {
    /// Starts supervising on the current tokio runtime and returns at once.
    pub fn start(
        spec: Arc<dyn ServerSpec>,
        launcher: Arc<dyn Launcher>,
        consumer: Arc<dyn ServerEvents>,
        limits: Limits,
        trust: Trust,
    ) -> Result<Self, NotTrusted> {
        let memory = platform::MemoryPressure::get().watch();
        Self::start_with_memory(memory, spec, launcher, consumer, limits, trust)
    }

    /// Starts with a given low-memory signal instead of the one of the system, for
    /// tests. The value at the start is the baseline: only a later change stops a
    /// server that is running.
    pub fn start_with_memory(
        memory: watch::Receiver<bool>,
        spec: Arc<dyn ServerSpec>,
        launcher: Arc<dyn Launcher>,
        consumer: Arc<dyn ServerEvents>,
        limits: Limits,
        trust: Trust,
    ) -> Result<Self, NotTrusted> {
        if trust == Trust::Untrusted {
            return Err(NotTrusted);
        }
        let (sender, status) = watch::channel(Status {
            state: State::Starting,
            generation: 0,
            alive: false,
            client: None,
            process_id: None,
            crashes: 0,
            last_error: None,
        });
        let (stop, stops) = mpsc::channel(1);
        let (restart, restarts) = mpsc::channel(1);
        let (reload, reloads) = mpsc::channel(1);
        let supervisor = Supervisor {
            spec,
            launcher,
            consumer,
            limits,
            status: sender,
            stops,
            restarts,
            reloads,
            memory,
        };
        let task = tokio::spawn(supervisor.run());
        Ok(Self {
            status,
            stop,
            restart,
            reload,
            task,
        })
    }

    /// Starts the server again after it has stopped, for example for low memory.
    /// False when it is not stopped or cannot start again.
    pub fn restart(&self) -> bool {
        if !matches!(self.status.borrow().state, State::Stopped | State::Degraded) {
            return false;
        }
        match self.restart.try_send(()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(())) => true,
            Err(mpsc::error::TrySendError::Closed(())) => false,
        }
    }

    /// Starts a running server again, for a setting that it reads only at startup. The
    /// documents and the state folder stay, so the window does not notice. False when
    /// the server is not running.
    pub fn reload(&self) -> bool {
        self.reload_handle().reload()
    }

    /// The same request for a background task that does not own the sidecar.
    pub fn reload_handle(&self) -> ReloadHandle {
        ReloadHandle {
            status: self.status.clone(),
            reload: self.reload.clone(),
        }
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    pub fn client(&self) -> Option<Client> {
        self.status.borrow().client.clone()
    }

    /// Stops the server politely, then by force, and waits for the end.
    pub async fn stop(mut self) {
        // A full queue means a stop is already on its way.
        let _ = self.stop.try_send(());
        let _ = (&mut self.task).await;
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Drops the process handle, which ends the server.
        self.task.abort();
    }
}

/// A request to start a running server again, held by a task that does not own the
/// sidecar.
#[derive(Clone)]
pub struct ReloadHandle {
    status: watch::Receiver<Status>,
    reload: mpsc::Sender<()>,
}

impl ReloadHandle {
    /// False when the server is not running; the next start reads the new setting when
    /// it begins.
    pub fn reload(&self) -> bool {
        if matches!(self.status.borrow().state, State::Stopped | State::Degraded) {
            return false;
        }
        match self.reload.try_send(()) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(())) => true,
            Err(mpsc::error::TrySendError::Closed(())) => false,
        }
    }
}

struct Running {
    handle: Box<dyn ProcessHandle>,
    client: Client,
}

enum Outcome {
    Crashed(String),
    /// The user ended the sidecar.
    Stop,
    /// Memory became low while this server ran.
    LowMemory,
    /// The user changed a setting that only a new process reads.
    Reload,
}

enum Backoff {
    Elapsed,
    Stop,
    LowMemory,
}

/// Status lines for a stop for low memory, shown in the window.
const MEMORY_AUTOMATIC: &str = "memory is low, it starts again when memory frees";
const MEMORY_MANUAL: &str = "memory is low, restart it manually";

/// What the supervisor does after a stop for low memory.
enum AfterMemory {
    /// Memory stayed high long enough: start without the user.
    Auto,
    /// The user asked for a start after automatic ones were turned off.
    Manual,
    /// The sidecar ends instead of starting again.
    Stop,
}

enum MemoryWait {
    /// Memory stayed high for the whole hold.
    Elapsed,
    /// The user asked for a start.
    Restart,
    /// The sidecar ends.
    Stop,
}

struct Supervisor {
    spec: Arc<dyn ServerSpec>,
    launcher: Arc<dyn Launcher>,
    consumer: Arc<dyn ServerEvents>,
    limits: Limits,
    status: watch::Sender<Status>,
    stops: mpsc::Receiver<()>,
    restarts: mpsc::Receiver<()>,
    reloads: mpsc::Receiver<()>,
    memory: watch::Receiver<bool>,
}

/// Waits for the next change of the memory state and returns when it is low. The
/// value seen before the call is the baseline, so a server that starts while memory
/// is already low is not stopped by that state.
async fn low_memory(memory: &mut watch::Receiver<bool>) {
    loop {
        if memory.changed().await.is_err() {
            // The watcher lives as long as the process; without it nothing can change.
            std::future::pending::<()>().await;
        }
        if *memory.borrow_and_update() {
            return;
        }
    }
}

/// The user's restart either starts the server or, when the channel is gone,
/// ends the sidecar like a stop.
fn user_started(started: Option<()>) -> MemoryWait {
    if started.is_some() {
        MemoryWait::Restart
    } else {
        MemoryWait::Stop
    }
}

impl Supervisor {
    async fn run(mut self) {
        let mut crashes = 0;
        // Short stops for memory after automatic starts, in a row.
        let mut memory_stops = 0;
        // Set when the current life was started by the memory timer.
        let mut auto_started = false;
        let mut automatic = false;
        loop {
            if let Err(reason) = self.spec.preflight() {
                self.cannot_start(reason);
                if !self.wait_for_restart().await {
                    break;
                }
                // The user may have installed the JDK the server asked for.
                crashes = 0;
                memory_stops = 0;
                continue;
            }
            let began = Instant::now();
            if automatic {
                auto_started = true;
            }
            automatic = false;
            match self.one_life().await {
                Outcome::Stop => break,
                Outcome::Reload => {
                    auto_started = false;
                    crashes = 0;
                    memory_stops = 0;
                }
                Outcome::LowMemory => {
                    let short = std::mem::take(&mut auto_started)
                        .then(|| began.elapsed() < self.limits.stable);
                    match self.wait_after_memory(short, &mut memory_stops).await {
                        AfterMemory::Stop => break,
                        AfterMemory::Auto => {
                            crashes = 0;
                            automatic = true;
                        }
                        AfterMemory::Manual => {
                            crashes = 0;
                            memory_stops = 0;
                        }
                    }
                }
                Outcome::Crashed(error) => {
                    auto_started = false;
                    if began.elapsed() >= self.limits.stable {
                        crashes = 0;
                    }
                    crashes += 1;
                    self.status.send_modify(|status| {
                        status.alive = false;
                        status.client = None;
                        status.process_id = None;
                        status.crashes = crashes;
                        status.last_error = Some(error);
                        status.state = State::Starting;
                    });
                    if crashes > self.limits.restarts {
                        self.status
                            .send_modify(|status| status.state = State::Degraded);
                        if !self.wait_for_restart().await {
                            break;
                        }
                        crashes = 0;
                        memory_stops = 0;
                        continue;
                    }
                    let backoff = self.limits.backoff * crashes;
                    let event = tokio::select! {
                        () = sleep(backoff) => Backoff::Elapsed,
                        _ = self.stops.recv() => Backoff::Stop,
                        _ = low_memory(&mut self.memory) => Backoff::LowMemory,
                    };
                    match event {
                        Backoff::Elapsed => {}
                        Backoff::Stop => break,
                        Backoff::LowMemory => {
                            // The life ended by a crash, so this is not a short
                            // stop after an automatic start.
                            match self.wait_after_memory(None, &mut memory_stops).await {
                                AfterMemory::Stop => break,
                                AfterMemory::Auto => {
                                    crashes = 0;
                                    automatic = true;
                                }
                                AfterMemory::Manual => {
                                    crashes = 0;
                                    memory_stops = 0;
                                }
                            }
                        }
                    }
                }
            }
        }
        self.status.send_modify(|status| {
            status.alive = false;
            status.client = None;
            status.process_id = None;
            status.state = State::Stopped;
        });
    }

    /// Publishes a stop for low memory and waits. `short` says whether the life
    /// that ended was an automatic start that did not live to `stable`.
    async fn wait_after_memory(
        &mut self,
        short: Option<bool>,
        memory_stops: &mut u32,
    ) -> AfterMemory {
        if let Some(short) = short {
            *memory_stops = if short {
                (*memory_stops).saturating_add(1)
            } else {
                0
            };
        }
        if *memory_stops >= self.limits.memory_attempts {
            self.stopped_for(MEMORY_MANUAL);
            return if self.wait_for_restart().await {
                AfterMemory::Manual
            } else {
                AfterMemory::Stop
            };
        }
        self.stopped_for(MEMORY_AUTOMATIC);
        let hold = self.memory_hold(*memory_stops);
        match self.wait_for_memory(hold).await {
            MemoryWait::Elapsed => AfterMemory::Auto,
            MemoryWait::Restart => AfterMemory::Manual,
            MemoryWait::Stop => AfterMemory::Stop,
        }
    }

    /// The hold doubles with every short stop in the series, up to the cap.
    fn memory_hold(&self, memory_stops: u32) -> Duration {
        let factor = 1u32 << memory_stops.min(16);
        self.limits
            .memory_wait
            .saturating_mul(factor)
            .min(self.limits.memory_wait_max)
    }

    /// Waits until memory has stayed high for `hold`. A new low reading starts
    /// the hold over, and the user can start or stop the sidecar at any moment.
    async fn wait_for_memory(&mut self, hold: Duration) -> MemoryWait {
        loop {
            // The low reading that stopped the server is the baseline: memory
            // must clear before the hold can begin.
            while *self.memory.borrow_and_update() {
                tokio::select! {
                    _ = self.stops.recv() => return MemoryWait::Stop,
                    started = self.restarts.recv() => return user_started(started),
                    changed = self.memory.changed() => {
                        if changed.is_err() {
                            return self.wait_for_user().await;
                        }
                    }
                }
            }
            let held = tokio::select! {
                _ = self.stops.recv() => return MemoryWait::Stop,
                started = self.restarts.recv() => return user_started(started),
                () = sleep(hold) => true,
                changed = self.memory.changed() => {
                    if changed.is_err() {
                        return self.wait_for_user().await;
                    }
                    false
                }
            };
            if held && !*self.memory.borrow_and_update() {
                return MemoryWait::Elapsed;
            }
        }
    }

    /// Without the memory watcher nothing can free memory, so only the user moves.
    async fn wait_for_user(&mut self) -> MemoryWait {
        tokio::select! {
            _ = self.stops.recv() => MemoryWait::Stop,
            started = self.restarts.recv() => user_started(started),
        }
    }

    /// Publishes a reason the server cannot start at all, and leaves the state as
    /// it is until the user asks for a start or closes the sidecar.
    fn cannot_start(&self, reason: String) {
        self.status.send_modify(|status| {
            status.alive = false;
            status.client = None;
            status.process_id = None;
            status.state = State::Degraded;
            status.last_error = Some(reason);
        });
    }

    /// The text for a start that ended: the cause from the server when it names one.
    fn explain(&self, fallback: String) -> String {
        self.spec.crash_reason().unwrap_or(fallback)
    }

    /// Publishes a stop that the user can see, and leaves the state as it is until
    /// the user asks for a start or closes the sidecar.
    fn stopped_for(&self, reason: &str) {
        let reason = reason.to_owned();
        self.status.send_modify(|status| {
            status.alive = false;
            status.client = None;
            status.process_id = None;
            status.state = State::Stopped;
            status.last_error = Some(reason);
        });
    }

    /// Waits while stopped. False when the sidecar ends instead of starting again.
    async fn wait_for_restart(&mut self) -> bool {
        tokio::select! {
            _ = self.stops.recv() => false,
            started = self.restarts.recv() => started.is_some(),
        }
    }

    async fn one_life(&mut self) -> Outcome {
        // A reload asked for while the server was down is stale: the start reads the new
        // settings anyway.
        let _ = self.reloads.try_recv();
        if *self.memory.borrow_and_update() {
            eprintln!("sidecar: memory is low at start, the server starts anyway");
        }
        let generation = self.status.borrow().generation + 1;
        self.status.send_modify(|status| {
            status.generation = generation;
            status.alive = true;
            status.state = State::Starting;
        });
        let plan = self.spec.plan();
        self.spec.starting();
        let ServerProcess {
            stdin,
            stdout,
            handle,
        } = match self.launcher.launch(&plan) {
            Ok(process) => process,
            Err(error) => return Outcome::Crashed(format!("launch failed: {error}")),
        };
        let bridge = Bridge {
            spec: self.spec.clone(),
            consumer: self.consumer.clone(),
            status: self.status.clone(),
            generation,
        };
        let client = Client::start(stdout, stdin, Arc::new(bridge));
        let mut running = Running { handle, client };
        let process_id = running.handle.id();
        self.status
            .send_modify(|status| status.process_id = process_id);

        tokio::select! {
            result = initialize(self.spec.clone(), self.limits.initialize, running.client.clone()) => {
                if let Err(error) = result {
                    running.handle.kill();
                    return Outcome::Crashed(
                        self.explain(format!("initialize failed: {error}")),
                    );
                }
            }
            () = running.handle.wait() => {
                return Outcome::Crashed(self.explain("ended during initialize".to_owned()));
            }
            _ = self.stops.recv() => {
                running.handle.kill();
                return Outcome::Stop;
            }
            _ = self.reloads.recv() => {
                running.handle.kill();
                return Outcome::Reload;
            }
            _ = low_memory(&mut self.memory) => {
                self.status.send_modify(|status| {
                    status.last_error = Some(MEMORY_AUTOMATIC.to_owned());
                });
                running.handle.kill();
                return Outcome::LowMemory;
            }
        }
        self.status.send_modify(|status| {
            status.client = Some(running.client.clone());
            if status.state == State::Starting {
                status.state = State::Importing;
            }
        });

        tokio::select! {
            () = running.handle.wait() => {
                let code = running.handle.exit_code();
                self.spec.crashed(code);
                Outcome::Crashed(self.explain(match code {
                    Some(code) => format!("process ended with {code}"),
                    None => "process ended".to_owned(),
                }))
            }
            _ = self.stops.recv() => {
                self.shut_down(running).await;
                Outcome::Stop
            }
            _ = self.reloads.recv() => {
                self.shut_down(running).await;
                Outcome::Reload
            }
            _ = low_memory(&mut self.memory) => {
                self.status.send_modify(|status| {
                    status.last_error = Some(MEMORY_AUTOMATIC.to_owned());
                });
                self.shut_down(running).await;
                Outcome::LowMemory
            }
        }
    }

    async fn shut_down(&self, mut running: Running) {
        self.status
            .send_modify(|status| status.state = State::Stopping);
        let limit = self.limits.shutdown;
        let polite = async {
            let _ = running.client.shutdown().await;
            running.handle.wait().await;
        };
        if timeout(limit, polite).await.is_err() {
            running.handle.kill();
            running.handle.wait().await;
        }
    }
}

async fn initialize(
    spec: Arc<dyn ServerSpec>,
    limit: Duration,
    client: Client,
) -> Result<(), io::Error> {
    let failed = |error: LspError| io::Error::other(error.to_string());
    let params = spec.initialize_params();
    let reply = timeout(limit, client.request::<Initialize>(params)).await;
    let result = reply.map_err(io::Error::other)?.map_err(failed)?;
    spec.initialize_result(&result);
    client
        .notify::<Initialized>(InitializedParams {})
        .map_err(failed)?;
    spec.initialized(&client);
    Ok(())
}
