//! Declarations of the whole project, for Go to Class and Go to Symbol. Filled from the
//! languages' [`SymbolProvider`]s, kept current file by file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use base::Cancel;
use lang_api::{Symbol, SymbolProvider};

use crate::fuzzy::{FuzzyMatcher, NameMatcher, Ranked, rank};

/// A provider for the language of a path, `None` for files without symbols.
pub type ProviderFor<'a> = dyn Fn(&Path) -> Option<&'a dyn SymbolProvider> + Sync + 'a;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolMatch {
    pub path: PathBuf,
    pub symbol: Symbol,
}

#[derive(Clone, Debug, Default)]
pub struct SymbolIndex {
    files: HashMap<PathBuf, Arc<[Symbol]>>,
    /// The symbols of every file, kept as files change so that a reader learns the size
    /// without walking the map.
    symbols: usize,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, path: PathBuf, symbols: Vec<Symbol>) {
        let count = symbols.len();
        match self.files.insert(path, symbols.into()) {
            Some(previous) => {
                self.symbols -= previous.len();
                self.symbols += count;
            }
            None => self.symbols += count,
        }
    }

    pub fn remove(&mut self, path: &Path) {
        if let Some(previous) = self.files.remove(path) {
            self.symbols -= previous.len();
        }
    }

    /// Declarations of one file in source order, for the file structure.
    pub fn file(&self, path: &Path) -> Option<Arc<[Symbol]>> {
        self.files.get(path).cloned()
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// How many declarations the index keeps in all its files.
    pub fn symbol_count(&self) -> usize {
        self.symbols
    }

    /// An estimate of the bytes the symbols keep alive, for the memory panel.
    pub fn memory_bytes(&self) -> usize {
        let entries: usize = self
            .files
            .iter()
            .map(|(path, symbols)| {
                let symbol_bytes: usize = symbols
                    .iter()
                    .map(|symbol| {
                        std::mem::size_of::<Symbol>()
                            + symbol.name.len()
                            + symbol.container.as_ref().map_or(0, String::len)
                    })
                    .sum();
                std::mem::size_of::<PathBuf>() + path.as_os_str().len() + symbol_bytes
            })
            .sum();
        entries + std::mem::size_of::<HashMap<PathBuf, Arc<[Symbol]>>>()
    }

    /// The best `limit` symbols that `wanted` accepts.
    pub fn search(
        &self,
        pattern: &str,
        wanted: impl Fn(&Symbol) -> bool,
        limit: usize,
    ) -> Vec<Ranked<SymbolMatch>> {
        let candidates = self.files.iter().flat_map(|(path, symbols)| {
            symbols
                .iter()
                .filter(|symbol| wanted(symbol))
                .map(move |symbol| (path, symbol))
        });
        let mut matcher = NameMatcher::new();
        let matcher: &mut dyn FuzzyMatcher = &mut matcher;
        rank(
            matcher,
            pattern,
            candidates,
            |(_, symbol)| &symbol.name,
            limit,
        )
        .into_iter()
        .map(|ranked| Ranked {
            item: SymbolMatch {
                path: ranked.item.0.clone(),
                symbol: ranked.item.1.clone(),
            },
            score: ranked.score,
            kind: ranked.kind,
        })
        .collect()
    }
}

/// How many threads reading files may take: leave one core to the UI.
pub fn pool_size() -> usize {
    let cores = thread::available_parallelism().map_or(2, |cores| cores.get());
    cores.saturating_sub(1).clamp(1, 8)
}

/// Reads and parses `paths` on a pool of threads. Blocks; runs off the UI thread. A file
/// that cannot be read is skipped, the watcher brings it back when it changes.
pub fn index_files(
    paths: &[PathBuf],
    provider_for: &ProviderFor,
    cancel: &Cancel,
) -> Vec<(PathBuf, Vec<Symbol>)> {
    let next = AtomicUsize::new(0);
    let found = Mutex::new(Vec::with_capacity(paths.len()));
    thread::scope(|scope| {
        for _ in 0..pool_size().min(paths.len()) {
            scope.spawn(|| {
                while !cancel.is_cancelled() {
                    let Some(path) = paths.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let Some(symbols) = file_symbols(path, provider_for) else {
                        continue;
                    };
                    found
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push((path.clone(), symbols));
                }
            });
        }
    });
    found
        .into_inner()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// One file from disk. `None` when the file has no language with symbols or is unreadable.
pub fn file_symbols(path: &Path, provider_for: &ProviderFor) -> Option<Vec<Symbol>> {
    let provider = provider_for(path)?;
    let file = text_core::read_file(path).ok()?;
    Some(provider.file_symbols(path, &file.text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            kind: lang_api::SymbolKind::Class,
            container: None,
            depth: 0,
            range: 0..name.len(),
            name_range: 0..name.len(),
        }
    }

    #[test]
    fn the_count_follows_updated_and_removed_files() {
        let mut index = SymbolIndex::new();
        assert_eq!(index.symbol_count(), 0);
        index.update(
            PathBuf::from("/p/A.src"),
            vec![symbol("Owner"), symbol("Pet")],
        );
        index.update(PathBuf::from("/p/B.src"), vec![symbol("Vet")]);
        assert_eq!(index.symbol_count(), 3);

        index.update(PathBuf::from("/p/A.src"), vec![symbol("Owner")]);
        assert_eq!(index.symbol_count(), 2);

        index.remove(Path::new("/p/A.src"));
        assert_eq!(index.symbol_count(), 1);
        index.remove(Path::new("/p/missing.src"));
        assert_eq!(index.symbol_count(), 1);
    }
}
