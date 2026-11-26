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
    /// A replacement takes the case of the text it replaces.
    pub preserve_case: bool,
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

/// The part of `text` that `range` covers, moved to the characters of the text.
fn inside(text: &str, range: &Range<usize>) -> Range<usize> {
    let mut start = range.start.min(text.len());
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = range.end.max(start).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    start..end
}

/// Every match of `query` in `text`, empty matches left out. An empty query finds nothing.
pub fn find_all(
    text: &str,
    query: &str,
    options: FindOptions,
) -> Result<Vec<Range<usize>>, String> {
    find_in_range(text, query, options, 0..text.len())
}

/// Every match of `query` inside `range` of `text`, with the offsets of the whole text.
/// A match that begins before the range does not count, as in Find in Selection.
pub fn find_in_range(
    text: &str,
    query: &str,
    options: FindOptions,
    range: Range<usize>,
) -> Result<Vec<Range<usize>>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let range = inside(text, &range);
    let found = pattern(query, options)?
        .find_iter(&text[range.clone()])
        .filter(|found| !found.is_empty())
        .take(MAX_MATCHES)
        .map(|found| range.start + found.start()..range.start + found.end())
        .collect();
    Ok(found)
}

/// `replacement` in the case of `found`: an all-capitals word keeps the whole replacement
/// in capitals, a capital first letter capitalises the first letter of the replacement,
/// a lower one lowercases it.
pub fn preserve_case(found: &str, replacement: &str) -> String {
    let mut letters = found.chars().filter(|c| c.is_alphabetic());
    let Some(first) = letters.next() else {
        return replacement.to_owned();
    };
    if first.is_uppercase() && letters.all(|c| c.is_uppercase()) {
        return replacement.to_uppercase();
    }
    let mut chars = replacement.chars();
    let Some(head) = chars.next() else {
        return String::new();
    };
    let head: String = if first.is_uppercase() {
        head.to_uppercase().collect()
    } else {
        head.to_lowercase().collect()
    };
    let mut result = head;
    result.push_str(chars.as_str());
    result
}

/// `replacement` with `$1` and `${name}` standing for groups, as IDEA writes it: a group
/// number is the digits after `$` and the rest of the name stays literal, so `$1_old`
/// is the first group and `_old`. `$$` is one dollar sign.
fn expand_replacement(captures: &regex::Captures<'_>, replacement: &str, out: &mut String) {
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        if let Some(after) = rest.strip_prefix('$') {
            out.push('$');
            rest = after;
        } else if let Some(after) = rest.strip_prefix('{') {
            let Some(close) = after.find('}') else {
                out.push('$');
                rest = after;
                continue;
            };
            push_group(captures, &after[..close], out);
            rest = &after[close + 1..];
        } else {
            let end = if rest.starts_with(|c: char| c.is_ascii_digit()) {
                rest.find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len())
            } else {
                rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len())
            };
            if end == 0 {
                out.push('$');
            } else {
                push_group(captures, &rest[..end], out);
                rest = &rest[end..];
            }
        }
    }
    out.push_str(rest);
}

/// The text of one capture, by number or by name; a group that does not exist is empty.
fn push_group(captures: &regex::Captures<'_>, name: &str, out: &mut String) {
    let found = match name.parse::<usize>() {
        Ok(index) => captures.get(index),
        Err(_) => captures.name(name),
    };
    if let Some(found) = found {
        out.push_str(found.as_str());
    }
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
    replacements_in(text, query, options, replacement, only, None)
}

/// [`replacements`] restricted to `within` when it is given, as Replace All with Find in
/// Selection does. The offsets of the edits are those of the whole text.
pub fn replacements_in(
    text: &str,
    query: &str,
    options: FindOptions,
    replacement: &str,
    only: Option<&Range<usize>>,
    within: Option<&Range<usize>>,
) -> Result<Vec<Edit>, String> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = pattern(query, options)?;
    let (searched, offset) = match within {
        Some(range) => {
            let range = inside(text, range);
            (&text[range.clone()], range.start)
        }
        None => (text, 0),
    };
    let mut edits = Vec::new();
    for captures in pattern.captures_iter(searched) {
        let Some(whole) = captures.get(0) else {
            continue;
        };
        if whole.is_empty() {
            continue;
        }
        let range = offset + whole.start()..offset + whole.end();
        if only.is_some_and(|only| only != &range) {
            continue;
        }
        let mut replaced = String::new();
        if options.regex {
            expand_replacement(&captures, replacement, &mut replaced);
        } else {
            replaced.push_str(replacement);
        }
        if options.preserve_case {
            replaced = preserve_case(&text[range.clone()], &replaced);
        }
        edits.push(Edit::new(range, replaced));
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
        preserve_case: false,
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

    #[test]
    fn preserve_case_takes_the_case_of_the_found_text() {
        assert_eq!(preserve_case("foo", "bar"), "bar");
        assert_eq!(preserve_case("Foo", "bar"), "Bar");
        assert_eq!(preserve_case("FOO", "bar"), "BAR");
        // A mixed word follows its first letter.
        assert_eq!(preserve_case("fooBar", "Baz"), "baz");
        assert_eq!(preserve_case("FooBar", "baz"), "Baz");
        assert_eq!(preserve_case("123", "bar"), "bar");
    }

    #[test]
    fn preserve_case_applies_to_an_expanded_regex() {
        let options = FindOptions {
            regex: true,
            preserve_case: true,
            ..FindOptions::default()
        };
        let edits = replacements(
            "foo(1) Foo(2) FOO(3)",
            r"foo\((\d)\)",
            options,
            "bar$1",
            None,
        )
        .unwrap();
        assert_eq!(
            edits,
            [
                Edit::new(0..6, "bar1"),
                Edit::new(7..13, "Bar2"),
                Edit::new(14..20, "BAR3"),
            ]
        );
    }

    #[test]
    fn a_search_stays_inside_a_range() {
        let text = "alpha\nbeta\nalpha\nalpha\n";
        let found = find_in_range(text, "alpha", FindOptions::default(), 0..17).unwrap();
        assert_eq!(
            found,
            [0..5, 11..16],
            "the match outside the range is left out"
        );

        let edits = replacements_in(
            text,
            "alpha",
            FindOptions::default(),
            "omega",
            None,
            Some(&(0..17)),
        )
        .unwrap();
        assert_eq!(
            edits,
            [Edit::new(0..5, "omega"), Edit::new(11..16, "omega")]
        );
    }
}
