//! Where an MCP client finds the running window of a project.
//!
//! While a project is open, its window writes one entry point into the state folder of
//! Arro: the project root, the address of its MCP server, the token of the window and the
//! process id. A client that starts in a folder of the project takes the entry of the
//! deepest root that holds it. The file belongs to the user alone and goes away with the
//! window; one whose process is gone is not used.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The folder of the entry points under the state folder of Arro.
pub const DIRECTORY: &str = "mcp";
const SUFFIX: &str = "json";

/// One window: what a client needs to reach the MCP server of its project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPoint {
    /// The root of the project the window has open.
    pub root: PathBuf,
    /// The Streamable HTTP endpoint, on the loopback.
    pub url: String,
    /// The token of the window. The file is readable by its owner alone.
    pub token: String,
    /// The process of the window, so an entry of a window that is gone is ignored.
    pub pid: u32,
}

impl EntryPoint {
    /// The short stable name of a project root, as the file name of its entry uses it.
    pub fn key(root: &Path) -> String {
        let text = root.to_string_lossy().to_lowercase();
        let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ byte as u64).wrapping_mul(0x100000001b3)
        });
        format!("{hash:016x}")
    }

    /// The file this entry is written to.
    pub fn file(&self, state_dir: &Path) -> PathBuf {
        entry_file(state_dir, &self.root)
    }

    /// Writes the entry, readable by its owner alone. The file goes away when the value
    /// that comes back is dropped, as the window does.
    pub fn write(&self, state_dir: &Path) -> io::Result<EntryPointFile> {
        let path = self.file(state_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = platform::private_file(&path)?;
        serde_json::to_writer(&mut file, self).map_err(io::Error::other)?;
        Ok(EntryPointFile { path })
    }

    /// Removes the entry of a project, as a closing window does. A missing file is no
    /// error: another window may have taken it already.
    pub fn remove(state_dir: &Path, root: &Path) {
        let _ = std::fs::remove_file(entry_file(state_dir, root));
    }

    /// Whether the process that wrote the entry is still there.
    pub fn is_alive(&self) -> bool {
        match platform::ProcessWaiter::open(self.pid) {
            Ok(Some(waiter)) => waiter.wait(Duration::ZERO).map_or(true, |ended| !ended),
            Ok(None) => false,
            // The process cannot be asked about, so the entry is kept rather than dropped:
            // a working window is worth more than a stale file.
            Err(_) => true,
        }
    }
}

fn entry_file(state_dir: &Path, root: &Path) -> PathBuf {
    state_dir
        .join(DIRECTORY)
        .join(format!("{}.{SUFFIX}", EntryPoint::key(root)))
}

/// The file of a running window; it goes away on drop.
pub struct EntryPointFile {
    path: PathBuf,
}

impl EntryPointFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for EntryPointFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The state folder of Arro, where the running windows keep their entry points.
pub fn state_folder() -> PathBuf {
    base::app::folder("LOCALAPPDATA")
}

/// Every entry of the folder whose process is still there. A damaged file and a file of
/// a window that is gone are skipped.
pub fn entries(state_dir: &Path) -> Vec<EntryPoint> {
    let Ok(folder) = std::fs::read_dir(state_dir.join(DIRECTORY)) else {
        return Vec::new();
    };
    folder
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == SUFFIX)
        })
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<EntryPoint>(&text).ok())
        .filter(EntryPoint::is_alive)
        .collect()
}

/// The entry of the deepest project root that holds `folder`, as a client started in a
/// subfolder of a project needs it.
pub fn find(state_dir: &Path, folder: &Path) -> Option<EntryPoint> {
    entries(state_dir)
        .into_iter()
        .filter(|entry| holds(&entry.root, folder))
        .max_by_key(|entry| entry.root.components().count())
}

fn holds(root: &Path, folder: &Path) -> bool {
    #[cfg(windows)]
    {
        platform::path_key(folder).starts_with(platform::path_key(root))
    }
    #[cfg(not(windows))]
    {
        folder.starts_with(root)
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use test_support::TempDir;

    use super::{EntryPoint, entries, find};

    fn entry(root: &std::path::Path, pid: u32) -> EntryPoint {
        EntryPoint {
            root: root.to_path_buf(),
            url: "http://127.0.0.1:4100/mcp".to_string(),
            token: "token".to_string(),
            pid,
        }
    }

    #[test]
    fn an_entry_survives_the_file() {
        let temp = TempDir::new("mcp-entry");
        let root = temp.path().join("shop");
        let point = entry(&root, std::process::id());
        let file = point.write(temp.path()).expect("write");
        assert!(file.path().is_file());

        let found = find(temp.path(), &root.join("src/main"));
        assert_eq!(found, Some(point));
        drop(file);
        assert!(
            entries(temp.path()).is_empty(),
            "the file goes away with the window"
        );
    }

    #[test]
    fn the_deepest_root_wins() {
        let temp = TempDir::new("mcp-deepest");
        let outer = temp.path().join("work");
        let inner = outer.join("inner");
        let point = entry(&inner, std::process::id());
        let _inner_file = point.write(temp.path()).expect("write");
        let _outer_file = entry(&outer, std::process::id())
            .write(temp.path())
            .expect("write");

        assert_eq!(find(temp.path(), &inner.join("src")), Some(point.clone()));
        assert_eq!(
            find(temp.path(), &outer.join("other")),
            Some(entry(&outer, std::process::id()))
        );
        assert_eq!(find(temp.path(), &temp.path().join("elsewhere")), None);
    }

    fn dead_pid() -> u32 {
        #[cfg(windows)]
        let mut child = Command::new("cmd")
            .args(["/c", "exit"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("a child");
        #[cfg(not(windows))]
        let mut child = Command::new("true").spawn().expect("a child");
        let pid = child.id();
        child.wait().expect("the end of the child");
        pid
    }

    #[test]
    fn an_entry_of_a_dead_process_is_ignored() {
        let temp = TempDir::new("mcp-dead");
        let root = temp.path().join("shop");
        let point = entry(&root, dead_pid());
        let _file = point.write(temp.path()).expect("write");

        assert!(entries(temp.path()).is_empty(), "a dead window is not used");
        assert_eq!(find(temp.path(), &root), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_belongs_to_its_owner_alone() {
        let temp = TempDir::new("mcp-private");
        let point = entry(&temp.path().join("shop"), std::process::id());

        let file = point.write(temp.path()).expect("write");

        assert_eq!(platform::mode(file.path()).unwrap(), 0o600);
    }
}
