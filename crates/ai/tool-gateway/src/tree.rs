//! The folder tree of a project for `list_directory_tree`.

use std::collections::VecDeque;
use std::path::Path;

use crate::backend::{DirectoryTree, SessionScope, TreeEntry};

/// Reads the tree below `start` down to `depth` levels. A folder on the last level is
/// listed but not read, and `clipped` says so.
pub fn directory_tree(start: &Path, scope: &SessionScope, depth: usize) -> DirectoryTree {
    let mut entries = Vec::new();
    let mut clipped = false;
    let mut queue = VecDeque::from([(start.to_path_buf(), 0usize)]);
    while let Some((folder, level)) = queue.pop_front() {
        let Ok(read) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(|entry| {
            let path = entry.path();
            (!path.is_dir(), entry.file_name())
        });
        for child in children {
            let path = child.path();
            let is_dir = child.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
            let name = child.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            entries.push(TreeEntry {
                path: crate::relative_path(&scope.project, &path),
                name,
                directory: is_dir,
                depth: level,
            });
            if is_dir {
                if level + 1 >= depth {
                    clipped = true;
                } else {
                    queue.push_back((path, level + 1));
                }
            }
        }
    }
    DirectoryTree { entries, clipped }
}
