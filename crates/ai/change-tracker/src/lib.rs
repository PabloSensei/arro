//! What an agent changed, whatever the harness: the agent writes to disk by itself, so the working
//! tree goes into a hidden repository outside the project, and turns are diffs of those snapshots.

mod hidden;
mod review;
mod tracker;

pub use hidden::{ChangeKind, ChangedFile, HiddenRepo};
pub use review::{FileReview, ReviewHunk};
pub use tracker::{ChangeSet, ChangeTracker, TrackerState};
pub use vcs::GitRunner;
