use std::sync::Arc;

use platform::CanonPath;

use crate::{DocumentId, Edit, EditSource};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Opened {
        document: DocumentId,
        path: CanonPath,
    },
    Changed {
        document: DocumentId,
        version: u64,
        source: EditSource,
        /// Sorted, with offsets from before the change.
        edits: Arc<[Edit]>,
    },
    Saved {
        document: DocumentId,
        version: u64,
    },
    Closed {
        document: DocumentId,
    },
}
