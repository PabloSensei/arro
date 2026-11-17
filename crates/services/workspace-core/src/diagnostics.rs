//! Problems in files, from any number of sources: the quick native checks, a language
//! server, a build. Each source replaces its own list for a file, older versions lose.

use std::collections::BTreeMap;
use std::ops::Range;

use platform::CanonPath;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    /// Bytes of the document version the source checked.
    pub range: Range<usize>,
    pub severity: Severity,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Clone, Debug)]
struct Published {
    version: u64,
    diagnostics: Vec<Diagnostic>,
}

/// Owned by the thread of the documents, like [`crate::DocumentStore`].
#[derive(Clone, Debug, Default)]
pub struct DiagnosticsStore {
    files: BTreeMap<CanonPath, BTreeMap<&'static str, Published>>,
    /// Grows on every accepted change, so a view knows when to look again.
    generation: u64,
}

impl DiagnosticsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces what `source` said about `path`. A list for a version older than the one
    /// already there is dropped and `false` comes back.
    pub fn publish(
        &mut self,
        path: CanonPath,
        source: &'static str,
        version: u64,
        mut diagnostics: Vec<Diagnostic>,
    ) -> bool {
        let sources = self.files.entry(path).or_default();
        if sources
            .get(source)
            .is_some_and(|known| known.version > version)
        {
            return false;
        }
        diagnostics.sort_by_key(|diagnostic| (diagnostic.range.start, diagnostic.severity));
        sources.insert(
            source,
            Published {
                version,
                diagnostics,
            },
        );
        self.generation += 1;
        true
    }

    pub fn clear(&mut self, path: &CanonPath, source: &'static str) {
        if let Some(sources) = self.files.get_mut(path)
            && sources.remove(source).is_some()
        {
            self.generation += 1;
        }
    }

    /// Everything about one file, sorted by position.
    pub fn for_path(&self, path: &CanonPath) -> Vec<(&'static str, &Diagnostic)> {
        let mut found: Vec<(&'static str, &Diagnostic)> = self
            .files
            .get(path)
            .into_iter()
            .flatten()
            .flat_map(|(source, published)| {
                published
                    .diagnostics
                    .iter()
                    .map(move |diagnostic| (*source, diagnostic))
            })
            .collect();
        found.sort_by_key(|(_, diagnostic)| (diagnostic.range.start, diagnostic.severity));
        found
    }

    /// How many of each severity, for the status bar and the Problems tab.
    pub fn count(&self, severity: Severity) -> usize {
        self.files
            .values()
            .flat_map(BTreeMap::values)
            .flat_map(|published| &published.diagnostics)
            .filter(|diagnostic| diagnostic.severity == severity)
            .count()
    }

    /// Files with anything to show, in path order.
    pub fn paths(&self) -> impl Iterator<Item = &CanonPath> {
        self.files
            .iter()
            .filter(|(_, sources)| sources.values().any(|known| !known.diagnostics.is_empty()))
            .map(|(path, _)| path)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}
