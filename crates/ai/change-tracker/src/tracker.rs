//! A thread per project takes the snapshots, in the order the turns happen. Everyone else reads
//! the latest `ChangeSet` without waiting.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use agent_core::AgentEvent;
use base::Snapshot;
use crossbeam_channel::{RecvTimeoutError, Sender, TrySendError, bounded};

use crate::git::GitRunner;
use crate::hidden::{ChangedFile, HiddenRepo};

const QUEUE: usize = 16;
/// Output stops and starts all the time in a TUI, and a snapshot of a large tree is not free.
/// A checkpoint asked for within the gap waits for its end instead of being lost.
const CHECKPOINT_GAP: Duration = Duration::from_secs(5);
/// How long the thread sleeps with nothing to do.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TrackerState {
    /// The first snapshot of a project takes a while on a large tree.
    #[default]
    Preparing,
    Ready,
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    pub state: TrackerState,
    /// Since the first snapshot.
    pub session: Vec<ChangedFile>,
    /// In the last finished turn.
    pub turn: Vec<ChangedFile>,
    /// Grows with every published change.
    pub version: u64,
}

enum Command {
    TurnStarted,
    TurnEnded,
    Checkpoint { force: bool },
}

pub struct ChangeTracker {
    commands: Sender<Command>,
    changes: Arc<Snapshot<ChangeSet>>,
}

impl ChangeTracker {
    /// The first snapshot starts at once, in the background.
    pub fn open(git: GitRunner, state_dir: &Path, project: &Path) -> io::Result<Self> {
        let (commands, inbox) = bounded(QUEUE);
        let changes = Arc::new(Snapshot::new(ChangeSet::default()));
        let shared = changes.clone();
        let (state_dir, project) = (state_dir.to_path_buf(), project.to_path_buf());
        thread::Builder::new()
            .name("change-tracker".to_string())
            .spawn(move || {
                let worker = Worker::start(&git, &state_dir, &project, shared);
                let Some(mut worker) = worker else {
                    return;
                };
                loop {
                    let wait = worker.deferred.map_or(IDLE_WAIT, |due| {
                        due.saturating_duration_since(Instant::now())
                    });
                    match inbox.recv_timeout(wait) {
                        Ok(command) => worker.handle(command),
                        Err(RecvTimeoutError::Timeout) => {
                            worker.handle(Command::Checkpoint { force: true })
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self { commands, changes })
    }

    pub fn changes(&self) -> Arc<ChangeSet> {
        self.changes.get()
    }

    /// Takes the state before the turn. A write landing while it runs may count as the base,
    /// the session list stays right in that case.
    pub fn turn_started(&self) {
        self.send(Command::TurnStarted);
    }

    pub fn turn_ended(&self) {
        self.send(Command::TurnEnded);
    }

    /// Feeds the tracker from the event stream of a session.
    pub fn observe(&self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStarted => self.turn_started(),
            AgentEvent::TurnEnded => self.turn_ended(),
            AgentEvent::Idle => self.send(Command::Checkpoint { force: false }),
            AgentEvent::Exited(_) => self.send(Command::Checkpoint { force: true }),
            _ => {}
        }
    }

    fn send(&self, command: Command) {
        // A full queue means snapshots are behind, and the next turn end covers this one.
        if let Err(TrySendError::Disconnected(_)) = self.commands.try_send(command) {
            self.publish_failure();
        }
    }

    fn publish_failure(&self) {
        let current = self.changes();
        if matches!(current.state, TrackerState::Failed(_)) {
            return;
        }
        self.changes.set(ChangeSet {
            state: TrackerState::Failed("the tracker has stopped".to_string()),
            ..(*current).clone()
        });
    }
}

struct Worker {
    repo: HiddenRepo,
    shared: Arc<Snapshot<ChangeSet>>,
    first: String,
    before_turn: String,
    last_checkpoint: Instant,
    /// When a checkpoint that came too early is due.
    deferred: Option<Instant>,
}

impl Worker {
    fn start(
        git: &GitRunner,
        state_dir: &Path,
        project: &Path,
        shared: Arc<Snapshot<ChangeSet>>,
    ) -> Option<Self> {
        let opened = HiddenRepo::open(git.clone(), state_dir, project);
        let baseline = opened.and_then(|repo| Ok((repo.snapshot()?, repo)));
        match baseline {
            Ok((first, repo)) => {
                let worker = Self {
                    repo,
                    shared,
                    before_turn: first.clone(),
                    last_checkpoint: Instant::now(),
                    deferred: None,
                    first,
                };
                worker.publish(ChangeSet {
                    state: TrackerState::Ready,
                    ..ChangeSet::default()
                });
                Some(worker)
            }
            Err(error) => {
                let failed = ChangeSet {
                    state: TrackerState::Failed(error.to_string()),
                    ..ChangeSet::default()
                };
                shared.set(failed);
                None
            }
        }
    }

    fn publish(&self, changes: ChangeSet) {
        let version = self.shared.get().version + 1;
        self.shared.set(ChangeSet { version, ..changes });
    }

    fn fail(&self, error: io::Error) {
        let current = self.shared.get();
        self.publish(ChangeSet {
            state: TrackerState::Failed(error.to_string()),
            ..(*current).clone()
        });
    }

    fn handle(&mut self, command: Command) {
        let result = match command {
            Command::TurnStarted => self.turn_started(),
            Command::TurnEnded => self.turn_ended(),
            Command::Checkpoint { force } => self.checkpoint(force),
        };
        if let Err(error) = result {
            self.fail(error);
        }
    }

    fn turn_started(&mut self) -> io::Result<()> {
        self.before_turn = self.repo.snapshot()?;
        self.publish((*self.shared.get()).clone());
        Ok(())
    }

    fn checkpoint(&mut self, force: bool) -> io::Result<()> {
        if !force && self.last_checkpoint.elapsed() < CHECKPOINT_GAP {
            self.deferred
                .get_or_insert(self.last_checkpoint + CHECKPOINT_GAP);
            return Ok(());
        }
        self.deferred = None;
        self.last_checkpoint = Instant::now();
        let now = self.repo.snapshot()?;
        let session = self.repo.changed(&self.first, &now)?;
        let current = self.shared.get();
        self.publish(ChangeSet {
            session,
            ..(*current).clone()
        });
        Ok(())
    }

    fn turn_ended(&mut self) -> io::Result<()> {
        let now = self.repo.snapshot()?;
        let session = self.repo.changed(&self.first, &now)?;
        let turn = self.repo.changed(&self.before_turn, &now)?;
        self.before_turn = now;
        self.publish(ChangeSet {
            state: TrackerState::Ready,
            session,
            turn,
            version: 0,
        });
        Ok(())
    }
}
