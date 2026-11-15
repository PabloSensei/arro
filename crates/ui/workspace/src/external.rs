//! Paths the system opens: a document of the Finder, an entry of the desktop menu or a
//! second start of the program. The platform passes them on; the window decides where
//! they go.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_channel::oneshot;

pub use platform::instance::Request;

/// The queue between the threads that listen for paths and the window that opens them.
/// Cheap to clone: one queue is shared by the listener and every window.
///
/// A window waits with [`OpenRequests::wait_for_push`]; a thread that pushes a request
/// wakes it through the channel the waiter left behind, so the window has no timer that
/// asks an empty queue.
#[derive(Clone, Default)]
pub struct OpenRequests {
    queue: Arc<Mutex<Vec<Request>>>,
    /// The wake-up of the window that waits for the next request. One window waits at a
    /// time, so one slot is enough.
    waker: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

impl OpenRequests {
    pub fn push(&self, request: Request) {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(request);
        let waker = self
            .waker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(waker) = waker {
            waker.send(()).ok();
        }
    }

    /// The requests that arrived since the last call, oldest first.
    pub fn take(&self) -> Vec<Request> {
        std::mem::take(
            &mut *self
                .queue
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
    }

    /// Waits until a request is pushed. True when there is one to take, false when the
    /// queue itself is gone: no one can push any more, and the window stops waiting.
    pub async fn wait_for_push(&self) -> bool {
        let (waker, woken) = oneshot::channel();
        *self
            .waker
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(waker);
        // A request that arrived before the waker was in place is taken at once; one
        // that arrives after it wakes the channel.
        if !self.is_empty() {
            return true;
        }
        woken.await.is_ok()
    }
}

/// Where a path that the system opens belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenTarget {
    /// The project folder of the open window: the window only comes to the front.
    Window,
    /// A file of the open project: a tab of its window.
    File(PathBuf),
    /// A path outside the open project: a project of its own, in a program of its own.
    Project {
        folder: PathBuf,
        /// The file to open after the project, when the path is a file.
        file: Option<PathBuf>,
    },
}

/// The window a path belongs to. A file of the open project opens in its window; a file
/// outside it opens the folder that holds it as a project, with the file on top.
pub fn open_target(root: &Path, path: &Path) -> OpenTarget {
    if path.is_dir() {
        return if same(root, path) || inside(path, root) {
            OpenTarget::Window
        } else {
            OpenTarget::Project {
                folder: path.to_path_buf(),
                file: None,
            }
        };
    }
    if inside(path, root) {
        return OpenTarget::File(path.to_path_buf());
    }
    let folder = path
        .parent()
        .map_or_else(|| root.to_path_buf(), Path::to_path_buf);
    OpenTarget::Project {
        folder,
        file: Some(path.to_path_buf()),
    }
}

fn inside(path: &Path, folder: &Path) -> bool {
    match (
        platform::CanonPath::new(path),
        platform::CanonPath::new(folder),
    ) {
        (Ok(path), Ok(folder)) => path.as_path().starts_with(folder.as_path()),
        _ => path.starts_with(folder),
    }
}

fn same(left: &Path, right: &Path) -> bool {
    match (
        platform::CanonPath::new(left),
        platform::CanonPath::new(right),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_support::TempDir;

    #[test]
    fn a_file_of_the_project_opens_in_its_window() {
        let root = TempDir::new("open-inside");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let file = root.join("src").join("Order.txt");
        std::fs::write(&file, "an order\n").unwrap();

        assert_eq!(open_target(&root, &file), OpenTarget::File(file.clone()));
    }

    #[test]
    fn a_file_outside_the_project_opens_a_project_of_its_own() {
        let root = TempDir::new("open-root");
        let other = TempDir::new("open-other");
        let file = other.join("Order.txt");
        std::fs::write(&file, "an order\n").unwrap();

        assert_eq!(
            open_target(&root, &file),
            OpenTarget::Project {
                folder: other.path().to_path_buf(),
                file: Some(file.clone()),
            }
        );
    }

    #[test]
    fn the_folder_of_the_window_only_comes_to_the_front() {
        let root = TempDir::new("open-folder");
        assert_eq!(open_target(&root, &root), OpenTarget::Window);
    }

    #[test]
    fn another_folder_opens_as_a_project() {
        let root = TempDir::new("open-folder-root");
        let other = TempDir::new("open-folder-other");
        assert_eq!(
            open_target(&root, &other),
            OpenTarget::Project {
                folder: other.path().to_path_buf(),
                file: None,
            }
        );
    }

    #[test]
    fn a_folder_of_the_project_only_comes_to_the_front() {
        let root = TempDir::new("open-subfolder");
        let folder = root.join("src");
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(open_target(&root, &folder), OpenTarget::Window);
    }

    #[test]
    fn the_queue_hands_the_requests_over_once() {
        let queue = OpenRequests::default();
        queue.push(Request::Focus);
        queue.push(Request::Open(PathBuf::from("/tmp/a.txt")));
        assert_eq!(
            queue.take(),
            [Request::Focus, Request::Open(PathBuf::from("/tmp/a.txt"))]
        );
        assert!(queue.take().is_empty());
        assert!(queue.is_empty());
    }
}
