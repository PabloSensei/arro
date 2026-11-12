//! Real files in a temporary folder, real notifications.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use fs_watch::{FileTable, FsChange, FsWatcher};

struct TempDir {
    path: PathBuf,
    _folder: test_support::TempDir,
}

impl TempDir {
    fn new() -> Self {
        let folder = test_support::TempDir::new("fs-watch");
        Self {
            path: fs::canonicalize(&folder).unwrap(),
            _folder: folder,
        }
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

fn names(table: &FileTable) -> Vec<String> {
    table
        .files()
        .iter()
        .map(|entry| {
            let name = entry.path.strip_prefix(table.root()).unwrap();
            name.to_string_lossy().replace('\\', "/")
        })
        .collect()
}

fn scan(directory: &TempDir, excludes: Vec<PathBuf>) -> FileTable {
    FileTable::scan(&directory.path, excludes).unwrap()
}

/// Waits until the table has seen a change that `done` accepts.
fn wait_for(
    table: &mut FileTable,
    batches: &Receiver<Vec<FsChange>>,
    done: impl Fn(&FileTable) -> bool,
) -> Vec<FsChange> {
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done(table) {
        let left = deadline.saturating_duration_since(Instant::now());
        let batch = batches.recv_timeout(left).expect("a change arrives");
        seen.extend(table.apply(batch));
    }
    seen
}

fn watch(root: &Path) -> (FsWatcher, Receiver<Vec<FsChange>>) {
    let (sender, batches) = mpsc::channel();
    let watcher = FsWatcher::start(root, move |batch| {
        let _ = sender.send(batch);
    })
    .unwrap();
    (watcher, batches)
}

#[test]
fn a_scan_honours_gitignore_and_excluded_folders() {
    let directory = TempDir::new();
    directory.file(".gitignore", "*.log\nout/\n");
    directory.file("src/Main.txt", "");
    directory.file("src/nested/.gitignore", "local.txt\n");
    directory.file("src/nested/local.txt", "");
    directory.file("app.log", "");
    directory.file("out/skip.txt", "");
    directory.file("gen/skip.txt", "");
    directory.file(".git/config", "");

    let table = scan(&directory, vec![directory.path.join("gen")]);

    assert_eq!(
        names(&table),
        [".gitignore", "src/Main.txt", "src/nested/.gitignore"]
    );
}

#[test]
fn the_watcher_reports_new_changed_and_removed_files() {
    let directory = TempDir::new();
    let file = directory.file("a.txt", "one");
    let mut table = scan(&directory, Vec::new());
    let (_watcher, batches) = watch(&directory.path);

    let new = directory.file("b.txt", "two");
    let seen = wait_for(&mut table, &batches, |table| table.contains(&new));
    assert!(seen.contains(&FsChange::Created(new.clone())));

    fs::write(&file, "changed").unwrap();
    let before = table.files()[0].modified;
    wait_for(&mut table, &batches, |table| {
        table.files()[0].modified != before
    });

    fs::remove_file(&new).unwrap();
    let seen = wait_for(&mut table, &batches, |table| !table.contains(&new));
    assert!(seen.contains(&FsChange::Removed(new)));
}

#[test]
fn the_table_ignores_what_a_scan_would_skip() {
    let directory = TempDir::new();
    directory.file(".gitignore", "*.log\n");
    let mut table = scan(&directory, Vec::new());

    let log = directory.file("app.log", "");
    let git = directory.file(".git/index", "");
    let keep = directory.file("keep.txt", "");
    let applied = table.apply(vec![
        FsChange::Created(log),
        FsChange::Created(git),
        FsChange::Created(keep.clone()),
    ]);

    assert_eq!(applied, [FsChange::Created(keep)]);
}

#[test]
fn a_new_folder_expands_and_a_removed_one_takes_its_files() {
    let directory = TempDir::new();
    let mut table = scan(&directory, Vec::new());
    let inner = directory.file("pack/inner/a.txt", "");
    let folder = directory.path.join("pack");

    let applied = table.apply(vec![FsChange::Created(folder.clone())]);
    assert_eq!(applied, [FsChange::Created(inner.clone())]);

    fs::remove_dir_all(&folder).unwrap();
    let applied = table.apply(vec![FsChange::Removed(folder)]);
    assert_eq!(applied, [FsChange::Removed(inner)]);
    assert!(table.is_empty());
}

#[test]
fn a_rescan_finds_what_the_watcher_missed() {
    let directory = TempDir::new();
    let keep = directory.file("keep.txt", "one");
    let gone = directory.file("gone.txt", "");
    let mut table = scan(&directory, Vec::new());

    fs::remove_file(&gone).unwrap();
    let new = directory.file("new.txt", "");
    let file = fs::File::options().write(true).open(&keep).unwrap();
    let later = table
        .files()
        .iter()
        .find_map(|entry| entry.modified)
        .unwrap();
    file.set_modified(later + Duration::from_secs(5)).unwrap();
    drop(file);

    let mut changes = table.rescan();
    changes.sort();

    let expected = [
        FsChange::Created(new.clone()),
        FsChange::Changed(keep),
        FsChange::Removed(gone),
    ];
    assert_eq!(changes, expected);
    assert!(table.contains(&new));
}
