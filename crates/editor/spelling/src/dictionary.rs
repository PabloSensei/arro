//! The words a check accepts: the built-in list of programmer words, the words of the
//! user, and the words of a hunspell dictionary of the system.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The words every project knows, compiled in: one per line, `#` starts a note.
const BUILTIN: &str = include_str!("builtin.txt");

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The dictionary the user shares between projects, in the settings folder.
pub const APPLICATION_DICTIONARY: &str = "spelling.txt";
/// The dictionary of one project.
pub const PROJECT_DICTIONARY: &str = "dictionary.txt";

pub fn project_dictionary(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(PROJECT_DICTIONARY)
}

pub fn application_dictionary(settings_dir: &Path) -> PathBuf {
    settings_dir.join(APPLICATION_DICTIONARY)
}

/// The words of one dictionary, in lower case for the lookup and as written for the
/// suggestions.
#[derive(Clone, Debug, Default)]
pub struct Dictionary {
    words: HashSet<String>,
    list: Vec<String>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in words of a programmer.
    pub fn builtin() -> Self {
        let mut dictionary = Self::new();
        dictionary.extend_lines(BUILTIN);
        dictionary
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn contains(&self, word: &str) -> bool {
        self.words.contains(&word.to_lowercase())
    }

    pub fn words(&self) -> impl Iterator<Item = &str> {
        self.list.iter().map(String::as_str)
    }

    pub fn insert(&mut self, word: &str) {
        let word = word.trim();
        if word.is_empty() || word.starts_with('#') {
            return;
        }
        let lower = word.to_lowercase();
        if self.words.insert(lower) {
            self.list.push(word.to_owned());
        }
    }

    /// Adds the words of a word list, one per line, where `#` starts a note.
    pub fn extend_lines(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            for word in line.split_whitespace() {
                self.insert(word);
            }
        }
    }

    /// Adds the words of a dictionary file. A file that cannot be read adds nothing.
    pub fn add_file(&mut self, path: &Path) -> io::Result<()> {
        let text = fs::read_to_string(path)?;
        self.extend_lines(&text);
        Ok(())
    }

    /// The words of a hunspell dictionary, its stems and the forms its simple suffix
    /// rules make. Anything more clever than a suffix rule is left out, so the check
    /// never accepts a form the file does not spell out.
    pub fn from_hunspell(dic: &str, aff: &str) -> Self {
        let affixes = Affixes::parse(aff);
        let mut dictionary = Self::new();
        for line in dic.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.chars().all(char::is_numeric) {
                continue;
            }
            let (stem, flags) = match line.split_once('/') {
                Some((stem, rest)) => {
                    let flags: Vec<char> = rest
                        .chars()
                        .take_while(|character| !character.is_whitespace())
                        .collect();
                    (stem.trim(), flags)
                }
                None => (line.split_whitespace().next().unwrap_or(line), Vec::new()),
            };
            if stem.is_empty() {
                continue;
            }
            dictionary.insert(stem);
            for flag in flags {
                for form in affixes.forms_of(stem, flag) {
                    dictionary.insert(&form);
                }
            }
        }
        dictionary
    }
}

/// The suffix rules of a hunspell `.aff` file, only the plain ones.
#[derive(Debug, Default)]
struct Affixes {
    suffixes: Vec<Suffix>,
}

#[derive(Clone, Debug)]
struct Suffix {
    flag: char,
    strip: String,
    add: String,
    condition: Condition,
}

