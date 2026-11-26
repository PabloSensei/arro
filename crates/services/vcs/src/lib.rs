//! Git through the command line of the machine: status, line changes, commit, push,
//! fetch, update with merge or rebase, stash, conflicts, branches and the log. Every call
//! blocks, so callers keep it off the UI thread.

mod askpass;
mod blame;
mod blob;
mod console;
mod diff;
mod git;
mod history;
mod merge;
mod rebase;
mod repo;
mod shelve;
mod status;
mod submodule;
mod temp;

pub mod graph;

pub use askpass::{ASKPASS_PORT, ASKPASS_TOKEN, AskpassServer, ask};
pub use blame::BlameLine;
pub use blob::BlobReader;
pub use console::{
    COMMAND_LIMIT, CommandConsole, CommandRecord, HIDDEN, command_console, hidden_argument,
    is_background, url_without_credentials,
};
pub use diff::{
    Hunk, LineChange, LineChangeKind, TextEdit, WordChanges, gutter, line_hunks, revert_hunk,
    word_changes,
};
pub use git::{GitRunner, command_id};
pub use history::LineHistory;
pub use merge::{Segment, SegmentKind, ThreeWay, merge};
pub use rebase::{RebaseAction, RebasePlan, RebaseReport, RebaseStep};
pub use repo::{
    Branch, Commit, CommitChange, CommitDetails, CommitOptions, CommitRequest, Compare,
    ConflictSide, ConflictVersions, FileDiff, FileRevert, Operation, PartialFile, PushRequest,
    RefKind, RefLabel, Repository, ResetMode, Stash, UpdateMode, UpdateReport,
};
pub use shelve::{ShelfDiff, patch_files, patch_without, patched_text};
pub use status::{Change, FileStatus, Status, parse_status};
pub use submodule::{RootsUpdate, Submodule, SubmoduleState, parse_submodule_status};
