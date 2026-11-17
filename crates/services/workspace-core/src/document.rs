use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::{Id, IdSource, Snapshot};
use platform::CanonPath;

use text_core::{
    FileFormat, FileUpdate, RopeStore, TextStore, apply_edits, difference, normalize_newlines,
    read_file, write_file, write_files,
};

use crate::{
    Applied, DocumentEdit, Edit, EditSource, Event, EventBus, TransactionError,
    WorkspaceTransaction,
};

/// The marker of [`DocumentId`].
pub struct Document;

pub type DocumentId = Id<Document>;

/// One version of a document, immutable. Holding it costs nothing and blocks nobody.
#[derive(Clone, Debug)]
pub struct DocumentState {
    pub id: DocumentId,
    pub path: CanonPath,
    pub text: RopeStore,
    /// Encoding and line ends of the file. `text` itself always uses `\n`.
    pub format: FileFormat,
    /// The file is too large, or its bytes do not survive decoding.
    pub read_only: bool,
    pub version: u64,
    pub saved_version: u64,
    pub last_source: Option<EditSource>,
}

impl DocumentState {
    pub fn is_dirty(&self) -> bool {
        self.version != self.saved_version
    }
}

/// Reads the latest state of one document from any thread, without the store.
#[derive(Clone)]
pub struct DocumentReader {
    state: Arc<Snapshot<DocumentState>>,
}

impl DocumentReader {
    pub fn latest(&self) -> Arc<DocumentState> {
        self.state.get()
    }
}

/// A file read from disk, not yet in the store.
pub struct LoadedFile {
    path: CanonPath,
    text: RopeStore,
    format: FileFormat,
    read_only: bool,
}

impl LoadedFile {
    /// Reads the disk, so this runs off the UI thread.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = CanonPath::new(path)?;
        let decoded = read_file(path.as_path())?;
        Ok(Self {
            path,
            text: RopeStore::from_text(&decoded.text),
            format: decoded.format,
            read_only: !decoded.lossless,
        })
    }
}

/// One version on its way to disk.
pub struct SaveRequest {
    document: DocumentId,
    version: u64,
    path: CanonPath,
    text: RopeStore,
    format: FileFormat,
}

impl SaveRequest {
    /// Writes the disk, so this runs off the UI thread. Pass the result to
    /// [`DocumentStore::saved`].
    pub fn write(self) -> io::Result<SavedFile> {
        write_file(self.path.as_path(), &self.text.text(), &self.format)?;
        let (document, version) = (self.document, self.version);
        Ok(SavedFile { document, version })
    }

    /// Writes every version or none of them. A file that cannot be written leaves
    /// the others as they were, and the error names it.
    pub fn write_all(requests: Vec<SaveRequest>) -> io::Result<Vec<SavedFile>> {
        let staged: Vec<(PathBuf, String, FileFormat)> = requests
            .iter()
            .map(|request| {
                (
                    request.path.as_path().to_path_buf(),
                    request.text.text(),
                    request.format.clone(),
                )
            })
            .collect();
        let files: Vec<FileUpdate<'_>> = staged
            .iter()
            .map(|(path, text, format)| FileUpdate {
                path: path.as_path(),
                text,
                format,
            })
            .collect();
        write_files(&files)?;
        Ok(requests
            .into_iter()
            .map(|request| SavedFile {
                document: request.document,
                version: request.version,
            })
            .collect())
    }
}

pub struct SavedFile {
    document: DocumentId,
    version: u64,
}

/// Open documents. Owned by one thread, which is the UI thread in the product. Everybody
/// else reads through [`DocumentReader`] and listens on [`DocumentStore::events`].
#[derive(Default)]
pub struct DocumentStore {
    documents: HashMap<DocumentId, Arc<Snapshot<DocumentState>>>,
    by_path: HashMap<CanonPath, DocumentId>,
    ids: IdSource<Document>,
    events: EventBus<Event>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &EventBus<Event> {
        &self.events
    }

    /// A file that is already open keeps its document and its unsaved edits.
    pub fn open(&mut self, file: LoadedFile) -> DocumentId {
        if let Some(&document) = self.by_path.get(&file.path) {
            return document;
        }
        let id = self.ids.next();
        let state = DocumentState {
            id,
            path: file.path.clone(),
            text: file.text,
            format: file.format,
            read_only: file.read_only,
            version: 0,
            saved_version: 0,
            last_source: None,
        };
        self.documents.insert(id, Arc::new(Snapshot::new(state)));
        self.by_path.insert(file.path.clone(), id);
        self.events.publish(Event::Opened {
            document: id,
            path: file.path,
        });
        id
    }

    pub fn close(&mut self, document: DocumentId) {
        if let Some(state) = self.documents.remove(&document) {
            self.by_path.remove(&state.get().path);
            self.events.publish(Event::Closed { document });
        }
    }

    pub fn reader(&self, document: DocumentId) -> Option<DocumentReader> {
        let state = self.documents.get(&document)?.clone();
        Some(DocumentReader { state })
    }

    pub fn state(&self, document: DocumentId) -> Option<Arc<DocumentState>> {
        Some(self.documents.get(&document)?.get())
    }

    /// One change of one document, the way typing arrives.
    pub fn edit(
        &mut self,
        document: DocumentId,
        source: EditSource,
        edits: Vec<Edit>,
    ) -> Result<Applied, TransactionError> {
        self.apply(WorkspaceTransaction::new(source).edit(document, edits))
    }

