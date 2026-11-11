use std::ops::Range;
use std::path::Path;

/// Neutral kinds, close to the LSP ones. A language maps its own declarations onto them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolKind {
    Module,
    Class,
    Interface,
    Enum,
    Function,
    Method,
    Constructor,
    Field,
    Constant,
}

impl SymbolKind {
    /// Types, the kinds Go to Class looks for.
    pub fn is_type(self) -> bool {
        matches!(self, Self::Class | Self::Interface | Self::Enum)
    }
}

/// A declaration in a source file. Ranges are byte offsets into the text it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    /// The enclosing declarations joined by `.`, `None` at the top of the file.
    pub container: Option<String>,
    /// How many declarations enclose this one.
    pub depth: u32,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
}

impl Symbol {
    /// `Outer.Inner.name`.
    pub fn qualified_name(&self) -> String {
        match &self.container {
            Some(container) => format!("{container}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Declarations of one file, in the order they appear. Blocks, so callers run it off the
/// UI thread. Answers for a whole project come from the core's index over this.
pub trait SymbolProvider: Send + Sync {
    fn file_symbols(&self, path: &Path, text: &str) -> Vec<Symbol>;
}
