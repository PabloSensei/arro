//! Where Go to Class and Go to Symbol get their rows. A source is a `lang_api::Provider`:
//! the query returns at once and rows arrive as a stream, so the project index answers
//! first and a language server can add its own rows later, classes of the JDK and of
//! dependencies for one. The popup merges whatever arrives for the query on screen.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::thread;

use base::Cancel;
use lang_api::{Answer, Confidence, Provider, Reply, SourceId, SymbolKind};

use crate::SymbolIndex;

/// What the popup asks for. Go to Class wants types, Go to Symbol the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolScope {
    Types,
    Members,
    All,
}

impl SymbolScope {
    pub fn accepts(self, kind: SymbolKind) -> bool {
        match self {
            Self::Types => kind.is_type(),
            Self::Members => !kind.is_type(),
            Self::All => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRequest {
    pub pattern: String,
    pub scope: SymbolScope,
    pub limit: usize,
}

/// Where a row leads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A file of the project and a byte offset in it.
    Offset { path: PathBuf, offset: usize },
    /// A file and a 0-based line and column, as servers speak.
    Line {
        path: PathBuf,
        line: usize,
        column: usize,
    },
    /// Something that is not a file of the project, such as a class in a jar. Whoever
    /// supplies it knows how to open it.
    Uri(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRow {
    pub name: String,
    pub kind: SymbolKind,
    /// The package or the enclosing declaration, shown next to the name.
    pub container: Option<String>,
    pub target: Target,
    /// Higher is better. Rows of different sources are ordered by it.
    pub score: u32,
}

/// A source of symbol rows. Implement `Provider<SymbolRequest, Vec<SymbolRow>>`.
pub type SymbolSource = dyn Provider<SymbolRequest, Vec<SymbolRow>>;

pub const NATIVE: SourceId = SourceId::new("index");

/// The symbols of the project sources, from the index kept current by the watcher.
#[derive(Clone)]
pub struct IndexedSymbols {
    index: Arc<RwLock<SymbolIndex>>,
}

impl IndexedSymbols {
    pub fn new(index: Arc<RwLock<SymbolIndex>>) -> Self {
        Self { index }
    }
}

impl Provider<SymbolRequest, Vec<SymbolRow>> for IndexedSymbols {
    fn start(&self, query: SymbolRequest, cancel: Cancel, mut reply: Reply<Vec<SymbolRow>>) {
        let index = self.index.clone();
        thread::spawn(move || {
            if cancel.is_cancelled() {
                return;
            }
            let found = {
                let index = index.read().unwrap_or_else(|poison| poison.into_inner());
                index.search(
                    &query.pattern,
                    |symbol| query.scope.accepts(symbol.kind),
                    query.limit,
                )
            };
            let rows = found
                .into_iter()
                .map(|ranked| SymbolRow {
                    name: ranked.item.symbol.name.clone(),
                    kind: ranked.item.symbol.kind,
                    container: ranked.item.symbol.container.clone(),
                    target: Target::Offset {
                        path: ranked.item.path,
                        offset: ranked.item.symbol.name_range.start,
                    },
                    score: ranked.score,
                })
                .collect();
            let _ = reply.send(Answer::new(rows, Confidence::Certain, NATIVE));
        });
    }
}

/// Merges rows of all sources for one query: best first, the same place once.
pub fn merge_rows(
    batches: impl IntoIterator<Item = Vec<SymbolRow>>,
    limit: usize,
) -> Vec<SymbolRow> {
    let mut rows: Vec<SymbolRow> = batches.into_iter().flatten().collect();
    rows.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.name.len().cmp(&b.name.len()))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = std::collections::HashSet::new();
    rows.retain(|row| seen.insert((row.name.clone(), row.container.clone(), row.kind)));
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use futures_executor::block_on;
    use futures_util::StreamExt;
    use lang_api::{Symbol, ask};

    use super::*;

    fn symbol(name: &str, kind: SymbolKind) -> Symbol {
        Symbol {
            name: name.to_owned(),
            kind,
            container: Some("pets".to_owned()),
            depth: 0,
            range: 0..10,
            name_range: 4..4 + name.len(),
        }
    }

    #[test]
    fn the_index_answers_as_a_stream_and_filters_by_scope() {
        let mut index = SymbolIndex::new();
        index.update(
            PathBuf::from("/p/Owner.src"),
            vec![
                symbol("Owner", SymbolKind::Class),
                symbol("getOwner", SymbolKind::Method),
            ],
        );
        let source = IndexedSymbols::new(Arc::new(RwLock::new(index)));
        let request = |scope| SymbolRequest {
            pattern: "owner".to_owned(),
            scope,
            limit: 10,
        };

        let answers: Vec<_> = block_on(ask(&source, request(SymbolScope::Types)).collect());
        let [answer] = answers.as_slice() else {
            panic!("one answer");
        };
        assert_eq!(answer.source, NATIVE);
        let [row] = answer.value.as_slice() else {
            panic!("one type");
        };
        assert_eq!(row.name, "Owner");
        assert_eq!(
            row.target,
            Target::Offset {
                path: PathBuf::from("/p/Owner.src"),
                offset: 4
            }
        );

        let answers: Vec<_> = block_on(ask(&source, request(SymbolScope::Members)).collect());
        assert_eq!(answers[0].value[0].name, "getOwner");
    }

    #[test]
    fn rows_of_two_sources_merge_without_repeats() {
        let row = |name: &str, score| SymbolRow {
            name: name.to_owned(),
            kind: SymbolKind::Class,
            container: None,
            target: Target::Uri(format!("jar:{name}")),
            score,
        };
        let merged = merge_rows([vec![row("A", 5), row("B", 9)], vec![row("A", 7)]], 10);
        let names: Vec<_> = merged
            .iter()
            .map(|row| (row.name.as_str(), row.score))
            .collect();
        assert_eq!(names, [("B", 9), ("A", 7)]);
    }
}