impl Affixes {
    fn parse(aff: &str) -> Self {
        let mut suffixes = Vec::new();
        for line in aff.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 5 || parts[0] != "SFX" {
                continue;
            }
            // `SFX flag strip add condition`: the header line of a block has a count
            // where the condition goes, and is no rule.
            let flag = match parts[1].chars().next() {
                Some(flag) if parts[1].chars().count() == 1 => flag,
                _ => continue,
            };
            let strip = if parts[2] == "0" {
                String::new()
            } else {
                parts[2].to_owned()
            };
            let add = if parts[3] == "0" {
                String::new()
            } else {
                parts[3].to_owned()
            };
            let condition = Condition::parse(parts.get(4).copied().unwrap_or("."));
            suffixes.push(Suffix {
                flag,
                strip,
                add,
                condition,
            });
        }
        Self { suffixes }
    }

    fn forms_of(&self, stem: &str, flag: char) -> Vec<String> {
        let mut forms = Vec::new();
        for suffix in self.suffixes.iter().filter(|suffix| suffix.flag == flag) {
            if !suffix.condition.matches(stem) {
                continue;
            }
            let body = match stem.strip_suffix(suffix.strip.as_str()) {
                Some(body) => body,
                None => continue,
            };
            forms.push(format!("{body}{}", suffix.add));
        }
        forms
    }
}

/// The end of a stem a suffix rule asks for: literals, one-character groups and the
/// `[^…]` negation of a group.
#[derive(Clone, Debug, Default)]
struct Condition {
    parts: Vec<ConditionPart>,
}

#[derive(Clone, Debug)]
enum ConditionPart {
    Literal(char),
    Any,
    OneOf(Vec<char>, bool),
}

impl Condition {
    fn parse(text: &str) -> Self {
        let mut parts = Vec::new();
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '.' => parts.push(ConditionPart::Any),
                '[' => {
                    let mut set = Vec::new();
                    let mut negated = false;
                    if characters.peek() == Some(&'^') {
                        characters.next();
                        negated = true;
                    }
                    for inside in characters.by_ref() {
                        if inside == ']' {
                            break;
                        }
                        set.push(inside);
                    }
                    parts.push(ConditionPart::OneOf(set, negated));
                }
                _ => parts.push(ConditionPart::Literal(character)),
            }
        }
        Self { parts }
    }

    fn matches(&self, stem: &str) -> bool {
        let characters: Vec<char> = stem.chars().collect();
        if self.parts.len() > characters.len() {
            return false;
        }
        let start = characters.len() - self.parts.len();
        self.parts
            .iter()
            .zip(&characters[start..])
            .all(|(part, character)| match part {
                ConditionPart::Literal(wanted) => wanted == character,
                ConditionPart::Any => true,
                ConditionPart::OneOf(set, negated) => set.contains(character) != *negated,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Dictionary {
        Dictionary::from_hunspell(
            "3\ncalculate/S\nserver/S\nconfig/S\n",
            "SET UTF-8\nSFX S Y 3\nSFX S 0 s [^sxzh]\nSFX S 0 es [sxzh]\n\
             SFX S y ies [^aeiou]y\n",
        )
    }

    #[test]
    fn a_hunspell_dictionary_gives_its_stems_and_simple_suffix_forms() {
        let dictionary = rules();
        for word in [
            "calculate",
            "calculates",
            "server",
            "servers",
            "config",
            "configs",
        ] {
            assert!(dictionary.contains(word), "{word} is in the dictionary");
        }
        assert!(
            !dictionary.contains("calculaties"),
            "the rules add only the forms they name"
        );
        assert!(!dictionary.contains("serverss"));
    }

    #[test]
    fn a_dictionary_file_is_read_one_word_a_line() {
        let root = test_support::scratch("spelling-dictionary-lines");
        let path = root.join("words.txt");
        fs::write(&path, "# a note\ncalculate\n\n  calcualte  \nserver\n").unwrap();
        let mut dictionary = Dictionary::new();
        dictionary.add_file(&path).unwrap();
        assert!(dictionary.contains("Calculate"));
        assert!(dictionary.contains("server"));
        assert!(
            dictionary.contains("calcualte"),
            "a word of the list is kept as written"
        );
        assert!(!dictionary.contains("note"), "a comment line adds no word");
        assert_eq!(
            dictionary
                .add_file(&root.join("missing.txt"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn the_builtin_list_names_the_terms_of_a_programmer() {
        let dictionary = Dictionary::builtin();
        for word in [
            "impl", "args", "params", "config", "jdbc", "async", "json", "utf",
        ] {
            assert!(dictionary.contains(word), "{word} is a built-in word");
        }
        assert!(dictionary.len() > 100);
    }
}
