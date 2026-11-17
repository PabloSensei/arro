//! The window's Local History: opened once in the background, snapshots taken off the UI
//! thread wherever the IDE is about to rewrite a file.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use local_history::{Cause, Limits, LocalHistory};
use ui_kit::*;

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

pub fn put_label(folder: &Path, label: String, cx: &App) {
    let folder: PathBuf = folder.to_path_buf();
    later(cx, move |history| {
        history.put_label(&folder, &label).map(drop)
    });
}
