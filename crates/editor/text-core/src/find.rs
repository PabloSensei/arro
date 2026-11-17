//! Searching in the text of one document: plain, whole words, case and regular
//! expressions, and the edits that replace the matches.

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::edit::Edit;

/// More matches than this are not collected.
const MAX_MATCHES: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

fn pattern(query: &str, options: FindOptions) -> Result<Regex, String> {
    let source = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let source = if options.whole_word {
        format!(r"\b(?:{source})\b")
    } else {
        source
    };
    RegexBuilder::new(&source)
        .case_insensitive(!options.case_sensitive)
        .multi_line(true)
        .build()
        .map_err(|error| {
            let message = error.to_string();
            message
                .lines()
                .last()
                .unwrap_or("bad pattern")
                .trim()
                .to_owned()
        })
}

/// Every match of `query` in `text`, empty matches left out. An empty query finds nothing.
pub fn find_all(
    text: &str,
    query: &str,
    options: FindOptions,
) -> Result<Vec<Range<usize>>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let found = pattern(query, options)?
        .find_iter(text)
        .filter(|found| !found.is_empty())
        .take(MAX_MATCHES)
        .map(|found| found.range())
        .collect();
    Ok(found)
}

/// The edits that replace the matches in `only` (all when `None`) with `replacement`. With
/// regular expressions `$1` and `${name}` in it stand for groups.
pub fn replacements(
    text: &str,
    query: &str,
    options: FindOptions,
    replacement: &str,
    only: Option<&Range<usize>>,
) -> Result<Vec<Edit>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = pattern(query, options)?;
    let mut edits = Vec::new();
    for captures in pattern.captures_iter(text) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        if whole.is_empty() || only.is_some_and(|only| only != &whole.range()) {
            continue;
        }
        let mut replaced = String::new();
        if options.regex {
            captures.expand(replacement, &mut replaced);
        } else {
            replaced.push_str(replacement);
        }
        edits.push(Edit::new(whole.range(), replaced));
    }
    Ok(edits)
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    const WORDS: FindOptions = FindOptions {
        case_sensitive: false,
        whole_word: true,
        regex: false,
    };

    #[test]
    fn plain_search_ignores_case_and_treats_symbols_literally() {
        let found = find_all("Foo foo f.o", "FOO", FindOptions::default()).unwrap();
        assert_eq!(found, [0..3, 4..7]);
        let dots = find_all("f.o fxo", "f.o", FindOptions::default()).unwrap();
        assert_eq!(dots, [0..3]);
    }

    #[test]
    fn whole_words_skip_parts_of_longer_names() {
        let found = find_all("pet petal Pet_ pet", "pet", WORDS).unwrap();
        assert_eq!(found, [0..3, 15..18]);
    }

    #[test]
    fn case_can_matter() {
        let options = FindOptions {
            case_sensitive: true,
            ..FindOptions::default()
        };
        assert_eq!(find_all("Pet pet", "pet", options).unwrap(), [4..7]);
    }

    #[test]
    fn a_bad_regex_is_reported() {
        let options = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        assert!(find_all("a", "(", options).is_err());
        assert_eq!(find_all("a1 b22", r"\d+", options).unwrap(), [1..2, 4..6]);
    }

    #[test]
    fn regex_replacements_expand_groups() {
        let options = FindOptions {
            regex: true,
            ..FindOptions::default()
        };
        let edits = replacements("a1 b22", r"([a-z])(\d+)", options, "$2$1", None).unwrap();
        assert_eq!(edits, [Edit::new(0..2, "1a"), Edit::new(3..6, "22b")]);
        let one = replacements("a1 b22", r"([a-z])(\d+)", options, "x", Some(&(3..6))).unwrap();
        assert_eq!(one, [Edit::new(3..6, "x")]);
    }
}