    /// Checks everything first, so a bad part leaves every document as it was.
    pub fn apply(
        &mut self,
        transaction: WorkspaceTransaction,
    ) -> Result<Applied, TransactionError> {
        let mut changes = Vec::with_capacity(transaction.changes.len());
        let mut seen = HashSet::new();
        for change in transaction.changes {
            if !seen.insert(change.document) {
                return Err(TransactionError::RepeatedDocument(change.document));
            }
            changes.push(self.checked(change)?);
        }

        let source = transaction.source;
        let mut inverse = WorkspaceTransaction::new(source);
        for (change, state) in changes {
            let mut next = (*state.get()).clone();
            let undo = inverse_edits(&next.text, &change.edits);
            apply_edits(&mut next.text, &mut next.format, &change.edits);
            next.version += 1;
            next.last_source = Some(source);
            let version = next.version;
            state.set(next);
            inverse = inverse.edit_at(change.document, version, undo);
            self.events.publish(Event::Changed {
                document: change.document,
                version,
                source,
                edits: change.edits.into(),
            });
        }
        Ok(Applied { inverse })
    }

    fn checked(
        &self,
        mut change: DocumentEdit,
    ) -> Result<(DocumentEdit, Arc<Snapshot<DocumentState>>), TransactionError> {
        let document = change.document;
        let snapshot = self.documents.get(&document);
        let snapshot = snapshot.ok_or(TransactionError::NoSuchDocument(document))?;
        let state = snapshot.get();
        if state.read_only {
            return Err(TransactionError::ReadOnly(document));
        }
        if let Some(expected) = change.expected_version
            && expected != state.version
        {
            let actual = state.version;
            return Err(TransactionError::StaleVersion {
                document,
                expected,
                actual,
            });
        }
        change
            .edits
            .sort_by_key(|edit| (edit.range.start, edit.range.end));
        for edit in &mut change.edits {
            if let Cow::Owned(text) = normalize_newlines(&edit.text) {
                edit.text = text;
            }
        }
        let mut end = 0;
        for edit in &change.edits {
            let range = &edit.range;
            let valid = end <= range.start
                && range.start <= range.end
                && range.end <= state.text.len()
                && state.text.is_char_boundary(range.start)
                && state.text.is_char_boundary(range.end);
            if !valid {
                let range = range.clone();
                return Err(TransactionError::BadRange { document, range });
            }
            end = range.end;
        }
        Ok((change, snapshot.clone()))
    }

    /// The current version, to write off the UI thread. A read-only file is never written:
    /// its bytes would not come back.
    pub fn save_request(&self, document: DocumentId) -> Option<SaveRequest> {
        let state = self.state(document).filter(|state| !state.read_only)?;
        Some(SaveRequest {
            document,
            version: state.version,
            path: state.path.clone(),
            text: state.text.clone(),
            format: state.format.clone(),
        })
    }

    /// The disk now holds that version. The document may have moved on while it was
    /// written, and then it stays dirty.
    pub fn saved(&mut self, file: SavedFile) {
        let Some(state) = self.documents.get(&file.document) else {
            return;
        };
        let mut next = (*state.get()).clone();
        next.saved_version = file.version;
        state.set(next);
        self.events.publish(Event::Saved {
            document: file.document,
            version: file.version,
        });
    }

    pub fn document_at(&self, path: &CanonPath) -> Option<DocumentId> {
        self.by_path.get(path).copied()
    }

    /// The file changed on disk. A clean document takes the new text as one external
    /// edit around what differs, so carets and folds elsewhere stay. A dirty document
    /// is left alone: the user decides.
    pub fn reload(&mut self, file: LoadedFile) -> Reload {
        let Some(document) = self.document_at(&file.path) else {
            return Reload::NotOpen;
        };
        let Some(state) = self.documents.get(&document).cloned() else {
            return Reload::NotOpen;
        };
        let current = state.get();
        if current.is_dirty() {
            return Reload::Conflict(document);
        }
        let old = current.text.text();
        let new = file.text.text();
        let Some(edit) = difference(&old, &new) else {
            return Reload::Unchanged(document);
        };
        let mut next = (*current).clone();
        next.text = file.text;
        next.format = file.format;
        next.read_only = file.read_only;
        next.version += 1;
        next.saved_version = next.version;
        next.last_source = Some(EditSource::External);
        let version = next.version;
        state.set(next);
        self.events.publish(Event::Changed {
            document,
            version,
            source: EditSource::External,
            edits: vec![edit.clone()].into(),
        });
        Reload::Changed { document, edit }
    }
}

/// What came of [`DocumentStore::reload`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reload {
    NotOpen,
    Unchanged(DocumentId),
    Changed {
        document: DocumentId,
        edit: Edit,
    },
    /// The document has unsaved edits, the disk has others.
    Conflict(DocumentId),
}

/// Edits that turn the result of `edits` back into `text`.
fn inverse_edits(text: &RopeStore, edits: &[Edit]) -> Vec<Edit> {
    let mut shift = 0isize;
    let mut undo = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = edit.range.start.wrapping_add_signed(shift);
        let range = start..start + edit.text.len();
        undo.push(Edit::new(range, text.slice(edit.range.clone())));
        shift += edit.text.len() as isize - edit.range.len() as isize;
    }
    undo
}
