//! A language whose text lives inside the text of another one, such as the query of a
//! string literal. The core knows nothing about the words of such a language: a window
//! asks it for the entries of a place, the checks of a document, the declarations its
//! names lead to and the colours of its words.

use std::path::Path;

use crate::{CompletionEntry, DiagnosticEntry, Replies, SemanticToken, SourceSpan};

/// The language of the text inside the text of another language.
pub trait EmbeddedTextProvider: Send + Sync {
    /// The entries offered at `offset` of the text of `path`. An empty list leaves the
    /// place to the language around the text.
    fn completions(&self, path: &Path, text: &str, offset: usize) -> Vec<CompletionEntry>;

    /// The checks of the text of `path`.
    fn diagnostics(&self, path: &Path, text: &str) -> Vec<DiagnosticEntry>;

    /// The declarations the name at `offset` stands for.
    fn definitions(&self, path: &Path, text: &str, offset: usize) -> Vec<SourceSpan>;

    /// The colours of the words of `text`, on top of the colours of the text around it.
    fn tokens(&self, path: &Path, text: &str) -> Vec<SemanticToken>;

    /// Sends a value after every rebuild of the model the language reads in the
    /// background, so that a window puts the checks into its editors again. `None` when
    /// the language has no such model.
    fn updates(&self) -> Option<Replies<()>> {
        None
    }
}
