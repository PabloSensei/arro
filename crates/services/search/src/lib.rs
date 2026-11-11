//! Search without an index: fuzzy names for Go to File, Class and Symbol and for Search
//! Everywhere, text through the ripgrep libraries for Find and Replace in Files. Nothing
//! here knows a language: symbols come from the languages' providers.

mod everywhere;
mod files;
mod fuzzy;
mod source;
mod symbols;
mod text;

pub use everywhere::{ActionItem, Found, Group, Sources, Tab, search_everywhere};
pub use files::{GotoQuery, rank_files};
pub use fuzzy::{FuzzyMatcher, NameMatcher, Ranked, rank};
pub use source::{
    IndexedSymbols, NATIVE, SymbolRequest, SymbolRow, SymbolScope, SymbolSource, Target, merge_rows,
};
pub use symbols::{ProviderFor, SymbolIndex, SymbolMatch, file_symbols, index_files, pool_size};
pub use text::{
    FileHits, LineHit, SearchError, TextQuery, find_in_files, find_in_selection, replace_edits,
};
