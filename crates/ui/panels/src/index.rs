//! The files and symbols of the project, kept current while the window runs. A thread
//! owns the file table: it scans, indexes symbols, watches the disk and tells the window
//! what changed. The window never touches the disk for this.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Instant;

use ::base::perf;
use base::Cancel;
use fs_watch::{FileTable, FsChange, FsWatcher};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use lang_api::SymbolProvider;
use search::{IndexedSymbols, SymbolIndex, SymbolSource, file_symbols, index_files};

/// The folder of the repository metadata, which the file table leaves out.
const GIT_FOLDER: &str = ".git";
/// The loose and packed objects of the repository metadata.
const GIT_OBJECTS_FOLDER: &str = "objects";

/// What the application tells the search about the project: which folders hold build
/// output, which language reads symbols from a file, and further sources of symbols,
/// such as a language server.
#[derive(Clone)]
pub struct SearchSetup {
    pub excludes: Vec<PathBuf>,
    pub symbols_for: fn(&Path) -> Option<&'static dyn SymbolProvider>,
    pub extra_sources: Vec<Arc<SymbolSource>>,
}

impl Default for SearchSetup {
    fn default() -> Self {
        Self {
            excludes: Vec::new(),
            symbols_for: |_| None,
            extra_sources: Vec::new(),
        }
    }
}

/// From the index thread to the window.
pub enum IndexEvent {
    /// The list of files is new, after the scan or after changes.
    Files(Arc<Vec<PathBuf>>),
    /// Files that changed on disk, as the table accepted them.
    Disk(Vec<FsChange>),
    /// Every path the watcher reported, including the ones the table leaves out: the
    /// project metadata folder may be ignored by `.gitignore`, and the window still has
    /// to hear about its files.
    Watched(Vec<FsChange>),
    /// The repository metadata under `.git` changed, or the working tree did: the Git
    /// status the window shows is stale. No batch comes with it; the status is one read.
    Repository,
    /// The keymap file of the settings folder changed on disk.
    Settings,
}

enum Command {
    Disk(Vec<FsChange>),
    /// The settings file the window reads, changed by another program.
    Settings,
    /// Walk again: the window came back to the front.
    Rescan,
}

/// Held by the window. Cloning shares the same index.
#[derive(Clone)]
pub struct ProjectIndex {
    pub root: Arc<PathBuf>,
    pub symbols: Arc<RwLock<SymbolIndex>>,
    pub sources: Arc<Vec<Arc<SymbolSource>>>,
    commands: Sender<Command>,
    /// Bumped after every new list of files reached the window.
    #[allow(dead_code)]
    files: Arc<AtomicU64>,
    /// Bumped after the symbols of the files in the last list are in the index.
    #[allow(dead_code)]
    symbols_generation: Arc<AtomicU64>,
}

impl ProjectIndex {
    /// Starts the thread. The receiver gives the events for the window to read. A
    /// settings file is watched in its own folder too, so that an edit of another
    /// program reaches the window without a clock.
    pub fn start(
        root: PathBuf,
        settings: Vec<PathBuf>,
        setup: SearchSetup,
    ) -> (Self, UnboundedReceiver<IndexEvent>) {
        let symbols = Arc::new(RwLock::new(SymbolIndex::new()));
        let mut sources: Vec<Arc<SymbolSource>> =
            vec![Arc::new(IndexedSymbols::new(symbols.clone()))];
        sources.extend(setup.extra_sources.iter().cloned());
        let (commands, inbox) = mpsc::channel();
        let (events, outbox) = unbounded();
        let files = Arc::new(AtomicU64::new(0));
        let symbols_generation = Arc::new(AtomicU64::new(0));
        let index = Self {
            root: Arc::new(root.clone()),
            symbols: symbols.clone(),
            sources: Arc::new(sources),
            commands: commands.clone(),
            files: files.clone(),
            symbols_generation: symbols_generation.clone(),
        };
        let spawned = thread::Builder::new()
            .name("project-index".to_owned())
            .spawn(move || {
                run(
                    root,
                    settings,
                    setup,
                    symbols,
                    commands,
                    inbox,
                    events,
                    files,
                    symbols_generation,
                )
            });
        if let Err(error) = spawned {
            eprintln!("the project index did not start: {error}");
        }
        (index, outbox)
    }

