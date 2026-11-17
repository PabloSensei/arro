//! The list of project files. Honours `.gitignore`, skips `.git` and the folders the
//! project model calls output.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;
use ignore::gitignore::Gitignore;

use crate::FsChange;

const IGNORE_FILE: &str = ".gitignore";
const GIT_FOLDER: &str = ".git";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
pub struct FileTable {
    root: PathBuf,
    excludes: Vec<PathBuf>,
    ignores: Vec<Gitignore>,
    /// Sorted by path.
    files: Vec<FileEntry>,
}

fn is_excluded(path: &Path, excludes: &[PathBuf]) -> bool {
    path.components().any(|part| part.as_os_str() == GIT_FOLDER)
        || excludes.iter().any(|folder| path.starts_with(folder))
}

fn walk(root: &Path, excludes: &[PathBuf]) -> (Vec<FileEntry>, Vec<PathBuf>) {
    let skip = excludes.to_vec();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .git_global(false)
        .filter_entry(move |entry| !is_excluded(entry.path(), &skip))
        .build();
    let mut files = Vec::new();
    let mut ignore_files = Vec::new();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry.file_name() == IGNORE_FILE {
            ignore_files.push(entry.path().to_path_buf());
        }
        let modified = entry.metadata().ok().and_then(|data| data.modified().ok());
        let path = entry.into_path();
        files.push(FileEntry { path, modified });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    (files, ignore_files)
}

impl FileTable {
    /// Walks `root`. Blocks.
    pub fn scan(root: impl Into<PathBuf>, excludes: Vec<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::metadata(&root)?;
        let (files, ignore_files) = walk(&root, &excludes);
        let ignores = ignore_files.iter().map(|path| Gitignore::new(path).0);
        let ignores = ignores.collect();
        Ok(Self {
            root,
            excludes,
            ignores,
            files,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// An estimate of the bytes the table keeps alive, for the memory panel.
    pub fn memory_bytes(&self) -> usize {
        let paths: usize = self
            .files
            .iter()
            .map(|entry| entry.path.as_os_str().len())
            .sum();
        self.files.len() * size_of::<FileEntry>() + paths + self.root.as_os_str().len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.position(path).is_ok()
    }

    fn position(&self, path: &Path) -> Result<usize, usize> {
        self.files
            .binary_search_by(|entry| entry.path.as_path().cmp(path))
    }

    fn is_tracked(&self, path: &Path, folder: bool) -> bool {
        path.starts_with(&self.root)
            && !is_excluded(path, &self.excludes)
            && !self.ignores.iter().any(|ignore| {
                path.starts_with(ignore.path())
                    && ignore.matched_path_or_any_parents(path, folder).is_ignore()
            })
    }

    fn insert(&mut self, entry: FileEntry) {
        match self.position(&entry.path) {
            Ok(at) => self.files[at] = entry,
            Err(at) => self.files.insert(at, entry),
        }
    }

    /// Everything at or under `path`, a file or a folder.
    fn remove_under(&mut self, path: &Path) -> Vec<FileEntry> {
        let first = self
            .files
            .partition_point(|entry| entry.path.as_path() < path);
        let count = self.files[first..]
            .iter()
            .take_while(|entry| entry.path.starts_with(path))
            .count();
        self.files.drain(first..first + count).collect()
    }

    /// Takes raw changes from the watcher and returns the ones that concern this table.
    /// A new folder expands into its files, so this may read the disk.
    pub fn apply(&mut self, changes: Vec<FsChange>) -> Vec<FsChange> {
        let mut applied = Vec::with_capacity(changes.len());
        for change in changes {
            match change {
                FsChange::Rescan => applied.push(FsChange::Rescan),
                FsChange::Removed(path) => {
                    let gone = self.remove_under(&path);
                    applied.extend(gone.into_iter().map(|entry| FsChange::Removed(entry.path)));
                }
                FsChange::Created(path) | FsChange::Changed(path) => {
                    self.apply_present(path, &mut applied);
                }
            }
        }
        applied
    }

    fn apply_present(&mut self, path: PathBuf, applied: &mut Vec<FsChange>) {
        let Ok(metadata) = fs::metadata(&path) else {
            return;
        };
        if metadata.is_dir() {
            if !self.is_tracked(&path, true) {
                return;
            }
            for entry in walk(&path, &self.excludes).0 {
                if self.is_tracked(&entry.path, false) && !self.contains(&entry.path) {
                    applied.push(FsChange::Created(entry.path.clone()));
                    self.insert(entry);
                }
            }
            return;
        }
        if !self.is_tracked(&path, false) {
            return;
        }
        if path.file_name().is_some_and(|name| name == IGNORE_FILE) {
            applied.push(FsChange::Rescan);
        }
        let known = self.contains(&path);
        let modified = metadata.modified().ok();
        self.insert(FileEntry {
            path: path.clone(),
            modified,
        });
        applied.push(if known {
            FsChange::Changed(path)
        } else {
            FsChange::Created(path)
        });
    }

    /// Walks again and reports what differs. Blocks. Catches whatever the watcher missed.
    pub fn rescan(&mut self) -> Vec<FsChange> {
        let (fresh, ignore_files) = walk(&self.root, &self.excludes);
        let known: HashSet<&Path> = fresh.iter().map(|entry| entry.path.as_path()).collect();
        let mut changes: Vec<FsChange> = self
            .files
            .iter()
            .filter(|entry| !known.contains(entry.path.as_path()))
            .map(|entry| FsChange::Removed(entry.path.clone()))
            .collect();
        for entry in &fresh {
            match self.position(&entry.path) {
                Err(_) => changes.push(FsChange::Created(entry.path.clone())),
                Ok(at) if self.files[at].modified != entry.modified => {
                    changes.push(FsChange::Changed(entry.path.clone()));
                }
                Ok(_) => {}
            }
        }
        self.ignores = ignore_files
            .iter()
            .map(|path| Gitignore::new(path).0)
            .collect();
        self.files = fresh;
        changes
    }
}
