//! The files of a project and how they change on disk. Scanning blocks, so it runs off
//! the UI thread. Lost notifications are an ordinary event here: `FsChange::Rescan` asks
//! for [`FileTable::rescan`], which also serves the check on window focus.

mod table;
mod watcher;

pub use table::{FileEntry, FileTable};
pub use watcher::{FsChange, FsWatcher};
