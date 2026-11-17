//! Local History: the bytes of a file as they were at a save and before anything rewrote
//! it, plus labels over a folder. Contents are stored once each, compressed, and the
//! index is a log that only grows until a prune rewrites it.

mod store;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use store::{BlobStore, Entry, Log};

/// Why a revision was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Saved,
    BeforeReload,
    BeforeRename,
    BeforeReplace,
    BeforeGitRevert,
    BeforeDelete,
    BeforeRevert,
    BeforeRestore,
    Label,
}

impl Cause {
    const ALL: [Cause; 9] = [
        Cause::Saved,
        Cause::BeforeReload,
        Cause::BeforeRename,
        Cause::BeforeReplace,
        Cause::BeforeGitRevert,
        Cause::BeforeDelete,
        Cause::BeforeRevert,
        Cause::BeforeRestore,
        Cause::Label,
    ];

    fn key(self) -> &'static str {
        match self {
            Cause::Saved => "saved",
            Cause::BeforeReload => "reload",
            Cause::BeforeRename => "rename",
            Cause::BeforeReplace => "replace",
            Cause::BeforeGitRevert => "git-revert",
            Cause::BeforeDelete => "delete",
            Cause::BeforeRevert => "revert",
            Cause::BeforeRestore => "restore",
            Cause::Label => "label",
        }
    }

    fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cause| cause.key() == key)
    }

    /// For the list of versions.
    pub fn title(self) -> &'static str {
        match self {
            Cause::Saved => "Saved",
            Cause::BeforeReload => "Before an external change",
            Cause::BeforeRename => "Before rename",
            Cause::BeforeReplace => "Before Replace All",
            Cause::BeforeGitRevert => "Before Git rollback",
            Cause::BeforeDelete => "Before delete",
            Cause::BeforeRevert => "Before revert from history",
            Cause::BeforeRestore => "Before restore from history",
            Cause::Label => "Label",
        }
    }
}

/// One version of a file, or a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub id: u64,
    pub at: SystemTime,
    /// The file, or the folder a label covers.
    pub path: PathBuf,
    pub cause: Cause,
    /// `None` when the file did not exist, and for labels.
    pub content: Option<String>,
    pub label: Option<String>,
}

