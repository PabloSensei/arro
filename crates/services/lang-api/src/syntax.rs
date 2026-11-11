use std::ops::Range;

use tree_sitter_language::LanguageFn;

/// Tree-sitter queries of one grammar. A language without folds leaves that query empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct Queries {
    pub highlights: &'static str,
    pub folds: &'static str,
    pub indents: &'static str,
    pub locals: &'static str,
}

/// One entry of Unwrap/Remove: what it does and the edit it applies. `start` and `end`
/// are byte offsets in the text that was asked about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnwrapEdit {
    pub title: String,
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeletionKind {
    Type,
    Method,
    Field,
    Parameter,
    Local,
}

/// The declaration under the caret, as Safe Delete needs it. `range` covers what is
/// removed with the declaration; `name_range` is the name the usages point at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deletion {
    pub kind: Option<DeletionKind>,
    pub name: String,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    /// The method around a parameter, and the name of that method.
    pub method: Option<Range<usize>>,
    pub method_name: Option<Range<usize>>,
    /// The place of the parameter in the signature of its method.
    pub parameter_index: Option<usize>,
    /// The file holds no other top-level type.
    pub sole_type: bool,
}

impl Deletion {
    pub fn kind(&self) -> DeletionKind {
        self.kind.unwrap_or(DeletionKind::Field)
    }
}

/// Highlighting, folding and indent. The runtime lives in the core, a language brings
/// only its grammar and queries. `LanguageFn` keeps this crate free of the C build.
pub trait SyntaxProvider: Send + Sync {
    fn grammar(&self) -> LanguageFn;
    fn queries(&self) -> &Queries;

    /// Ctrl+Shift+Delete: the constructs around `offset`, nearest first.
    fn unwrap_edits(&self, text: &str, offset: usize) -> Vec<UnwrapEdit> {
        let _ = (text, offset);
        Vec::new()
    }

    /// Alt+Delete: the declaration under `offset`, when there is one.
    fn deletion(&self, text: &str, offset: usize) -> Option<Deletion> {
        let _ = (text, offset);
        None
    }

    /// The offset of the first top-level type of the text, for Safe Delete on a file.
    fn first_type_offset(&self, text: &str) -> Option<usize> {
        let _ = text;
        None
    }
}
