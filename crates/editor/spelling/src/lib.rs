//! Spelling of the text in the editor: the words worth checking, the dictionaries that
//! judge them, and the corrections a menu offers. Nothing here knows a programming
//! language: a language names the pieces of its text, this crate judges the words.

mod dictionary;
mod distance;
mod regions;
mod split;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

pub use dictionary::{
    APPLICATION_DICTIONARY, Dictionary, PROJECT_DICTIONARY, application_dictionary,
    project_dictionary,
};
pub use distance::{distance, match_case, nearest};
pub use regions::{Format, format_of, regions as format_regions};
pub use split::{Word, checkable, split_identifier, words, words_between};

/// The language the check asks the system about.
pub const LANGUAGE: &str = "en-US";
/// How many corrections the menu offers for one word.
pub const MAX_SUGGESTIONS: usize = 5;

/// The hunspell dictionaries a Unix system keeps, the first that exists.
const HUNSPELL: &[&str] = &[
    "/usr/share/hunspell/en_US.dic",
    "/usr/share/myspell/en_US.dic",
    "/usr/share/myspell/dicts/en_US.dic",
];
/// The plain word list of a Unix system, for a machine without hunspell.
const WORD_LISTS: &[&str] = &["/usr/share/dict/words", "/usr/share/dict/american-english"];

/// A piece of a text the check reads. A declaration is the name of something, so a
/// correction of its word renames the symbol rather than editing one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub range: Range<usize>,
    pub declaration: bool,
}

/// A word the dictionary does not know, and what it could be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Misspelling {
    pub range: Range<usize>,
    pub word: String,
    pub suggestions: Vec<String>,
    pub declaration: bool,
}

/// Where the words of the check come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The spell checker of the operating system, by the language it checks.
    System(String),
    /// A hunspell dictionary of the system.
    Hunspell(PathBuf),
    /// A plain word list of the system.
    Words(PathBuf),
    /// The words the caller gave, for a test.
    Given,
    /// Nothing on this machine: the check is off.
    None,
}

impl Source {
    /// The line the Spelling page of the settings shows.
    pub fn line(&self) -> String {
        match self {
            Source::System(language) => format!("System spell checker ({language})"),
            Source::Hunspell(path) => format!("hunspell: {}", path.display()),
            Source::Words(path) => format!("words: {}", path.display()),
            Source::Given => "Given words".to_owned(),
            Source::None => "No dictionary found: spell checking is off".to_owned(),
        }
    }
}

/// The dictionaries of one window: the built-in words, the words of the user and the
/// words of the system. Shared between the editors and the background checks, so a
/// word the user saved is known everywhere at once.
pub struct Speller {
    dictionary: RwLock<Dictionary>,
    /// The words of the user's own dictionary file, as the settings list them.
    user: RwLock<Vec<String>>,
    source: Source,
}

impl Speller {
    /// Reads the dictionaries of the user and asks the system what it has.
    pub fn load(language: &str, settings_dir: &Path, root: &Path) -> Self {
        let mut dictionary = Dictionary::builtin();
        let application = application_dictionary(settings_dir);
        let user = read_words(&application);
        for word in &user {
            dictionary.insert(word);
        }
        let _ = dictionary.add_file(&project_dictionary(root));
        let source = if platform::spelling_supported(language) {
            Source::System(language.to_owned())
        } else {
            load_system_dictionary(&mut dictionary).unwrap_or(Source::None)
        };
        Self {
            dictionary: RwLock::new(dictionary),
            user: RwLock::new(user),
            source,
        }
    }

    /// A check with no source of words: every word passes, and the settings say the
    /// check is off. The window tests start here and give their own words.
    pub fn off() -> Self {
        Self {
            dictionary: RwLock::new(Dictionary::builtin()),
            user: RwLock::new(Vec::new()),
            source: Source::None,
        }
    }

