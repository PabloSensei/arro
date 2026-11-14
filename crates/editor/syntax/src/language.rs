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

/// Another language parsed inside the nodes of this one, such as the text of a
/// fenced block.
#[derive(Clone)]
pub struct Injection {
    /// The kind of node that holds the layer.
    pub node: &'static str,
    /// The kind of child whose text the layer parses; `None` parses all of `node`.
    pub text_node: Option<&'static str>,
    /// The kind of child whose text names the language; `None` uses `language`.
    pub name_node: Option<&'static str>,
    /// Children whose text is container syntax, not part of the layer.
    pub skip_nodes: &'static [&'static str],
    /// The language of every node when `name_node` is `None`.
    pub language: Option<Language>,
    /// The highlight query of `language`; a named layer brings the query of its config.
    pub highlights: Option<&'static str>,
    /// The language a name stands for; `None` when the layer has a fixed language.
    pub resolve: Option<fn(&str) -> Option<LanguageConfig>>,
    /// Whether the layer replaces the outer highlights of exactly its range.
    pub replace: bool,
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
    /// The languages parsed inside the nodes of this one.
    pub injections: Vec<Injection>,
}

impl LanguageConfig {
    /// The config where every named injection asks `resolve` for its language.
    pub fn with_injection_resolver(mut self, resolve: fn(&str) -> Option<LanguageConfig>) -> Self {
        for injection in &mut self.injections {
            if injection.name_node.is_some() {
                injection.resolve = Some(resolve);
            }
        }
        self
    }
}
