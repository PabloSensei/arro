//! Search by the shape of the code instead of its text: a template with variables that
//! stand for a name, an expression or a sequence of nodes, and the filters on them.

/// One variable of a template. An empty `count` means exactly one node; `text` is a
/// regular expression and a leading `!` inverts it; `type_pattern` is a regular
/// expression over the simple name of the type a language reads next to the place.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralVariable {
    pub name: String,
    pub text: Option<String>,
    pub count: Option<(usize, Option<usize>)>,
    pub type_pattern: Option<String>,
}

impl StructuralVariable {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// The filter as it is typed in the window: `text=`, `count=` and `type=` joined by
    /// commas, which the language parses back into this value.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(text) = &self.text {
            parts.push(format!("text={text}"));
        }
        if let Some((min, max)) = self.count {
            let max = max.map_or_else(|| "∞".to_owned(), |max| max.to_string());
            parts.push(format!("count=[{min},{max}]"));
        }
        if let Some(pattern) = &self.type_pattern {
            parts.push(format!("type={pattern}"));
        }
        parts.join(", ")
    }
}

/// A template and what the window narrowed it to. `replacement` is `None` for a search
/// that only lists the places.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralQuery {
    pub pattern: String,
    pub replacement: Option<String>,
    pub variables: Vec<StructuralVariable>,
}

/// One place the template matches. The offsets are bytes of the text that was asked
/// about; `text` and `replacement` are for the list and its preview. `notes` says what a
/// filter could not check, such as a type without a declaration nearby.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralMatch {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub replacement: Option<String>,
    pub notes: Vec<String>,
}

/// One edit of Replace Structurally. The edits of a file do not overlap.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralEdit {
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

/// A template the language offers ready-made, with the filters it sets on its variables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StructuralTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub pattern: String,
    pub replacement: Option<String>,
    pub variables: Vec<StructuralVariable>,
}

/// Structural search of one language, over the text of one file at a time. The window
/// walks the project and calls [`Self::find`] for every file in the background.
pub trait StructuralSearchProvider: Send + Sync {
    /// The templates of the language, in the order the window lists them.
    fn templates(&self) -> Vec<StructuralTemplate>;

    /// The variables of a template, in the order they first appear. An error is a
    /// template the language cannot parse.
    fn variables(&self, pattern: &str) -> Result<Vec<String>, String>;

    fn find(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralMatch>, String>;

    /// Replaces every match. The edits are sorted and do not overlap.
    fn replace(&self, text: &str, query: &StructuralQuery) -> Result<Vec<StructuralEdit>, String>;
}