    /// A check over the given words alone, for a test or a user who wants only their
    /// own dictionary.
    pub fn with_words(words: impl IntoIterator<Item = String>) -> Self {
        let mut dictionary = Dictionary::builtin();
        for word in words {
            dictionary.insert(&word);
        }
        Self {
            dictionary: RwLock::new(dictionary),
            user: RwLock::new(Vec::new()),
            source: Source::Given,
        }
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// The line the Spelling page shows about the source of the words.
    pub fn source_line(&self) -> String {
        self.source.line()
    }

    /// Whether asking this machine about a word can answer at all.
    pub fn enabled(&self) -> bool {
        self.source != Source::None
    }

    pub fn add_word(&self, word: &str) -> bool {
        let mut dictionary = self
            .dictionary
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        let before = dictionary.len();
        dictionary.insert(word);
        dictionary.len() != before
    }

    /// The words of the user's own dictionary, as the settings list them.
    pub fn user_words(&self) -> Vec<String> {
        self.user
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Whether `word` is spelled right: a word of a dictionary, or a word the system
    /// accepts. A machine without any dictionary accepts every word.
    pub fn check_word(&self, word: &str) -> bool {
        {
            let dictionary = self
                .dictionary
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            if dictionary.contains(word) {
                return true;
            }
        }
        match &self.source {
            Source::System(language) => platform::spelling_check(language, word).unwrap_or(true),
            Source::Hunspell(_) | Source::Words(_) | Source::Given => false,
            Source::None => true,
        }
    }

    /// The corrections of `word`, nearest first and at most [`MAX_SUGGESTIONS`].
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut suggestions = Vec::new();
        if let Source::System(language) = &self.source {
            suggestions.extend(platform::spelling_suggest(language, word, MAX_SUGGESTIONS));
        }
        if suggestions.len() < MAX_SUGGESTIONS {
            let dictionary = self
                .dictionary
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            suggestions.extend(nearest(
                word,
                dictionary.words(),
                MAX_SUGGESTIONS - suggestions.len(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        suggestions.retain(|suggestion| {
            let lower = suggestion.to_lowercase();
            lower != word.to_lowercase() && seen.insert(lower)
        });
        suggestions.truncate(MAX_SUGGESTIONS);
        suggestions
            .into_iter()
            .map(|suggestion| match_case(&suggestion, word))
            .collect()
    }

    /// The words of `text` that the dictionaries do not know, with their corrections.
    pub fn check(&self, text: &str, regions: &[Region]) -> Vec<Misspelling> {
        if !self.enabled() {
            return Vec::new();
        }
        let mut found: Vec<Misspelling> = Vec::new();
        for region in regions {
            let start = region.range.start.min(text.len());
            let end = region.range.end.min(text.len());
            if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            let piece = &text[start..end];
            let words = if region.declaration {
                split_identifier(piece, start)
            } else {
                words_between(piece, start)
            };
            for word in words {
                if !checkable(&word.text) || self.check_word(&word.text) {
                    continue;
                }
                found.push(Misspelling {
                    range: word.range(),
                    word: word.text.clone(),
                    suggestions: self.suggest(&word.text),
                    declaration: region.declaration,
                });
            }
        }
        found.sort_by_key(|misspelling| misspelling.range.start);
        found.dedup_by(|left, right| left.range == right.range);
        found
    }
}

/// The words of a dictionary file, without its notes.
fn read_words(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut dictionary = Dictionary::new();
    dictionary.extend_lines(&text);
    dictionary.words().map(str::to_owned).collect()
}

/// The biggest dictionary the system has, or nothing when the machine has no words for
/// the check.
fn load_system_dictionary(dictionary: &mut Dictionary) -> Option<Source> {
    for path in HUNSPELL {
        let dic = Path::new(path);
        let aff = dic.with_extension("aff");
        let (Ok(words), Ok(rules)) = (std::fs::read_to_string(dic), std::fs::read_to_string(&aff))
        else {
            continue;
        };
        let hunspell = Dictionary::from_hunspell(&words, &rules);
        for word in hunspell.words() {
            dictionary.insert(word);
        }
        return Some(Source::Hunspell(dic.to_path_buf()));
    }
    for path in WORD_LISTS {
        let file = Path::new(path);
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        dictionary.extend_lines(&text);
        return Some(Source::Words(file.to_path_buf()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speller() -> Speller {
        Speller::with_words([
            "calculate".to_owned(),
            "server".to_owned(),
            "total".to_owned(),
            "impl".to_owned(),
        ])
    }

    fn regions_of(text: &str) -> Vec<Region> {
        vec![Region {
            range: 0..text.len(),
            declaration: false,
        }]
    }

    #[test]
    fn a_wrong_word_gets_its_nearest_correction_first() {
        let speller = speller();
        let text = "calcualte server";
        let found = speller.check(text, &regions_of(text));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].word, "calcualte");
        assert_eq!(
            found[0].suggestions.first().map(String::as_str),
            Some("calculate")
        );
        assert!(found[0].suggestions.len() <= MAX_SUGGESTIONS);
    }

    #[test]
    fn a_word_of_the_builtin_list_is_no_typo() {
        let speller = Speller::with_words(Vec::new());
        assert!(
            speller
                .check(
                    "impl args params config jdbc async json utf",
                    &regions_of("impl args params config jdbc async json utf")
                )
                .is_empty()
        );
    }

    #[test]
    fn a_declaration_reports_every_hump_of_its_name() {
        let speller = speller();
        let text = "calcualteTotal";
        let found = speller.check(
            text,
            &[Region {
                range: 0..text.len(),
                declaration: true,
            }],
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].range, 0..9);
        assert!(found[0].declaration);
    }

    #[test]
    fn a_capital_of_the_word_stays_on_the_correction() {
        let speller = speller();
        let text = "Calcualte Server";
        let found = speller.check(text, &regions_of(text));
        assert_eq!(
            found[0].suggestions.first().map(String::as_str),
            Some("Calculate")
        );
    }

    #[test]
    fn the_user_dictionaries_are_read_and_added() {
        let root = test_support::scratch("spelling-user-dictionaries");
        let settings = root.join("settings");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::write(application_dictionary(&settings), "calcualte\n").unwrap();
        std::fs::create_dir_all(root.join(".arro")).unwrap();
        std::fs::write(project_dictionary(&root), "projet\n").unwrap();
        let speller = Speller::load(LANGUAGE, &settings, &root);
        assert_eq!(
            speller.user_words(),
            ["calcualte"],
            "the page lists the user's words"
        );
        assert!(
            speller.check_word("projet"),
            "the project dictionary is read too"
        );
        assert!(speller.add_word("antwort"));
        assert!(speller.check_word("antwort"));
    }
}