impl Revision {
    pub fn is_label(&self) -> bool {
        self.cause == Cause::Label
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Older revisions go at the next prune.
    pub max_age: Duration,
    /// The compressed contents together; the oldest revisions go first, but the last
    /// version with content of each file stays while the file is on disk.
    pub max_bytes: u64,
    /// A larger file is not recorded, and neither is one bigger than half of `max_bytes`:
    /// one build output must not push the history of every other file out.
    pub max_file_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(7 * 24 * 60 * 60),
            max_bytes: 512 * 1024 * 1024,
            max_file_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Paths compare without case and with either slash, as on Windows.
fn key_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/").to_lowercase();
    text.trim_end_matches('/').to_string()
}

fn is_under(path: &Path, folder: &Path) -> bool {
    let (path, folder) = (key_of(path), key_of(folder));
    path == folder || path.starts_with(&format!("{folder}/"))
}

fn millis(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

struct State {
    log: Log,
    entries: Vec<Entry>,
    next_id: u64,
}

pub struct LocalHistory {
    blobs: BlobStore,
    limits: Limits,
    state: Mutex<State>,
}

impl LocalHistory {
    /// Opens or creates the history in `dir` and drops what the limits no longer keep.
    pub fn open(dir: &Path, limits: Limits) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let blobs = BlobStore::new(dir.join("blobs"))?;
        let log = Log::new(dir.join("index.log"));
        let entries = log.read()?;
        let next_id = entries.iter().map(|entry| entry.id + 1).max().unwrap_or(1);
        let history = Self {
            blobs,
            limits,
            state: Mutex::new(State {
                log,
                entries,
                next_id,
            }),
        };
        history.prune()?;
        Ok(history)
    }

    fn add(
        &self,
        path: &Path,
        cause: Cause,
        content: Option<String>,
        label: Option<String>,
        at: SystemTime,
    ) -> io::Result<Revision> {
        let mut state = self.state.lock().unwrap();
        let entry = Entry {
            id: state.next_id,
            at: millis(at),
            path: path.to_path_buf(),
            cause,
            content,
            label,
        };
        state.log.append(&entry)?;
        state.next_id += 1;
        state.entries.push(entry.clone());
        Ok(entry.revision())
    }

    /// The largest file a version is taken of.
    fn file_cap(&self) -> u64 {
        self.limits.max_file_bytes.min(self.limits.max_bytes / 2)
    }

    /// Takes `bytes` as the version of `path`; `None` means the file is not there. Nothing
    /// is added when the last version of the file is the same, or when the file is too big
    /// for the limits.
    pub fn record(
        &self,
        path: &Path,
        bytes: Option<&[u8]>,
        cause: Cause,
    ) -> io::Result<Option<Revision>> {
        self.record_at(path, bytes, cause, SystemTime::now())
    }

    fn record_at(
        &self,
        path: &Path,
        bytes: Option<&[u8]>,
        cause: Cause,
        at: SystemTime,
    ) -> io::Result<Option<Revision>> {
        if bytes.is_some_and(|bytes| bytes.len() as u64 > self.file_cap()) {
            return Ok(None);
        }
        let content = match bytes {
            Some(bytes) => Some(self.blobs.put(bytes)?),
            None => None,
        };
        let key = key_of(path);
        let last = {
            let state = self.state.lock().unwrap();
            let mut files = state
                .entries
                .iter()
                .rev()
                .filter(|entry| entry.cause != Cause::Label);
            files
                .find(|entry| key_of(&entry.path) == key)
                .map(|entry| entry.content.clone())
        };
        // A file never seen and not there has no history to start.
        if last == Some(content.clone()) || (last.is_none() && content.is_none()) {
            return Ok(None);
        }
        let revision = self.add(path, cause, content, None, at)?;
        if self.blobs.total()? > self.limits.max_bytes {
            self.prune()?;
        }
        Ok(Some(revision))
    }

    /// Reads `path` from disk and records it.
    pub fn record_file(&self, path: &Path, cause: Cause) -> io::Result<Option<Revision>> {
        match fs::read(path) {
            Ok(bytes) => self.record(path, Some(&bytes), cause),
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.record(path, None, cause),
            Err(error) => Err(error),
        }
    }

    /// Records every file under `path`, or the file itself.
    pub fn record_tree(&self, path: &Path, cause: Cause) -> io::Result<()> {
        if !path.is_dir() {
            return self.record_file(path, cause).map(drop);
        }
        for entry in fs::read_dir(path)? {
            self.record_tree(&entry?.path(), cause)?;
        }
        Ok(())
    }

    /// Records that `path` is gone, after a delete.
    pub fn record_removed(&self, path: &Path) -> io::Result<()> {
        let under: Vec<PathBuf> = {
            let state = self.state.lock().unwrap();
            let files = state
                .entries
                .iter()
                .filter(|entry| entry.cause != Cause::Label);
            let mut paths: Vec<PathBuf> = files
                .filter(|entry| is_under(&entry.path, path))
                .map(|entry| entry.path.clone())
                .collect();
            paths.sort_by_key(|path| key_of(path));
            paths.dedup_by_key(|path| key_of(path));
            paths
        };
        for file in under {
            self.record(&file, None, Cause::BeforeDelete)?;
        }
        Ok(())
    }

    /// A label over everything under `folder`.
    pub fn put_label(&self, folder: &Path, label: &str) -> io::Result<Revision> {
        let label = label.replace(['\t', '\n', '\r'], " ");
        self.add(folder, Cause::Label, None, Some(label), SystemTime::now())
    }

    /// An estimate of the bytes the index of versions keeps in memory, for the memory panel.
    pub fn memory_bytes(&self) -> u64 {
        let state = self.state.lock().unwrap();
        state
            .entries
            .iter()
            .map(|entry| {
                (std::mem::size_of::<Entry>()
                    + entry.path.as_os_str().len()
                    + entry.content.as_ref().map_or(0, String::len)
                    + entry.label.as_ref().map_or(0, String::len)) as u64
            })
            .sum()
    }

    fn select(&self, keep: impl Fn(&Entry) -> bool) -> Vec<Revision> {
        let state = self.state.lock().unwrap();
        let mut found: Vec<Revision> = state
            .entries
            .iter()
            .filter(|entry| keep(entry))
            .map(Entry::revision)
            .collect();
        found.reverse();
        found
    }

    /// The versions of a file and the labels over it, newest first.
    pub fn file_history(&self, path: &Path) -> Vec<Revision> {
        let key = key_of(path);
        self.select(|entry| match entry.cause {
            Cause::Label => is_under(path, &entry.path),
            _ => key_of(&entry.path) == key,
        })
    }

    /// The versions of every file under `folder` and the labels over it, newest first.
    pub fn folder_history(&self, folder: &Path) -> Vec<Revision> {
        self.select(|entry| match entry.cause {
            Cause::Label => is_under(folder, &entry.path) || is_under(&entry.path, folder),
            _ => is_under(&entry.path, folder),
        })
    }

    /// Files under `folder` that are gone from disk, each with its last version that has
    /// content, newest first.
    pub fn deleted_under(&self, folder: &Path) -> Vec<Revision> {
        let mut seen = Vec::new();
        let mut found = Vec::new();
        for revision in self.folder_history(folder) {
            if revision.is_label() || seen.contains(&key_of(&revision.path)) {
                continue;
            }
            if revision.content.is_some() {
                seen.push(key_of(&revision.path));
                if !revision.path.exists() {
                    found.push(revision);
                }
            }
        }
        found
    }

    /// The bytes of a version as they were.
    pub fn content(&self, revision: &Revision) -> io::Result<Vec<u8>> {
        let id = revision.content.as_deref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "this revision has no content")
        })?;
        self.blobs.get(id)
    }

    /// Writes a version back to its path, making the folders it needs. A file that is there
    /// now is recorded first, so the restore can be taken back.
    pub fn restore(&self, revision: &Revision) -> io::Result<()> {
        let bytes = self.content(revision)?;
        if revision.path.exists() {
            let refuse =
                || io::Error::other("the file there now could not be kept, it is left as it is");
            let now = fs::read(&revision.path).map_err(|_| refuse())?;
            if now.len() as u64 > self.file_cap() {
                return Err(refuse());
            }
            // Nothing is added when this is the last version already: it is kept either way.
            self.record(&revision.path, Some(&now), Cause::BeforeRestore)
                .map_err(|_| refuse())?;
        }
        if let Some(parent) = revision.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&revision.path, bytes)
    }

    /// Drops revisions past the limits and the contents nothing refers to any more.
    pub fn prune(&self) -> io::Result<()> {
        self.prune_at(SystemTime::now())
    }

    fn prune_at(&self, now: SystemTime) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        let oldest = millis(now).saturating_sub(self.limits.max_age.as_millis() as u64);
        let before = state.entries.len();
        state.entries.retain(|entry| entry.at >= oldest);
        let mut sizes = self.blobs.sizes()?;
        let referenced = |entries: &[Entry], id: &str| {
            entries
                .iter()
                .any(|entry| entry.content.as_deref() == Some(id))
        };
        let total = |entries: &[Entry], sizes: &[(String, u64)]| -> u64 {
            sizes
                .iter()
                .filter(|(id, _)| referenced(entries, id))
                .map(|(_, size)| size)
                .sum()
        };
        // The last version of a file is its last entry that has content: the tombstone
        // written after a delete has none. That version of a file still on disk stays,
        // whatever the size.
        let mut last: Vec<(String, u64, bool)> = Vec::new();
        for entry in state.entries.iter().filter(|entry| entry.content.is_some()) {
            let key = key_of(&entry.path);
            let live = entry.path.exists();
            match last.iter_mut().find(|(known, _, _)| *known == key) {
                Some((_, id, on_disk)) => {
                    *id = entry.id;
                    *on_disk = live;
                }
                None => last.push((key, entry.id, live)),
            }
        }
        let kept = |entry: &Entry| last.iter().any(|(_, id, live)| *live && *id == entry.id);
        let over = |entries: &[Entry]| total(entries, &sizes) > self.limits.max_bytes;
        let gone = |entry: &Entry| entry.content.is_some() && !entry.path.exists();
        // Older versions of files still on disk, labels and tombstones go first, oldest
        // first. The content of a deleted file can still be restored, so it goes only after
        // them, its newest version last.
        let mut at = 0;
        while over(&state.entries) && at < state.entries.len() {
            let entry = &state.entries[at];
            if kept(entry) || gone(entry) {
                at += 1;
            } else {
                state.entries.remove(at);
            }
        }
        let mut at = 0;
        while over(&state.entries) && at < state.entries.len() {
            if kept(&state.entries[at]) {
                at += 1;
            } else {
                state.entries.remove(at);
            }
        }
        sizes.retain(|(id, _)| !referenced(&state.entries, id));
        for (id, _) in &sizes {
            self.blobs.remove(id)?;
        }
        if state.entries.len() != before || !sizes.is_empty() {
            let entries = state.entries.clone();
            state.log.rewrite(&entries)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
