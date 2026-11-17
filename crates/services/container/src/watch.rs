use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::{Container, Context, DaemonState, DockerCli, Image};

/// A daemon that is not there is tried again less and less often, up to this.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// What the panel shows. A daemon that is down has no containers and images, and that is
/// a state, not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerSnapshot {
    pub daemon: DaemonState,
    pub contexts: Vec<Context>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    /// A list that could not be read while the daemon was up.
    pub problem: Option<String>,
}

/// Asks the CLI on its own thread and sends a snapshot whenever it changed, so the panel
/// follows a daemon that goes down and comes back. Dropping it ends the thread.
pub struct DockerWatch {
    snapshots: Receiver<DockerSnapshot>,
    refresh: Sender<()>,
    stop: Arc<AtomicBool>,
}

impl DockerWatch {
    pub fn start(cli: DockerCli, every: Duration) -> Self {
        let (sender, snapshots) = mpsc::channel();
        let (refresh, asked) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::Builder::new()
            .name("docker-watch".into())
            .spawn(move || watch(&cli, every, &sender, &asked, &stopped))
            .expect("a thread for the docker watch");
        Self {
            snapshots,
            refresh,
            stop,
        }
    }

    pub fn snapshots(&self) -> &Receiver<DockerSnapshot> {
        &self.snapshots
    }

    /// Looks again now, after an action of the panel.
    pub fn refresh(&self) {
        let _ = self.refresh.send(());
    }
}

impl Drop for DockerWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.refresh.send(());
    }
}

fn watch(
    cli: &DockerCli,
    every: Duration,
    snapshots: &Sender<DockerSnapshot>,
    asked: &Receiver<()>,
    stop: &AtomicBool,
) {
    let mut last = None;
    let mut wait = every;
    while !stop.load(Ordering::Relaxed) {
        let snapshot = take(cli);
        // A daemon that is down stays down for a while: asking it every couple of
        // seconds only spawns processes that fail.
        wait = if matches!(snapshot.daemon, DaemonState::Up { .. }) {
            every
        } else {
            (wait * 2).min(MAX_WAIT)
        };
        if last.as_ref() != Some(&snapshot) {
            if snapshots.send(snapshot.clone()).is_err() {
                return;
            }
            last = Some(snapshot);
        }
        match asked.recv_timeout(wait) {
            // The panel asked again after an action of the user.
            Ok(()) => wait = every,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

pub fn take(cli: &DockerCli) -> DockerSnapshot {
    let daemon = cli.daemon();
    // Without a CLI no context can answer either: a missing `docker` starts no process.
    let contexts = if matches!(daemon, DaemonState::NoCli) {
        Vec::new()
    } else {
        cli.contexts().unwrap_or_default()
    };
    let mut snapshot = DockerSnapshot {
        daemon,
        contexts,
        containers: Vec::new(),
        images: Vec::new(),
        problem: None,
    };
    if let DaemonState::Up { .. } = snapshot.daemon {
        match (cli.containers(), cli.images()) {
            (Ok(containers), Ok(images)) => {
                snapshot.containers = containers;
                snapshot.images = images;
            }
            (Err(problem), _) | (_, Err(problem)) => snapshot.problem = Some(problem),
        }
    }
    snapshot
}
