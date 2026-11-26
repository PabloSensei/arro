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

/// Opens the history in `state_dir/history` without blocking the window. The folders named in
/// `skipped` are left out of it, as the TODO scan leaves them out: the output of the build tool
/// and the like.
pub fn open(state_dir: &Path, skipped: Vec<String>, cx: &mut App) {
    cx.set_global(History::default());
    let dir = state_dir.join("history");
    let task = cx.background_executor().spawn(async move {
        let skip = move |name: &str| crate::todo::skipped(name, &skipped);
        LocalHistory::open_skipping(&dir, Limits::default(), Box::new(skip))
    });
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
/// about to rewrite a lot of files: the command runs after the bytes are read. The root is
/// recorded whatever its name; the folders inside it that the history skips are left out.
pub fn record_tree_blocking(history: &LocalHistory, root: &Path, cause: Cause) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if let Err(error) = history.record_tree(&path, cause) {
            eprintln!("local history: {error}");
        }
    }
}

pub fn put_label(folder: &Path, label: String, cx: &App) {
    let folder: PathBuf = folder.to_path_buf();
    later(cx, move |history| {
        history.put_label(&folder, &label).map(drop)
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use local_history::{Cause, Limits, LocalHistory};
    use window_language::skipped_folders;

    /// A snapshot of a project whose build output has a thousand files records none of them,
    /// and the sources next to it are recorded.
    #[test]
    fn a_snapshot_leaves_out_the_build_output_of_the_project() {
        let root = test_support::scratch("history-output");
        let project = root.join("project");
        let output = project.join(&skipped_folders()[0]);
        fs::create_dir_all(output.join("classes")).unwrap();
        for number in 0..1000u32 {
            fs::write(
                output.join("classes").join(format!("C{number}.class")),
                [1u8; 32],
            )
            .unwrap();
        }
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src").join("Main.txt"), "class Main {}").unwrap();
        let skip = skipped_folders();
        let history = LocalHistory::open_skipping(
            &root.join("history"),
            Limits::default(),
            Box::new(move |name: &str| crate::todo::skipped(name, &skip)),
        )
        .unwrap();

        super::record_tree_blocking(&history, &project, Cause::BeforeDelete);

        assert!(history.folder_history(&output).is_empty());
        assert_eq!(history.folder_history(&project).len(), 1);
    }

    /// A snapshot of a root named like a skipped folder still records what is in it: the root
    /// is a repository, not output.
    #[test]
    fn a_snapshot_of_a_root_named_like_a_skipped_folder_records_its_files() {
        let root = test_support::scratch("history-root-name");
        let project = root.join(&skipped_folders()[0]);
        fs::create_dir_all(project.join("src")).unwrap();
        fs::write(project.join("src").join("Main.txt"), "class Main {}").unwrap();
        let skip = skipped_folders();
        let history = LocalHistory::open_skipping(
            &root.join("history"),
            Limits::default(),
            Box::new(move |name: &str| crate::todo::skipped(name, &skip)),
        )
        .unwrap();

        super::record_tree_blocking(&history, &project, Cause::BeforeDelete);

        assert_eq!(history.folder_history(&project).len(), 1);
    }
}
