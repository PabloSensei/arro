//! What a language brings: its grammar, queries and comment token.

use tree_sitter::Language;

/// How a captured fold looks on screen. `keep` bytes of the node stay visible before
/// the placeholder.
#[derive(Clone, Copy, Debug)]
pub struct FoldStyle {
    pub capture: &'static str,
    pub placeholder: &'static str,
    pub keep: usize,
    /// Neighbouring nodes with this capture fold together.
    pub joined: bool,
    pub collapsed_on_open: bool,
}

/// What Complete Statement writes for a language.
#[derive(Clone, Copy, Debug)]
pub struct StatementRules {
    /// Ends a statement.
    pub terminator: &'static str,
    /// Words that open a statement with a block, such as `if` and `while`, and the
    /// declarations that have a body, such as `class`.
    pub block_words: &'static [&'static str],
}

#[derive(Clone)]
pub struct LanguageConfig {
    pub name: &'static str,
    pub grammar: Language,
    pub highlights: &'static str,
    pub folds: &'static str,
    pub indents: &'static str,
    pub fold_styles: &'static [FoldStyle],
    pub line_comment: Option<&'static str>,
    /// The opening and closing marks of a block comment.
    pub block_comment: Option<(&'static str, &'static str)>,
    /// What Complete Statement adds to a line.
    pub statements: Option<StatementRules>,
    pub indent_unit: &'static str,
    /// What Find in Files offers in its mask field for a project of this language.
    pub file_mask: &'static str,
}
