//! What a language says about the spelling of its text: the pieces worth checking and
//! the names of the declarations, which a correction renames rather than edits.

use std::path::PathBuf;

/// The name of the check in the settings and in the code of its marks.
pub const SPELLING_INSPECTION: &str = "Spelling";
/// The title the settings and the menu of the check show.
pub const SPELLING_TITLE: &str = "Typo";
/// The group of the settings tree the check belongs to.
pub const SPELLING_GROUP: &str = "Spelling";

/// A piece of a text the check reads, by bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpellPlace {
    pub start: usize,
    pub end: usize,
    /// The place is the name of a declaration, so a correction renames the symbol.
    pub declaration: bool,
}

/// A document the language says the places of.
#[derive(Clone, Debug)]
pub struct SpellQuery {
    pub path: PathBuf,
    pub text: String,
}

/// The pieces of its text a language wants the words of checked.
pub trait SpellingProvider: Send + Sync {
    /// The places to check, or `None` for a file this language does not own: the editor
    /// then reads the text by the file name.
    fn places(&self, query: &SpellQuery) -> Option<Vec<SpellPlace>>;
}
