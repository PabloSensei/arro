//! Git through the command line of the machine: status, line changes, commit, push,
//! fetch, update with merge or rebase, stash, conflicts, branches and the log. Every call
//! blocks, so callers keep it off the UI thread.

mod askpass;
mod blob;
mod diff;
mod git;
mod repo;
mod shelve;
mod status;

pub use askpass::{ASKPASS_PORT, ASKPASS_TOKEN, AskpassServer, ask};
pub use blob::BlobReader;
pub use diff::{Hunk, LineChange, LineChangeKind, TextEdit, gutter, line_hunks, revert_hunk};
pub use git::GitRunner;
pub use repo::{
    Branch, Commit, CommitOptions, CommitRequest, Compare, ConflictSide, FileDiff, Operation,
    Repository, Stash, UpdateMode, UpdateReport,
};
pub use shelve::{ShelfDiff, patch_files, patched_text};
pub use status::{Change, FileStatus, Status, parse_status};
