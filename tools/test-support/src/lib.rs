//! Temporary folders of tests: one root under the system temporary folder, a folder of its
//! own for every test, and removal without help from the test.
//!
//! Every folder is `<system temp>/arro-tests/<name>-<pid>-<serial>`. [`TempDir`] removes its
//! folder on drop. [`scratch`] hands the path out and keeps the folder until the thread of
//! the test ends: the window, the language server and the child processes of a test are
//! gone by then, so Windows does not refuse the removal.
//!
//! The first use in a process also removes the folders of earlier runs that are older than
//! a day, so that a crashed run does not fill the disk.

use std::cell::RefCell;
use std::fs;
use std::io;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The folder of all test folders, under the system temporary folder.
const ROOT: &str = "arro-tests";
/// A folder older than this is left over by a crashed run.
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// Windows refuses a removal while a file is held by a closing process: try again for a
/// few seconds, then leave the folder and say so.
const ATTEMPTS: usize = 60;
const RETRY_DELAY: Duration = Duration::from_millis(100);
/// Long names of tests stay readable, but a folder name stays far below the path limit.
const NAME_LIMIT: usize = 96;

static NEXT: AtomicU64 = AtomicU64::new(0);
static SWEEP: Once = Once::new();

/// A folder of one test, removed on drop.
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// A fresh folder under the root. The serial makes the name of the folder unique, even
    /// when one test asks for the same name twice.
    pub fn new(name: &str) -> Self {
        SWEEP.call_once(sweep_stale);
        let root = root();
        fs::create_dir_all(&root).expect("the folder of the temporary folders of tests");
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("{}-{}-{serial}", slug(name), std::process::id()));
        remove(&path);
        fs::create_dir_all(&path).expect("a temporary folder of a test");
        Self { path }
    }

    /// The path of the folder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for TempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        remove(&self.path);
    }
}

thread_local! {
    static OWNED: RefCell<Vec<TempDir>> = const { RefCell::new(Vec::new()) };
}

/// A fresh folder that lives until the thread of the test ends.
///
/// Tests that hand the folder to a window or a server do not keep it in a local of their
/// own, and the folder must outlive them. The thread of the test owns it instead: the
/// locals are dropped first, then the folder, so a window still holds a live folder while
/// the test runs.
pub fn scratch(name: &str) -> PathBuf {
    let folder = TempDir::new(name);
    let path = folder.path().to_path_buf();
    OWNED.with(|owned| owned.borrow_mut().push(folder));
    path
}

/// The root of all test folders, `<system temp>/arro-tests`.
pub fn root() -> PathBuf {
    std::env::temp_dir().join(ROOT)
}

/// A name that is safe in a path: the characters of a test name stay, the rest becomes `-`.
fn slug(name: &str) -> String {
    let mut slug: String = name
        .chars()
        .take(NAME_LIMIT)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        slug.push_str("test");
    }
    slug
}

/// Removes a folder, retrying while a closing process still holds a file inside it.
fn remove(path: &Path) {
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(RETRY_DELAY);
        }
        match try_remove(path) {
            Ok(()) => return,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) if attempt + 1 == ATTEMPTS => {
                eprintln!(
                    "test-support: the temporary folder {} stays: {error}",
                    path.display()
                );
                return;
            }
            Err(_) => {}
        }
    }
}

fn try_remove(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(first) => {
            // A file with the read-only attribute stays until the attribute is cleared.
            clear_read_only(path);
            match fs::remove_dir_all(path) {
                Ok(()) => Ok(()),
                Err(_) => Err(first),
            }
        }
    }
}

#[allow(clippy::permissions_set_readonly_false)]
fn clear_read_only(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir()
        && let Ok(entries) = fs::read_dir(path)
    {
        for entry in entries.flatten() {
            clear_read_only(&entry.path());
        }
    }
    let mut permissions = metadata.permissions();
    if permissions.readonly() {
        permissions.set_readonly(false);
        let _ = fs::set_permissions(path, permissions);
    }
}

/// Removes the folders of runs that are older than a day.
fn sweep_stale() {
    let Ok(entries) = fs::read_dir(root()) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age < STALE_AFTER {
            continue;
        }
        let path = entry.path();
        if metadata.is_dir() {
            remove(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_goes_when_its_owner_goes() {
        let folder = TempDir::new("check");
        let path = folder.path().to_path_buf();
        assert!(path.is_dir());
        assert!(path.starts_with(root()));
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("check-"), "{name}");
        assert!(
            name.contains(&format!("-{}-", std::process::id())),
            "{name}"
        );
        drop(folder);
        assert!(!path.exists());
    }

    #[test]
    fn a_name_of_a_test_stays_readable_in_the_folder() {
        let folder = TempDir::new("window_tests::a folder");
        let name = folder
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("window_tests--a-folder-"), "{name}");
    }

    #[test]
    fn a_scratch_folder_is_there_after_the_call() {
        let path = scratch("check-scratch");
        assert!(path.is_dir());
    }
}
