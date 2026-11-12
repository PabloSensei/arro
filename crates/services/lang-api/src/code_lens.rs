//! Labels a language puts above a declaration, such as how many places use it. A label
//! is a row of its own: no byte of the text stands for it.
//!
//! A server may send a label without its command and resolve the command later, for the
//! labels the editor shows. The entry keeps the answer of the server, so that the resolve
//! request sends it back unchanged.

use crate::{Provider, TextSpan};

/// What a label tells, when the language sorts its labels for Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CodeLensKind {
    /// A label Settings says nothing about.
    #[default]
    Other,
    /// The places that use the declaration.
    Usages,
    /// The types or methods that implement the declaration.
    Implementations,
    /// The types that inherit the declaration; shown with the implementations.
    Inheritors,
}

/// The command a click on a label runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeLensCommand {
    pub title: String,
    pub command: String,
    /// The arguments of the command as JSON, as the server sent them.
    pub arguments: Option<String>,
}

/// One label above a declaration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeLensEntry {
    /// The range of the declaration the label speaks about.
    pub span: TextSpan,
    /// The text of the label; empty until the server resolves it.
    pub title: String,
    pub kind: CodeLensKind,
    /// The label as the server sent it, for [`CodeLensResolveQuery`].
    pub resolve: Option<String>,
    /// The command a click runs.
    pub command: Option<CodeLensCommand>,
}

/// The labels of a whole document. Code lenses cover every declaration, so the editor
/// asks for the document and keeps the visible rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLensQuery {
    pub uri: String,
    pub version: i32,
}

/// One label the editor shows, sent back to the server for its command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLensResolveQuery {
    pub uri: String,
    pub version: i32,
    pub lens: CodeLensEntry,
}

/// Labels above the declarations of a document.
pub type CodeLensProvider = dyn Provider<CodeLensQuery, Vec<CodeLensEntry>>;

/// Commands of the labels the editor shows.
pub type CodeLensResolveProvider = dyn Provider<CodeLensResolveQuery, CodeLensEntry>;

/// Which labels Settings shows above the declarations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeVisionSettings {
    pub usages: bool,
    pub implementations: bool,
}

impl Default for CodeVisionSettings {
    fn default() -> Self {
        Self {
            usages: true,
            implementations: true,
        }
    }
}

/// The kind of a label from its title and the command of its click. The words are the
/// usual names of these counts, not those of one language.
pub fn kind_of(title: &str, command: Option<&str>) -> CodeLensKind {
    let text = format!("{} {}", title, command.unwrap_or_default()).to_ascii_lowercase();
    if text.contains("inheritor") {
        CodeLensKind::Inheritors
    } else if text.contains("implementation") || text.contains("subtype") {
        CodeLensKind::Implementations
    } else if text.contains("usage") || text.contains("reference") {
        CodeLensKind::Usages
    } else {
        CodeLensKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::{CodeLensKind, kind_of};

    #[test]
    fn the_words_of_a_label_name_what_it_counts() {
        assert_eq!(kind_of("2 usages", None), CodeLensKind::Usages);
        assert_eq!(
            kind_of("1 reference", Some("lang.show.references")),
            CodeLensKind::Usages
        );
        assert_eq!(
            kind_of("", Some("lang.show.implementations")),
            CodeLensKind::Implementations
        );
        assert_eq!(kind_of("1 inheritor", None), CodeLensKind::Inheritors);
        assert_eq!(kind_of("Run", Some("lang.run")), CodeLensKind::Other);
    }
}
