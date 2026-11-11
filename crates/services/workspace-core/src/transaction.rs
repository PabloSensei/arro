use std::error::Error;
use std::fmt;
use std::ops::Range;

use crate::{DocumentId, Edit, EditSource};

/// Edits of one document. With a version the change applies only to that exact state, so
/// a rename computed from an old snapshot cannot land on newer text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentEdit {
    pub document: DocumentId,
    pub expected_version: Option<u64>,
    pub edits: Vec<Edit>,
}

/// Changes to any number of documents that apply together or not at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceTransaction {
    pub source: EditSource,
    pub changes: Vec<DocumentEdit>,
}

impl WorkspaceTransaction {
    pub fn new(source: EditSource) -> Self {
        let changes = Vec::new();
        Self { source, changes }
    }

    pub fn edit(mut self, document: DocumentId, edits: Vec<Edit>) -> Self {
        let expected_version = None;
        self.changes.push(DocumentEdit {
            document,
            expected_version,
            edits,
        });
        self
    }

    pub fn edit_at(mut self, document: DocumentId, version: u64, edits: Vec<Edit>) -> Self {
        let expected_version = Some(version);
        self.changes.push(DocumentEdit {
            document,
            expected_version,
            edits,
        });
        self
    }
}

/// What came of a transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Undoes the whole transaction, as long as these documents have not changed since.
    pub inverse: WorkspaceTransaction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransactionError {
    NoSuchDocument(DocumentId),
    /// The same document twice: the offsets of the second change would mean nothing.
    RepeatedDocument(DocumentId),
    StaleVersion {
        document: DocumentId,
        expected: u64,
        actual: u64,
    },
    /// The file opened for reading only.
    ReadOnly(DocumentId),
    /// Out of the text, overlapping another edit or inside a character.
    BadRange {
        document: DocumentId,
        range: Range<usize>,
    },
}

impl fmt::Display for TransactionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::NoSuchDocument(document) => write!(f, "no open document {document:?}"),
            Self::RepeatedDocument(document) => {
                write!(f, "{document:?} appears twice in one transaction")
            }
            Self::StaleVersion {
                document,
                expected,
                actual,
            } => write!(f, "{document:?} is at version {actual}, not {expected}"),
            Self::ReadOnly(document) => write!(f, "{document:?} is read-only"),
            Self::BadRange { document, range } => {
                write!(f, "bad range {range:?} in {document:?}")
            }
        }
    }
}

impl Error for TransactionError {}