    pub fn rescan(&self) {
        let _ = self.commands.send(Command::Rescan);
    }

    /// How many lists of files the index has published. A test waits for this to grow
    /// instead of waiting for a clock.
    #[allow(dead_code)]
    pub fn files_generation(&self) -> u64 {
        self.files.load(Ordering::Acquire)
    }

    /// How many times the symbols of the files on disk have reached the index. A test
    /// waits for this to grow instead of waiting for a clock.
    #[allow(dead_code)]
    pub fn symbols_generation(&self) -> u64 {
        self.symbols_generation.load(Ordering::Acquire)
    }
}

/// The index of the window, for scripts and tests that wait for the project to be read.
#[allow(dead_code)]
pub struct ActiveIndex(pub ProjectIndex);

impl ui_kit::Global for ActiveIndex {}

#[allow(clippy::too_many_arguments)]
fn run(
    root: PathBuf,
    settings: Vec<PathBuf>,
    setup: SearchSetup,
    symbols: Arc<RwLock<SymbolIndex>>,
    commands: Sender<Command>,
    inbox: Receiver<Command>,
    events: UnboundedSender<IndexEvent>,
    files: Arc<AtomicU64>,
    symbols_generation: Arc<AtomicU64>,
) {
    // The watcher starts before the scan: a change made while the project is scanned
    // and indexed waits in the inbox instead of being lost, so an open tab and the
    // tree still see it. A change the scan already saw arrives again, which is harmless.
    let watching = commands.clone();
    let _watcher = FsWatcher::start(&root, move |batch| {
        // A write of a git object is not a change of the working tree: the index and the
        // refs mark the status stale, so these events only add work.
        let batch: Vec<FsChange> = batch
            .into_iter()
            .filter(|change| !is_repository_object(change))
            .collect();
        if !batch.is_empty() {
            let _ = watching.send(Command::Disk(batch));
        }
    })
    .map_err(|error| eprintln!("{} is not watched: {error}", root.display()))
    .ok();
    // The settings folder is outside the project: the files of the user get a watcher of
    // their own, and only those files wake the window.
    let _settings_watcher = settings
        .first()
        .and_then(|file| file.parent())
        .and_then(|folder| {
            let watched = settings.clone();
            let settings = commands.clone();
            FsWatcher::start(folder, move |batch| {
                let touched: Vec<FsChange> = batch
                    .into_iter()
                    .filter(|change| match change {
                        FsChange::Created(path)
                        | FsChange::Changed(path)
                        | FsChange::Removed(path) => watched.contains(path),
                        // A rescan wakes the keymap and the templates only through a
                        // file that really changed: it must not undo an edit in flight.
                        FsChange::Rescan => false,
                    })
                    .collect();
                if !touched.is_empty() {
                    let _ = settings.send(Command::Settings);
                }
            })
            .map_err(|error| eprintln!("{} is not watched: {error}", folder.display()))
            .ok()
        });
    let started = Instant::now();
    let mut table = match FileTable::scan(&root, setup.excludes.clone()) {
        Ok(table) => table,
        Err(error) => {
            eprintln!("{} is not scanned: {error}", root.display());
            return;
        }
    };
    perf::observe("index.scan", started.elapsed());
    let publish = |table: &FileTable| {
        perf::set_memory("index.files", table.memory_bytes() as u64);
        let files: Vec<PathBuf> = table
            .files()
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        let _ = events.unbounded_send(IndexEvent::Files(Arc::new(files)));
    };
    publish(&table);
    files.fetch_add(1, Ordering::Release);

    let paths: Vec<PathBuf> = table
        .files()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    let provider_for = setup.symbols_for;
    let indexed = index_files(&paths, &provider_for, &Cancel::new());
    {
        let mut index = symbols.write().unwrap_or_else(|poison| poison.into_inner());
        for (path, found) in indexed {
            index.update(path, found);
        }
        perf::set_memory("index.symbols", index.memory_bytes() as u64);
    }
    symbols_generation.fetch_add(1, Ordering::Release);

    while let Ok(command) = inbox.recv() {
        // The table drops everything under `.git`, so the raw batch is the only thing that
        // tells the repository metadata changed.
        let metadata = match &command {
            Command::Disk(batch) => batch.iter().any(is_repository_change),
            Command::Settings | Command::Rescan => false,
        };
        let (changes, watched) = match command {
            Command::Disk(batch) => {
                let watched = batch.clone();
                (table.apply(batch), Some(watched))
            }
            Command::Settings => {
                let _ = events.unbounded_send(IndexEvent::Settings);
                continue;
            }
            Command::Rescan => (table.rescan(), Some(vec![FsChange::Rescan])),
        };
        if let Some(watched) = watched
            && !watched.is_empty()
        {
            let _ = events.unbounded_send(IndexEvent::Watched(watched));
        }
        let changes = if changes.contains(&FsChange::Rescan) {
            let mut all: Vec<FsChange> = changes
                .into_iter()
                .filter(|change| *change != FsChange::Rescan)
                .collect();
            all.extend(table.rescan());
            all
        } else {
            changes
        };
        if changes.is_empty() {
            if metadata {
                let _ = events.unbounded_send(IndexEvent::Repository);
            }
            continue;
        }
        refresh_symbols(&changes, &symbols, setup.symbols_for);
        {
            let index = symbols.read().unwrap_or_else(|poison| poison.into_inner());
            perf::set_memory("index.symbols", index.memory_bytes() as u64);
        }
        symbols_generation.fetch_add(1, Ordering::Release);
        publish(&table);
        files.fetch_add(1, Ordering::Release);
        let _ = events.unbounded_send(IndexEvent::Disk(changes));
        // A change of the working tree changes the status as well.
        let _ = events.unbounded_send(IndexEvent::Repository);
    }
}

