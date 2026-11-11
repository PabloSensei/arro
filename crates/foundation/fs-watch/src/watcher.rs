//! Watches a folder tree and hands out debounced batches on its own thread.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use notify::event::EventKind;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};

/// A batch ends after this much silence.
const QUIET: Duration = Duration::from_millis(40);
/// A steady stream of events, a build for example, still gets through this often.
const LONGEST_WAIT: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FsChange {
    Created(PathBuf),
    Changed(PathBuf),
    Removed(PathBuf),
    /// Events were lost: only a new walk tells what happened.
    Rescan,
}

/// Stops watching when dropped.
pub struct FsWatcher {
    _watcher: RecommendedWatcher,
}

impl FsWatcher {
    /// `on_batch` runs on the thread of the watcher.
    pub fn start(
        root: &Path,
        on_batch: impl FnMut(Vec<FsChange>) + Send + 'static,
    ) -> notify::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = recommended_watcher(sender)?;
        watcher.watch(root, RecursiveMode::Recursive)?;
        thread::Builder::new()
            .name("fs-watch".to_owned())
            .spawn(move || debounce(&receiver, on_batch))
            .map_err(notify::Error::io)?;
        Ok(Self { _watcher: watcher })
    }
}

#[derive(Default)]
struct Batch {
    /// Path and whether it was created in this batch.
    paths: BTreeMap<PathBuf, bool>,
    rescan: bool,
}

impl Batch {
    fn add(&mut self, event: notify::Result<Event>) {
        let Ok(event) = event else {
            self.rescan = true;
            return;
        };
        self.rescan |= event.need_rescan();
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let created = matches!(event.kind, EventKind::Create(_));
        for path in event.paths {
            *self.paths.entry(path).or_default() |= created;
        }
    }

    /// What is on disk now decides. A file made and deleted inside a batch is a removal of
    /// nothing, which the table ignores.
    fn into_changes(self) -> Vec<FsChange> {
        let mut changes: Vec<FsChange> = self
            .paths
            .into_iter()
            .filter_map(|(path, created)| match fs::metadata(&path) {
                Err(_) => Some(FsChange::Removed(path)),
                Ok(_) if created => Some(FsChange::Created(path)),
                Ok(metadata) if metadata.is_dir() => None,
                Ok(_) => Some(FsChange::Changed(path)),
            })
            .collect();
        if self.rescan {
            changes.push(FsChange::Rescan);
        }
        changes
    }
}

fn debounce(receiver: &Receiver<notify::Result<Event>>, mut on_batch: impl FnMut(Vec<FsChange>)) {
    while let Ok(first) = receiver.recv() {
        let mut batch = Batch::default();
        batch.add(first);
        let deadline = Instant::now() + LONGEST_WAIT;
        let mut open = true;
        while open && Instant::now() < deadline {
            match receiver.recv_timeout(QUIET) {
                Ok(event) => batch.add(event),
                Err(RecvTimeoutError::Timeout) => open = false,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let changes = batch.into_changes();
        if !changes.is_empty() {
            on_batch(changes);
        }
    }
}
