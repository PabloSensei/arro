//! The state of open documents, shared by the UI and by agents. No GPUI here: everything
//! a window can do to a document, a test or an agent can do without one.
//!
//! The store does no file I/O. [`LoadedFile::read`] and [`SaveRequest::write`] run on a
//! background thread, the thread that owns the store only exchanges their results.

mod commands;
mod diagnostics;
mod document;
mod editors;
mod event;
mod event_bus;
mod transaction;

pub use commands::{CommandInfo, CommandRegistry};
pub use diagnostics::{Diagnostic, DiagnosticsStore, Severity};
pub use document::{
    Document, DocumentId, DocumentReader, DocumentState, DocumentStore, LoadedFile, Reload,
    SaveRequest, SavedFile,
};
pub use editors::{EditorSnapshot, Editors, Place, Selection};
pub use event::Event;
pub use event_bus::{Delivery, EventBus, Subscription};
pub use text_core::{Edit, EditSource, FileFormat, RopeStore, TextStore};
pub use transaction::{Applied, DocumentEdit, TransactionError, WorkspaceTransaction};