/// Whether the change is a file of `.git/objects`. The working tree and the status do
/// not follow them, and a fetch writes thousands of them.
fn is_repository_object(change: &FsChange) -> bool {
    match change {
        FsChange::Created(path) | FsChange::Changed(path) | FsChange::Removed(path) => {
            let mut parts = path.components();
            while let Some(part) = parts.next() {
                if part.as_os_str() == GIT_FOLDER
                    && parts
                        .next()
                        .is_some_and(|part| part.as_os_str() == GIT_OBJECTS_FOLDER)
                {
                    return true;
                }
            }
            false
        }
        FsChange::Rescan => false,
    }
}

/// Whether a batch touches the metadata of a repository.
fn is_repository_change(change: &FsChange) -> bool {
    match change {
        FsChange::Rescan => true,
        FsChange::Created(path) | FsChange::Changed(path) | FsChange::Removed(path) => {
            path.components().any(|part| part.as_os_str() == GIT_FOLDER)
        }
    }
}

fn refresh_symbols(
    changes: &[FsChange],
    symbols: &RwLock<SymbolIndex>,
    provider_for: fn(&Path) -> Option<&'static dyn SymbolProvider>,
) {
    for change in changes {
        match change {
            FsChange::Created(path) | FsChange::Changed(path) => {
                // Read first, the lock is only for the swap.
                let found = file_symbols(path, &provider_for);
                if let Some(found) = found {
                    let mut index = symbols.write().unwrap_or_else(|poison| poison.into_inner());
                    index.update(path.clone(), found);
                }
            }
            FsChange::Removed(path) => {
                let mut index = symbols.write().unwrap_or_else(|poison| poison.into_inner());
                index.remove(path);
            }
            FsChange::Rescan => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use std::path::PathBuf;

    #[cfg(windows)]
    use super::{FsChange, is_repository_object};

    #[cfg(windows)]
    #[test]
    fn object_files_of_the_metadata_are_left_out() {
        let change = |path: &str| FsChange::Changed(PathBuf::from(path));

        assert!(is_repository_object(&change(
            r"C:\p\.git\objects\ab\cdef0123"
        )));
        assert!(!is_repository_object(&change(r"C:\p\.git\HEAD")));
        assert!(!is_repository_object(&change(r"C:\p\.git\refs\heads\main")));
        assert!(!is_repository_object(&change(r"C:\p\src\objects\Main.txt")));
    }
}
