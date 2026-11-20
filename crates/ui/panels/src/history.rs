//! The window's Local History: opened once in the background, snapshots taken off the UI
//! thread wherever the IDE is about to rewrite a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use local_history::{Cause, Limits, LocalHistory};
use ui_kit::*;

/// How often a changed buffer that is not saved yet is recorded, so that a crash or a
/// process that rewrites the file loses no more than this much work.
pub const AUTO_SNAPSHOT_INTERVAL: Duration = Duration::from_secs(2 * 60);

/// The largest file a snapshot of a whole project reads: the same cap the history applies
/// to a version, so a large binary is not read only to be dropped.
const MAX_TREE_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// The interval the automatic snapshots of the window use. A test sets a short one.
#[derive(Clone, Copy, Debug)]
pub struct AutoSnapshotInterval(pub Duration);

impl Default for AutoSnapshotInterval {
    fn default() -> Self {
        Self(AUTO_SNAPSHOT_INTERVAL)
    }
}

impl Global for AutoSnapshotInterval {}

/// The interval of the automatic snapshots, never zero so that a loop always waits.
pub fn auto_interval(cx: &App) -> Duration {
    cx.try_global::<AutoSnapshotInterval>()
        .copied()
        .unwrap_or_default()
        .0
        .max(Duration::from_millis(1))
}

/// `None` until the history is open, and when it could not be opened.
#[derive(Default)]
pub struct History(pub Option<Arc<LocalHistory>>);

impl Global for History {}

/// Opens the history in `state_dir/history` without blocking the window.
pub fn open(state_dir: &Path, cx: &mut App) {
    cx.set_global(History::default());
    let dir = state_dir.join("history");
    let task = cx
        .background_executor()
        .spawn(async move { LocalHistory::open(&dir, Limits::default()) });
    cx.spawn(async move |cx| match task.await {
        Ok(history) => {
            cx.update(|cx| cx.set_global(History(Some(Arc::new(history)))));
        }
        Err(error) => eprintln!("local history is off: {error}"),
    })
    .detach();
}

pub fn get(cx: &App) -> Option<Arc<LocalHistory>> {
    cx.try_global::<History>()?.0.clone()
}

/// Runs `job` on the history in the background; a failure is only printed, a missed
/// snapshot must not stop what the user does.
fn later(cx: &App, job: impl FnOnce(&LocalHistory) -> std::io::Result<()> + Send + 'static) {
    let Some(history) = get(cx) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            if let Err(error) = job(&history) {
                eprintln!("local history: {error}");
            }
        })
        .detach();
}

/// The file as it is on disk now.
pub fn snapshot_file(path: &Path, cause: Cause, cx: &App) {
    let path = path.to_path_buf();
    later(cx, move |history| {
        history.record_file(&path, cause).map(drop)
    });
}

/// The text of an editor, as a save would write it.
pub fn snapshot_bytes(path: &Path, bytes: Option<Vec<u8>>, cause: Cause, cx: &App) {
    let Some(bytes) = bytes else {
        return;
    };
    let path = path.to_path_buf();
    later(cx, move |history| {
        history.record(&path, Some(&bytes), cause).map(drop)
    });
}

/// The file as it is on disk now, read before the caller changes it. The read happens on
/// the thread that calls this, so a snapshot taken before a write sees the old bytes even
/// when the write follows at once.
pub fn snapshot_now(path: &Path, cause: Cause, cx: &App) {
    let bytes = std::fs::read(path).ok();
    let path = path.to_path_buf();
    later(cx, move |history| {
        history.record(&path, bytes.as_deref(), cause).map(drop)
    });
}

/// Records every file under `root` from the thread that calls this, for a command that is
/// about to rewrite a lot of files: the command runs after the bytes are read.
pub fn record_tree_blocking(history: &LocalHistory, root: &Path, cause: Cause, skipped: &[String]) {
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !crate::todo::skipped(&name, skipped) {
                    folders.push(entry.path());
                }
                continue;
            }
            if kind.is_file() {
                let small = entry
                    .metadata()
                    .is_ok_and(|meta| meta.len() <= MAX_TREE_FILE_BYTES);
                if !small {
                    continue;
                }
                if let Err(error) = history.record_file(&entry.path(), cause) {
                    eprintln!("local history: {error}");
                }
            }
        }
    }
}

/// The folders a snapshot of a whole project leaves out, as the TODO scan does.
pub fn skipped_folders(cx: &App) -> Vec<String> {
    cx.try_global::<crate::todo::SkippedFolders>()
        .map_or_else(Vec::new, |skipped| skipped.0.clone())
}

pub fn put_label(folder: &Path, label: String, cx: &App) {
    let folder: PathBuf = folder.to_path_buf();
    later(cx, move |history| {
        history.put_label(&folder, &label).map(drop)
    });
}
