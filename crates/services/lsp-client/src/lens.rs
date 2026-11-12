//! The code lenses a server gives for a document.
//!
//! A server answers `textDocument/codeLens` for the version it saw. The editor keeps
//! showing those labels while the user types, so each edit moves them. A label inside a
//! replaced piece goes away: its declaration says nothing any more.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lsp_types::{CodeLens, ServerCapabilities, TextDocumentContentChangeEvent};

use crate::hints::move_position;

/// What a server says about its code lenses at `initialize`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodeLensCapability {
    /// The labels arrive without their commands and are resolved one by one.
    pub resolve: bool,
}

impl CodeLensCapability {
    /// Reads `codeLensProvider` of the server capabilities. `None` when the server has
    /// no code lenses.
    pub fn from_server(capabilities: &ServerCapabilities) -> Option<Self> {
        let options = capabilities.code_lens_provider.as_ref()?;
        Some(Self {
            resolve: options.resolve_provider.unwrap_or(false),
        })
    }
}

/// The capability of the running server, filled when `initialize` answers and read by
/// the requests. A new answer replaces the old one.
#[derive(Clone, Default)]
pub struct CodeLensSetup(Arc<Mutex<Option<CodeLensCapability>>>);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CodeLensSetup {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, capability: Option<CodeLensCapability>) {
        *lock(&self.0) = capability;
    }

    pub fn get(&self) -> Option<CodeLensCapability> {
        *lock(&self.0)
    }
}

/// The labels of one version of a document.
#[derive(Clone, Debug, Default)]
pub struct CodeLenses {
    version: i32,
    lenses: Vec<CodeLens>,
}

impl CodeLenses {
    pub fn new(version: i32, lenses: Vec<CodeLens>) -> Self {
        Self { version, lenses }
    }

    pub fn version(&self) -> i32 {
        self.version
    }

    pub fn lenses(&self) -> &[CodeLens] {
        &self.lenses
    }

    /// Whether this answer describes `version`.
    pub fn matches(&self, version: i32) -> bool {
        self.version == version
    }

    /// Moves the labels along the changes that lead to `version`. A change without a
    /// range replaces the whole text, and every label of the answer is dropped.
    pub fn apply(&mut self, version: i32, changes: &[TextDocumentContentChangeEvent]) {
        for change in changes {
            let Some(range) = change.range else {
                self.lenses.clear();
                continue;
            };
            self.lenses.retain_mut(|lens| {
                let Some(start) =
                    move_position(lens.range.start, range, end_of(range, &change.text))
                else {
                    return false;
                };
                let Some(end) = move_position(lens.range.end, range, end_of(range, &change.text))
                else {
                    return false;
                };
                lens.range.start = start;
                lens.range.end = end;
                true
            });
        }
        self.version = version;
    }
}

fn end_of(range: lsp_types::Range, text: &str) -> lsp_types::Position {
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or_default();
    let mut line = range.start.line;
    let mut character = range.start.character + first.encode_utf16().count() as u32;
    for next in lines {
        line += 1;
        character = next.encode_utf16().count() as u32;
    }
    lsp_types::Position::new(line, character)
}

#[cfg(test)]
mod tests {
    use lsp_types::{
        CodeLens, Position, Range, ServerCapabilities, TextDocumentContentChangeEvent,
    };
    use serde_json::json;

    use super::{CodeLensCapability, CodeLenses};

    fn capability(value: serde_json::Value) -> Option<CodeLensCapability> {
        let server: ServerCapabilities = serde_json::from_value(value).unwrap();
        CodeLensCapability::from_server(&server)
    }

    fn lens(line: u32) -> CodeLens {
        CodeLens {
            range: Range::new(Position::new(line, 0), Position::new(line, 4)),
            command: None,
            data: Some(json!({"kind": "references"})),
        }
    }

    fn edit(start: (u32, u32), end: (u32, u32), text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range: Some(Range::new(
                Position::new(start.0, start.1),
                Position::new(end.0, end.1),
            )),
            range_length: None,
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_server_says_whether_it_resolves_its_labels_later() {
        assert_eq!(
            capability(json!({"codeLensProvider": {"resolveProvider": true}})),
            Some(CodeLensCapability { resolve: true })
        );
        assert_eq!(
            capability(json!({"codeLensProvider": {}})),
            Some(CodeLensCapability { resolve: false })
        );
        assert_eq!(capability(json!({})), None);
    }

    #[test]
    fn an_edit_moves_a_label_and_one_over_it_takes_it_away() {
        let mut lenses = CodeLenses::new(1, vec![lens(0), lens(2)]);
        lenses.apply(2, &[edit((0, 1), (0, 2), "xxx")]);
        let moved: Vec<_> = lenses
            .lenses()
            .iter()
            .map(|lens| (lens.range.start.line, lens.range.start.character))
            .collect();
        assert_eq!(moved, [(0, 0), (2, 0)]);
        assert_eq!(lenses.version(), 2);
        assert!(lenses.matches(2));
        assert!(!lenses.matches(1), "an answer of an old version is dropped");

        lenses.apply(3, &[edit((0, 0), (1, 0), "")]);
        assert!(
            !lenses
                .lenses()
                .iter()
                .any(|lens| lens.range.start.line == 0),
            "the label whose declaration was replaced goes away"
        );
    }

    #[test]
    fn a_whole_document_change_drops_every_label() {
        let mut lenses = CodeLenses::new(1, vec![lens(0), lens(2)]);
        lenses.apply(
            2,
            &[TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "new".to_owned(),
            }],
        );
        assert!(lenses.lenses().is_empty());
    }
}
