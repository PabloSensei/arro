//! What the window offers when the build tool of a project is not installed, and how far
//! the download of it has come. The values are plain: the tool names itself and the
//! buttons, the window draws them.

use std::sync::atomic::{AtomicU64, Ordering};

/// The notification of a missing build tool and the ways to get one. A tool with nothing
/// to download or to choose leaves those fields empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolSetup {
    /// The name of the tool, for the messages of the download.
    pub name: String,
    /// The title of the notification.
    pub title: String,
    /// The line under the title.
    pub text: String,
    /// The label of the button that downloads the tool.
    pub download: Option<String>,
    /// The label of the button that chooses a folder of the tool.
    pub choose: Option<String>,
    /// The label of the settings button and the row it opens.
    pub settings: Option<(String, &'static str)>,
}

/// How far the download of a missing tool has come: the bytes written and the size the
/// server announced. The worker writes, the window only reads.
#[derive(Debug, Default)]
pub struct ToolProgress {
    written: AtomicU64,
    total: AtomicU64,
}

impl ToolProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the bytes written so far; `total` is `None` until the server says.
    pub fn set(&self, written: u64, total: Option<u64>) {
        self.written.store(written, Ordering::Relaxed);
        self.total.store(total.unwrap_or(0), Ordering::Relaxed);
    }

    /// The bytes written so far.
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    /// The whole size of the file, when the server announced it.
    pub fn total(&self) -> Option<u64> {
        match self.total.load(Ordering::Relaxed) {
            0 => None,
            total => Some(total),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ToolProgress;

    #[test]
    fn the_progress_keeps_the_bytes_and_the_size() {
        let progress = ToolProgress::new();
        assert_eq!(progress.written(), 0);
        assert_eq!(progress.total(), None);

        progress.set(42, None);
        assert_eq!(progress.written(), 42);
        assert_eq!(
            progress.total(),
            None,
            "the server said nothing of the size"
        );

        progress.set(42, Some(100));
        assert_eq!(progress.written(), 42);
        assert_eq!(progress.total(), Some(100));
    }
}
