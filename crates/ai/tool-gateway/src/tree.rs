//! The folder tree of a project for `list_directory_tree`.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::backend::{DirectoryTree, SessionScope, TreeEntry};

/// Reads the tree below `start` down to `depth` levels. A folder on the last level is
/// listed but not read, and `clipped` says so. A `timeout` in milliseconds stops the read
/// and keeps what it found by then.
pub fn directory_tree(
    start: &Path,
    scope: &SessionScope,
    depth: usize,
    timeout: Option<u64>,
) -> DirectoryTree {
    let deadline = timeout.map(|milliseconds| Instant::now() + Duration::from_millis(milliseconds));
    let mut entries = Vec::new();
    let mut clipped = false;
    let mut timed_out = false;
    let mut queue = VecDeque::from([(start.to_path_buf(), 0usize)]);
    'folders: while let Some((folder, level)) = queue.pop_front() {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            timed_out = true;
            break;
        }
        let Ok(read) = std::fs::read_dir(&folder) else {
            continue;
        };
        let mut children: Vec<_> = read.flatten().collect();
        children.sort_by_key(|entry| {
            let path = entry.path();
            (!path.is_dir(), entry.file_name())
        });
        for child in children {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                timed_out = true;
                break 'folders;
            }
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
    DirectoryTree {
        entries,
        clipped,
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::directory_tree;
    use crate::backend::SessionScope;

    #[test]
    fn a_zero_time_limit_marks_the_tree_incomplete() {
        let root = test_support::TempDir::new("tree-timeout");
        std::fs::write(root.path().join("a.txt"), "a").unwrap();
        let scope = SessionScope {
            session: "tree".to_string(),
            project: root.path().to_path_buf(),
            cwd: root.path().to_path_buf(),
        };

        let tree = directory_tree(root.path(), &scope, 3, Some(0));

        assert!(tree.timed_out, "the answer says the time limit stopped it");
        assert!(tree.entries.is_empty(), "the read stops at once");
    }
}
