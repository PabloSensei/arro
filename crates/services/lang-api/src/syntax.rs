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

/// The identifiers of the lines a stopped debugger covers: from the first line of the
/// declaration around the stop through the stop line. Each inner list holds the names
/// of one line in the order they appear, without repeats.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdentifierLines {
    /// The line the list starts at, from zero.
    pub first_line: usize,
    pub lines: Vec<Vec<String>>,
}

/// Reads the words of `text` line by line, from the nearest line above `line` that opens
/// a brace to `line`. A language with a grammar overrides this with its own tree.
pub fn identifiers_around(text: &str, line: usize) -> IdentifierLines {
    let all: Vec<&str> = text.lines().collect();
    if all.is_empty() {
        return IdentifierLines::default();
    }
    let line = line.min(all.len() - 1);
    let first_line = (0..=line)
        .rev()
        .find(|row| {
            let trimmed = all[*row].trim_end();
            trimmed.ends_with('{') && trimmed.contains('(')
        })
        .unwrap_or(0);
    let lines = all[first_line..=line]
        .iter()
        .map(|text| words_of(text))
        .collect();
    IdentifierLines { first_line, lines }
}

/// The whole words of one line, left to right, without repeats.
fn words_of(line: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for character in line.chars() {
        if character.is_alphanumeric() || character == '_' || character == '$' {
            word.push(character);
            continue;
        }
        push_word(&mut words, &mut word);
    }
    push_word(&mut words, &mut word);
    words
}

fn push_word(words: &mut Vec<String>, word: &mut String) {
    if word.is_empty() {
        return;
    }
    let starts_with_digit = word
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit());
    if !starts_with_digit && !words.iter().any(|kept| kept == word) {
        words.push(word.clone());
    }
    word.clear();
}

/// Highlighting, folding and indent. The runtime lives in the core, a language brings
/// only its grammar and queries. `LanguageFn` keeps this crate free of the C build.
pub trait SyntaxProvider: Send + Sync {
    fn grammar(&self) -> LanguageFn;
    fn queries(&self) -> &Queries;

    /// The identifiers of the lines a stopped debugger shows values for, from the
    /// declaration around `line` through `line` itself.
    fn identifiers_around(&self, text: &str, line: usize) -> IdentifierLines {
        identifiers_around(text, line)
    }

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
