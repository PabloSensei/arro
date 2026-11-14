//! Git through the command line of the machine: status, line changes, commit, push,
//! fetch, update with merge or rebase, stash, conflicts, branches and the log. Every call
//! blocks, so callers keep it off the UI thread.

mod askpass;
mod blame;
mod blob;
mod diff;
mod git;
mod merge;
mod repo;
mod shelve;
mod status;

pub mod graph;

pub use askpass::{ASKPASS_PORT, ASKPASS_TOKEN, AskpassServer, ask};
pub use blame::BlameLine;
pub use blob::BlobReader;
pub use diff::{
    Hunk, LineChange, LineChangeKind, TextEdit, WordChanges, gutter, line_hunks, revert_hunk,
    word_changes,
};
pub use git::GitRunner;
pub use merge::{Segment, SegmentKind, ThreeWay, merge};
pub use repo::{
    Branch, Commit, CommitOptions, CommitRequest, Compare, ConflictSide, ConflictVersions,
    FileDiff, Operation, RefKind, RefLabel, Repository, ResetMode, Stash, UpdateMode, UpdateReport,
};
pub use shelve::{ShelfDiff, patch_files, patched_text};
pub use status::{Change, FileStatus, Status, parse_status};
