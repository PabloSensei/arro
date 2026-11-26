//! Folders the git commands of the IDE write their index files and scripts into. A folder
//! has a random name and only its owner can enter it, so another user of the machine can
//! neither guess the name nor put a file in it, and it is removed when it is dropped.

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many random names are tried before the creation gives up.
const ATTEMPTS: u32 = 16;

/// A folder that only its owner can enter, with a random name, removed when it is dropped.
#[derive(Debug)]
pub(crate) struct TempFolder {
    path: PathBuf,
}

impl TempFolder {
    /// A new folder in the temporary folder of the system, named after `kind`.
    pub(crate) fn create(kind: &str) -> io::Result<Self> {
        Self::create_in(&std::env::temp_dir(), kind)
    }

    /// A new folder in `parent`, named after `kind` and a random suffix. A name that is taken
    /// is never reused: the creation fails for it and another name is tried.
    pub(crate) fn create_in(parent: &Path, kind: &str) -> io::Result<Self> {
        let mut taken = None;
        for _ in 0..ATTEMPTS {
            let path = parent.join(format!("ide-{kind}-{}", random_suffix()));
            match platform::private_new_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => taken = Some(error),
                Err(error) => return Err(error),
            }
        }
        Err(taken.unwrap_or_else(|| io::Error::other("no free name for a temporary folder")))
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The path of the file `name` in the folder.
    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Writes `bytes` into `path` as a file only its owner can read and write.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    platform::private_file(path)?.write_all(bytes)
}

/// A suffix that differs for every folder: a counter, the process id, the time and a hash
/// with random keys.
fn random_suffix() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_nanos());
    let hash = RandomState::new().hash_one((std::process::id(), serial, nanos));
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_every_folder_gets_a_name_of_its_own() {
        let parent = test_support::scratch("vcs-temp-names");

        let first = TempFolder::create_in(&parent, "commit").unwrap();
        let second = TempFolder::create_in(&parent, "commit").unwrap();

        assert_ne!(first.path(), second.path());
        assert!(first.path().starts_with(&parent));
        assert!(
            first
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ide-commit-")),
            "{}",
            first.path().display()
        );
    }

    #[test]
    fn safety_a_folder_is_removed_when_it_is_dropped() {
        let parent = test_support::scratch("vcs-temp-drop");
        let folder = TempFolder::create_in(&parent, "reword").unwrap();
        write_private(&folder.join("todo.txt"), b"pick\n").unwrap();
        let path = folder.path().to_path_buf();

        drop(folder);

        assert!(!path.exists(), "the folder and its files are gone");
    }

    #[cfg(unix)]
    #[test]
    fn safety_a_folder_and_its_files_are_private_on_unix() {
        let parent = test_support::scratch("vcs-temp-private");
        let folder = TempFolder::create_in(&parent, "reword").unwrap();
        let file = folder.join("message.txt");
        write_private(&file, b"message\n").unwrap();

        assert_eq!(platform::mode(folder.path()).unwrap(), 0o700);
        assert_eq!(platform::mode(&file).unwrap(), 0o600);
    }
}
