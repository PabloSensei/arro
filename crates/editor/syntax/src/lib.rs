//! tree-sitter runtime: parses on its own thread, hands out immutable results. The
//! language comes from outside as a grammar and its queries.

mod highlight;
mod language;
mod outline;
mod service;
mod symbols;

pub use highlight::{HighlightKind, HighlightSpan, normalize_spans, shift_spans};
pub use language::{FoldStyle, LanguageConfig, StatementRules};
pub use outline::{FoldRange, Region, custom_regions, enclosing_range, indent_after, region_folds};
pub use service::{DocumentId, SyntaxService, SyntaxSnapshot};
pub use symbols::TagSymbols;
pub use tree_sitter::{Language, Query, Tree};
