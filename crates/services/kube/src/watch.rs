use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use crate::{ClusterState, KubeContext, Kubectl, Resource};

/// A cluster that is not there is tried again less and less often, up to this.
const MAX_WAIT: Duration = Duration::from_secs(60);

/// What the panel shows. A cluster that is down has no namespaces and no resources, and
/// that is a state, not an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KubeSnapshot {
    pub cluster: ClusterState,
    pub contexts: Vec<KubeContext>,
    pub current_context: String,
    pub namespaces: Vec<String>,
    /// The namespace the resources below belong to.
    pub namespace: String,
    pub resources: Vec<Resource>,
    /// A list that could not be read while the cluster was up.
    pub problem: Option<String>,
}

impl Default for KubeSnapshot {
    fn default() -> Self {
        Self {
            cluster: ClusterState::NoCli,
            contexts: Vec::new(),
            current_context: String::new(),
            namespaces: Vec::new(),
            namespace: String::new(),
            resources: Vec::new(),
            problem: None,
        }
    }
}

/// Asks the CLI on its own thread and sends a snapshot whenever it changed, so the panel
/// follows a cluster that goes away and comes back. Dropping it ends the thread.
pub struct KubeWatch {
    snapshots: Receiver<KubeSnapshot>,
    refresh: Sender<()>,
    stop: Arc<AtomicBool>,
}

impl KubeWatch {
    pub fn start(cli: Kubectl, namespace: Option<String>, every: Duration) -> Self {
        let (sender, snapshots) = mpsc::channel();
        let (refresh, asked) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        thread::Builder::new()
            .name("kube-watch".into())
            .spawn(move || watch(&cli, namespace, every, &sender, &asked, &stopped))
            .expect("a thread for the kube watch");
        Self {
            snapshots,
            refresh,
            stop,
        }
    }

    pub fn snapshots(&self) -> &Receiver<KubeSnapshot> {
        &self.snapshots
    }

    /// Looks again now, after an action of the panel.
    pub fn refresh(&self) {
        let _ = self.refresh.send(());
    }
}

impl Drop for KubeWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.refresh.send(());
    }
}

fn watch(
    cli: &Kubectl,
    namespace: Option<String>,
    every: Duration,
    snapshots: &Sender<KubeSnapshot>,
    asked: &Receiver<()>,
    stop: &AtomicBool,
) {
    let mut last = None;
    let mut wait = every;
    while !stop.load(Ordering::Relaxed) {
        let snapshot = take(cli, namespace.as_deref());
        // A cluster that is down stays down for a while: asking it every couple of
        // seconds only spawns processes that fail.
        wait = if matches!(snapshot.cluster, ClusterState::Up { .. }) {
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

pub fn take(cli: &Kubectl, namespace: Option<&str>) -> KubeSnapshot {
    let cluster = cli.state();
    // Without a CLI no context can answer either: a missing `kubectl` starts no process.
    let contexts = if matches!(cluster, ClusterState::NoCli) {
        Vec::new()
    } else {
        cli.contexts().unwrap_or_default()
    };
    let current_context = contexts
        .iter()
        .find(|context| context.current)
        .map(|context| context.name.clone())
        .unwrap_or_default();
    let mut snapshot = KubeSnapshot {
        cluster,
        contexts,
        current_context,
        namespaces: Vec::new(),
        namespace: String::new(),
        resources: Vec::new(),
        problem: None,
    };
    if !matches!(snapshot.cluster, ClusterState::Up { .. }) {
        return snapshot;
    }
    snapshot.namespace = namespace
        .filter(|namespace| !namespace.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            cli.current_namespace()
                .ok()
                .filter(|namespace| !namespace.is_empty())
        })
        .unwrap_or_else(|| "default".to_owned());
    let cli = cli.clone().with_namespace(Some(snapshot.namespace.clone()));
    match (cli.namespaces(), cli.resources()) {
        (Ok(namespaces), Ok(resources)) => {
            snapshot.namespaces = namespaces;
            snapshot.resources = resources;
        }
        (Err(problem), _) | (_, Err(problem)) => snapshot.problem = Some(problem),
    }
    snapshot
}
